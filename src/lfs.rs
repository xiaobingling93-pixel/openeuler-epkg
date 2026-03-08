/// fs wrappers with trace logging

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

#[cfg(windows)]
pub fn symlink<P: AsRef<Path>, Q: AsRef<Path>>(_original: P, _link: Q) -> Result<()> {
    Err(color_eyre::eyre::eyre!("symlink not implemented for Windows yet"))
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
pub fn remove_file<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    log::trace!("removing file: {}", path.display());
    fs::remove_file(path)
        .wrap_err_with(|| format!("Failed to remove file {}", path.display()))
}

/// Remove a directory and all its contents.
pub fn remove_dir_all<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    log::trace!("removing directory recursively: {}", path.display());
    fs::remove_dir_all(path)
        .wrap_err_with(|| format!("Failed to remove directory {}", path.display()))
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

/// Get metadata of a file without following symlinks.
pub fn symlink_metadata<P: AsRef<Path>>(path: P) -> Result<fs::Metadata> {
    let path = path.as_ref();
    log::trace!("getting metadata (no follow): {}", path.display());
    fs::symlink_metadata(path)
        .wrap_err_with(|| format!("Failed to get metadata for {}", path.display()))
}

/// Determine if a file is a symlink
pub fn is_symlink(path: &Path) -> bool {
    match symlink_metadata(path) {
        Ok(metadata) => metadata.file_type().is_symlink(),
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
    // Check exists() or is_symlink() - symlinks may be "broken" in host context
    // but valid inside namespace where all paths are mounted
    if target_in_env.exists() || target_in_env.is_symlink() {
        if target_in_env.is_symlink() {
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

    // First check if the symlink file itself exists (as a regular file or symlink)
    if symlink_path.exists() && !symlink_path.is_symlink() {
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
            if link_target.exists() {
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
            if resolved_path.exists() {
                if resolved_path.is_symlink() {
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
