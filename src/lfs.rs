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

/// Create a symbolic link on Windows.
///
/// Windows has different link types with different permission requirements:
/// - **Junction** (directory): No admin privileges required, but needs absolute path
/// - **Hard link** (file): No admin privileges required
/// - **Symlink** (file/dir): Requires admin privileges or Developer Mode
///
/// This function uses Junction for directories. For existing files it tries
/// `symlink_file` first, then falls back to a hard link, then to a full copy.
/// The behavior differs from Unix symlink:
///
/// 1. For existing directories: Creates a Junction (requires absolute path)
/// 2. For existing files: `symlink_file`, then `hard_link`, then `copy`
/// 3. If the resolved target does not exist: skips creation (returns Ok)
///
/// Note: `force_symlink()` in utils.rs calls this function after removing any
/// existing link at the target path. The "force" in `force_symlink` means
/// "overwrite if exists", not "create if target doesn't exist".
#[cfg(windows)]
fn symlink_resolve_original(original: &Path, link: &Path) -> PathBuf {
    // Relative paths resolve relative to link's parent — matches Unix symlink
    // semantics (target string is interpreted from link's location).
    if original.is_relative() {
        if let Some(parent) = link.parent() {
            parent.join(original)
        } else {
            original.to_path_buf()
        }
    } else {
        original.to_path_buf()
    }
}

#[cfg(windows)]
fn symlink_abs_for_junction(resolved_original: &Path) -> PathBuf {
    if resolved_original.is_relative() {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(resolved_original),
            Err(_) => resolved_original.to_path_buf(),
        }
    } else {
        resolved_original.to_path_buf()
    }
}

#[cfg(windows)]
fn symlink_windows_existing_directory(resolved_original: &Path, link: &Path) -> Result<()> {
    let abs_original = symlink_abs_for_junction(resolved_original);
    junction::create(&abs_original, link)
        .wrap_err_with(|| format!("Failed to create junction from {} to {}", link.display(), abs_original.display()))
}

#[cfg(windows)]
fn symlink_windows_existing_file(original: &Path, link: &Path, resolved_original: &Path) -> Result<()> {
    match std::os::windows::fs::symlink_file(original, link) {
        Ok(()) => Ok(()),
        Err(e_symlink) => {
            log::debug!(
                "symlink_file {} -> {} failed: {}; trying hard link",
                link.display(),
                original.display(),
                e_symlink
            );
            match fs::hard_link(resolved_original, link) {
                Ok(()) => Ok(()),
                Err(e_hard) => {
                    log::debug!(
                        "hard_link {} -> {} failed: {}; trying copy",
                        link.display(),
                        resolved_original.display(),
                        e_hard
                    );
                    fs::copy(resolved_original, link)
                        .map(|_| ())
                        .wrap_err_with(|| {
                            format!(
                                "Failed to symlink, hardlink, or copy from {} to {} (symlink: {}; hardlink: {})",
                                resolved_original.display(),
                                link.display(),
                                e_symlink,
                                e_hard
                            )
                        })
                }
            }
        }
    }
}

#[cfg(windows)]
fn symlink_windows_missing_target(original: &Path, link: &Path) -> Result<()> {
    // When target doesn't exist, we can't create a junction (needs existing target).
    // Try to create a directory symlink first (requires Developer Mode or admin),
    // fall back to file symlink if that fails.
    use std::os::windows::fs::{symlink_dir, symlink_file};

    log::debug!(
        "Creating symlink at {} -> {} (target doesn't exist yet)",
        link.display(),
        original.display()
    );

    // Try directory symlink first (for paths like bin, sbin, lib, etc.)
    if symlink_dir(original, link).is_ok() {
        return Ok(());
    }

    // Fall back to file symlink
    symlink_file(original, link)
        .wrap_err_with(|| format!("Failed to create symlink from {} to {}", original.display(), link.display()))
}

#[cfg(windows)]
pub fn symlink<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> Result<()> {
    let original = original.as_ref();
    let link = link.as_ref();
    log::trace!("creating symlink: {} -> {}", link.display(), original.display());

    let resolved_original = symlink_resolve_original(original, link);

    if resolved_original.is_dir() {
        symlink_windows_existing_directory(&resolved_original, link)
    } else if resolved_original.is_file() {
        symlink_windows_existing_file(original, link, &resolved_original)
    } else {
        symlink_windows_missing_target(original, link)
    }
}

/// Create a hard link.
pub fn hard_link<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> Result<()> {
    let original = original.as_ref();
    let link = link.as_ref();
    log::trace!("creating hard link: {} -> {}", link.display(), original.display());
    fs::hard_link(original, link)
        .wrap_err_with(|| format!("Failed to create hard link from {} to {}", link.display(), original.display()))
}

/// Copy a file.
pub fn copy<P: AsRef<Path>, Q: AsRef<Path>>(source: P, target: Q) -> Result<u64> {
    let source = source.as_ref();
    let target = target.as_ref();
    log::trace!("copying file: {} -> {}", source.display(), target.display());
    fs::copy(source, target)
        .wrap_err_with(|| format!("Failed to copy {} to {}", source.display(), target.display()))
}

/// Rename a file or directory.
pub fn rename<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> Result<()> {
    let from = from.as_ref();
    let to = to.as_ref();
    log::trace!("renaming: {} -> {}", from.display(), to.display());
    fs::rename(from, to)
        .wrap_err_with(|| format!("Failed to rename {} to {}", from.display(), to.display()))
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
#[cfg(windows)]
pub fn can_create_symlinks() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};

    static CAN_SYMLINK: AtomicBool = AtomicBool::new(false);
    static CHECKED: AtomicBool = AtomicBool::new(false);

    if CHECKED.load(Ordering::Relaxed) {
        return CAN_SYMLINK.load(Ordering::Relaxed);
    }

    // Test symlink creation in temp directory
    let result = std::env::temp_dir();
    let test_file = result.join("epkg_symlink_test_file");
    let test_link = result.join("epkg_symlink_test_link");

    // Create a test file
    let can_create = (|| {
        let _ = std::fs::File::create(&test_file).ok()?;
        let result = std::os::windows::fs::symlink_file(&test_file, &test_link).is_ok();
        // Clean up
        let _ = std::fs::remove_file(&test_file);
        let _ = std::fs::remove_file(&test_link);
        Some(result)
    })();

    let can_create = can_create.unwrap_or(false);

    CAN_SYMLINK.store(can_create, Ordering::Relaxed);
    CHECKED.store(true, Ordering::Relaxed);

    if can_create {
        log::debug!("Windows symlink creation is available");
    } else {
        log::info!("Windows symlink creation is NOT available (requires Admin or Developer Mode); will use hardlinks/junctions");
    }

    can_create
}

/// On Unix systems, symlinks are always available.
#[cfg(unix)]
pub fn can_create_symlinks() -> bool {
    true
}

/// Create a directory and all its parent directories if they are missing.
pub fn create_dir_all<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
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

/// Check if path is a symlink or Windows junction
/// On Windows, junctions are directory reparse points created by lfs::symlink()
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
                let target_in_env = env_root.join(link_target.strip_prefix("/").unwrap_or(&link_target));
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
            let target_in_env = env_root.join(link_target.strip_prefix("/").unwrap_or(&link_target));
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
    sanitize_filename_for_windows,
    sanitize_path_for_windows,
};
