use std::slice;
use std::sync::OnceLock;

use windows::core::w;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardOwner, GetClipboardSequenceNumber, IsClipboardFormatAvailable,
    OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use crate::{windows_active::window_app, ClipboardCapture, ClipboardContent, ClipboardImage};

/// Формат «обычный текст в юникоде».
const CF_UNICODETEXT: u32 = 13;
/// Формат «растровое изображение (DIB)» — PrintScreen, Win+Shift+S, копирование картинок.
const CF_DIB: u32 = 8;
/// Длинные тексты обрезаем: в истории важно узнать запись, а не хранить целые документы.
const MAX_CHARS: usize = 5000;
/// Максимальное разрешение захватываемого изображения.
const MAX_IMAGE_DIM: u32 = 8192;

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
    pub fn poll(&mut self, ignore: u32) -> Option<ClipboardCapture> {
        let seq = unsafe { GetClipboardSequenceNumber() };
        if seq == self.last_seq {
            return None;
        }
        if seq == ignore {
            self.last_seq = seq;
            return None;
        }
        let owner_app = unsafe { GetClipboardOwner().ok().and_then(|hwnd| window_app(hwnd)) };
        let content = unsafe { read_content() };
        if unsafe { GetClipboardSequenceNumber() } != seq {
            return None;
        }
        // Если буфер был занят другой программой, пробуем снова на следующем опросе.
        if content.is_ok() {
            self.last_seq = seq;
        }
        content.ok().flatten().map(|content| ClipboardCapture { content, owner_app })
    }

    /// Синхронизирует номер изменения, не читая содержимое буфера.
    pub fn skip(&mut self) {
        self.last_seq = unsafe { GetClipboardSequenceNumber() };
    }
}

/// `Err` — буфер сейчас недоступен, `Ok(None)` — поддерживаемых данных нет или нужно пропустить.
unsafe fn read_content() -> Result<Option<ClipboardContent>, ()> {
    // Менеджеры паролей помечают скопированные пароли этим форматом: такие не записываем.
    static EXCLUDE_FMT: OnceLock<u32> = OnceLock::new();
    let excluded = *EXCLUDE_FMT
        .get_or_init(|| RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")));
    if excluded != 0 && IsClipboardFormatAvailable(excluded).is_ok() {
        return Ok(None);
    }
    let has_text = IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok();
    let has_dib = !has_text && IsClipboardFormatAvailable(CF_DIB).is_ok();
    if !has_text && !has_dib {
        return Ok(None);
    }
    OpenClipboard(None).map_err(|_| ())?;
    let result = if has_text {
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
        text.map(|t| t.trim().chars().take(MAX_CHARS).collect::<String>())
            .filter(|t| !t.is_empty())
            .map(ClipboardContent::Text)
    } else {
        read_dib().map(ClipboardContent::Image)
    };
    let _ = CloseClipboard();

    Ok(result)
}

unsafe fn read_dib() -> Option<ClipboardImage> {
    let handle = GetClipboardData(CF_DIB).ok()?;
    let hglobal = HGLOBAL(handle.0);
    let total_size = GlobalSize(hglobal);
    if total_size < 40 {
        return None;
    }
    let ptr = GlobalLock(hglobal) as *const u8;
    if ptr.is_null() {
        return None;
    }
    let bytes = slice::from_raw_parts(ptr, total_size);
    let parsed = parse_dib_to_rgba(bytes);
    let _ = GlobalUnlock(hglobal);
    parsed
}

fn parse_dib_to_rgba(bytes: &[u8]) -> Option<ClipboardImage> {
    if bytes.len() < 40 {
        return None;
    }
    let header_size = u32::from_le_bytes(bytes[0..4].try_into().ok()?) as usize;
    if header_size < 40 || bytes.len() < header_size {
        return None;
    }
    let width_i = i32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let height_i = i32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let planes = u16::from_le_bytes(bytes[12..14].try_into().ok()?);
    let bpp = u16::from_le_bytes(bytes[14..16].try_into().ok()?);
    let compression = u32::from_le_bytes(bytes[16..20].try_into().ok()?);
    let clr_used = u32::from_le_bytes(bytes[32..36].try_into().ok()?) as usize;

    if planes != 1 || width_i <= 0 || height_i == 0 {
        return None;
    }
    let width = width_i as u32;
    let top_down = height_i < 0;
    let height = height_i.unsigned_abs();
    if width > MAX_IMAGE_DIM || height > MAX_IMAGE_DIM {
        return None;
    }
    if (bpp != 24 && bpp != 32) || (compression != 0 && compression != 3) {
        return None;
    }
    let bitfields_extra = if compression == 3 && header_size == 40 { 12 } else { 0 };
    let palette_bytes = if bpp <= 8 { clr_used * 4 } else { clr_used * 4 };
    let pixel_offset = header_size + bitfields_extra + palette_bytes;
    let row_bytes = ((width as usize * bpp as usize + 31) / 32) * 4;
    let needed = pixel_offset.checked_add(row_bytes.checked_mul(height as usize)?)?;
    if bytes.len() < needed {
        return None;
    }

    let pixels = &bytes[pixel_offset..];
    let mut rgba = vec![255u8; width as usize * height as usize * 4];
    for y in 0..height as usize {
        let src_y = if top_down { y } else { height as usize - 1 - y };
        let row = &pixels[src_y * row_bytes..src_y * row_bytes + row_bytes];
        let dst_row = &mut rgba[y * width as usize * 4..(y + 1) * width as usize * 4];
        if bpp == 32 {
            for x in 0..width as usize {
                let s = x * 4;
                let d = x * 4;
                dst_row[d] = row[s + 2];
                dst_row[d + 1] = row[s + 1];
                dst_row[d + 2] = row[s];
                dst_row[d + 3] = 255;
            }
        } else {
            for x in 0..width as usize {
                let s = x * 3;
                let d = x * 4;
                dst_row[d] = row[s + 2];
                dst_row[d + 1] = row[s + 1];
                dst_row[d + 2] = row[s];
                dst_row[d + 3] = 255;
            }
        }
    }
    Some(ClipboardImage { width, height, rgba })
}

pub fn clipboard_seq() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

/// Читает весь обычный текст из буфера обмена (до 1M символов) без обрезки пробелов.
pub fn read_full_text() -> Option<String> {
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            return None;
        }
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !opened {
            return None;
        }
        let text = (|| {
            let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
            let hglobal = HGLOBAL(handle.0);
            let ptr = GlobalLock(hglobal) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let mut len = 0usize;
            while len < 1_000_000 && *ptr.add(len) != 0 {
                len += 1;
            }
            let text = String::from_utf16_lossy(slice::from_raw_parts(ptr, len));
            let _ = GlobalUnlock(hglobal);
            Some(text)
        })();
        let _ = CloseClipboard();
        text.filter(|t| !t.is_empty())
    }
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

/// Кладёт RGBA8-изображение в буфер обмена как `CF_DIB` (32-bit BGRA).
pub fn set_image_rgba(rgba: &[u8], width: u32, height: u32) -> Option<u32> {
    if width == 0 || height == 0 || rgba.len() < width as usize * height as usize * 4 {
        return None;
    }
    unsafe {
        let pixel_bytes = width as usize * height as usize * 4;
        let total = 40 + pixel_bytes;
        let hmem = GlobalAlloc(GMEM_MOVEABLE, total).ok()?;
        let ptr = GlobalLock(hmem) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(hmem);
            return None;
        }
        let out = slice::from_raw_parts_mut(ptr, total);
        out[..40].fill(0);
        out[0..4].copy_from_slice(&40u32.to_le_bytes());
        out[4..8].copy_from_slice(&(width as i32).to_le_bytes());
        out[8..12].copy_from_slice(&(height as i32).to_le_bytes());
        out[12..14].copy_from_slice(&1u16.to_le_bytes());
        out[14..16].copy_from_slice(&32u16.to_le_bytes());
        out[20..24].copy_from_slice(&(pixel_bytes as u32).to_le_bytes());

        let stride = width as usize * 4;
        for y in 0..height as usize {
            let src_y = height as usize - 1 - y;
            let src_row = &rgba[src_y * stride..(src_y + 1) * stride];
            let dst_row = &mut out[40 + y * stride..40 + (y + 1) * stride];
            for x in 0..width as usize {
                let i = x * 4;
                dst_row[i] = src_row[i + 2];
                dst_row[i + 1] = src_row[i + 1];
                dst_row[i + 2] = src_row[i];
                dst_row[i + 3] = src_row[i + 3];
            }
        }
        let _ = GlobalUnlock(hmem);

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
        let ok = SetClipboardData(CF_DIB, HANDLE(hmem.0)).is_ok();
        let _ = CloseClipboard();
        if !ok {
            let _ = GlobalFree(hmem);
            return None;
        }
        Some(GetClipboardSequenceNumber())
    }
}
