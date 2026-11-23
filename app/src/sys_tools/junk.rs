use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct JunkCategory {
    pub id: &'static str,
    pub bytes: u64,
    pub files: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CleanResult {
    pub freed_bytes: u64,
    pub deleted_files: u64,
    pub skipped_files: u64,
}

enum Target {
    Dir(PathBuf),
    MatchingFiles {
        dir: PathBuf,
        prefixes: &'static [&'static str],
        suffix: &'static str,
    },
}

fn junk_targets(id: &str) -> Vec<Target> {
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    match id {
        "temp" => vec![Target::Dir(std::env::temp_dir())],
        "caches" => {
            let Some(local) = local else { return Vec::new() };
            vec![
                Target::MatchingFiles {
                    dir: local.join("Microsoft").join("Windows").join("Explorer"),
                    prefixes: &["thumbcache_", "iconcache_"],
                    suffix: ".db",
                },
                Target::Dir(local.join("D3DSCache")),
            ]
        }
        "dumps" => {
            let Some(local) = local else { return Vec::new() };
            vec![
                Target::Dir(local.join("CrashDumps")),
                Target::Dir(local.join("Microsoft").join("Windows").join("WER")),
            ]
        }
        _ => Vec::new(),
    }
}

fn walk_dir(dir: &Path, cutoff: Option<SystemTime>, on_file: &mut impl FnMut(&Path, u64)) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            walk_dir(&path, cutoff, on_file);
        } else if meta.is_file() {
            if let Some(limit) = cutoff {
                if meta.modified().ok().is_some_and(|t| t > limit) {
                    continue;
                }
            }
            on_file(&path, meta.len());
        }
    }
}

fn remove_empty_subdirs(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if !meta.file_type().is_symlink() && meta.is_dir() {
            remove_empty_subdirs(&path);
            let _ = fs::remove_dir(&path);
        }
    }
}

fn for_each_target_file(
    target: &Target,
    cutoff: Option<SystemTime>,
    on_file: &mut impl FnMut(&Path, u64),
) {
    match target {
        Target::Dir(dir) => walk_dir(dir, cutoff, on_file),
        Target::MatchingFiles {
            dir,
            prefixes,
            suffix,
        } => {
            let Ok(entries) = fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(meta) = fs::symlink_metadata(&path) else { continue };
                if meta.file_type().is_symlink() || !meta.is_file() {
                    continue;
                }
                let name = entry.file_name();
                let name = name.to_string_lossy().to_ascii_lowercase();
                if prefixes.iter().any(|p| name.starts_with(p)) && name.ends_with(suffix) {
                    on_file(&path, meta.len());
                }
            }
        }
    }
}

fn temp_cutoff(id: &str, min_age_hours: u64) -> Option<SystemTime> {
    (id == "temp" && min_age_hours > 0)
        .then(|| {
            SystemTime::now().checked_sub(Duration::from_secs(min_age_hours.saturating_mul(3600)))
        })
        .flatten()
}

pub fn scan_junk(min_age_hours: u64) -> Vec<JunkCategory> {
    ["temp", "caches", "dumps"]
        .into_iter()
        .map(|id| {
            let mut bytes = 0u64;
            let mut files = 0u64;
            let cutoff = temp_cutoff(id, min_age_hours);
            for target in junk_targets(id) {
                for_each_target_file(&target, cutoff, &mut |_, len| {
                    bytes += len;
                    files += 1;
                });
            }
            JunkCategory { id, bytes, files }
        })
        .collect()
}

pub fn clean_junk(ids: &[String], min_age_hours: u64) -> CleanResult {
    let mut result = CleanResult {
        freed_bytes: 0,
        deleted_files: 0,
        skipped_files: 0,
    };
    for id in ids {
        let cutoff = temp_cutoff(id, min_age_hours);
        for target in junk_targets(id) {
            for_each_target_file(&target, cutoff, &mut |path, len| {
                if fs::remove_file(path).is_ok() {
                    result.freed_bytes += len;
                    result.deleted_files += 1;
                } else {
                    result.skipped_files += 1;
                }
            });
            if let Target::Dir(dir) = &target {
                remove_empty_subdirs(dir);
            }
        }
    }
    result
}
