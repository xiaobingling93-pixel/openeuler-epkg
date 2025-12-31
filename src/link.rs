//! Package linking module
//!
//! This module provides functions for linking package files from the store to the environment,
//! supporting different link types (hardlink, symlink, move, runpath).

use std::path::{Path, PathBuf};
use std::fs;
use std::os::unix::fs::{symlink, MetadataExt};
use color_eyre::Result;
use color_eyre::eyre::{self, eyre, WrapErr};
use crate::models::{LinkType, InstalledPackageInfo};
use crate::utils;
use crate::risks::{is_config_file_path, get_config_file_action, FileAction};
use log;

// link files from env_root to store_fs_dir
pub fn link_package(store_fs_dir: &PathBuf, env_root: &PathBuf, link_type: LinkType, can_reflink: bool) -> Result<()> {
    let fs_files = utils::list_package_files_with_info(store_fs_dir.to_str().ok_or_else(|| eyre::eyre!("Invalid store_fs_dir path: {}", store_fs_dir.display()))?)
        .with_context(|| format!("Failed to list package files in {}", store_fs_dir.display()))?;
    mirror_dir(env_root, store_fs_dir, &fs_files, link_type, can_reflink)
        .with_context(|| format!("Failed to mirror directory from {} to {}", store_fs_dir.display(), env_root.display()))?;

    // For link=move, remove the 'fs' dir after moving all files
    if link_type == LinkType::Move {
        // Remove the fs directory after all files have been moved
        if store_fs_dir.exists() {
            // Try to remove the directory (should be empty or nearly empty after moves)
            if let Err(e) = fs::remove_dir_all(store_fs_dir) {
                log::warn!("Failed to remove fs directory {} after move: {}", store_fs_dir.display(), e);
                // Don't fail the entire operation if we can't remove the dir
            } else {
                log::debug!("Removed fs directory {} after move", store_fs_dir.display());
            }
        }
    }

    Ok(())
}

/// Unlink files that are in old_package but not in new_package
/// This implements the Set(old_pkg - new_pkg) logic
pub fn unlink_package_diff(
    old_package_info: &InstalledPackageInfo,
    new_package_info: &InstalledPackageInfo,
    store_root: &Path,
    env_root: &Path,
) -> Result<()> {
        // Get file lists for both packages
        let old_store_fs_dir = store_root.join(&old_package_info.pkgline).join("fs");
        let new_store_fs_dir = store_root.join(&new_package_info.pkgline).join("fs");

        let old_files = utils::list_package_files(old_store_fs_dir.to_str()
            .ok_or_else(|| eyre::eyre!("Invalid old package fs path"))?)?;
        let new_files = utils::list_package_files(new_store_fs_dir.to_str()
            .ok_or_else(|| eyre::eyre!("Invalid new package fs path"))?)?;

        // Convert to sets of relative paths for comparison
        let old_rel_paths: std::collections::HashSet<PathBuf> = old_files
            .iter()
            .filter_map(|path| path.strip_prefix(&old_store_fs_dir).ok().map(|p| p.to_path_buf()))
            .collect();

        let new_rel_paths: std::collections::HashSet<PathBuf> = new_files
            .iter()
            .filter_map(|path| path.strip_prefix(&new_store_fs_dir).ok().map(|p| p.to_path_buf()))
            .collect();

        // Find files that are in old package but not in new package
        let files_to_remove: Vec<PathBuf> = old_rel_paths
            .difference(&new_rel_paths)
            .cloned()
            .collect();

        log::debug!(
            "Found {} files to remove during upgrade: old_pkg={}, new_pkg={}",
            files_to_remove.len(),
            old_package_info.pkgline,
            new_package_info.pkgline
        );

        // Remove the files from environment
        for rel_path in &files_to_remove {
            let env_file_path = env_root.join(rel_path);

            if env_file_path.exists() {
                if env_file_path.is_dir() {
                    // Only remove directory if it's empty
                    match std::fs::read_dir(&env_file_path) {
                        Ok(mut entries) => {
                            if entries.next().is_none() {
                                log::debug!("Removing empty directory: {}", env_file_path.display());
                                std::fs::remove_dir(&env_file_path)
                                    .with_context(|| format!("Failed to remove directory {}", env_file_path.display()))?;
                            } else {
                                log::debug!("Directory not empty, skipping: {}", env_file_path.display());
                            }
                        }
                        Err(_) => {
                            log::debug!("Cannot read directory, skipping: {}", env_file_path.display());
                        }
                    }
                } else {
                    log::debug!("Removing file: {}", env_file_path.display());
                    std::fs::remove_file(&env_file_path)
                        .with_context(|| format!("Failed to remove file {}", env_file_path.display()))?;
                }
            }
        }

        if !files_to_remove.is_empty() {
            log::info!(
                "Removed {} unique files from old package during upgrade",
                files_to_remove.len()
            );
        }

        Ok(())
}

/// Check if two paths are on the same filesystem by comparing device IDs
fn same_filesystem(path1: &Path, path2: &Path) -> Result<bool> {
    let meta1 = fs::metadata(path1)
        .with_context(|| format!("Failed to get metadata for {}", path1.display()))?;
    let meta2 = fs::metadata(path2)
        .with_context(|| format!("Failed to get metadata for {}", path2.display()))?;
    Ok(meta1.dev() == meta2.dev())
}

/// Check if reflink (copy-on-write) is supported on the filesystem
/// This is done by attempting a test reflink operation using reflink_copy()
#[cfg(target_os = "linux")]
fn check_reflink_support(store_root: &Path, env_root: &Path) -> bool {
    use std::io::Write;

    // Only check if paths are on the same filesystem
    if same_filesystem(store_root, env_root).unwrap_or(false) {
        // Create a temporary test file
        let test_file = env_root.join(".epkg_reflink_test");

        // Create a small test file
        if let Ok(mut file) = fs::File::create(&test_file) {
            if file.write_all(b"test").is_ok() {
                file.sync_all().ok();

                // Try to create a reflink using reflink_copy()
                let test_target = env_root.join(".epkg_reflink_test_target");
                let result = reflink_copy(&test_file, &test_target).is_ok();

                // Clean up test files
                let _ = fs::remove_file(&test_file);
                let _ = fs::remove_file(&test_target);

                return result;
            } else {
                let _ = fs::remove_file(&test_file);
            }
        }
    }
    false
}

#[cfg(not(target_os = "linux"))]
fn check_reflink_support(_store_root: &Path, _env_root: &Path) -> bool {
    false
}

/// Create a reflink (copy-on-write) copy of a file
#[cfg(target_os = "linux")]
fn reflink_copy(source: &Path, target: &Path) -> Result<()> {
    use std::os::unix::io::AsRawFd;

    // Open source file for reading
    let src_file = fs::File::open(source)
        .with_context(|| format!("Failed to open source file {}", source.display()))?;

    // Create target file
    let dst_file = fs::File::create(target)
        .with_context(|| format!("Failed to create target file {}", target.display()))?;

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
fn reflink_copy(_source: &Path, _target: &Path) -> Result<()> {
    Err(eyre!("Reflink not supported on this platform"))
}

/// Compute link type and reflink support for installation plan
/// Returns (link_type, can_reflink)
pub fn compute_link_type_and_reflink(
    env_link: LinkType,
    store_root: &Path,
    env_root: &Path,
) -> Result<(LinkType, bool)> {
    let mut link_type = env_link;
    let mut can_reflink = false;

    if link_type == LinkType::Hardlink {
        match same_filesystem(store_root, env_root) {
            Ok(true) => {
                // Same filesystem, keep hardlink and check for reflink support
                can_reflink = check_reflink_support(store_root, env_root);
                if can_reflink {
                    log::debug!("Reflink support detected on filesystem");
                }
            }
            Ok(false) => {
                // Different filesystems, downgrade to symlink
                log::debug!("Store root and env root are on different filesystems, downgrading hardlink to symlink");
                link_type = LinkType::Symlink;
            }
            Err(e) => {
                log::warn!("Failed to check filesystem compatibility: {}, downgrading hardlink to symlink", e);
                link_type = LinkType::Symlink;
            }
        }
    } else if link_type == LinkType::Move || link_type == LinkType::Runpath {
        match same_filesystem(store_root, env_root) {
            Ok(true) => {
                // Same filesystem, rename() will work
            }
            Ok(false) => {
                // Different filesystems, rename() will fail
                return Err(eyre::eyre!(
                    "Link type {:?} requires store and environment to be on the same filesystem, but they are on different filesystems (store: {}, env: {})",
                    link_type,
                    store_root.display(),
                    env_root.display()
                ));
            }
            Err(e) => {
                return Err(eyre::eyre!(
                    "Failed to check filesystem compatibility for {:?} link type: {}",
                    link_type,
                    e
                ));
            }
        }
    }

    Ok((link_type, can_reflink))
}

// symlink1 = target_path.replace("ebin", "bin")
pub fn replace_existing_symlink1(target_path: &Path, fs_file: &Path) -> Result<bool> {
    let target_path_str = target_path.to_string_lossy();
    let symlink1_path = PathBuf::from(target_path_str.replace("/ebin/", "/bin/"));

    if !symlink1_path.exists() {
        return Ok(false);
    }

    // Check if symlink1 points to fs_file
    match fs::read_link(&symlink1_path) {
        Ok(current_target) => {
            if current_target == fs_file {
                // symlink1 already points to the correct target or has been updated
                return Ok(true);
            }

            log::debug!("symlink1 {} exists but points to {:?}, updating to point to {:?}",
                       symlink1_path.display(), current_target, fs_file);
            // Remove existing symlink and create new one
            fs::remove_file(&symlink1_path)
                .with_context(|| format!("Failed to remove existing symlink {}", symlink1_path.display()))?;

            // Create parent directory if it doesn't exist
            if let Some(parent) = symlink1_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create parent directory {}", parent.display()))?;
            }

            symlink(fs_file, &symlink1_path)
                .with_context(|| format!(
                    "Failed to create symlink from {} to {}",
                    symlink1_path.display(),
                    fs_file.display()
                ))?;
            Ok(true)
        }
        Err(_) => {
            // symlink1 exists but is not a symlink (regular file/directory)
            // Don't modify it, indicate that symlink2 is needed
            Ok(false)
        }
    }
}

// Create symlink2: "{dirname(target_path)}/.{filename(target_path)}" -> fs_file
pub fn create_symlink2(target_path: &Path, fs_file: &Path) -> Result<()> {
    let target_filename = target_path.file_name()
        .ok_or_else(|| eyre::eyre!("Failed to get filename from {}", target_path.display()))?
        .to_string_lossy();
    let target_dirname = target_path.parent()
        .ok_or_else(|| eyre::eyre!("Failed to get parent directory from {}", target_path.display()))?;

    let symlink2_path = target_dirname.join(format!(".{}", target_filename));

    // Remove existing symlink2 if it exists
    if symlink2_path.exists() {
        fs::remove_file(&symlink2_path)
            .with_context(|| format!("Failed to remove existing symlink {}", symlink2_path.display()))?;
    }

    // Create symlink2 -> fs_file
    symlink(fs_file, &symlink2_path)
        .with_context(|| format!(
            "Failed to create symlink from {} to {}",
            symlink2_path.display(),
            fs_file.display()
        ))?;

    log::debug!("Created symlink2: {} -> {}", symlink2_path.display(), fs_file.display());
    Ok(())
}

fn mirror_dir(env_root: &Path, store_fs_dir: &Path, fs_files: &[utils::MtreeFileInfo], link_type: LinkType, can_reflink: bool) -> Result<()> {
    for fs_file_info in fs_files {
        let fs_file = &fs_file_info.path;
        let fhs_file = fs_file.strip_prefix(store_fs_dir)
            .with_context(|| format!("Failed to strip prefix {} from {}", store_fs_dir.display(), fs_file.display()))?;
        let target_path = env_root.join(fhs_file);

        if fs_file_info.is_dir() {
            // Check if target path exists and is not a directory
            if target_path.exists() && !target_path.is_dir() {
                // Remove the non-directory file first
                fs::remove_file(&target_path)
                    .with_context(|| format!("Failed to remove non-directory file {} for mirror_dir", target_path.display()))?;
            }
            fs::create_dir_all(&target_path)
                .with_context(|| format!("Failed to create directory {}", target_path.display()))?;
            continue;
        }

        // Create parent directory if it doesn't exist
        // No longer necessary, since filelist.txt always show dir before files under it
        // if let Some(parent) = target_path.parent() {
        //     fs::create_dir_all(parent)
        //         .with_context(|| format!("Failed to create parent directory {}", parent.display()))?;
        // }

        if matches!(fhs_file.to_string_lossy().as_ref(), "sbin" | "bin" | "lib" | "lib64" | "lib32" | "usr/sbin" | "usr/lib64") {
            // No modify top-level directories/symlinks created by create_environment_directories()
        } else if fs_file_info.is_link() {
            mirror_symlink_file(fs_file, &target_path)
                .with_context(|| format!("Failed to handle symlink file {}", fs_file.display()))?;
        } else {
            mirror_regular_file(fs_file, &target_path, fhs_file, link_type, can_reflink)
                .with_context(|| format!("Failed to handle regular file {}", fs_file.display()))?;
        }
    }
    Ok(())
}

/// Try to create a hardlink from source to target.
/// If hardlink fails (cross-device or other error), fall back to copying the file.
/// This is useful when files need to be actual files (not symlinks) for tools to work properly.
///
/// Parameters:
/// - source: Path to the source file
/// - target: Path to the target file
/// - preserve_permissions: If true, preserve file permissions when copying (ignored for hardlinks)
pub fn hard_link_or_copy(source: &Path, target: &Path, preserve_permissions: bool) -> Result<()> {
    match fs::hard_link(source, target) {
        Ok(()) => {
            log::trace!("Created hardlink from {} to {}", source.display(), target.display());
            Ok(())
        }
        Err(hardlink_err) => {
            // Check if it's a cross-device error (EXDEV = 18)
            if hardlink_err.raw_os_error() == Some(18) {
                log::debug!("Cross-device hardlink failed for {} -> {}, falling back to copy",
                           source.display(), target.display());
            } else {
                // Other error, try copy as fallback
                log::debug!("Hardlink failed for {} -> {}: {}, falling back to copy",
                           source.display(), target.display(), hardlink_err);
            }
            fs::copy(source, target)
                .map(|_| ())
                .with_context(|| format!("Failed to copy {} to {} (hardlink also failed: {})",
                                        source.display(), target.display(), hardlink_err))?;

            // Preserve permissions if requested
            if preserve_permissions {
                if let Ok(metadata) = fs::metadata(source) {
                    fs::set_permissions(target, metadata.permissions())
                        .with_context(|| format!("Failed to set permissions for {}", target.display()))?;
                }
            }

            Ok(())
        }
    }
}

/// Handle symlink files in mirror_dir function
///
/// This function processes symlinks that may point to either files or directories.
/// For top-level directory symlinks (sbin, bin, lib, lib64, lib32), it skips them
/// as they are handled by the environment setup process.
/// For other symlinks pointing to files, it creates a shortcut symlink.
///
/// Note: Files in gconv-modules.d/ are handled as regular files (not symlinks) in
/// mirror_regular_file(), so symlinks in that directory won't appear in the file list.
///
/// Examples:
/// - sbin -> usr/sbin (top-level dir symlink): skipped (handled by env setup)
/// - bin -> usr/bin (top-level dir symlink): skipped (handled by env setup)
/// - python3 -> python3.11 (file symlink): creates shortcut symlink
/// - /usr/bin/python3 -> /usr/bin/python3.11 (absolute file symlink): creates shortcut symlink
///
/// Parameters:
/// - fs_file: Path to the symlink in the store
/// - target_path: Where to create the symlink in the environment
/// - _fhs_file: Relative path from store_fs_dir (unused, kept for consistency)
fn mirror_symlink_file(fs_file: &Path, target_path: &Path) -> Result<()> {
    utils::remove_any_existing_file(target_path, true)?;

    // Handle regular symlink (not pointing to directory)
    shortcut_symlink(fs_file, target_path)
        .with_context(|| format!("Failed to shortcut_symlink from {} to {}", fs_file.display(), target_path.display()))?;
    Ok(())
}

/// Handle regular files in mirror_dir function
///
/// This function processes regular files (not symlinks or directories).
/// Behavior depends on link_type:
/// - hardlink: prefer hardlink, fall back to symlink
/// - symlink: prefer symlink, use hard_link_or_copy for special files
/// - move: prefer mv (moves file from store to env)
/// - runpath: not supported for now
///
/// Examples:
/// - /etc/resolv.conf: copied to environment (preserves content)
/// - /usr/lib/gconv/gconv-modules.d/gconv-modules-extra.conf: hardlinked or copied
/// - /usr/bin/python3.11: symlinked to store location (or hardlinked/moved based on link_type)
/// - /usr/lib/libpython3.11.so: symlinked to store location (or hardlinked/moved based on link_type)
///
/// Parameters:
/// - fs_file: Path to the file in the store
/// - target_path: Where to create the file/symlink in the environment
/// - fhs_file: Relative path from store_fs_dir (used to determine if file is in /etc/)
/// - link_type: Link type to use (hardlink, symlink, move, runpath)
/// - can_reflink: Whether reflink (copy-on-write) is supported
fn mirror_regular_file(fs_file: &Path, target_path: &Path, fhs_file: &Path, link_type: LinkType, can_reflink: bool) -> Result<()> {
    // Check if this is a config file (in /etc/)
    let is_config_file = is_config_file_path(fhs_file);

    // For config files, use transaction module to decide file action
    // For now, we'll infer NOREPLACE from common patterns (can be enhanced later with RPM metadata)
    let is_noreplace = is_config_file && target_path.exists() &&
        (target_path.file_name().and_then(|n| n.to_str())
            .map(|s| s.contains("config") || s.contains("conf") || s.contains(".cfg"))
            .unwrap_or(false));

    if is_config_file && target_path.exists() {
        // Use transaction module to decide config file fate
        let action = get_config_file_action(target_path, fs_file, is_noreplace);

        match action {
            FileAction::Skip => {
                log::debug!("Skipping config file {} (identical to existing)", target_path.display());
                return Ok(());
            }
            FileAction::AltName => {
                // Create .rpmnew file for NOREPLACE configs
                let rpmnew_path = target_path.with_extension(format!(
                    "{}.rpmnew",
                    target_path.extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                ));
                log::info!("Creating .rpmnew file for config {}: {}", target_path.display(), rpmnew_path.display());
                if can_reflink {
                    if reflink_copy(fs_file, &rpmnew_path).is_err() {
                        fs::copy(fs_file, &rpmnew_path)
                            .with_context(|| format!("Failed to copy config to .rpmnew: {}", rpmnew_path.display()))?;
                    }
                } else {
                    fs::copy(fs_file, &rpmnew_path)
                        .with_context(|| format!("Failed to copy config to .rpmnew: {}", rpmnew_path.display()))?;
                }
                return Ok(());
            }
            FileAction::Backup => {
                // Backup existing config file
                let backup_path = target_path.with_extension(format!(
                    "{}.rpmsave",
                    target_path.extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                ));
                log::info!("Backing up config {} to {}", target_path.display(), backup_path.display());
                fs::copy(target_path, &backup_path)
                    .with_context(|| format!("Failed to backup config: {}", backup_path.display()))?;
                // Continue to create new file below
            }
            _ => {
                // Create or overwrite (default behavior)
            }
        }
    }

    // Remove any existing file/dirs (if not already handled by config logic)
    if fs::symlink_metadata(target_path).is_ok() && !is_config_file {
        // On upgrade, it's normal to overwrite old files from previous version
        log::trace!("File already exists, overwriting {} with {}", target_path.display(), fs_file.display());
        // Check if target path is a directory and handle accordingly
        if target_path.is_dir() {
            fs::remove_dir_all(target_path)
                .with_context(|| format!("Failed to remove directory {} for mirror_dir", target_path.display()))?;
        } else {
            fs::remove_file(target_path)
                .with_context(|| format!("Failed to remove file {} for mirror_dir", target_path.display()))?;
        }
    } else if is_config_file && target_path.exists() {
        // For config files, remove old file before creating new one (unless we're creating .rpmnew)
        if !matches!(get_config_file_action(target_path, fs_file, is_noreplace),
                     FileAction::AltName) {
            fs::remove_file(target_path)
                .with_context(|| format!("Failed to remove old config file {} for mirror_dir", target_path.display()))?;
        }
    }

    // /etc/ files: use reflink if supported and link_type is hardlink, otherwise copy
    if is_config_file {
        if can_reflink {
            // Try to use reflink (copy-on-write) for /etc/ files
            if reflink_copy(fs_file, target_path).is_ok() {
                log::trace!("Created reflink from {} to {}", fs_file.display(), target_path.display());
                return Ok(());
            }
            // Fall back to regular copy if reflink fails
            log::debug!("Reflink failed for {} -> {}, falling back to copy", fs_file.display(), target_path.display());
        }
        fs::copy(fs_file, target_path)
            .with_context(|| format!("Failed to copy {} to {}", fs_file.display(), target_path.display()))?;
        return Ok(());
    }

    // Apply link_type for regular files
    match link_type {
        LinkType::Hardlink => {
            // Try hardlink first, fall back to symlink on failure
            match fs::hard_link(fs_file, target_path) {
                Ok(()) => {
                    log::trace!("Created hardlink from {} to {}", fs_file.display(), target_path.display());
                }
                Err(e) => {
                    // Fall back to symlink if hardlink fails
                    log::debug!("Hardlink failed for {} -> {}: {}, falling back to symlink",
                               fs_file.display(), target_path.display(), e);
                    symlink_or_copy(fs_file, target_path, fhs_file)?;
                }
            }
        }
        LinkType::Symlink => {
            // Current behavior: prefer symlink
            symlink_or_copy(fs_file, target_path, fhs_file)?;
        }
        LinkType::Move => {
            // Move file from store to env (will be removed from store later)
            fs::rename(fs_file, target_path)
                .with_context(|| format!("Failed to move {} to {}", fs_file.display(), target_path.display()))?;
        }
        LinkType::Runpath => {
            // Move file from store to env (will be removed from store later)
            // Same as Move - requires same filesystem (checked in compute_link_type_and_reflink)
            fs::rename(fs_file, target_path)
                .with_context(|| format!("Failed to move {} to {}", fs_file.display(), target_path.display()))?;
        }
    }
    Ok(())
}

fn symlink_or_copy(fs_file: &Path, target_path: &Path, fhs_file: &Path) -> Result<()> {
    if let Some(preserve_permissions) = needs_hard_link_or_copy(fhs_file) {
        // Certain paths (script-language trees, gconv modules, rustc driver) must be
        // materialized as real files in env_root (via hardlink or copy) so that
        // their consumers behave correctly when resolving modules or sysroots.
        hard_link_or_copy(fs_file, target_path, preserve_permissions)?;
        return Ok(());
    }

    symlink(fs_file, target_path)
        .with_context(|| format!("Failed to create symlink from {} to {}", fs_file.display(), target_path.display()))?;

    Ok(())
}

/// Decide whether a regular file should be materialized via hardlink-or-copy
/// instead of left as a symlink into the store, and if so whether permissions
/// should be preserved.
///
/// Returns:
/// - Some(false) -> use hard_link_or_copy(..., preserve_permissions = false)
/// - Some(true)  -> use hard_link_or_copy(..., preserve_permissions = true)
/// - None        -> caller should fall back to symlink()
fn needs_hard_link_or_copy(fhs_file: &Path) -> Option<bool> {
    let rel = fhs_file.to_string_lossy();

    // Many script runtimes (Node.js, Python, Ruby, Perl, Lua, PHP, Tcl, Guile, OCaml,
    // R, Haskell, Julia, Erlang, etc.) derive their
    // module search paths from the real path of the interpreter or library tree. If
    // these trees are left as symlinks into the store (whose paths may contain colons
    // or other special characters), the runtime can end up resolving modules relative
    // to the store path instead of env_root, and miss dependencies that are properly
    // mirrored only under env_root.
    //
    // For known "well-known" library roots, we therefore materialize real files
    // (via hardlink or copy) in env_root so that module resolution stays within
    // env_root.
    // Node.js global modules (various distro layouts)
    if rel.starts_with("usr/lib/node_modules/")
        || rel.starts_with("usr/share/nodejs/")
    // Python site/dist-packages under common prefixes
        || rel.contains("/site-packages/")
        || rel.contains("/dist-packages/")
    // Ruby standard libs and gems
        || rel.starts_with("usr/lib/ruby/")
        || rel.contains("/gems/")
    // Perl libs
        || rel.starts_with("usr/share/perl5/")
        || rel.starts_with("usr/lib/perl5/")
        || rel.contains("/site_perl/")
    // Lua modules
        || rel.starts_with("usr/share/lua/")
        || rel.starts_with("usr/lib/lua/")
    // PHP common library tree in distros
        || rel.starts_with("usr/share/php/")
    // Tcl / Tk modules
        || rel.starts_with("usr/lib/tcl")
        || rel.starts_with("usr/share/tcltk/")
        || rel.starts_with("usr/lib/tk")
    // Guile scheme modules
        || rel.starts_with("usr/share/guile/")
        || rel.starts_with("usr/lib/guile/")
    // OCaml libraries (findlib / compiler libs)
        || rel.starts_with("usr/lib/ocaml/")
    // R libraries
        || rel.starts_with("usr/lib/R/library/")
    // Haskell / GHC libraries
        || rel.starts_with("usr/lib/ghc/")
        || rel.starts_with("usr/lib/haskell-packages/")
    // Julia modules
        || rel.starts_with("usr/share/julia/")
        || rel.starts_with("usr/lib/julia/")
    // Erlang / Elixir beam libraries
        || rel.starts_with("usr/lib/erlang/lib/")
    {
        return Some(true);
    }

    // rustc determines its sysroot based on the location of librustc_driver*.so.
    // If this .so is a symlink into the store and that path contains a colon
    // (e.g. "__rust__1:1.92.0-1__x86_64"), then Cargo will see a sysroot
    // under that store path and construct LD_LIBRARY_PATH entries that
    // include a colon *inside* a path component, which std::env::join_paths
    // rejects with "path segment contains separator `:`".
    //
    // To avoid this, make librustc_driver*.so an actual file in the env_root
    // (via hardlink or copy) instead of a symlink into the store, so that
    // `rustc --print=sysroot` returns the env_root-based sysroot.
    if rel.starts_with("usr/lib/librustc_driver") {
        return Some(true);
    }

    // iconvconfig handles ONLY normal files, NOT symlinks
    if rel.contains("/gconv-modules.d/") {
        return Some(false);
    }

    None
}

// Like symlink() but try to remove one level of indirection
fn shortcut_symlink(fs_file: &Path, target_path: &Path) -> Result<()> {
    if let Ok(link_target) = fs::read_link(fs_file) {
        let new_link_target = if link_target.is_absolute() || !link_target.exists() {
            // This prevents
            //      /usr/bin/python3 -> /home/wfg/.epkg/store/lsl4sc64f2ccp62cxfquizdaj5k4fpcu__python3-minimal__3.13.3-1__amd64/fs/usr/bin/python3.13
            // in case
            //      /home/wfg/.epkg/store/lsl4sc64f2ccp62cxfquizdaj5k4fpcu__python3-minimal__3.13.3-1__amd64/fs/usr/bin/python3 -> python3.13
            //
            // Prevents
            //      /home/wfg/.epkg/envs/main/bin/sh -> /home/wfg/.epkg/store/g53cxe55pxbwqgq2k2nk7owjnv7zmlsj__busybox-binsh__1.37.0-r18__noarch/fs//bin/busybox
            // in case /bin/busybox happen to exist in host os but not in env:
            //      /home/wfg/.epkg/store/g53cxe55pxbwqgq2k2nk7owjnv7zmlsj__busybox-binsh__1.37.0-r18__noarch/fs//bin/sh -> /bin/busybox
            link_target
        } else if link_target.starts_with("../") {
            // For parent-relative paths like ../bin/pidof, normalize against fs_file
            normalize_join(fs_file.parent().ok_or_else(|| eyre::eyre!("Failed to get parent directory for {}", fs_file.display()))?,
                           &link_target)
        } else {
            // For sibling-relative paths like python3.11, join with source file's parent
            fs_file.parent()
                .ok_or_else(|| eyre::eyre!("Failed to get parent directory for {}", fs_file.display()))?
                .join(link_target)
        };

        symlink(&new_link_target, target_path)
            .with_context(|| format!("Failed to create symlink from {} to {}", fs_file.display(), target_path.display()))?;
    }
    Ok(())
}

fn normalize_join(base: &Path, subpath: &Path) -> PathBuf {
    let mut components: Vec<_> = base.components().collect();

    for component in subpath.components() {
        match component {
            std::path::Component::ParentDir if !components.is_empty() => {
                components.pop();
            },
            std::path::Component::CurDir => {},
            _ => components.push(component),
        }
    }

    components.iter().collect()
}
