use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LockingProcess {
    pub pid: u32,
    pub name: String,
    pub app_name: String,
}

pub(crate) fn is_in_use_dialog_title(title: &str) -> bool {
    let lower = title.trim().to_lowercase();
    matches!(
        lower.as_str(),
        "папка уже используется"
            | "файл используется"
            | "файл уже используется"
            | "folder in use"
            | "file in use"
    )
}

pub(crate) fn is_protected_process(exe_name: &str, pid: u32) -> bool {
    if pid == 0 || pid == 4 || pid == std::process::id() {
        return true;
    }
    let lower = exe_name.trim().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "explorer.exe"
            | "csrss.exe"
            | "wininit.exe"
            | "services.exe"
            | "lsass.exe"
            | "smss.exe"
            | "dwm.exe"
            | "svchost.exe"
            | "system"
    )
}

pub(crate) fn extract_candidate_names(texts: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for raw in texts {
        let s = raw.trim();
        if s.is_empty()
            || s.chars().count() > 260
            || s.contains('\n')
            || s.contains('\r')
            || s.contains(['<', '>', '"', '|', '?', '*'])
        {
            continue;
        }
        let key = s.to_string();
        if seen.insert(key.clone()) {
            out.push(key);
        }
    }
    out
}

pub(crate) fn resolve_locked_path(candidates: &[String], search_dirs: &[PathBuf]) -> Option<PathBuf> {
    for cand in candidates {
        let p = Path::new(cand);
        if p.is_absolute() && p.exists() {
            return Some(p.to_path_buf());
        }
    }
    for dir in search_dirs {
        for cand in candidates {
            if cand.contains(':') || cand.contains('\\') || cand.contains('/') {
                continue;
            }
            let joined = dir.join(cand);
            if joined.exists() {
                return Some(joined);
            }
        }
    }
    None
}

pub(crate) fn file_url_to_path(raw: &str) -> Option<PathBuf> {
    let parsed = url::Url::parse(raw.trim()).ok()?;
    if parsed.scheme() != "file" {
        return None;
    }
    parsed.to_file_path().ok()
}

pub(crate) fn format_prompt_message(target: &Path, lockers: &[LockingProcess]) -> String {
    let mut lines = Vec::with_capacity(lockers.len() + 4);
    lines.push(format!("Объект: {}", target.display()));
    lines.push(String::new());
    lines.push("Занято процессами:".to_string());
    for p in lockers {
        if !p.app_name.is_empty() && !p.app_name.eq_ignore_ascii_case(&p.name) {
            lines.push(format!("• {} ({}, PID {})", p.name, p.app_name, p.pid));
        } else {
            lines.push(format!("• {} (PID {})", p.name, p.pid));
        }
    }
    lines.push(String::new());
    lines.push("Закрыть указанные процессы?".to_string());
    lines.join("\n")
}

#[cfg(windows)]
mod imp {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use sysinfo::{ProcessRefreshKind, System, UpdateKind};
    use windows::core::{Interface, PCWSTR, VARIANT};
    use windows::Win32::Foundation::{CloseHandle, ERROR_MORE_DATA, HWND, WIN32_ERROR};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::System::RestartManager::{
        RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY,
        RM_PROCESS_INFO,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, TerminateProcess, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, TreeScope_Descendants, UIA_ControlTypePropertyId,
        UIA_TextControlTypeId,
    };
    use windows::Win32::UI::Shell::{IWebBrowser2, IShellWindows, ShellWindows};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId, IsWindow,
        IsWindowVisible, MessageBoxW, IDYES, MB_ICONQUESTION, MB_SETFOREGROUND, MB_TOPMOST,
        MB_YESNO,
    };

    use super::{
        extract_candidate_names, file_url_to_path, format_prompt_message, is_in_use_dialog_title,
        is_protected_process, resolve_locked_path, LockingProcess,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn window_title(hwnd: HWND) -> String {
        let mut buf = [0u16; 512];
        let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
        if len <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize])
    }

    fn window_class(hwnd: HWND) -> String {
        let mut buf = [0u16; 128];
        let len = unsafe { GetClassNameW(hwnd, &mut buf) };
        if len <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize])
    }

    fn process_exe_name(pid: u32) -> String {
        if pid == 0 {
            return String::new();
        }
        unsafe {
            let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return String::new();
            };
            let mut buf = vec![0u16; 512];
            let mut len = buf.len() as u32;
            let name = if QueryFullProcessImageNameW(
                h,
                PROCESS_NAME_WIN32,
                windows::core::PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .is_ok()
            {
                let full = String::from_utf16_lossy(&buf[..len as usize]);
                Path::new(&full)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string()
            } else {
                String::new()
            };
            let _ = CloseHandle(h);
            name
        }
    }

    fn is_explorer_in_use_dialog(hwnd: HWND) -> bool {
        let class = window_class(hwnd);
        if class != "OperationStatusWindow" && class != "#32770" {
            return false;
        }
        let title = window_title(hwnd);
        if !is_in_use_dialog_title(&title) {
            return false;
        }
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        process_exe_name(pid).eq_ignore_ascii_case("explorer.exe")
    }

    fn read_dialog_texts(hwnd: HWND) -> Vec<String> {
        let mut out = Vec::new();
        unsafe {
            let Ok(uia): Result<IUIAutomation, _> =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
            else {
                return out;
            };
            let Ok(root) = uia.ElementFromHandle(hwnd) else {
                return out;
            };
            let cond_val = VARIANT::from(UIA_TextControlTypeId.0);
            let Ok(cond) = uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &cond_val) else {
                return out;
            };
            let Ok(elements) = root.FindAll(TreeScope_Descendants, &cond) else {
                return out;
            };
            let count = elements.Length().unwrap_or(0);
            for i in 0..count {
                if let Ok(el) = elements.GetElement(i) {
                    if let Ok(bstr) = el.CurrentName() {
                        let s = bstr.to_string();
                        if !s.is_empty() {
                            out.push(s);
                        }
                    }
                }
            }
        }
        out
    }

    fn explorer_open_dirs() -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        let mut seen = HashSet::new();
        unsafe {
            if let Ok(shell_windows) =
                CoCreateInstance::<_, IShellWindows>(&ShellWindows, None, CLSCTX_LOCAL_SERVER)
            {
                let count = shell_windows.Count().unwrap_or(0);
                for i in 0..count {
                    let idx = VARIANT::from(i);
                    if let Ok(disp) = shell_windows.Item(&idx) {
                        if let Ok(wb) = disp.cast::<IWebBrowser2>() {
                            if let Ok(url_bstr) = wb.LocationURL() {
                                if let Some(path) = file_url_to_path(&url_bstr.to_string()) {
                                    if path.is_dir() && seen.insert(path.clone()) {
                                        dirs.push(path);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            let base = PathBuf::from(profile);
            for sub in ["Desktop", "Downloads"] {
                let p = base.join(sub);
                if p.is_dir() && seen.insert(p.clone()) {
                    dirs.push(p);
                }
            }
        }
        dirs
    }

    fn restart_manager_lockers(target: &Path) -> Vec<LockingProcess> {
        let mut files: Vec<PathBuf> = Vec::new();
        if target.is_file() {
            files.push(target.to_path_buf());
        } else if target.is_dir() {
            for entry in walkdir::WalkDir::new(target)
                .max_depth(5)
                .into_iter()
                .flatten()
            {
                if entry.file_type().is_file() {
                    files.push(entry.into_path());
                    if files.len() >= 300 {
                        break;
                    }
                }
            }
        }
        if files.is_empty() {
            return Vec::new();
        }

        let wide_paths: Vec<Vec<u16>> = files
            .iter()
            .map(|p| wide(&p.to_string_lossy()))
            .collect();
        let pcwstrs: Vec<PCWSTR> = wide_paths.iter().map(|w| PCWSTR(w.as_ptr())).collect();

        let mut session = 0u32;
        let mut key = vec![0u16; (CCH_RM_SESSION_KEY + 1) as usize];
        let err =
            unsafe { RmStartSession(&mut session, 0, windows::core::PWSTR(key.as_mut_ptr())) };
        if err != WIN32_ERROR(0) {
            return Vec::new();
        }

        struct SessionGuard(u32);
        impl Drop for SessionGuard {
            fn drop(&mut self) {
                unsafe {
                    let _ = RmEndSession(self.0);
                }
            }
        }
        let _guard = SessionGuard(session);

        if unsafe { RmRegisterResources(session, Some(&pcwstrs), None, None) } != WIN32_ERROR(0) {
            return Vec::new();
        }

        let mut needed = 0u32;
        let mut count = 0u32;
        let mut reboot_reasons = 0u32;
        let list_err = unsafe {
            RmGetList(session, &mut needed, &mut count, None, &mut reboot_reasons)
        };
        if list_err == WIN32_ERROR(0) && needed == 0 {
            return Vec::new();
        }
        if list_err != ERROR_MORE_DATA && list_err != WIN32_ERROR(0) {
            return Vec::new();
        }

        let mut infos = vec![RM_PROCESS_INFO::default(); needed as usize];
        count = needed;
        if unsafe {
            RmGetList(
                session,
                &mut needed,
                &mut count,
                Some(infos.as_mut_ptr()),
                &mut reboot_reasons,
            )
        } != WIN32_ERROR(0)
        {
            return Vec::new();
        }

        let mut out = Vec::new();
        for info in infos.into_iter().take(count as usize) {
            let pid = info.Process.dwProcessId;
            let app_len = info
                .strAppName
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(info.strAppName.len());
            let app_name = String::from_utf16_lossy(&info.strAppName[..app_len])
                .trim()
                .to_string();
            let exe_name = process_exe_name(pid);
            let name = if exe_name.is_empty() {
                if app_name.is_empty() {
                    format!("PID {pid}")
                } else {
                    app_name.clone()
                }
            } else {
                exe_name
            };
            out.push(LockingProcess {
                pid,
                name,
                app_name,
            });
        }
        out
    }

    fn norm_win_path(p: &Path) -> String {
        let s = p.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
        s.trim_start_matches(r"\\?\")
            .trim_end_matches('\\')
            .to_string()
    }

    fn cwd_lockers(target: &Path, sys: &mut System) -> Vec<LockingProcess> {
        if !target.is_dir() {
            return Vec::new();
        }
        let target_norm = norm_win_path(target);
        if target_norm.is_empty() {
            return Vec::new();
        }
        let target_prefix = format!("{target_norm}\\");
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::new().with_cwd(UpdateKind::Always),
        );
        let mut out = Vec::new();
        for (pid, proc_) in sys.processes() {
            let Some(cwd) = proc_.cwd() else { continue };
            let cwd_norm = norm_win_path(cwd);
            if cwd_norm == target_norm || cwd_norm.starts_with(&target_prefix) {
                let raw_pid = pid.as_u32();
                let exe_name = process_exe_name(raw_pid);
                let proc_name = proc_.name().to_string_lossy().to_string();
                let name = if exe_name.is_empty() {
                    proc_name.clone()
                } else {
                    exe_name
                };
                out.push(LockingProcess {
                    pid: raw_pid,
                    name,
                    app_name: proc_name,
                });
            }
        }
        out
    }

    fn find_lockers(target: &Path, sys: &mut System) -> Vec<LockingProcess> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for item in restart_manager_lockers(target)
            .into_iter()
            .chain(cwd_lockers(target, sys))
        {
            if is_protected_process(&item.name, item.pid) {
                continue;
            }
            if seen.insert(item.pid) {
                out.push(item);
            }
        }
        out
    }

    fn terminate_pid(pid: u32) -> Result<(), String> {
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, false, pid)
                .map_err(|e| format!("не удалось открыть PID {pid}: {e}"))?;
            let res = TerminateProcess(handle, 1);
            let _ = CloseHandle(handle);
            res.map_err(|e| format!("не удалось завершить PID {pid}: {e}"))
        }
    }

    pub fn spawn_lock_watch() {
        let _ = std::thread::Builder::new()
            .name("lock-watch".into())
            .spawn(|| {
                let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
                let mut sys = System::new();
                let mut last_hwnd: isize = 0;

                loop {
                    std::thread::sleep(Duration::from_millis(450));

                    if last_hwnd != 0 {
                        let prev = HWND(last_hwnd as *mut _);
                        let alive = unsafe { IsWindow(prev).as_bool() && IsWindowVisible(prev).as_bool() };
                        if !alive {
                            last_hwnd = 0;
                        }
                    }

                    let hwnd = unsafe { GetForegroundWindow() };
                    let raw_hwnd = hwnd.0 as isize;
                    if raw_hwnd == 0 || raw_hwnd == last_hwnd {
                        continue;
                    }
                    if !is_explorer_in_use_dialog(hwnd) {
                        continue;
                    }
                    last_hwnd = raw_hwnd;

                    let texts = read_dialog_texts(hwnd);
                    let candidates = extract_candidate_names(&texts);
                    let dirs = explorer_open_dirs();
                    let Some(target) = resolve_locked_path(&candidates, &dirs) else {
                        crate::log::info(&format!(
                            "lock-watch: dialog detected, path unresolved (candidates={:?}, dirs={})",
                            candidates,
                            dirs.len()
                        ));
                        continue;
                    };

                    let lockers = find_lockers(&target, &mut sys);
                    if lockers.is_empty() {
                        crate::log::info(&format!(
                            "lock-watch: {} — closable lockers not found",
                            target.display()
                        ));
                        continue;
                    }

                    crate::log::info(&format!(
                        "lock-watch: {} locked by {:?}",
                        target.display(),
                        lockers
                            .iter()
                            .map(|l| format!("{}:{}", l.name, l.pid))
                            .collect::<Vec<_>>()
                    ));

                    let body_w = wide(&format_prompt_message(&target, &lockers));
                    let title_w = wide("Memory Log — Кто держит файл");
                    let answer = unsafe {
                        MessageBoxW(
                            None,
                            PCWSTR(body_w.as_ptr()),
                            PCWSTR(title_w.as_ptr()),
                            MB_YESNO | MB_ICONQUESTION | MB_TOPMOST | MB_SETFOREGROUND,
                        )
                    };

                    if answer == IDYES {
                        for p in &lockers {
                            match terminate_pid(p.pid) {
                                Ok(()) => crate::log::info(&format!(
                                    "lock-watch: terminated {} (PID {})",
                                    p.name, p.pid
                                )),
                                Err(e) => crate::log::info(&format!("lock-watch: {e}")),
                            }
                        }
                    }
                }
            });
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn spawn_lock_watch() {}
}

pub use imp::spawn_lock_watch;
