/// fs wrappers with trace logging

use std::fs;
use std::path::Path;
use color_eyre::eyre::{eyre, WrapErr};
use color_eyre::Result;

/// Create a symbolic link.
pub fn symlink<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> Result<()> {
    let original = original.as_ref();
    let link = link.as_ref();
    log::trace!("creating symlink: {} -> {}", link.display(), original.display());
    std::os::unix::fs::symlink(original, link)
        .wrap_err_with(|| format!("Failed to create symlink from {} to {}", link.display(), original.display()))
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
            log::debug!("reflink failed for {} -> {}: {}, falling back to copy",
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

