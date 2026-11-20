use ml_core::Sample;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, UOI_NAME,
};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId};

use crate::Collector;

/// Читает время последнего ввода в сеансе, без перехвата клавиш или мыши.
pub fn idle_seconds() -> Option<u32> {
    let mut input = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    unsafe { GetLastInputInfo(&mut input).as_bool().then(|| GetTickCount().wrapping_sub(input.dwTime) / 1000) }
}

/// На экране блокировки и защищённом рабочем столе обычные окна не активны.
pub fn interactive_desktop() -> bool {
    unsafe {
        let Ok(desktop) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) else {
            return false;
        };
        let mut name = [0u16; 128];
        let result = GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            std::mem::size_of_val(&name) as u32,
            None,
        );
        let _ = CloseDesktop(desktop);
        let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        result.is_ok() && String::from_utf16_lossy(&name[..len]).eq_ignore_ascii_case("Default")
    }
}

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
            idle_secs: idle_seconds().unwrap_or_default(),
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

    let app = window_app(hwnd).unwrap_or_else(|| "unknown".to_string());
    Some((app, title))
}

pub(crate) unsafe fn window_app(hwnd: HWND) -> Option<String> {
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    process_name(pid)
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
