use std::slice;

use windows::core::w;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, IsClipboardFormatAvailable,
    OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

/// Формат «обычный текст в юникоде».
const CF_UNICODETEXT: u32 = 13;
/// Длинные тексты обрезаем: в истории важно узнать запись, а не хранить целые документы.
const MAX_CHARS: usize = 5000;

/// Следит за буфером обмена по номеру изменения и отдаёт новый скопированный текст.
pub struct ClipboardWatcher {
    last_seq: u32,
}

impl ClipboardWatcher {
    /// Уже лежащее в буфере при запуске не записывается, только новые копирования.
    pub fn new() -> Self {
        Self { last_seq: unsafe { GetClipboardSequenceNumber() } }
    }

    /// `ignore` — номер изменения буфера, сделанного самой программой (листание истории):
    /// такое изменение в историю заново не записывается.
    pub fn poll(&mut self, ignore: u32) -> Option<String> {
        let seq = unsafe { GetClipboardSequenceNumber() };
        if seq == self.last_seq {
            return None;
        }
        if seq == ignore {
            self.last_seq = seq;
            return None;
        }
        let text = unsafe { read_text() };
        // Если буфер был занят другой программой, пробуем снова на следующем опросе.
        if text.is_ok() {
            self.last_seq = seq;
        }
        text.ok().flatten()
    }
}

/// `Err` — буфер сейчас недоступен, `Ok(None)` — текста нет или копирование нужно пропустить.
unsafe fn read_text() -> Result<Option<String>, ()> {
    // Менеджеры паролей помечают скопированные пароли этим форматом: такие не записываем.
    let excluded = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
    if excluded != 0 && IsClipboardFormatAvailable(excluded).is_ok() {
        return Ok(None);
    }
    if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
        return Ok(None);
    }
    OpenClipboard(None).map_err(|_| ())?;
    let text = (|| {
        let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
        let hglobal = HGLOBAL(handle.0);
        let ptr = GlobalLock(hglobal) as *const u16;
        if ptr.is_null() {
            return None;
        }
        let mut len = 0usize;
        // Читаем не больше нужного: скопированный гигантский текст не должен целиком загружаться в память.
        while len < MAX_CHARS * 2 && *ptr.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(slice::from_raw_parts(ptr, len));
        let _ = GlobalUnlock(hglobal);
        Some(text)
    })();
    let _ = CloseClipboard();

    Ok(text.map(|t| t.trim().chars().take(MAX_CHARS).collect::<String>()).filter(|t| !t.is_empty()))
}

/// Кладёт текст в буфер обмена. Возвращает номер изменения буфера (см. `ClipboardWatcher::poll`).
pub fn set_text(text: &str) -> Option<u32> {
    unsafe {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let hmem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).ok()?;
        let ptr = GlobalLock(hmem) as *mut u16;
        if ptr.is_null() {
            let _ = GlobalFree(hmem);
            return None;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        let _ = GlobalUnlock(hmem);

        // Буфер может быть занят другой программой на доли секунды.
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !opened {
            let _ = GlobalFree(hmem);
            return None;
        }
        let _ = EmptyClipboard();
        // После успешной передачи память принадлежит системе.
        let ok = SetClipboardData(CF_UNICODETEXT, HANDLE(hmem.0)).is_ok();
        let _ = CloseClipboard();
        if !ok {
            let _ = GlobalFree(hmem);
            return None;
        }
        Some(GetClipboardSequenceNumber())
    }
}
