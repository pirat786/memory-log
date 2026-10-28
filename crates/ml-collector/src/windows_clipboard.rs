use std::slice;

use windows::core::w;
use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::System::DataExchange::{
    CloseClipboard, GetClipboardData, GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};

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

    pub fn poll(&mut self) -> Option<String> {
        let seq = unsafe { GetClipboardSequenceNumber() };
        if seq == self.last_seq {
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
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(slice::from_raw_parts(ptr, len));
        let _ = GlobalUnlock(hglobal);
        Some(text)
    })();
    let _ = CloseClipboard();

    Ok(text.map(|t| t.trim().chars().take(MAX_CHARS).collect::<String>()).filter(|t| !t.is_empty()))
}
