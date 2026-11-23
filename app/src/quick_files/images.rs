use std::fs::{self, File};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex};
use std::time::Instant;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, ImageEncoder, ImageFormat, ImageReader, Limits};
use tauri::AppHandle;
use walkdir::WalkDir;

use super::{ext_lower, is_cancelled, unique_path, ProgressReporter, QuickFileReport, IO_BUF_SIZE};

const MAX_IMAGE_ALLOC_BYTES: u64 = 512 * 1024 * 1024;
const MAX_IMAGE_DIM: u32 = 32_768;

pub(crate) fn is_image_ext(ext: &str) -> bool {
    matches!(
        ext,
        "jpg" | "jpeg" | "png" | "webp" | "bmp" | "gif" | "tif" | "tiff"
    )
}

pub(crate) fn dir_has_only_images(dir: &Path) -> bool {
    let mut has_image = false;
    for entry in WalkDir::new(dir).into_iter().flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "thumbs.db" | "desktop.ini" | ".ds_store"
        ) {
            continue;
        }
        if is_image_ext(&ext_lower(p)) {
            has_image = true;
        } else {
            return false;
        }
    }
    has_image
}

fn read_jpeg_exif_orientation(path: &Path) -> u16 {
    let Ok(mut f) = File::open(path) else { return 1 };
    let mut buf = vec![0u8; 64 * 1024];
    let Ok(n) = io::Read::read(&mut f, &mut buf) else { return 1 };
    let data = &buf[..n];
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return 1;
    }
    let mut i = 2;
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            break;
        }
        let marker = data[i + 1];
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        if marker == 0x00 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let seg_len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if seg_len < 2 || i + 2 + seg_len > data.len() {
            break;
        }
        if marker == 0xE1 && seg_len >= 14 && &data[i + 4..i + 10] == b"Exif\0\0" {
            let tiff = &data[i + 10..i + 2 + seg_len];
            if tiff.len() < 8 {
                return 1;
            }
            let le = match &tiff[0..2] {
                b"II" => true,
                b"MM" => false,
                _ => return 1,
            };
            let u16_at = |off: usize| -> Option<u16> {
                let b = tiff.get(off..off + 2)?;
                Some(if le {
                    u16::from_le_bytes([b[0], b[1]])
                } else {
                    u16::from_be_bytes([b[0], b[1]])
                })
            };
            let u32_at = |off: usize| -> Option<u32> {
                let b = tiff.get(off..off + 4)?;
                Some(if le {
                    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
                } else {
                    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
                })
            };
            let Some(ifd0) = u32_at(4).map(|v| v as usize) else { return 1 };
            let Some(count) = u16_at(ifd0).map(|v| v as usize) else { return 1 };
            for idx in 0..count {
                let entry = ifd0 + 2 + idx * 12;
                if u16_at(entry) == Some(0x0112) {
                    return u16_at(entry + 8).unwrap_or(1);
                }
            }
            return 1;
        }
        i += 2 + seg_len;
    }
    1
}

fn apply_exif_orientation(img: DynamicImage, orientation: u16) -> DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

fn has_transparent_pixels(img: &DynamicImage) -> bool {
    match img {
        DynamicImage::ImageRgba8(buf) => buf.pixels().any(|p| p.0[3] < 255),
        DynamicImage::ImageLumaA8(buf) => buf.pixels().any(|p| p.0[1] < 255),
        _ if !img.color().has_alpha() => false,
        _ => img.to_rgba8().pixels().any(|p| p.0[3] < 255),
    }
}

fn decode_image_safe(path: &Path) -> Result<(DynamicImage, ImageFormat), String> {
    let mut reader = ImageReader::open(path)
        .map_err(|e| format!("не удалось открыть {}: {e}", path.display()))?
        .with_guessed_format()
        .map_err(|e| format!("не удалось определить формат {}: {e}", path.display()))?;

    let fmt = reader.format().unwrap_or(ImageFormat::Png);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIM);
    limits.max_image_height = Some(MAX_IMAGE_DIM);
    limits.max_alloc = Some(MAX_IMAGE_ALLOC_BYTES);
    reader.limits(limits);

    let mut img = reader
        .decode()
        .map_err(|e| format!("ошибка декодирования {}: {e}", path.display()))?;
    if fmt == ImageFormat::Jpeg {
        let orient = read_jpeg_exif_orientation(path);
        img = apply_exif_orientation(img, orient);
    }
    Ok((img, fmt))
}

fn encode_image(
    img: &DynamicImage,
    out_path: &Path,
    target_fmt: ImageFormat,
    quality: u8,
) -> Result<(), String> {
    let file = File::create(out_path)
        .map_err(|e| format!("не удалось создать {}: {e}", out_path.display()))?;
    let mut writer = BufWriter::with_capacity(IO_BUF_SIZE, file);

    match target_fmt {
        ImageFormat::Jpeg => {
            let rgb = img.to_rgb8();
            let enc = JpegEncoder::new_with_quality(&mut writer, quality.clamp(30, 95));
            enc.write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )
            .map_err(|e| format!("ошибка JPEG-кодирования: {e}"))?;
        }
        ImageFormat::Png => {
            if has_transparent_pixels(img) {
                let rgba = img.to_rgba8();
                let enc = PngEncoder::new_with_quality(
                    &mut writer,
                    CompressionType::Default,
                    PngFilter::Adaptive,
                );
                enc.write_image(
                    rgba.as_raw(),
                    rgba.width(),
                    rgba.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|e| format!("ошибка PNG-кодирования: {e}"))?;
            } else {
                let rgb = img.to_rgb8();
                let enc = PngEncoder::new_with_quality(
                    &mut writer,
                    CompressionType::Default,
                    PngFilter::Adaptive,
                );
                enc.write_image(
                    rgb.as_raw(),
                    rgb.width(),
                    rgb.height(),
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(|e| format!("ошибка PNG-кодирования: {e}"))?;
            }
        }
        ImageFormat::WebP => {
            img.write_to(&mut writer, ImageFormat::WebP)
                .map_err(|e| format!("ошибка WebP-кодирования: {e}"))?;
        }
        other => {
            img.write_to(&mut writer, other)
                .map_err(|e| format!("ошибка кодирования: {e}"))?;
        }
    }

    io::Write::flush(&mut writer).map_err(|e| format!("ошибка записи: {e}"))?;
    Ok(())
}

struct ImageTask {
    src: PathBuf,
    out_dir: PathBuf,
    clean_name: bool,
    size: u64,
}

fn process_one_image(
    task: &ImageTask,
    mode: &str,
    max_dim: u32,
    quality: u8,
    path_lock: &Mutex<()>,
) -> Result<(u64, u64, String, PathBuf), String> {
    let path = &task.src;
    let file_size = task.size;
    fs::create_dir_all(&task.out_dir)
        .map_err(|e| format!("не удалось создать {}: {e}", task.out_dir.display()))?;

    let (mut img, orig_fmt) = decode_image_safe(path)?;
    let resized =
        max_dim > 0 && mode != "strip" && (img.width() > max_dim || img.height() > max_dim);
    if resized {
        let filter = if img.width() > max_dim * 2 || img.height() > max_dim * 2 {
            FilterType::Triangle
        } else {
            FilterType::CatmullRom
        };
        img = img.resize(max_dim, max_dim, filter);
    }

    let (target_fmt, out_ext, suffix) = match mode {
        "webp" => (ImageFormat::WebP, "webp", ""),
        "jpg" | "jpeg" => (ImageFormat::Jpeg, "jpg", ""),
        "strip" => {
            let fmt = match orig_fmt {
                ImageFormat::Jpeg => ImageFormat::Jpeg,
                ImageFormat::WebP => ImageFormat::WebP,
                _ => ImageFormat::Png,
            };
            let e = match fmt {
                ImageFormat::Jpeg => "jpg",
                ImageFormat::WebP => "webp",
                _ => "png",
            };
            (fmt, e, if task.clean_name { "" } else { "_clean" })
        }
        _ => match orig_fmt {
            ImageFormat::Jpeg | ImageFormat::Bmp | ImageFormat::Tiff => {
                (ImageFormat::Jpeg, "jpg", if task.clean_name { "" } else { "_opt" })
            }
            ImageFormat::WebP => {
                (ImageFormat::WebP, "webp", if task.clean_name { "" } else { "_opt" })
            }
            _ => (ImageFormat::Png, "png", if task.clean_name { "" } else { "_opt" }),
        },
    };

    let parent = &task.out_dir;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image");
    let out_stem = format!("{stem}{suffix}");

    let mut out_path = {
        let _g = path_lock.lock().map_err(|e| e.to_string())?;
        let p = unique_path(parent, &out_stem, out_ext);
        File::create(&p).map_err(|e| format!("не удалось создать {}: {e}", p.display()))?;
        p
    };

    encode_image(&img, &out_path, target_fmt, quality)?;
    let mut out_size = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);

    if matches!(mode, "auto" | "compress") && out_size >= file_size {
        if target_fmt == ImageFormat::Jpeg && quality > 72 {
            encode_image(&img, &out_path, ImageFormat::Jpeg, 72)?;
            out_size = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
        }
        if out_size >= file_size && orig_fmt == ImageFormat::WebP && !has_transparent_pixels(&img) {
            let jpg_path = {
                let _g = path_lock.lock().map_err(|e| e.to_string())?;
                unique_path(parent, &out_stem, "jpg")
            };
            if encode_image(&img, &jpg_path, ImageFormat::Jpeg, quality.min(80)).is_ok() {
                let jpg_size = fs::metadata(&jpg_path).map(|m| m.len()).unwrap_or(0);
                if jpg_size < out_size {
                    let _ = fs::remove_file(&out_path);
                    out_path = jpg_path;
                    out_size = jpg_size;
                } else {
                    let _ = fs::remove_file(&jpg_path);
                }
            }
        }
        if out_size >= file_size && !resized && out_path.extension() == path.extension() {
            fs::copy(path, &out_path)
                .map_err(|e| format!("не удалось скопировать {}: {e}", path.display()))?;
            out_size = file_size;
        }
    }

    let dims = format!("{}×{}", img.width(), img.height());
    Ok((file_size, out_size, dims, out_path))
}

pub fn process_images(
    paths: Vec<String>,
    mode: &str,
    max_dim: u32,
    quality: u8,
    app: Option<&AppHandle>,
) -> Result<QuickFileReport, String> {
    let started = Instant::now();
    if paths.is_empty() {
        return Err("не переданы изображения".into());
    }

    let mut tasks: Vec<ImageTask> = Vec::new();
    let mut folder_out: Option<PathBuf> = None;

    for raw in &paths {
        let p = PathBuf::from(raw);
        if p.is_file() {
            if is_image_ext(&ext_lower(&p)) {
                let size = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                let out_dir = p.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
                tasks.push(ImageTask { src: p, out_dir, clean_name: false, size });
            }
        } else if p.is_dir() {
            let parent = p.parent().unwrap_or_else(|| Path::new("."));
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("photos");
            let root_out = unique_path(parent, &format!("{name}_opt"), "");
            let mut found_in_dir = false;
            for entry in WalkDir::new(&p).into_iter().flatten() {
                if is_cancelled() {
                    return Err("отменено".into());
                }
                let f = entry.path();
                if !f.is_file() || !is_image_ext(&ext_lower(f)) {
                    continue;
                }
                let rel = f.strip_prefix(&p).unwrap_or(f);
                let sub_out = match rel.parent() {
                    Some(par) if !par.as_os_str().is_empty() => root_out.join(par),
                    _ => root_out.clone(),
                };
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                tasks.push(ImageTask {
                    src: f.to_path_buf(),
                    out_dir: sub_out,
                    clean_name: true,
                    size,
                });
                found_in_dir = true;
            }
            if found_in_dir && folder_out.is_none() {
                folder_out = Some(root_out);
            }
        }
    }

    if tasks.is_empty() {
        return Err("подходящих изображений не найдено".into());
    }

    let total_expected: u64 = tasks.iter().map(|t| t.size).sum();
    let mut progress = ProgressReporter::new(app, total_expected);

    let mut total_in = 0u64;
    let mut total_out = 0u64;
    let mut processed = 0usize;
    let mut last_out = PathBuf::new();
    let mut last_dims = String::new();
    let path_lock = Mutex::new(());

    if tasks.len() == 1 {
        let (in_b, out_b, dims, out_p) =
            process_one_image(&tasks[0], mode, max_dim, quality, &path_lock)?;
        total_in = in_b;
        total_out = out_b;
        last_dims = dims;
        last_out = out_p;
        processed = 1;
        progress.add(in_b);
    } else {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get().clamp(2, 4))
            .unwrap_or(2)
            .min(tasks.len());
        let queue = Mutex::new(tasks.iter());
        let (tx, rx) = mpsc::channel::<Result<(u64, u64, String, PathBuf), String>>();

        std::thread::scope(|s| -> Result<(), String> {
            for _ in 0..workers {
                let q = &queue;
                let tx = tx.clone();
                let lock = &path_lock;
                s.spawn(move || loop {
                    if is_cancelled() {
                        break;
                    }
                    let next = q.lock().ok().and_then(|mut g| g.next());
                    let Some(task) = next else { break };
                    let res = process_one_image(task, mode, max_dim, quality, lock);
                    let err = res.is_err();
                    if tx.send(res).is_err() || err {
                        break;
                    }
                });
            }
            drop(tx);
            for item in rx {
                if is_cancelled() {
                    return Err("отменено".into());
                }
                let (in_b, out_b, dims, out_p) = item?;
                total_in += in_b;
                total_out += out_b;
                last_dims = dims;
                last_out = out_p;
                processed += 1;
                progress.add(in_b);
            }
            Ok(())
        })?;
        if is_cancelled() {
            return Err("отменено".into());
        }
    }

    let report_out = if paths.len() == 1 {
        folder_out.unwrap_or(last_out)
    } else {
        last_out
    };

    Ok(QuickFileReport {
        kind: "image".into(),
        output_path: report_out.to_string_lossy().into_owned(),
        count: processed,
        input_bytes: total_in,
        output_bytes: total_out,
        elapsed_ms: started.elapsed().as_millis(),
        detail: last_dims,
    })
}
