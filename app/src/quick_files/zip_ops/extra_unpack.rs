use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use sevenz_rust2::{ArchiveReader, Password};
use tauri::AppHandle;

use super::{check_free_space, solid_unpack::sanitize_rel_path};
use crate::quick_files::{is_cancelled, unique_path, ProgressReporter, QuickFileReport};

const MAX_RAR_ENTRY_BYTES: u64 = 256 * 1024 * 1024;

fn entry_destination(root: &Path, name: &Path) -> io::Result<PathBuf> {
    let portable = name.to_string_lossy().replace('\\', "/");
    sanitize_rel_path(Path::new(&portable))
        .map(|relative| root.join(relative))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("недопустимый путь в архиве: {}", name.display())))
}

fn output_file(path: &Path) -> io::Result<BufWriter<File>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    File::create_new(path).map(BufWriter::new)
}

fn unpack_to_new_folder(
    archive: String,
    extract: impl FnOnce(&Path, &Path) -> Result<(usize, u64), String>,
) -> Result<QuickFileReport, String> {
    let started = Instant::now();
    let path = Path::new(&archive);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_stem().and_then(|name| name.to_str()).unwrap_or("unpacked");
    let input_bytes = fs::metadata(path).map_err(|e| e.to_string())?.len();
    let staging = tempfile::Builder::new().prefix(".memory-log-unpack-").tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let (count, output_bytes) = extract(path, staging.path()).map_err(|error| {
        if is_cancelled() { "отменено".into() } else { format!("{archive}: {error}") }
    })?;
    if is_cancelled() {
        return Err("отменено".into());
    }
    let output = unique_path(parent, stem, "");
    fs::rename(staging.path(), &output).map_err(|e| e.to_string())?;
    Ok(QuickFileReport {
        kind: "zip-unpack".into(),
        output_path: output.to_string_lossy().into_owned(),
        count,
        input_bytes,
        output_bytes,
        elapsed_ms: started.elapsed().as_millis(),
        detail: String::new(),
        deleted_original: false,
    })
}

pub fn unpack_sevenz(archive: String, app: Option<&AppHandle>) -> Result<QuickFileReport, String> {
    unpack_to_new_folder(archive, |path, root| {
        let mut reader = ArchiveReader::open(path, Password::empty()).map_err(|e| e.to_string())?;
        let total = reader.archive().files.iter().fold(0u64, |sum, entry| sum.saturating_add(entry.size()));
        check_free_space(root, total)?;
        let mut progress = ProgressReporter::new(app, total);
        let mut count = 0;
        let mut bytes = 0;
        let mut buffer = vec![0; super::UNPACK_BUF_SSD];
        reader.for_each_entries(|entry, data| {
            if is_cancelled() {
                return Ok(false);
            }
            if entry.is_anti_item() {
                return Ok(true);
            }
            let destination = entry_destination(root, Path::new(entry.name()))?;
            if entry.is_directory() {
                fs::create_dir_all(destination)?;
                return Ok(true);
            }
            let mut file = output_file(&destination)?;
            loop {
                if is_cancelled() {
                    return Ok(false);
                }
                let read = data.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                file.write_all(&buffer[..read])?;
                bytes += read as u64;
                progress.add(read as u64);
            }
            file.flush()?;
            count += 1;
            Ok(true)
        }).map_err(|e| e.to_string())?;
        Ok((count, bytes))
    })
}

pub fn unpack_rar(archive: String, app: Option<&AppHandle>) -> Result<QuickFileReport, String> {
    unpack_to_new_folder(archive, |path, root| {
        let mut reader = unrar::Archive::new(&path).open_for_processing().map_err(|e| e.to_string())?;
        let mut progress = ProgressReporter::new(app, 0);
        let mut count = 0;
        let mut bytes = 0;
        while let Some(entry) = reader.read_header().map_err(|e| e.to_string())? {
            if is_cancelled() {
                return Err("отменено".into());
            }
            let header = entry.entry();
            if header.is_encrypted() || header.is_split() {
                return Err("RAR с паролем или несколькими томами пока не поддерживается".into());
            }
            let destination = entry_destination(root, &header.filename).map_err(|e| e.to_string())?;
            if header.is_directory() {
                fs::create_dir_all(destination).map_err(|e| e.to_string())?;
                reader = entry.skip().map_err(|e| e.to_string())?;
                continue;
            }
            if header.unpacked_size > MAX_RAR_ENTRY_BYTES {
                return Err("файл внутри RAR превышает 256 МиБ".into());
            }
            check_free_space(root, header.unpacked_size)?;
            let (data, next) = entry.read().map_err(|e| e.to_string())?;
            let mut file = output_file(&destination).map_err(|e| e.to_string())?;
            file.write_all(&data).and_then(|()| file.flush()).map_err(|e| e.to_string())?;
            count += 1;
            bytes += data.len() as u64;
            progress.add(data.len() as u64);
            reader = next;
        }
        Ok((count, bytes))
    })
}
