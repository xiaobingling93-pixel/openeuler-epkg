//! lfs.rs - File system operations wrapper with trace logging
//!
//! ═══════════════════════════════════════════════════════════════════════════
//! ★★★ IMPORTANT: About using .exists() ★★★
//! ═══════════════════════════════════════════════════════════════════════════
//!
//! In epkg, we handle paths in two contexts:
//!   • Host context: ~/.epkg/envs/alpine/... are real paths on the host
//!   • Guest context: Files inside env may symlink to guest paths like /usr/bin
//!
//! Problem: When checking symlinks inside env from host context, the target may
//!          not exist on host (broken symlink), but is valid in guest namespace!
//!
//! Rules:
//!   ✗ NEVER use path.exists() directly - unclear intent, error-prone
//!   ✓ Use explicit functions from this module, function names express intent:
//!
//! ═══════════════════════════════════════════════════════════════════════════
//! Function Classification
//! ═══════════════════════════════════════════════════════════════════════════
//!
//! [Class 1: Env-Aware Functions] - For checking paths inside env_root
//!   These functions handle env internal files, correctly handling broken symlinks
//!   • exists_in_env()              - ★Check env file: regular file OR symlink★
//!   • exists_or_any_symlink()      - Check: exists OR is symlink (broken or valid)
//!   • resolve_symlink_in_env()     - Resolve symlink target path within env
//!
//! [Class 2: Host-Only Functions] - For pure host paths only
//!   These functions assume paths are entirely on host, no broken symlinks
//!   • exists_on_host()          - Check if host path exists (follow symlinks)
//!   • is_regular_file_on_host() - Check if regular file on host
//!
//! [Class 3: Utility Functions] - Neutral, no context assumptions
//!   These functions check file attributes only, no path context involved
//!   • is_symlink()              - Check if path is a symlink
//!   • exists_no_follow()        - Check path itself exists (incl. broken symlink)
//!   • symlink_metadata()        - Get metadata (no symlink follow)
//!   • metadata_on_host()        - Get metadata (follows symlinks) - for host paths
//!   • metadata_in_env()         - Get metadata (resolves symlink in env) - for env paths
//!
//! Exception: Internal lfs.rs functions (like resolve_symlink_in_env_recursive)
//!            may use .exists() directly as they already handle symlink logic.
//!
//!  ┌───────────────┬────────────────────────────┬───────────────────────────────────────┐
//!  │     类别      │            函数            │                 用途                  │
//!  ├───────────────┼────────────────────────────┼───────────────────────────────────────┤
//!  │ Env 感知函数  │ exists_in_env()            │ 检查 env 内文件（普通文件或 symlink） │
//!  ├───────────────┼────────────────────────────┼───────────────────────────────────────┤
//!  │               │ exists_or_any_symlink()    │ 检查：存在 或 是 symlink              │
//!  ├───────────────┼────────────────────────────┼───────────────────────────────────────┤
//!  │               │ resolve_symlink_in_env()   │ 在 env 内解析 symlink                 │
//!  ├───────────────┼────────────────────────────┼───────────────────────────────────────┤
//!  │               │ metadata_in_env()          │ 获取 metadata（在 env 内解析 symlink）│
//!  ├───────────────┼────────────────────────────┼───────────────────────────────────────┤
//!  │ Host 专用函数 │ exists_on_host()           │ 检查 host 路径（follow symlinks）     │
//!  ├───────────────┼────────────────────────────┼───────────────────────────────────────┤
//!  │               │ metadata_on_host()         │ 获取 metadata（follow symlinks）      │
//!  ├───────────────┼────────────────────────────┼───────────────────────────────────────┤
//!  │               │ is_regular_file_on_host()  │ 检查 host 上的普通文件                │
//!  ├───────────────┼────────────────────────────┼───────────────────────────────────────┤
//!  │ 工具性函数    │ is_symlink()               │ 检查是否为 symlink                    │
//!  ├───────────────┼────────────────────────────┼───────────────────────────────────────┤
//!  │               │ exists_no_follow()         │ 检查路径本身（包括 broken symlink）   │
//!  ├───────────────┼────────────────────────────┼───────────────────────────────────────┤
//!  │               │ symlink_metadata()         │ 获取 metadata（不 follow）            │
//!  └───────────────┴────────────────────────────┴───────────────────────────────────────┘

use std::fs;
use std::path::{Path, PathBuf};
use color_eyre::eyre::{eyre, WrapErr};
use color_eyre::Result;

/// Create a symbolic link that must point at a directory (or a missing path that should be a
/// **directory symlink** on Windows).
///
/// On Unix this is identical to [`symlink`]. On Windows, see [`symlink_to_directory`] in the
/// `cfg(windows)` impl (shared with libkrun virtiofs).
#[cfg(unix)]
pub fn symlink_to_directory<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> Result<()> {
    symlink(original, link)
}

/// Create a symbolic link that must point at a regular file (or a missing path that should be a
/// **file symlink** on Windows).
///
/// On Unix this is identical to [`symlink`]. On Windows, see [`symlink_to_file`] in the
/// `cfg(windows)` impl (shared with libkrun virtiofs).
#[cfg(unix)]
pub fn symlink_to_file<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> Result<()> {
    symlink(original, link)
}

/// Create a symbolic link.
#[cfg(unix)]
pub fn symlink<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> Result<()> {
    use std::os::unix::fs::symlink;
    let original = original.as_ref();
    let link = link.as_ref();
    log::trace!("creating symlink: {} -> {}", link.display(), original.display());
    symlink(original, link)
        .wrap_err_with(|| format!("Failed to create symlink from {} to {}", link.display(), original.display()))
}

/// Windows: directory symlink / junction / LX reparse (same implementation as libkrun
/// `git/libkrun/src/devices/src/virtio/fs/windows/symlink.rs`, `include!`d from `main.rs`).
///
/// ## Junction vs Symlink
///
/// When native symlink creation is unavailable (no Developer Mode or admin), this function
/// falls back to creating a **directory junction** for existing directories. Key differences:
///
/// | Feature | Junction | Symlink |
/// |---------|----------|---------|
/// | Target type | Local directories only | Files, directories, remote paths |
/// | Path format | Absolute path required | Relative or absolute |
/// | Separators | Backslash `\` only | Both `/` and `\` |
/// | Case sensitivity | Case-insensitive resolution | Case-insensitive resolution |
/// | Privilege | No special privilege | Developer Mode or admin |
///
/// Note: `force_symlink_to_directory` in utils.rs removes an existing link first. "force" means
/// overwrite, not infer symlink kind when the target is missing.
#[cfg(windows)]
pub fn symlink_to_directory<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> Result<()> {
    let original = original.as_ref();
    let link = link.as_ref();
    debug_assert_no_forward_slash(link);
    // Normalize the symlink target to use backslashes for Windows native access.
    // Forward slashes in symlink targets cause "InvalidFilename" errors when Windows
    // joins them with backslash paths, creating mixed separators like:
    // C:\...\env\usr/share\file.txt
    let normalized_original = normalize_symlink_target(original);
    // Decode PUA-encoded characters to get the original POSIX path for the LX symlink.
    // The LX symlink stores the target that the Linux guest will read, which expects
    // the original POSIX path (e.g., "Text::WrapI18N.3pm.gz", not PUA-encoded).
    let decoded_original = decode_path_from_windows(&normalized_original);
    let posix_target = decoded_original.to_string_lossy();
    log::trace!(
        "symlink_to_directory: {} -> {}",
        link.display(),
        normalized_original.display()
    );
    crate::krun_virtiofs_windows::symlink::symlink_to_directory(&normalized_original, link, posix_target.as_ref())
        .wrap_err_with(|| {
            format!(
                "Failed symlink_to_directory from {} to {}",
                normalized_original.display(),
                link.display()
            )
        })
}

/// Windows: file symlink, else hardlink/copy, else LX reparse (shared with libkrun virtiofs).
///
/// Note: `force_symlink_to_file` in utils.rs removes an existing link first.
#[cfg(windows)]
pub fn symlink_to_file<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> Result<()> {
    let original = original.as_ref();
    let link = link.as_ref();
    debug_assert_no_forward_slash(link);
    // Normalize the symlink target to use backslashes for Windows native access.
    let normalized_original = normalize_symlink_target(original);
    // Decode PUA-encoded characters to get the original POSIX path for the LX symlink.
    let decoded_original = decode_path_from_windows(&normalized_original);
    let posix_target = decoded_original.to_string_lossy();
    log::trace!("symlink_to_file: {} -> {}", link.display(), normalized_original.display());
    crate::krun_virtiofs_windows::symlink::symlink_to_file(&normalized_original, link, posix_target.as_ref())
        .wrap_err_with(|| {
            format!(
                "Failed symlink_to_file from {} to {}",
                normalized_original.display(),
                link.display()
            )
        })
}

/// Windows: generic symlink when target type is unknown; missing target → LX reparse (see virtiofs docs).
#[cfg(windows)]
pub fn symlink<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> Result<()> {
    let original = original.as_ref();
    let link = link.as_ref();
    debug_assert_no_forward_slash(link);
    // Normalize the symlink target to use backslashes for Windows native access.
    let normalized_original = normalize_symlink_target(original);
    // Decode PUA-encoded characters to get the original POSIX path for the LX symlink.
    let decoded_original = decode_path_from_windows(&normalized_original);
    let posix_target = decoded_original.to_string_lossy();
    log::trace!("creating symlink: {} -> {}", link.display(), normalized_original.display());
    crate::krun_virtiofs_windows::symlink::symlink(&normalized_original, link, posix_target.as_ref())
        .wrap_err_with(|| {
            format!(
                "Failed symlink from {} to {}",
                normalized_original.display(),
                link.display()
            )
        })
}

/// Create a hard link.
pub fn hard_link<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> Result<()> {
    let original = original.as_ref();
    let link = link.as_ref();
    debug_assert_no_forward_slash(original);
    debug_assert_no_forward_slash(link);
    log::trace!("creating hard link: {} -> {}", link.display(), original.display());
    fs::hard_link(original, link)
        .wrap_err_with(|| format!("Failed to create hard link from {} to {}", link.display(), original.display()))
}

/// Copy a file.
pub fn copy<P: AsRef<Path>, Q: AsRef<Path>>(source: P, target: Q) -> Result<u64> {
    let source = source.as_ref();
    let target = target.as_ref();
    debug_assert_no_forward_slash(source);
    debug_assert_no_forward_slash(target);
    log::trace!("copying file: {} -> {}", source.display(), target.display());
    fs::copy(source, target)
        .wrap_err_with(|| format!("Failed to copy {} to {}", source.display(), target.display()))
}

/// Rename a file or directory.
#[cfg(not(windows))]
pub fn rename<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> Result<()> {
    let from = from.as_ref();
    let to = to.as_ref();
    debug_assert_no_forward_slash(from);
    debug_assert_no_forward_slash(to);
    log::trace!("renaming: {} -> {}", from.display(), to.display());
    fs::rename(from, to)
        .wrap_err_with(|| format!("Failed to rename {} to {}", from.display(), to.display()))
}

/// Rename a file or directory on Windows.
/// On Windows, fs::rename fails if the destination exists, so we use MoveFileEx with REPLACE_EXISTING.
#[cfg(windows)]
pub fn rename<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let from = from.as_ref();
    let to = to.as_ref();
    debug_assert_no_forward_slash(from);
    debug_assert_no_forward_slash(to);
    log::trace!("renaming: {} -> {}", from.display(), to.display());

    // Convert paths to wide strings
    let from_wide: Vec<u16> = from.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let to_wide: Vec<u16> = to.as_os_str().encode_wide().chain(std::iter::once(0)).collect();

    // SAFETY: We're calling MoveFileExW with valid null-terminated wide strings
    unsafe {
        let result = MoveFileExW(
            windows::core::PCWSTR(from_wide.as_ptr()),
            windows::core::PCWSTR(to_wide.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        );
        result.map_err(|e| color_eyre::eyre::eyre!(
            "Failed to rename {} to {}: {}",
            from.display(),
            to.display(),
            e
        ))?;
    }
    Ok(())
}

/// Remove a file.
#[cfg(not(windows))]
pub fn remove_file<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    log::trace!("removing file: {}", path.display());
    fs::remove_file(path)
        .wrap_err_with(|| format!("Failed to remove file {}", path.display()))
}

/// Remove a file or junction on Windows.
/// On Windows, junctions (directory reparse points) require remove_dir instead of remove_file.
#[cfg(windows)]
pub fn remove_file<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    log::trace!("removing file: {}", path.display());
    // First try remove_file for regular files
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(5) => {
            // Access denied - might be a junction, try remove_dir
            log::trace!("remove_file failed with access denied, trying remove_dir for potential junction: {}", path.display());
            fs::remove_dir(path)
                .wrap_err_with(|| format!("Failed to remove file/junction {}", path.display()))
        }
        Err(e) => Err(e).wrap_err_with(|| format!("Failed to remove file {}", path.display())),
    }
}

/// Remove a directory and all its contents.
#[cfg(not(windows))]
pub fn remove_dir_all<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    log::trace!("removing directory recursively: {}", path.display());
    fs::remove_dir_all(path)
        .wrap_err_with(|| format!("Failed to remove directory {}", path.display()))
}

#[cfg(windows)]
pub fn remove_dir_all<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    log::trace!("removing directory recursively: {}", path.display());
    remove_dir_all::remove_dir_all(path)
        .wrap_err_with(|| format!("Failed to remove directory {}", path.display()))
}

/// Check if the current Windows user has permission to create symbolic links.
///
/// On Windows, creating symlinks requires either:
/// - Administrator privileges, OR
/// - Developer Mode enabled (Windows 10+)
///
/// This function tests symlink creation capability by attempting to create
/// a temporary symlink. The result is cached for the process lifetime.
///
/// Delegates to the same implementation as libkrun virtiofs (`krun_virtiofs_windows::symlink`).
#[cfg(windows)]
pub fn can_create_symlinks() -> bool {
    crate::krun_virtiofs_windows::symlink::can_create_symlinks()
}

/// On Unix systems, symlinks are always available.
#[cfg(unix)]
pub fn can_create_symlinks() -> bool {
    true
}

/// Create a directory and all its parent directories if they are missing.
pub fn create_dir_all<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    debug_assert_no_forward_slash(path);
    log::trace!("creating directory: {}", path.display());
    fs::create_dir_all(path)
        .wrap_err_with(|| format!("Failed to create directory {}", path.display()))
}

/// Create a file.
pub fn file_create<P: AsRef<Path>>(path: P) -> Result<fs::File> {
    let path = path.as_ref();
    log::trace!("creating file: {}", path.display());
    fs::File::create(path)
        .wrap_err_with(|| format!("Failed to create file {}", path.display()))
}

/// Write content to a file.
pub fn write<P: AsRef<Path>, C: AsRef<[u8]>>(path: P, content: C) -> Result<()> {
    let path = path.as_ref();
    log::trace!("writing file: {}", path.display());
    fs::write(path, content)
        .wrap_err_with(|| format!("Failed to write file {}", path.display()))
}

/// Set permissions of a file or directory.
pub fn set_permissions<P: AsRef<Path>>(path: P, permissions: fs::Permissions) -> Result<()> {
    let path = path.as_ref();
    log::trace!("setting permissions for: {} {:?}", path.display(), permissions);
    fs::set_permissions(path, permissions)
        .wrap_err_with(|| format!("Failed to set permissions for {}", path.display()))
}

/// Remove an empty directory.
pub fn remove_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    log::trace!("removing directory: {}", path.display());
    fs::remove_dir(path)
        .wrap_err_with(|| format!("Failed to remove directory {}", path.display()))
}

/// Create a single directory (parent directories must exist).
#[allow(dead_code)]
pub fn create_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    log::trace!("creating single directory: {}", path.display());
    fs::create_dir(path)
        .wrap_err_with(|| format!("Failed to create directory {}", path.display()))
}

///////////////////////////////
// Case sensitivity support //
///////////////////////////////

/// Enable case sensitivity for a directory on Windows NTFS.
///
/// This requires either:
/// - Administrator privileges, OR
/// - Developer Mode enabled (Windows 10+)
///
/// On success, the directory will have case-sensitive semantics (like Linux).
/// Subdirectories created after this call will inherit case sensitivity.
///
/// Returns Ok(()) on success or when case sensitivity is already enabled.
/// Returns Ok(()) with log::info on failure (best-effort, silent failure).
#[cfg(windows)]
pub fn set_case_sensitive<P: AsRef<Path>>(path: P) -> Result<()> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING, SetFileInformationByHandle, FILE_INFO_BY_HANDLE_CLASS,
    };
    use windows::core::PCWSTR;

    let path = path.as_ref();
    log::trace!("setting case sensitivity for: {}", path.display());

    // Convert path to wide string with null terminator
    let path_wide: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // Open directory with GENERIC_READ (0x80000000) | GENERIC_WRITE (0x40000000)
    // for setting case sensitivity
    let handle = unsafe {
        CreateFileW(
            PCWSTR(path_wide.as_ptr()),
            0x80000000u32 | 0x40000000u32, // GENERIC_READ | GENERIC_WRITE
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            HANDLE::default(),
        )
    };

    match handle {
        Ok(h) if h != INVALID_HANDLE_VALUE => {
            // FILE_CASE_SENSITIVE_INFORMATION: Flags = 1 enables case sensitivity
            // See: https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_case_sensitive_information
            #[repr(C)]
            struct FileCaseSensitiveInformation {
                flags: u32,
            }

            let info = FileCaseSensitiveInformation { flags: 1 };

            let result = unsafe {
                SetFileInformationByHandle(
                    h,
                    FILE_INFO_BY_HANDLE_CLASS(33), // FileCaseSensitiveInformation
                    &info as *const _ as *const std::ffi::c_void,
                    std::mem::size_of::<FileCaseSensitiveInformation>() as u32,
                )
            };

            let _ = unsafe { CloseHandle(h) };

            if result.is_ok() {
                log::debug!("Enabled case sensitivity for: {}", path.display());
            } else {
                log::info!(
                    "Could not enable case sensitivity for {} (requires admin or developer mode)",
                    path.display()
                );
            }
        }
        _ => {
            let err = std::io::Error::last_os_error();
            log::info!(
                "Could not open directory for case sensitivity setting {}: {}",
                path.display(),
                err
            );
        }
    }

    Ok(())
}

/// On Unix systems, case sensitivity is always enabled.
#[cfg(not(windows))]
pub fn set_case_sensitive<P: AsRef<Path>>(_path: P) -> Result<()> {
    Ok(())
}

/// Create a directory and all its parent directories, then enable case sensitivity on Windows.
///
/// Case sensitivity setting is best-effort: failures are logged but don't cause errors.
/// This is useful for directories that will contain Linux-style packages where
/// case-sensitive file names matter (e.g., OpenSSL vs openssl).
#[cfg(windows)]
pub fn create_dir_all_with_case_sensitivity<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    debug_assert_no_forward_slash(path);
    create_dir_all(path)?;
    set_case_sensitive(path)?;
    Ok(())
}

/// On Unix systems, case sensitivity is always enabled; delegate to create_dir_all.
#[cfg(not(windows))]
pub fn create_dir_all_with_case_sensitivity<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    create_dir_all(path)?;
    set_case_sensitive(path)?;
    Ok(())
}

/// Create a single directory, then enable case sensitivity on Windows.
///
/// Parent directories must exist. Case sensitivity setting is best-effort.
#[cfg(windows)]
#[allow(dead_code)]
pub fn create_dir_with_case_sensitivity<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    debug_assert_no_forward_slash(path);
    create_dir(path)?;
    set_case_sensitive(path)?;
    Ok(())
}

/// On Unix systems, case sensitivity is always enabled; delegate to create_dir.
#[cfg(not(windows))]
#[allow(dead_code)]
pub fn create_dir_with_case_sensitivity<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    create_dir(path)?;
    set_case_sensitive(path)?;
    Ok(())
}

/////////////////////
// Reflink support //
/////////////////////

/// Check if reflink (copy-on-write) is supported on the filesystem
/// This is done by attempting a test reflink operation using reflink()
/// Requires that paths are on the same filesystem (checked via same_fs parameter)
#[cfg(target_os = "linux")]
pub fn check_reflink_support(env_root: &Path, same_fs: bool) -> bool {
    use std::io::Write;

    // Only check if paths are on the same filesystem
    if same_fs {
        // Create a temporary test file
        let test_file = env_root.join(".epkg_reflink_test");

        // Create a small test file
        log::trace!("creating test file for reflink check: {}", test_file.display());
        if let Ok(mut file) = fs::File::create(&test_file) {
            if file.write_all(b"test").is_ok() {
                file.sync_all().ok();

                // Try to create a reflink using reflink()
                let test_target = env_root.join(".epkg_reflink_test_target");
                let result = reflink(&test_file, &test_target).is_ok();

                // Clean up test files
                let _ = remove_file(&test_file);
                let _ = remove_file(&test_target);

                return result;
            } else {
                let _ = remove_file(&test_file);
            }
        }
    }
    false
}

#[cfg(not(target_os = "linux"))]
pub fn check_reflink_support(_env_root: &Path, _same_fs: bool) -> bool {
    false
}

/// Create a reflink (copy-on-write) copy of a file
#[cfg(target_os = "linux")]
pub fn reflink(source: &Path, target: &Path) -> Result<()> {
    use std::os::unix::io::AsRawFd;

    log::trace!("creating reflink: {} -> {}", source.display(), target.display());

    // Open source file for reading
    let src_file = fs::File::open(source)
        .with_context(|| format!("Failed to open source file {}", source.display()))?;

    // Create target file
    let dst_file = fs::File::create(target)?;

    // FICLONE ioctl request value
    // libc::Ioctl is a type alias that's c_int on musl and c_ulong on GNU
    const FICLONE: libc::Ioctl = 0x4004_9409;
    unsafe {
        let result = libc::ioctl(dst_file.as_raw_fd(), FICLONE, src_file.as_raw_fd());
        if result != 0 {
            return Err(eyre!("ioctl FICLONE failed: {}", std::io::Error::last_os_error()));
        }
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn reflink(_source: &Path, _target: &Path) -> Result<()> {
    Err(eyre!("Reflink not supported on this platform"))
}

/// Try to create a reflink (copy-on-write) copy of a file; fall back to regular copy if reflink fails.
/// If can_reflink is false, directly perform a regular copy.
pub fn reflink_or_copy<P: AsRef<Path>, Q: AsRef<Path>>(source: P, target: Q, can_reflink: bool) -> Result<u64> {
    let source = source.as_ref();
    let target = target.as_ref();
    if !can_reflink {
        log::trace!("reflink not supported, copying: {} -> {}", source.display(), target.display());
        return copy(source, target);
    }
    log::trace!("trying reflink: {} -> {}", source.display(), target.display());
    match reflink(source, target) {
        Ok(()) => {
            // Get file size to return, matching copy() behavior
            let metadata = fs::metadata(source)
                .wrap_err_with(|| format!("Failed to get metadata for {}", source.display()))?;
            log::trace!("created reflink: {} -> {}", source.display(), target.display());
            Ok(metadata.len())
        }
        Err(e) => {
            log::debug!("reflink not work for {} -> {}: {}, falling back to copy",
                       source.display(), target.display(), e);
            copy(source, target)
        }
    }
}


//////////////////////////
// Read-only operations //
//////////////////////////

// ═══════════════════════════════════════════════════════════════════════════
// 【第三类：工具性函数】(Utility Functions) - 中性，无上下文假设
// ═══════════════════════════════════════════════════════════════════════════

/// 获取文件 metadata，不 follow symlink
/// 用于检查 symlink 本身（而非 target）的属性
pub fn symlink_metadata<P: AsRef<Path>>(path: P) -> Result<fs::Metadata> {
    let path = path.as_ref();
    log::trace!("getting metadata (no follow): {}", path.display());
    fs::symlink_metadata(path)
        .wrap_err_with(|| format!("Failed to get metadata for {}", path.display()))
}

/// 获取文件 metadata，follow symlink 到 target
/// 用于 host 路径，需要获取 symlink target 的实际属性
pub fn metadata_on_host<P: AsRef<Path>>(path: P) -> Result<fs::Metadata> {
    let path = path.as_ref();
    log::trace!("getting metadata (follow symlinks, host path): {}", path.display());
    fs::metadata(path)
        .wrap_err_with(|| format!("Failed to get metadata for {}", path.display()))
}

/// 获取 env 内文件的 metadata，自动解析 symlink 到 target
/// 用于 env 路径，当需要获取 symlink target 的实际属性时使用
/// 注意：如果 symlink 指向 guest 路径（在 host 上不存在），此函数会失败
/// 对于这种情况，应该使用 symlink_metadata() 检查 symlink 本身
#[allow(dead_code)]
pub fn metadata_in_env<P: AsRef<Path>>(path: P, env_root: &Path) -> Result<fs::Metadata> {
    let path = path.as_ref();
    log::trace!("getting metadata (resolve symlink in env): {}", path.display());

    // If it's a symlink, resolve it first
    if let Some(link_target) = resolve_symlink_in_env(path, env_root) {
        fs::metadata(&link_target)
            .wrap_err_with(|| format!("Failed to get metadata for symlink target {}", link_target.display()))
    } else {
        // Not a symlink, get metadata directly
        fs::metadata(path)
            .wrap_err_with(|| format!("Failed to get metadata for {}", path.display()))
    }
}

/// Check if path is a symlink
/// ★ NOTE: Only checks for true symlinks, NOT Windows junctions
/// Use is_symlink_or_junction() when checking directories that may be junctions
pub fn is_symlink(path: &Path) -> bool {
    match symlink_metadata(path) {
        Ok(metadata) => metadata.file_type().is_symlink(),
        Err(_) => false,
    }
}

/// Check if path is a directory symlink (symlink_dir, not symlink_file)
/// On Windows, this checks FILE_ATTRIBUTE_DIRECTORY flag on the symlink itself.
/// This is reliable even when the symlink target doesn't exist (dead symlink).
/// Returns false if path is not a symlink or check fails.
#[cfg(windows)]
pub fn is_directory_symlink(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    match symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_symlink() {
                return false;
            }
            // FILE_ATTRIBUTE_DIRECTORY = 0x10
            // On Windows, symlink_dir sets this flag, symlink_file doesn't
            const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
            metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0
        }
        Err(_) => false,
    }
}

#[cfg(not(windows))]
pub fn is_directory_symlink(path: &Path) -> bool {
    // On Unix, check if symlink target is a directory
    match symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_symlink() {
                return false;
            }
            // Follow the symlink to check target type
            path.metadata().map(|m| m.is_dir()).unwrap_or(false)
        }
        Err(_) => false,
    }
}

/// Check if path is a symlink or Windows junction
/// On Windows, junctions are directory reparse points created by lfs::symlink() /
/// lfs::symlink_to_directory() when the target directory exists
/// Use this when checking directory links that may be junctions
#[cfg(windows)]
pub fn is_symlink_or_junction(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    match symlink_metadata(path) {
        Ok(metadata) => {
            // Check if it's a regular symlink
            if metadata.file_type().is_symlink() {
                return true;
            }
            // Check if it's a reparse point (junction)
            // FILE_ATTRIBUTE_REPARSE_POINT = 0x400
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
            metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        }
        Err(_) => false,
    }
}

#[cfg(not(windows))]
pub fn is_symlink_or_junction(path: &Path) -> bool {
    is_symlink(path)
}

/// Normalize path separators for Windows symlink targets.
///
/// Converts forward slashes `/` to backslashes `\` to ensure compatibility with:
/// - **Junction creation**: Junctions require backslash separators only
/// - **Windows native paths**: Mixed separators cause error 123 (InvalidFilename)
///
/// Symlink targets from POSIX sources (tar archives, msys2/cygwin) may contain
/// forward slashes. This normalization prevents errors like:
/// - `C:\path/to/file` (mixed separators)
/// - Junction creation failure due to forward slashes
#[cfg(windows)]
fn normalize_symlink_target(target: &Path) -> PathBuf {
    let target_str = target.to_string_lossy();
    PathBuf::from(target_str.replace('/', "\\"))
}

/// Debug assertion to catch mixed path separators on Windows.
/// Forward slashes in Windows paths cause error 123 (InvalidFilename).
/// This should be called at control points where paths enter filesystem operations.
#[cfg(all(windows, debug_assertions))]
fn debug_assert_no_forward_slash(path: &Path) {
    let path_str = path.to_string_lossy();
    // Allow forward slashes only in:
    // 1. Pure relative paths (no backslash) - e.g., "usr/lib"
    // 2. Paths starting with "/" (Unix absolute paths, used internally)
    // Disallow mixed separators like "C:\path/to/file"
    if path_str.contains('\\') && path_str.contains('/') {
        // Check if it's a UNC path prefix (\\?\) which is valid
        if !path_str.starts_with("//?/") && !path_str.starts_with("\\\\?\\") {
            debug_assert!(
                false,
                "Mixed path separators detected: {:?}\n\
                 This will cause Windows error 123 (InvalidFilename).\n\
                 Path should be normalized to use consistent separators.",
                path
            );
        }
    }
}

#[cfg(not(all(windows, debug_assertions)))]
fn debug_assert_no_forward_slash(_path: &Path) {
    // No-op on non-Windows or release builds
}

/// Resolve ancestor directory symlinks - simplified to no-op.
///
/// Windows can create files inside symlinked directories transparently.
/// The symlink target normalization is handled by normalize_symlink_target()
/// when symlinks are created, ensuring consistent path separators.
#[cfg(windows)]
pub fn resolve_ancestor_symlink(path: &Path) -> PathBuf {
    path.to_path_buf()
}

#[cfg(not(windows))]
pub fn resolve_ancestor_symlink(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// Check if path itself exists (does NOT follow symlinks)
/// Returns true for regular files, directories, AND symlinks (even broken ones)
/// Use case: Check if symlink file itself exists before removing
pub fn exists_no_follow<P: AsRef<Path>>(path: P) -> bool {
    symlink_metadata(path.as_ref()).is_ok()
}

// ═══════════════════════════════════════════════════════════════════════════
// [Class 1: Env-Aware Functions] - For checking paths inside env_root
// ═══════════════════════════════════════════════════════════════════════════

/// ★ Check if file exists in environment ★
/// Use case: Check files in env_root from host context
/// Returns true: file is regular file OR is symlink (regardless of target existence)
/// Example: Check env_root/usr/local/bin/tool (may be broken symlink)
pub fn exists_in_env<P: AsRef<Path>>(path: P) -> bool {
    let path = path.as_ref();
    is_regular_file_on_host(path) || is_symlink(path)
}

/// Check if path exists OR is any symlink (broken or valid)
/// Use case: Check env paths where symlinks may point to guest paths
/// Returns true if: target exists OR path is a symlink (any target)
pub fn exists_or_any_symlink<P: AsRef<Path>>(path: P) -> bool {
    path.as_ref().exists() || is_symlink(path.as_ref())
}

// ═══════════════════════════════════════════════════════════════════════════
// [Class 2: Host-Only Functions] - For pure host paths only
// ═══════════════════════════════════════════════════════════════════════════

/// Check if host path exists (follows symlinks to target)
/// Use case: Regular file/directory checks on host, NOT for env_root internal paths
/// Example: Check ~/.config/epkg/tool/env_vars directory
/// WARNING: Do NOT use for paths inside env_root!
pub fn exists_on_host<P: AsRef<Path>>(path: P) -> bool {
    path.as_ref().exists()
}

/// Check if path is a regular file on host (excludes symlinks and directories)
/// Use case: Confirm it's a real file, not a symlink
pub fn is_regular_file_on_host<P: AsRef<Path>>(path: P) -> bool {
    match symlink_metadata(path.as_ref()) {
        Ok(metadata) => metadata.file_type().is_file(),
        Err(_) => false,
    }
}

/// Touch a file to update its modification time (set to current time)
#[cfg(unix)]
pub fn touch(path: &Path) -> Result<()> {
    use crate::posix::posix_utime;
    posix_utime(path, None, None)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to touch file {}: {:?}", path.display(), e))
}

#[cfg(windows)]
pub fn touch(path: &Path) -> Result<()> {
    // On Windows, we can't easily update file times without opening the file
    // For now, just try to open the file in read-write mode which will update access time
    match std::fs::OpenOptions::new().write(true).read(true).open(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // File doesn't exist, create it
            std::fs::File::create(path)
                .wrap_err_with(|| format!("Failed to create file {}", path.display()))?;
            Ok(())
        }
        Err(e) => Err(color_eyre::eyre::eyre!("Failed to touch file {}: {}", path.display(), e)),
    }
}

//////////////////////////////
// Symlink resolution utils //
//////////////////////////////

/// Resolve a symlink within an environment root, handling relative paths and preventing escape.
///
/// This function safely resolves symlinks that may be part of a chroot-like environment,
/// ensuring the resolved path stays within the environment root. It handles both absolute
/// and relative symlink targets, including system paths that need to be mapped into the
/// environment.
///
/// # Arguments
/// * `symlink_path` - Path to the symlink to resolve
/// * `env_root` - Root directory of the environment
///
/// # Returns
/// * `Some(PathBuf)` - The resolved target path within the environment
/// * `None` - If the symlink cannot be resolved or would escape the environment
///
/// # Security
/// The resolved path is guaranteed to be within `env_root` (or `None`). Relative symlinks
/// containing `..` components that would escape the environment root are rejected.
pub fn resolve_symlink_in_env(symlink_path: &std::path::Path, env_root: &std::path::Path) -> Option<std::path::PathBuf> {
    resolve_symlink_in_env_recursive(symlink_path, env_root, 0)
}

/// Helper to check if a target exists (including broken symlinks) and resolve it.
/// Returns Some(resolved_path) if target exists or is a symlink (recursively resolved).
/// Returns None if target doesn't exist.
fn resolve_target_in_env(target_in_env: &Path, env_root: &Path, depth: usize) -> Option<PathBuf> {
    // Use exists_or_any_symlink - symlinks may be "broken" in host context
    // but valid inside namespace where all paths are mounted
    if exists_or_any_symlink(target_in_env) {
        if is_symlink(target_in_env) {
            // Recursively resolve symlinks
            return resolve_symlink_in_env_recursive(target_in_env, env_root, depth + 1);
        }
        return Some(target_in_env.to_path_buf());
    }
    None
}

fn resolve_symlink_in_env_recursive(symlink_path: &std::path::Path, env_root: &std::path::Path, depth: usize) -> Option<std::path::PathBuf> {
    log::trace!("resolve_symlink_in_env_recursive: symlink_path={:?}, env_root={:?}, depth={}", symlink_path, env_root, depth);
    // Prevent infinite recursion
    if depth > 20 {
        log::trace!("resolve_symlink_in_env_recursive: depth limit exceeded");
        return None;
    }

    // First check if the path exists and is a regular file (not a symlink)
    if is_regular_file_on_host(symlink_path) {
        // It's a regular file, not a symlink
        // Example: ~/.epkg/envs/alpine/usr/bin/bash is a regular executable file
        // Return: Some(~/.epkg/envs/alpine/usr/bin/bash) - the resolved target path (same as input)
        log::trace!("resolve_symlink_in_env_recursive: regular file, returning {:?}", symlink_path);
        return Some(symlink_path.to_path_buf());
    }

    // If it's a symlink, read the target and check if the target exists within the environment
    if let Ok(link_target) = std::fs::read_link(symlink_path) {

        if link_target.is_absolute() {
            log::trace!("resolve_symlink_in_env_recursive: absolute symlink target={:?}", link_target);
            // For system paths, map them into the environment root
            // This avoids checking host system paths that might coincidentally exist
            // Example: ~/.epkg/envs/alpine/usr/bin/sh -> /usr/bin/bash -> Some(~/.epkg/envs/alpine/usr/bin/bash)
            let is_system_path = link_target.starts_with("/usr") ||
                                 link_target.starts_with("/bin") ||
                                 link_target.starts_with("/sbin") ||
                                 link_target.starts_with("/lib") ||
                                 link_target.starts_with("/lib64") ||
                                 link_target.starts_with("/lib32") ||
                                 link_target.starts_with("/libx32");
            log::trace!("resolve_symlink_in_env_recursive: is_system_path={}", is_system_path);
            if is_system_path {
                let target_rel = link_target.strip_prefix("/").unwrap_or(&link_target);
                let target_in_env = normalize_path_separators(&env_root.join(target_rel));
                log::trace!("resolve_symlink_in_env_recursive: mapped to target_in_env={:?}", target_in_env);
                match resolve_target_in_env(&target_in_env, env_root, depth) {
                    Some(result) => return Some(result),
                    None => log::trace!("resolve_symlink_in_env_recursive: target_in_env does not exist"),
                }
            }

            // Allow symlinks pointing within the same environment root
            // if link_target.starts_with(env_root) && link_target.exists() {
            //     return Some(link_target);
            // }

            // Allow symlinks pointing into the epkg store
            // if link_target.starts_with(&dirs().epkg_store) && link_target.exists() {
            //     return Some(link_target);
            // }

            // Special case: symlink pointing to the current executable (epkg binary)
            // if let Ok(current_exe) = std::env::current_exe() {
            //     if link_target == current_exe && link_target.exists() {
            //         return Some(link_target);
            //     }
            // }

            // For other absolute paths (e.g., /etc), first check if target exists within env_root,
            // then fall back to host path check.
            let target_rel = link_target.strip_prefix("/").unwrap_or(&link_target);
            let target_in_env = normalize_path_separators(&env_root.join(target_rel));
            log::debug!("resolve_symlink_in_env_recursive: checking other path {:?}, target_in_env={:?}", link_target, target_in_env);
            match resolve_target_in_env(&target_in_env, env_root, depth) {
                Some(result) => {
                    log::debug!("resolve_symlink_in_env_recursive: target exists in env_root, returning {:?}", target_in_env);
                    return Some(result);
                }
                None => {}
            }
            // Check if exists on host (for paths that are truly on host)
            if exists_on_host(&link_target) {
                log::debug!("resolve_symlink_in_env_recursive: absolute path exists on host, returning {:?}", link_target);
                return Some(link_target);
            } else {
                log::debug!("resolve_symlink_in_env_recursive: other absolute path does not exist on host: {:?}", link_target);
            }
        } else {
            // Relative symlink: resolve relative to the symlink's directory
            // Example: ~/.epkg/envs/alpine/usr/bin/sh -> bash -> Some(~/.epkg/envs/alpine/usr/bin/bash)
            let symlink_dir = symlink_path.parent()?;
            let resolved_path = symlink_dir.join(&link_target);
            if exists_on_host(&resolved_path) {
                if is_symlink(&resolved_path) {
                    // Recursively resolve within environment
                    return resolve_symlink_in_env_recursive(&resolved_path, env_root, depth + 1);
                }
                log::trace!("resolve_symlink_in_env_recursive: relative symlink resolved to regular file, returning {:?}", resolved_path);
                return Some(resolved_path);
            }
        }
    }

    // Return: None - symlink_path doesn't exist on host, symlink target doesn't exist in environment, or symlink couldn't be read
    log::trace!("resolve_symlink_in_env_recursive: no resolution found for {:?}", symlink_path);
    None
}

// ============================================================================
// Win32 PUA filename/path mapping (shared implementation, see virtio/fs/windows/win32_pua_paths.rs)
// ============================================================================

#[allow(dead_code)]
mod win32_pua_paths {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/git/libkrun/src/devices/src/virtio/fs/windows/win32_pua_paths.rs"
    ));
}

#[allow(unused_imports)]
pub use win32_pua_paths::{
    decode_filename_from_windows,
    decode_path_from_windows,
    has_invalid_windows_chars,
    host_path_from_manifest_rel_path,
    sanitize_path_for_windows,
};

/// Normalize path separators for Windows.
/// Converts forward slashes to backslashes to avoid mixed separators.
/// This is needed when joining Windows paths with Unix-style relative paths.
#[cfg(windows)]
pub fn normalize_path_separators(path: &Path) -> PathBuf {
    let path_str = path.to_string_lossy();
    PathBuf::from(path_str.replace('/', "\\"))
}

#[cfg(not(windows))]
pub fn normalize_path_separators(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// Normalize path by resolving `.` and `..` components.
/// This is needed when comparing paths that may contain relative components.
/// Unlike `canonicalize()`, this does NOT require the path to exist.
///
/// Example: `/foo/bar/../baz` -> `/foo/baz`
pub fn normalize_path_components(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => continue,
            Component::ParentDir => {
                match components.last() {
                    Some(Component::Normal(_)) => {
                        components.pop();
                    }
                    Some(Component::RootDir) => {
                        // /.. -> /
                        continue;
                    }
                    _ => components.push(component),
                }
            }
            _ => components.push(component),
        }
    }
    if components.is_empty() {
        components.push(Component::CurDir);
    }
    components.iter().collect()
}
