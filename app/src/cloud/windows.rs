use std::ffi::c_void;
use std::ptr::{null, null_mut};

use windows::core::{w, Error, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Credentials::*;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const CREDENTIAL: PCWSTR = w!("app.memorylog.desktop/GoogleDrive");

pub(crate) fn http_error(error: Error) -> String {
    if error.code() == HRESULT::from_win32(ERROR_WINHTTP_TIMEOUT) {
        super::NETWORK_TIMEOUT.into()
    } else {
        error.to_string()
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void) -> Result<Self, String> {
        if raw.is_null() { Err(http_error(Error::from_win32())) } else { Ok(Self(raw)) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { let _ = WinHttpCloseHandle(self.0); }
    }
}

pub fn https(host: &str, method: &str, path: &str, headers: &str, body: &[u8]) -> Result<(u32, Vec<u8>), String> {
    let host = wide(host);
    let method = wide(method);
    let path = wide(path);
    let headers: Vec<_> = headers.encode_utf16().collect();
    let length = u32::try_from(body.len()).map_err(|_| "upload too large".to_string())?;
    unsafe {
        let session = Handle::new(WinHttpOpen(w!("Memory Log"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0))?;
        WinHttpSetTimeouts(session.0, 15_000, 15_000, 30_000, 30_000).map_err(http_error)?;
        let connection = Handle::new(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), 443, 0))?;
        let request = Handle::new(WinHttpOpenRequest(
            connection.0, PCWSTR(method.as_ptr()), PCWSTR(path.as_ptr()), PCWSTR::null(), PCWSTR::null(),
            null(), WINHTTP_FLAG_SECURE,
        ))?;
        WinHttpSetOption(Some(request.0), WINHTTP_OPTION_DISABLE_FEATURE,
            Some(&WINHTTP_DISABLE_REDIRECTS.to_ne_bytes())).map_err(http_error)?;
        WinHttpSendRequest(request.0, Some(&headers), (!body.is_empty()).then_some(body.as_ptr().cast()), length, length, 0)
            .map_err(http_error)?;
        WinHttpReceiveResponse(request.0, null_mut()).map_err(http_error)?;
        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(request.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(), Some((&mut status as *mut u32).cast()), &mut size, null_mut()).map_err(http_error)?;
        let mut contents = Vec::new();
        let mut buffer = [0u8; 16384];
        loop {
            let mut received = 0;
            WinHttpReadData(request.0, buffer.as_mut_ptr().cast(), buffer.len() as u32, &mut received).map_err(http_error)?;
            if received == 0 { break; }
            if contents.len() + received as usize > 24 * 1024 * 1024 {
                return Err("Google Drive response exceeds synchronization limit".into());
            }
            contents.extend_from_slice(&buffer[..received as usize]);
        }
        Ok((status, contents))
    }
}

pub fn open_browser(url: &str) -> Result<(), String> {
    let url = wide(url);
    let result = unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(url.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
    };
    if result.0 as usize <= 32 { Err("could not open the default browser".into()) } else { Ok(()) }
}

pub fn write_credentials(bytes: &[u8]) -> Result<(), String> {
    let mut target = wide("app.memorylog.desktop/GoogleDrive");
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC, TargetName: PWSTR(target.as_mut_ptr()),
        CredentialBlobSize: bytes.len() as u32, CredentialBlob: bytes.as_ptr() as *mut u8,
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        ..Default::default()
    };
    unsafe { CredWriteW(&credential, 0).map_err(|e| e.to_string()) }
}

pub fn read_credentials() -> Result<Vec<u8>, String> {
    unsafe {
        let mut ptr = null_mut::<CREDENTIALW>();
        CredReadW(CREDENTIAL, CRED_TYPE_GENERIC, 0, &mut ptr).map_err(|e| e.to_string())?;
        let credential = &*ptr;
        let contents = if credential.CredentialBlobSize == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(credential.CredentialBlob, credential.CredentialBlobSize as usize).to_vec()
        };
        CredFree(ptr.cast());
        Ok(contents)
    }
}

pub fn delete_credentials() {
    unsafe { let _ = CredDeleteW(CREDENTIAL, CRED_TYPE_GENERIC, 0); }
}
