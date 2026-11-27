use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{ext_lower, is_cancelled};

pub(crate) const CODEC_MANIFEST_NAME: &str = ".memory-log-codecs.json";
pub(crate) const MAX_JXL_INPUT_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const MAX_CODEC_MANIFEST_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ImageFormat {
    Png,
    Jpeg,
}

impl ImageFormat {
    pub(crate) fn from_header(header: &[u8]) -> Option<Self> {
        if header.starts_with(b"\x89PNG\r\n\x1A\n") {
            Some(Self::Png)
        } else if header.starts_with(b"\xFF\xD8\xFF") {
            Some(Self::Jpeg)
        } else {
            None
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }

    pub(crate) fn from_str(s: &str) -> Option<Self> {
        match s {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CodecManifest {
    pub(crate) v: u8,
    pub(crate) jxl: Vec<JxlManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JxlManifestEntry {
    pub(crate) stored: String,
    pub(crate) original: String,
    pub(crate) format: String,
}

fn executable(name: &str) -> Result<PathBuf, String> {
    let app = std::env::current_exe().map_err(|e| e.to_string())?;
    let root = app.parent().ok_or("не удалось определить папку Memory Log")?;
    let bin = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let direct = root.join("codecs").join(&bin);
    if direct.is_file() {
        return Ok(direct);
    }

    #[cfg(debug_assertions)]
    {
        for rel in ["../codecs", "../../release/codecs"] {
            let candidate = root.join(rel).join(&bin);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    Err(format!("утилита JPEG XL не найдена: {}", direct.display()))
}

pub(crate) fn is_available() -> bool {
    executable("cjxl").is_ok()
}

pub(crate) fn detect_image_format(path: &Path) -> Option<ImageFormat> {
    if !matches!(ext_lower(path).as_str(), "png" | "jpg" | "jpeg") {
        return None;
    }
    let mut prefix = [0u8; 8];
    let mut f = File::open(path).ok()?;
    let n = f.read(&mut prefix).ok()?;
    ImageFormat::from_header(&prefix[..n])
}

fn run(tool: &Path, work: &Path, args: &[&str]) -> Result<(), String> {
    let diagnostics = work.join("diagnostics.txt");
    let errors = File::create(&diagnostics).map_err(|e| e.to_string())?;
    let mut command = Command::new(tool);
    command
        .current_dir(work)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(errors);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows::Win32::System::Threading::CREATE_NO_WINDOW;
        command.creation_flags(CREATE_NO_WINDOW.0);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("не удалось запустить JPEG XL: {e}"))?;
    loop {
        if is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("отменено".into());
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            if status.success() {
                return Ok(());
            }
            let mut message = Vec::new();
            if let Ok(file) = File::open(&diagnostics) {
                let _ = file.take(2048).read_to_end(&mut message);
            }
            return Err(format!(
                "JPEG XL ({status}): {}",
                String::from_utf8_lossy(&message).trim()
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub(crate) fn encode_file_to_jxl(
    src: &Path,
    format: ImageFormat,
    threads: usize,
) -> Result<Vec<u8>, String> {
    if is_cancelled() {
        return Err("отменено".into());
    }
    let encoder = executable("cjxl")?;
    let work = tempfile::Builder::new()
        .prefix("memory-log-jxl-enc-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let root = work.path();
    let input_name = match format {
        ImageFormat::Png => "in.png",
        ImageFormat::Jpeg => "in.jpg",
    };
    fs::copy(src, root.join(input_name)).map_err(|e| e.to_string())?;
    let threads_flag = format!("--num_threads={}", threads.max(1));
    let mut args = vec![input_name, "out.jxl", "-d", "0", "-e", "5", &threads_flag, "--quiet"];
    if format == ImageFormat::Jpeg {
        args.push("--lossless_jpeg=1");
    }
    run(&encoder, root, &args)?;
    fs::read(root.join("out.jxl")).map_err(|e| e.to_string())
}

pub(crate) fn decode_jxl_to_file(
    src_jxl: &Path,
    dst: &Path,
    format: ImageFormat,
    threads: usize,
) -> Result<u64, String> {
    if is_cancelled() {
        return Err("отменено".into());
    }
    let decoder = executable("djxl")?;
    let work = tempfile::Builder::new()
        .prefix("memory-log-jxl-dec-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let root = work.path();
    fs::copy(src_jxl, root.join("in.jxl")).map_err(|e| e.to_string())?;
    let threads_flag = format!("--num_threads={}", threads.max(1));
    let out_name = match format {
        ImageFormat::Jpeg => {
            run(
                &decoder,
                root,
                &["in.jxl", "out.jpg", "--reconstruct_jpeg", &threads_flag, "--quiet"],
            )?;
            "out.jpg"
        }
        ImageFormat::Png => {
            run(&decoder, root, &["in.jxl", "out.png", &threads_flag, "--quiet"])?;
            "out.png"
        }
    };
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("не удалось создать папку: {e}"))?;
    }
    fs::copy(root.join(out_name), dst).map_err(|e| format!("ошибка записи {}: {e}", dst.display()))
}
