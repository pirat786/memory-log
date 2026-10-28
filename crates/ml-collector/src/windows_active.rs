use ml_core::Sample;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId};

use crate::Collector;

pub struct ActiveWindowCollector;

impl ActiveWindowCollector {
    pub fn new() -> Self {
        Self
    }
}

impl Collector for ActiveWindowCollector {
    fn sample(&mut self) -> Option<Sample> {
        let (app, title) = unsafe { foreground_window()? };
        Some(Sample {
            ts: chrono::Utc::now().timestamp(),
            app,
            title,
        })
    }
}

unsafe fn foreground_window() -> Option<(String, String)> {
    let hwnd = GetForegroundWindow();
    if hwnd == HWND::default() {
        return None;
    }

    let mut buf = [0u16; 512];
    let n = GetWindowTextW(hwnd, &mut buf);
    let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);

    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    let app = process_name(pid).unwrap_or_else(|| "unknown".to_string());
    Some((app, title))
}

unsafe fn process_name(pid: u32) -> Option<String> {
    let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
    let _ = CloseHandle(handle);
    ok.ok()?;

    let path = String::from_utf16_lossy(&buf[..len as usize]);
    let file = path.rsplit('\\').next()?;
    Some(file.trim_end_matches(".exe").to_string())
}
