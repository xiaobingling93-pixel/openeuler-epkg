// NTFS Extended Attributes for POSIX metadata (WSL-compatible)
//
// This module provides functions to read/write NTFS Extended Attributes (EA)
// for storing POSIX file metadata on Windows. The EA names are compatible
// with WSL (Windows Subsystem for Linux) for interoperability.
//
// EA Names:
// - $LXUID: User Owner ID (uid, 4 bytes LE)
// - $LXGID: Group Owner ID (gid, 4 bytes LE)
// - $LXMOD: File mode (permissions + file type, 4 bytes LE)
// - $LXDEV: Device ID (for device files, 4 bytes LE)

use std::io;
use std::path::Path;

/// WSL-compatible Extended Attribute names for storing POSIX metadata
pub const LX_EA_UID:   &str = "$LXUID";
pub const LX_EA_GID:   &str = "$LXGID";
pub const LX_EA_MODE:  &str = "$LXMOD";
pub const LX_EA_DEV:   &str = "$LXDEV";

/// Read an NTFS Extended Attribute value from a file.
///
/// Returns None if the EA doesn't exist or on any error.
#[cfg(target_os = "windows")]
pub fn get_file_ea(path: &Path, name: &str) -> Option<Vec<u8>> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_GENERIC_READ, FILE_READ_EA, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };

    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();

    unsafe {
        let handle = CreateFileW(
            windows::core::PCWSTR(path_wide.as_ptr()),
            FILE_GENERIC_READ.0 | FILE_READ_EA,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            Some(ptr::null_mut()),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            HANDLE::default(),
        );

        if handle.is_invalid() {
            return None;
        }

        let result = read_ea_via_ntdll(handle, name);
        let _ = CloseHandle(handle);
        result
    }
}

/// Read EA using NtQueryEaFile (internal helper)
#[cfg(target_os = "windows")]
unsafe fn read_ea_via_ntdll(handle: windows::Win32::Foundation::HANDLE, name: &str) -> Option<Vec<u8>> {
    // Buffer for EA information (large enough for our needs)
    let mut buffer = vec![0u8; 4096];
    let mut return_length: u32 = 0;

    // Load ntdll and get NtQueryEaFile function
    let ntdll = libloading::Library::new("ntdll.dll").ok()?;
    let nt_query_ea_file: libloading::Symbol<
        unsafe extern "system" fn(
            handle: windows::Win32::Foundation::HANDLE,
            ea_info: *mut u8,
            length: u32,
            return_length: *mut u32,
        ) -> i32
    > = ntdll.get(b"NtQueryEaFile").ok()?;

    let status = nt_query_ea_file(
        handle,
        buffer.as_mut_ptr(),
        buffer.len() as u32,
        &mut return_length,
    );

    if status < 0 {
        return None;
    }

    // Parse the returned EA list to find our attribute
    let mut offset = 0usize;
    let name_bytes = name.as_bytes();

    while offset < return_length as usize {
        let ptr = buffer.as_ptr().add(offset);
        let next_offset = *(ptr as *const u32);
        let name_len = *(ptr.add(5) as *const u8) as usize;
        let value_len = *(ptr.add(6) as *const u16) as usize;

        let ea_name_ptr = ptr.add(8);
        let ea_value_ptr = ea_name_ptr.add(name_len + 1);

        // Compare EA name (null-terminated)
        if name_len == name_bytes.len() {
            let ea_name = std::slice::from_raw_parts(ea_name_ptr, name_len);
            if ea_name == name_bytes {
                let value = std::slice::from_raw_parts(ea_value_ptr, value_len).to_vec();
                return Some(value);
            }
        }

        if next_offset == 0 {
            break;
        }
        offset += next_offset as usize;
    }

    None
}

/// Write an NTFS Extended Attribute value to a file.
///
/// Returns Ok(()) on success, Err on failure.
#[cfg(target_os = "windows")]
pub fn set_file_ea(path: &Path, name: &str, value: &[u8]) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_GENERIC_WRITE, FILE_WRITE_EA, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };

    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();

    unsafe {
        let handle = CreateFileW(
            windows::core::PCWSTR(path_wide.as_ptr()),
            FILE_GENERIC_WRITE.0 | FILE_WRITE_EA,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            Some(ptr::null_mut()),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            HANDLE::default(),
        );

        if handle.is_invalid() {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Failed to open file for EA write"));
        }

        let result = write_ea_via_ntdll(handle, name, value);
        let _ = CloseHandle(handle);
        result
    }
}

/// Write EA using NtSetEaFile (internal helper)
#[cfg(target_os = "windows")]
unsafe fn write_ea_via_ntdll(handle: windows::Win32::Foundation::HANDLE, name: &str, value: &[u8]) -> io::Result<()> {
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len();
    let value_len = value.len();

    // FILE_FULL_EA_INFORMATION structure for writing
    // Layout: next_entry_offset(4) + flags(1) + ea_name_length(1) + ea_value_length(2) + ea_name(N+1) + ea_value(V)
    let buffer_size = 8 + name_len + 1 + value_len;
    let mut buffer = vec![0u8; buffer_size];

    // Fill the structure
    // next_entry_offset = 0 (single entry)
    buffer[0..4].copy_from_slice(&0u32.to_le_bytes());
    // flags = 0
    buffer[4] = 0;
    // ea_name_length
    buffer[5] = name_len as u8;
    // ea_value_length
    buffer[6..8].copy_from_slice(&(value_len as u16).to_le_bytes());
    // ea_name (null-terminated)
    buffer[8..8+name_len].copy_from_slice(name_bytes);
    buffer[8+name_len] = 0; // null terminator
    // ea_value
    buffer[9+name_len..9+name_len+value_len].copy_from_slice(value);

    // Load ntdll and get NtSetEaFile function
    let ntdll = libloading::Library::new("ntdll.dll")
        .map_err(|e| io::Error::new(io::ErrorKind::NotFound, format!("ntdll.dll: {}", e)))?;

    let nt_set_ea_file: libloading::Symbol<
        unsafe extern "system" fn(
            handle: windows::Win32::Foundation::HANDLE,
            ea_info: *const u8,
            length: u32,
        ) -> i32
    > = ntdll.get(b"NtSetEaFile")
        .map_err(|e| io::Error::new(io::ErrorKind::NotFound, format!("NtSetEaFile: {}", e)))?;

    let status = nt_set_ea_file(handle, buffer.as_ptr(), buffer.len() as u32);

    if status < 0 {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("NtSetEaFile failed with status {}", status)
        ));
    }

    Ok(())
}

// Stub implementations for non-Windows targets (used during cross-compilation)
#[cfg(not(target_os = "windows"))]
pub fn get_file_ea(_path: &Path, _name: &str) -> Option<Vec<u8>> {
    // Not available on non-Windows targets
    None
}

#[cfg(not(target_os = "windows"))]
pub fn set_file_ea(_path: &Path, _name: &str, _value: &[u8]) -> io::Result<()> {
    // Not available on non-Windows targets
    Err(io::Error::new(io::ErrorKind::Unsupported, "EA not supported on this platform"))
}

/// Helper function to read uid from EA
pub fn get_file_uid(path: &Path) -> Option<u32> {
    get_file_ea(path, LX_EA_UID)
        .and_then(|v| v.try_into().ok())
        .map(|b: [u8; 4]| u32::from_le_bytes(b))
}

/// Helper function to read gid from EA
pub fn get_file_gid(path: &Path) -> Option<u32> {
    get_file_ea(path, LX_EA_GID)
        .and_then(|v| v.try_into().ok())
        .map(|b: [u8; 4]| u32::from_le_bytes(b))
}

/// Helper function to read mode from EA
pub fn get_file_mode(path: &Path) -> Option<u32> {
    get_file_ea(path, LX_EA_MODE)
        .and_then(|v| v.try_into().ok())
        .map(|b: [u8; 4]| u32::from_le_bytes(b))
}

/// Helper function to read device ID from EA
pub fn get_file_dev(path: &Path) -> Option<u32> {
    get_file_ea(path, LX_EA_DEV)
        .and_then(|v| v.try_into().ok())
        .map(|b: [u8; 4]| u32::from_le_bytes(b))
}

/// Helper function to write uid to EA
pub fn set_file_uid(path: &Path, uid: u32) -> io::Result<()> {
    set_file_ea(path, LX_EA_UID, &uid.to_le_bytes())
}

/// Helper function to write gid to EA
pub fn set_file_gid(path: &Path, gid: u32) -> io::Result<()> {
    set_file_ea(path, LX_EA_GID, &gid.to_le_bytes())
}

/// Helper function to write mode to EA
pub fn set_file_mode(path: &Path, mode: u32) -> io::Result<()> {
    set_file_ea(path, LX_EA_MODE, &mode.to_le_bytes())
}

/// Helper function to write device ID to EA
pub fn set_file_dev(path: &Path, dev: u32) -> io::Result<()> {
    set_file_ea(path, LX_EA_DEV, &dev.to_le_bytes())
}