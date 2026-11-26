use std::fs::{File, OpenOptions};
use std::io;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;

#[cfg(windows)]
const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;

/// Opens an existing file with sequential-scan hint for faster read-ahead.
pub fn open_sequential_read(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(windows)]
    opts.custom_flags(FILE_FLAG_SEQUENTIAL_SCAN);
    opts.open(path)
}

/// Creates or truncates a file with sequential-scan hint for streaming writes.
pub fn create_sequential_write(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(windows)]
    opts.custom_flags(FILE_FLAG_SEQUENTIAL_SCAN);
    opts.open(path)
}

#[cfg(windows)]
fn drive_letter(path: &Path) -> Option<char> {
    use std::path::{Component, Prefix};
    let extract = |p: &Path| -> Option<char> {
        match p.components().next()? {
            Component::Prefix(pref) => match pref.kind() {
                Prefix::Disk(d) | Prefix::VerbatimDisk(d) => Some((d as char).to_ascii_uppercase()),
                _ => None,
            },
            _ => None,
        }
    };
    extract(path).or_else(|| std::env::current_dir().ok().and_then(|cwd| extract(&cwd)))
}

#[cfg(windows)]
mod win_disk {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    type Handle = *mut core::ffi::c_void;
    const INVALID_HANDLE_VALUE: Handle = (-1isize) as Handle;
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    const OPEN_EXISTING: u32 = 3;
    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;

    #[repr(C)]
    struct StoragePropertyQuery {
        property_id: u32,
        query_type: u32,
        additional_parameters: [u8; 1],
    }

    #[repr(C)]
    struct DeviceSeekPenaltyDescriptor {
        version: u32,
        size: u32,
        incurs_seek_penalty: u8,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            lp_file_name: *const u16,
            dw_desired_access: u32,
            dw_share_mode: u32,
            lp_security_attributes: *const core::ffi::c_void,
            dw_creation_disposition: u32,
            dw_flags_and_attributes: u32,
            h_template_file: Handle,
        ) -> Handle;
        fn DeviceIoControl(
            h_device: Handle,
            dw_io_control_code: u32,
            lp_in_buffer: *const core::ffi::c_void,
            n_in_buffer_size: u32,
            lp_out_buffer: *mut core::ffi::c_void,
            n_out_buffer_size: u32,
            lp_bytes_returned: *mut u32,
            lp_overlapped: *mut core::ffi::c_void,
        ) -> i32;
        fn CloseHandle(h_object: Handle) -> i32;
        fn GetDiskFreeSpaceExW(
            lp_directory_name: *const u16,
            lp_free_bytes_available_to_caller: *mut u64,
            lp_total_number_of_bytes: *mut u64,
            lp_total_number_of_free_bytes: *mut u64,
        ) -> i32;
    }

    pub fn free_space_bytes(path: &Path) -> Option<u64> {
        let dir = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        let mut dir_str = dir.to_string_lossy().into_owned();
        if dir_str.is_empty() {
            dir_str.push('.');
        }
        let wide: Vec<u16> = std::ffi::OsStr::new(&dir_str)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut avail = 0u64;
        let mut total = 0u64;
        let mut total_free = 0u64;
        let ok = unsafe {
            GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut total_free)
        };
        (ok != 0).then_some(avail)
    }

    pub fn is_rotational(path: &Path) -> bool {
        let Some(letter) = super::drive_letter(path) else {
            return false;
        };
        let vol = format!(r"\\.\{letter}:");
        let wide: Vec<u16> = std::ffi::OsStr::new(&vol)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        // Opening the volume with desired_access=0 queries storage properties without admin rights.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                core::ptr::null(),
                OPEN_EXISTING,
                0,
                core::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return false;
        }

        let query = StoragePropertyQuery {
            property_id: 7, // StorageDeviceSeekPenaltyProperty
            query_type: 0,  // PropertyStandardQuery
            additional_parameters: [0],
        };
        let mut desc = DeviceSeekPenaltyDescriptor {
            version: 0,
            size: 0,
            incurs_seek_penalty: 0,
        };
        let mut returned: u32 = 0;

        let ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                (&raw const query).cast(),
                core::mem::size_of::<StoragePropertyQuery>() as u32,
                (&raw mut desc).cast(),
                core::mem::size_of::<DeviceSeekPenaltyDescriptor>() as u32,
                &raw mut returned,
                core::ptr::null_mut(),
            )
        };
        unsafe {
            CloseHandle(handle);
        }
        ok != 0 && desc.incurs_seek_penalty != 0
    }
}

/// Returns `true` when the underlying storage incurs seek penalties (mechanical HDD).
pub fn is_rotational_drive(path: &Path) -> bool {
    #[cfg(windows)]
    {
        win_disk::is_rotational(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

/// Returns `true` if both paths live on the same Windows drive letter (or on non-Windows).
pub fn same_drive(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        match (drive_letter(a), drive_letter(b)) {
            (Some(da), Some(db)) => da == db,
            _ => true,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (a, b);
        true
    }
}

/// Returns available free bytes on the filesystem containing `path`.
pub fn available_disk_space(path: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        win_disk::free_space_bytes(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}
