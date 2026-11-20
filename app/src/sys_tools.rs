use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, Serialize)]
pub struct StartupItem {
    pub id: String,
    pub name: String,
    pub command: String,
    pub source: &'static str,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListeningPort {
    pub proto: String,
    pub address: String,
    pub port: u16,
    pub pid: u32,
    pub process: String,
}

enum Target {
    Dir(PathBuf),
    MatchingFiles { dir: PathBuf, prefixes: &'static [&'static str], suffix: &'static str },
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

fn walk_dir(dir: &Path, on_file: &mut impl FnMut(&Path, u64)) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            walk_dir(&path, on_file);
        } else if meta.is_file() {
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

fn for_each_target_file(target: &Target, on_file: &mut impl FnMut(&Path, u64)) {
    match target {
        Target::Dir(dir) => walk_dir(dir, on_file),
        Target::MatchingFiles { dir, prefixes, suffix } => {
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

pub fn scan_junk() -> Vec<JunkCategory> {
    ["temp", "caches", "dumps"]
        .into_iter()
        .map(|id| {
            let mut bytes = 0u64;
            let mut files = 0u64;
            for target in junk_targets(id) {
                for_each_target_file(&target, &mut |_, len| {
                    bytes += len;
                    files += 1;
                });
            }
            JunkCategory { id, bytes, files }
        })
        .collect()
}

pub fn clean_junk(ids: &[String]) -> CleanResult {
    let mut result = CleanResult { freed_bytes: 0, deleted_files: 0, skipped_files: 0 };
    for id in ids {
        for target in junk_targets(id) {
            for_each_target_file(&target, &mut |path, len| {
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

#[cfg(windows)]
mod win {
    use std::ffi::OsString;
    use std::fs;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Command;

    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::ERROR_NO_MORE_ITEMS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_BINARY,
        REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE,
    };

    use super::{ListeningPort, StartupItem};

    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const RUN32_KEY: &str = "Software\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Run";
    const APPROVED_RUN: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";
    const APPROVED_RUN32: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run32";
    const APPROVED_FOLDER: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\StartupFolder";
    const UWP_STARTUP: &str = "Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\AppModel\\SystemAppData";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn wide(s: &str) -> Vec<u16> {
        OsString::from(s).encode_wide().chain(std::iter::once(0)).collect()
    }

    struct KeyGuard(HKEY);
    impl Drop for KeyGuard {
        fn drop(&mut self) {
            unsafe { let _ = RegCloseKey(self.0); }
        }
    }

    fn open_key(root: HKEY, path: &str, access: u32) -> Option<KeyGuard> {
        let w = wide(path);
        let mut key = HKEY::default();
        let status = unsafe { RegOpenKeyExW(root, PCWSTR(w.as_ptr()), 0, windows::Win32::System::Registry::REG_SAM_FLAGS(access), &mut key) };
        status.is_ok().then_some(KeyGuard(key))
    }

    fn is_approved(approved: Option<&KeyGuard>, name: &[u16]) -> bool {
        let Some(key) = approved else { return true };
        let mut ty = REG_VALUE_TYPE::default();
        let mut buf = [0u8; 16];
        let mut len = buf.len() as u32;
        let status = unsafe {
            RegQueryValueExW(key.0, PCWSTR(name.as_ptr()), None, Some(&mut ty), Some(buf.as_mut_ptr()), Some(&mut len))
        };
        if status.is_err() || ty != REG_BINARY || len == 0 {
            return true;
        }
        buf[0] & 1 == 0
    }

    fn read_run_key(
        root: HKEY,
        run_path: &str,
        approved_path: &str,
        prefix: &str,
        source: &'static str,
        items: &mut Vec<StartupItem>,
    ) {
        let Some(run_key) = open_key(root, run_path, KEY_READ.0) else { return };
        let approved_key = open_key(root, approved_path, KEY_READ.0);
        let mut index = 0u32;
        loop {
            let mut name_buf = [0u16; 512];
            let mut name_len = name_buf.len() as u32;
            let mut ty = 0u32;
            let mut data_buf = vec![0u8; 4096];
            let mut data_len = data_buf.len() as u32;
            let status = unsafe {
                RegEnumValueW(
                    run_key.0,
                    index,
                    PWSTR(name_buf.as_mut_ptr()),
                    &mut name_len,
                    None,
                    Some(&mut ty),
                    Some(data_buf.as_mut_ptr()),
                    Some(&mut data_len),
                )
            };
            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            index += 1;
            if status.is_err() || (ty != REG_SZ.0 && ty != 2) {
                continue;
            }
            let name = OsString::from_wide(&name_buf[..name_len as usize]).to_string_lossy().into_owned();
            let u16_len = (data_len as usize) / 2;
            let wide_data: Vec<u16> = data_buf[..u16_len * 2]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&c| c != 0)
                .collect();
            let command = OsString::from_wide(&wide_data).to_string_lossy().into_owned();
            let mut name_z = name_buf[..name_len as usize].to_vec();
            name_z.push(0);
            let enabled = is_approved(approved_key.as_ref(), &name_z);
            items.push(StartupItem {
                id: format!("{prefix}:{name}"),
                name,
                command,
                source,
                enabled,
            });
        }
    }

    fn read_startup_folder(
        dir: Option<PathBuf>,
        root: HKEY,
        prefix: &str,
        source: &'static str,
        items: &mut Vec<StartupItem>,
    ) {
        let Some(dir) = dir else { return };
        let Ok(entries) = fs::read_dir(&dir) else { return };
        let approved_key = open_key(root, APPROVED_FOLDER, KEY_READ.0);
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = fs::metadata(&path) else { continue };
            if !meta.is_file() {
                continue;
            }
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if file_name.eq_ignore_ascii_case("desktop.ini") {
                continue;
            }
            let display = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| file_name.clone());
            let name_w = wide(&file_name);
            let enabled = is_approved(approved_key.as_ref(), &name_w);
            items.push(StartupItem {
                id: format!("{prefix}:{file_name}"),
                name: display,
                command: path.display().to_string(),
                source,
                enabled,
            });
        }
    }

    fn enum_subkeys(key: HKEY) -> Vec<String> {
        let mut out = Vec::new();
        let mut index = 0u32;
        loop {
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let status = unsafe {
                RegEnumKeyExW(key, index, PWSTR(buf.as_mut_ptr()), &mut len, None, PWSTR::null(), None, None)
            };
            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            index += 1;
            if status.is_ok() {
                out.push(OsString::from_wide(&buf[..len as usize]).to_string_lossy().into_owned());
            }
        }
        out
    }

    fn friendly_uwp_name(pkg: &str) -> String {
        let base = pkg.split('_').next().unwrap_or(pkg);
        let short = base.rsplit('.').next().unwrap_or(base);
        short.strip_suffix("-Desktop").unwrap_or(short).to_string()
    }

    fn read_uwp_startup(items: &mut Vec<StartupItem>) {
        let Some(root) = open_key(HKEY_CURRENT_USER, UWP_STARTUP, KEY_READ.0) else { return };
        for pkg in enum_subkeys(root.0) {
            let Some(pkg_key) = open_key(root.0, &pkg, KEY_READ.0) else { continue };
            for task in enum_subkeys(pkg_key.0) {
                let Some(task_key) = open_key(pkg_key.0, &task, KEY_READ.0) else { continue };
                let state_w = wide("State");
                let mut ty = REG_VALUE_TYPE::default();
                let mut state = 0u32;
                let mut len = std::mem::size_of::<u32>() as u32;
                let ok = unsafe {
                    RegQueryValueExW(
                        task_key.0,
                        PCWSTR(state_w.as_ptr()),
                        None,
                        Some(&mut ty),
                        Some(&mut state as *mut u32 as *mut u8),
                        Some(&mut len),
                    )
                }.is_ok();
                if !ok || ty != REG_DWORD {
                    continue;
                }
                items.push(StartupItem {
                    id: format!("uwp:{pkg}|{task}"),
                    name: friendly_uwp_name(&pkg),
                    command: format!("{pkg} ({task})"),
                    source: "Store",
                    enabled: state == 2 || state == 4,
                });
            }
        }
    }

    pub fn startup_items() -> Result<Vec<StartupItem>, String> {
        let mut items = Vec::new();
        read_run_key(HKEY_CURRENT_USER, RUN_KEY, APPROVED_RUN, "hkcu_run", "HKCU", &mut items);
        read_run_key(HKEY_LOCAL_MACHINE, RUN_KEY, APPROVED_RUN, "hklm_run", "HKLM", &mut items);
        read_run_key(HKEY_LOCAL_MACHINE, RUN32_KEY, APPROVED_RUN32, "hklm_run32", "HKLM32", &mut items);
        let user_startup = std::env::var_os("APPDATA")
            .map(|p| PathBuf::from(p).join("Microsoft").join("Windows").join("Start Menu").join("Programs").join("Startup"));
        let common_startup = std::env::var_os("ProgramData")
            .map(|p| PathBuf::from(p).join("Microsoft").join("Windows").join("Start Menu").join("Programs").join("Startup"));
        read_startup_folder(user_startup, HKEY_CURRENT_USER, "hkcu_folder", "Startup", &mut items);
        read_startup_folder(common_startup, HKEY_LOCAL_MACHINE, "hklm_folder", "Common", &mut items);
        read_uwp_startup(&mut items);
        items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(items)
    }

    fn write_approved(root: HKEY, subkey: &str, name: &str, enabled: bool) -> Result<(), String> {
        let path_w = wide(subkey);
        let mut key = HKEY::default();
        let status = unsafe {
            RegCreateKeyExW(
                root,
                PCWSTR(path_w.as_ptr()),
                0,
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut key,
                None,
            )
        };
        if status.is_err() {
            return Err("system.startup_admin".into());
        }
        let _guard = KeyGuard(key);
        let mut value = [0u8; 12];
        value[0] = if enabled { 0x02 } else { 0x03 };
        let name_w = wide(name);
        let status = unsafe { RegSetValueExW(key, PCWSTR(name_w.as_ptr()), 0, REG_BINARY, Some(&value)) };
        if status.is_err() {
            return Err("system.startup_admin".into());
        }
        Ok(())
    }

    pub fn set_startup_item(id: &str, enabled: bool) -> Result<(), String> {
        let Some((kind, rest)) = id.split_once(':') else {
            return Err("Invalid startup item".into());
        };
        match kind {
            "hkcu_run" => write_approved(HKEY_CURRENT_USER, APPROVED_RUN, rest, enabled),
            "hklm_run" => write_approved(HKEY_LOCAL_MACHINE, APPROVED_RUN, rest, enabled),
            "hklm_run32" => write_approved(HKEY_LOCAL_MACHINE, APPROVED_RUN32, rest, enabled),
            "hkcu_folder" => write_approved(HKEY_CURRENT_USER, APPROVED_FOLDER, rest, enabled),
            "hklm_folder" => write_approved(HKEY_LOCAL_MACHINE, APPROVED_FOLDER, rest, enabled),
            "uwp" => {
                let Some((pkg, task)) = rest.split_once('|') else {
                    return Err("Invalid UWP startup item".into());
                };
                let path = format!("{UWP_STARTUP}\\{pkg}\\{task}");
                let Some(key) = open_key(HKEY_CURRENT_USER, &path, KEY_WRITE.0) else {
                    return Err("system.startup_admin".into());
                };
                let state_w = wide("State");
                let state: u32 = if enabled { 2 } else { 1 };
                let bytes = state.to_le_bytes();
                let status = unsafe { RegSetValueExW(key.0, PCWSTR(state_w.as_ptr()), 0, REG_DWORD, Some(&bytes)) };
                if status.is_err() {
                    return Err("system.startup_admin".into());
                }
                Ok(())
            }
            _ => Err("Unknown startup source".into()),
        }
    }

    pub fn listening_ports(proc_names: &std::collections::HashMap<u32, String>) -> Result<Vec<ListeningPort>, String> {
        let output = Command::new("netstat")
            .args(["-ano"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&output.stdout);
        let mut seen = super::BTreeSet::new();
        let mut out = Vec::new();
        for line in text.lines() {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let (proto, local, pid_str) = match cols.as_slice() {
                ["TCP", local, _, "LISTENING", pid] => ("TCP", *local, *pid),
                ["UDP", local, "*:*", pid] => ("UDP", *local, *pid),
                _ => continue,
            };
            let Some((addr, port_str)) = local.rsplit_once(':') else { continue };
            let Ok(port) = port_str.parse::<u16>() else { continue };
            let Ok(pid) = pid_str.parse::<u32>() else { continue };
            if !seen.insert((proto, port, pid)) {
                continue;
            }
            let process = proc_names.get(&pid).cloned().unwrap_or_else(|| format!("PID {pid}"));
            out.push(ListeningPort {
                proto: proto.to_string(),
                address: addr.to_string(),
                port,
                pid,
                process,
            });
        }
        out.sort_by_key(|p| (p.port, p.proto.clone(), p.pid));
        Ok(out)
    }
}

#[cfg(not(windows))]
mod win {
    use super::{ListeningPort, StartupItem};
    pub fn startup_items() -> Result<Vec<StartupItem>, String> { Ok(Vec::new()) }
    pub fn set_startup_item(_: &str, _: bool) -> Result<(), String> { Err("Windows only".into()) }
    pub fn listening_ports(_: &std::collections::HashMap<u32, String>) -> Result<Vec<ListeningPort>, String> { Ok(Vec::new()) }
}

pub use win::{listening_ports, set_startup_item, startup_items};
