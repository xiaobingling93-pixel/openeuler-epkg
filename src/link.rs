//! Package linking module
//!
//! This module provides functions for linking package files from the store to the environment,
//! supporting different link types (hardlink, symlink, move, runpath).

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use crate::models::{LinkType, PackageFormat, InstalledPackageInfo};
use crate::plan::InstallationPlan;
use crate::utils;
use crate::lfs;
use log;

/// File action types (simplified version for config file handling)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EtcFileAction {
    Create,
    Identical,
    Backup,
    AltName, // For NOREPLACE configs - creates .rpmnew file
}

/// Determine if a file path is a config file (in /etc/)
fn is_config_file_path(file_path: &Path) -> bool {
    file_path.to_string_lossy().starts_with("etc/")
}

/// Get config file action based on existing file and flags
fn get_config_file_action(
    target_path: &Path,
    fs_file: &Path,
) -> EtcFileAction {
    // Check if existing file exists
    if !lfs::exists_in_env(target_path) {
        return EtcFileAction::Create;
    }

    // Check if files are identical (simplified - just check if they exist and are same size)
    // Use symlink_metadata to avoid following symlinks in env context
    if let (Ok(meta1), Ok(meta2)) = (lfs::symlink_metadata(target_path), fs::metadata(fs_file)) {
        #[cfg(unix)]
        {
            if meta1.len() == meta2.len() && meta1.mode() == meta2.mode() {
                return EtcFileAction::Identical;
            }
        }
        #[cfg(not(unix))]
        {
            if meta1.len() == meta2.len() {
                return EtcFileAction::Identical;
            }
        }
    }

    // For config files, use transaction module to decide file action
    // For now, we'll infer NOREPLACE from common patterns (can be enhanced later with RPM metadata)
    let is_noreplace = target_path.file_name().and_then(|n| n.to_str())
            .map(|s| s.contains("config") || s.contains("conf") || s.contains(".cfg"))
            .unwrap_or(false);

    // Files differ - handle based on flags
    if is_noreplace {
        EtcFileAction::AltName // Create .rpmnew file
    } else {
        EtcFileAction::Backup // Backup existing, then create new
    }
}

/// Process config file actions (skip, create .rpmnew, backup, or continue)
fn process_config_file(
    fs_file: &Path,
    target_path: &Path,
    can_reflink: bool,
) -> Result<EtcFileAction> {
    // Use transaction module to decide config file fate
    let action = get_config_file_action(target_path, fs_file);

    match action {
        EtcFileAction::Identical => {
            log::debug!("Skipping config file {} (identical to existing)", target_path.display());
            Ok(EtcFileAction::Identical)
        }
        EtcFileAction::AltName => {
            // Create .rpmnew file for NOREPLACE configs
            let rpmnew_path = target_path.with_extension(format!(
                "{}.rpmnew",
                target_path.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
            ));
            log::info!("Creating .rpmnew file for config {}: {}", target_path.display(), rpmnew_path.display());
            lfs::reflink_or_copy(fs_file, &rpmnew_path, can_reflink)?;
            Ok(EtcFileAction::AltName)
        }
        EtcFileAction::Backup => {
            // Backup existing config file
            let backup_path = target_path.with_extension(format!(
                "{}.rpmsave",
                target_path.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
            ));
            log::info!("Backing up config {} to {}", target_path.display(), backup_path.display());
            lfs::copy(target_path, &backup_path)?;
            Ok(EtcFileAction::Backup)
        }
        _ => {
            // Create or overwrite (default behavior)
            Ok(EtcFileAction::Create)
        }
    }
}

/// Handle config files (/etc/ files) with special processing
fn mirror_config_file(
    fs_file: &Path,
    target_path: &Path,
    can_reflink: bool,
) -> Result<()> {
    // Process config file actions
    let config_result = process_config_file(fs_file, target_path, can_reflink)?;
    match config_result {
        EtcFileAction::Identical => return Ok(()),
        EtcFileAction::AltName => return Ok(()),
        _ => (), // Continue with linking
    }

    // Remove existing target file if present (already backed up if needed)
    if lfs::exists_in_env(target_path) {
        lfs::remove_file(target_path)?;
    }

    // Copy config file using reflink if supported, otherwise regular copy
    lfs::reflink_or_copy(fs_file, target_path, can_reflink)?;
    Ok(())
}

// link files from env_root to store_fs_dir
pub fn link_package(plan: &InstallationPlan, store_fs_dir: &PathBuf) -> Result<()> {
    log::debug!("link_package: link={:?} package_format={:?} env_root={} store_fs_dir={}", plan.link, plan.package_format, plan.env_root.display(), store_fs_dir.display());
    // Check if this is a conda package and use conda-specific linking
    if plan.package_format == PackageFormat::Conda {
        return crate::conda_link::link_conda_package(plan, store_fs_dir);
    }
    link_package_generic(plan, store_fs_dir)?;

    // For brew packages, rewrite dylib paths to use absolute paths pointing to this env
    #[cfg(target_os = "macos")]
    if plan.package_format == PackageFormat::Brew && matches!(plan.link, LinkType::Move | LinkType::Hardlink) {
        log::info!("Rewriting brew dylib paths for env: {}", plan.env_root.display());
        if let Err(e) = crate::brew_pkg::rewrite_dylib_paths_for_env(&plan.env_root) {
            log::warn!("Failed to rewrite brew dylib paths: {}", e);
        }
    }

    Ok(())
}

/// Link package files using generic (non-format-specific) linking
/// This handles standard file linking without format-specific metadata processing
pub fn link_package_generic(plan: &InstallationPlan, store_fs_dir: &PathBuf) -> Result<()> {
    // For LinkType::Move, create consumed marker before moving files
    // This ensures the store is marked as consumed even if the move fails partway
    if plan.link == LinkType::Move {
        if let Some(store_path) = store_fs_dir.parent() {
            // store_fs_dir is like /path/to/store/pkgline/fs
            // store_path is like /path/to/store/pkgline
            crate::store::create_consumed_marker(store_path, &plan.env_root.display().to_string(), &plan.env_root)
                .with_context(|| format!("Failed to create consumed marker for {}", store_path.display()))?;
        }
    }

    // Standard linking for non-conda packages
    let fs_files = utils::list_package_files_with_info(store_fs_dir.to_str().ok_or_else(|| eyre::eyre!("Invalid store_fs_dir path: {}", store_fs_dir.display()))?)
        .with_context(|| format!("Failed to list package files in {}", store_fs_dir.display()))?;
    mirror_dir(&plan.env_root, store_fs_dir, &fs_files, plan.link, plan.can_reflink)
        .with_context(|| format!("Failed to mirror directory from {} to {}", store_fs_dir.display(), plan.env_root.display()))?;

    // Note: We don't remove fs/ directory after Move because:
    // 1. expose_package() needs to read filelist.txt from info/ and check fs/ structure
    // 2. The consumed.json marker already indicates the store is consumed
    // 3. Empty directories in fs/ don't take significant space

    Ok(())
}

/// Unlink files that are in old_package but not in new_package
/// This implements the Set(old_pkg - new_pkg) logic
/// If old_pkgkey or old_package_info is None, this is a no-op
pub fn unlink_package_diff(
    old_pkgkey: Option<&str>,
    old_package_info: Option<&std::sync::Arc<InstalledPackageInfo>>,
    new_package_info: &InstalledPackageInfo,
    store_root: &Path,
    env_root: &Path,
    new_files_union: &std::collections::HashSet<std::path::PathBuf>,
) -> Result<()> {
    let (_old_key, old_info) = match (old_pkgkey, old_package_info) {
        (Some(key), Some(info)) => (key, info),
        _ => return Ok(()),
    };
    // Get file lists for both packages
    let old_files = crate::package_cache::map_pkgline2filelist(store_root, &old_info.pkgline)?;

    // Convert to sets of relative paths for comparison (already relative paths as strings)
    let old_rel_paths: std::collections::HashSet<PathBuf> = old_files
        .iter()
        .map(|s| PathBuf::from(s))
        .collect();


    // Find files that are in old package but not in any new package in batch
    log::debug!("Batch union contains {} files", new_files_union.len());
    let files_to_remove: Vec<PathBuf> = old_rel_paths
        .difference(new_files_union)
        .cloned()
        .collect();

    log::debug!(
        "Found {} files to remove during upgrade (batch union): old_pkg={}, new_pkg={}",
        files_to_remove.len(),
        old_info.pkgline,
        new_package_info.pkgline
    );

    // Remove the files from environment
    for rel_path in &files_to_remove {
        let env_file_path = env_root.join(lfs::host_path_from_manifest_rel_path(
            rel_path.to_string_lossy().as_ref(),
        ));

        if lfs::exists_in_env(&env_file_path) {
            if lfs::symlink_metadata(&env_file_path).map(|m| m.file_type().is_dir()).unwrap_or(false) {
                // Only remove directory if it's empty
                match std::fs::read_dir(&env_file_path) {
                    Ok(mut entries) => {
                        if entries.next().is_none() {
                            lfs::remove_dir(&env_file_path)?;
                        } else {
                            log::debug!("Directory not empty, skipping: {}", env_file_path.display());
                        }
                    }
                    Err(_) => {
                        log::debug!("Cannot read directory, skipping: {}", env_file_path.display());
                    }
                }
            } else {
                lfs::remove_file(&env_file_path)?;
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

/// Check if two paths are on the same filesystem by comparing filesystem IDs
/// Returns true if both filesystems have valid fsid (!= 0) and same fsid, false otherwise
pub fn same_filesystem(
    fs1: &crate::plan::FilesystemInfo,
    fs2: &crate::plan::FilesystemInfo,
) -> bool {
    // Check if either filesystem info is invalid (fsid == 0)
    // On Windows, fsid is always 0 (statvfs not available), so this is expected
    if fs1.fsid == 0 && fs2.fsid == 0 {
        log::warn!("Cannot determine filesystem ID for '{}' and '{}', assuming different filesystems",
                fs1.path.display(), fs2.path.display());
        return false;
    }
    if fs1.fsid == 0 || fs2.fsid == 0 {
        log::warn!("Filesystem info incomplete: '{}' (fsid={}) vs '{}' (fsid={}), assuming different filesystems",
                fs1.path.display(), fs1.fsid,
                fs2.path.display(), fs2.fsid);
        return false;
    }

    // Both have valid fsid, compare them
    fs1.fsid == fs2.fsid
}

/// Compute link type and reflink support for installation plan
/// Sets plan.link, plan.can_reflink, plan.can_hardlink, and plan.can_symlink directly
/// Uses stored filesystem info from plan to avoid duplicate statvfs calls
pub fn compute_link_type_and_reflink(
    plan: &mut InstallationPlan,
) -> Result<()> {
    use crate::models::env_config;
    let mut link_type = env_config().link;
    let mut can_reflink = false;

    // Brew packages require Move link type because dylib paths are rewritten
    // to use absolute paths pointing to the specific environment.
    // This makes store sharing impossible, so each env gets its own copy.
    if plan.package_format == PackageFormat::Brew {
        link_type = LinkType::Move;
        log::debug!("Forcing LinkType::Move for Brew packages");
    }

    // Use stored filesystem info from plan
    let same_fs = same_filesystem(
        &plan.store_root_fs,
        &plan.env_root_fs,
    );

    // can_hardlink: hardlinks only work on the same filesystem
    let can_hardlink = same_fs;

    // Check symlink capability on Windows
    // Windows users need Admin or Developer Mode to create symlinks
    let can_create_symlinks = lfs::can_create_symlinks();

    // can_symlink: symlinks work across filesystems, but not if link type is Move
    // On Windows, also check if user has symlink creation permission
    let can_symlink = link_type != LinkType::Move && can_create_symlinks;

    // Check reflink support only once when needed
    let should_check_reflink = plan.package_format == PackageFormat::Conda
                                || link_type == LinkType::Hardlink
                                || link_type == LinkType::Reflink;
    if should_check_reflink {
        can_reflink = lfs::check_reflink_support(&plan.env_root, same_fs);
        if can_reflink {
            log::debug!("Reflink support detected on filesystem");
        }
    }

    if link_type == LinkType::Hardlink {
        if same_fs {
            // Same filesystem, keep hardlink (reflink support already checked)
            // can_reflink already set above
        } else {
            // Different filesystems, downgrade to symlink
            log::debug!("Store root and env root are on different filesystems, downgrading hardlink to symlink");
            link_type = LinkType::Symlink;
        }
    } else if link_type == LinkType::Reflink {
        // Reflink can attempt reflink on same filesystem, fall back to copy otherwise
        // can_reflink already set above if same_fs, otherwise remains false
        // If not same_fs, can_reflink remains false, link_type stays Reflink (will fall back to copy)
    } else if link_type == LinkType::Move || link_type == LinkType::Runpath {
        if !same_fs {
            // Different filesystems, rename() will fail
            return Err(eyre::eyre!(
                "Link type {:?} requires store and environment to be on the same filesystem, but they are on different filesystems (store: {}, env: {})",
                link_type,
                plan.store_root.display(),
                plan.env_root.display()
            ));
        }
        // Same filesystem, rename() will work
    } else if link_type == LinkType::Symlink && !can_create_symlinks {
        // On Windows without symlink permission, downgrade to hardlink if same filesystem
        if same_fs {
            log::info!("Windows symlink creation not available; using hardlinks instead");
            link_type = LinkType::Hardlink;
        }
        // If different filesystems, keep symlink type - lfs::symlink / symlink_to_file
        // handle the fallback (junction for dirs, hardlink/copy for files)
    }

    plan.link = link_type;
    plan.can_reflink = can_reflink;
    plan.can_hardlink = can_hardlink;
    plan.can_symlink = can_symlink;
    Ok(())
}

// Check if bin/<program> file exists (any file type, any target).
// We only care that a file exists at that path, regardless of whether it's a symlink
// or what it points to. When running from host, this file is inside the env mount;
// we don't need to validate that its target exists on the host filesystem.
pub fn bin_file_exists(target_path: &Path, _fs_file: &Path) -> Result<bool> {
    let target_path_str = target_path.to_string_lossy();
    let bin_path = PathBuf::from(target_path_str.replace("/ebin/", "/bin/"));
    // Use symlink_metadata to check existence without following symlinks
    Ok(lfs::symlink_metadata(&bin_path).is_ok())
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
    if lfs::symlink_metadata(&symlink2_path).is_ok() {
        lfs::remove_file(&symlink2_path)?;
    }

    // Create symlink2 -> fs_file
    lfs::symlink(fs_file, &symlink2_path)?;

    log::debug!("Created symlink2: {} -> {}", symlink2_path.display(), fs_file.display());
    Ok(())
}

fn mirror_dir(env_root: &Path, store_fs_dir: &Path, fs_files: &[crate::mtree::MtreeFileInfo], link_type: LinkType, can_reflink: bool) -> Result<()> {
    for fs_file_info in fs_files {
        let rel_host = lfs::host_path_from_manifest_rel_path(fs_file_info.path.trim_start_matches('/'));
        let fs_file = store_fs_dir.join(&rel_host);
        let fhs_file = &fs_file_info.path;
        let target_path = env_root.join(&rel_host);
        log::trace!("mirror_dir: processing fhs_file={}, is_link={}, is_dir={}", fhs_file, fs_file_info.is_link(), fs_file_info.is_dir());

        // No modify top-level directories/symlinks created by create_environment_dirs_early()
        // NOTE: On macOS, usr/libexec is a symlink (only created for Brew packages), so we skip it.
        // On Linux, usr/libexec is a real directory (RPM/Debian), so we DON'T skip it here.
        // The usr/libexec skip on macOS only affects Brew environments because only Brew creates this symlink.
        #[cfg(target_os = "macos")]
        if matches!(fhs_file.trim_end_matches('/'), "sbin" | "bin" | "lib" | "lib64" | "share" | "include" | "usr/sbin" | "usr/lib64" | "usr/libexec") {
            continue;
        }
        #[cfg(not(target_os = "macos"))]
        if matches!(fhs_file.trim_end_matches('/'), "sbin" | "bin" | "lib" | "lib64" | "share" | "include" | "usr/sbin" | "usr/lib64") {
            continue;
        }

        // On Windows, resolve ancestor directory symlinks (e.g., lib64 -> usr/lib64, Lib -> usr/lib)
        // This handles cases where packages install files to lib64/ or Lib/ which are symlinks
        #[cfg(windows)]
        let target_path = lfs::resolve_ancestor_symlink(&target_path);

        if fs_file_info.is_dir() {
            // Check if target path exists
            if let Ok(metadata) = lfs::symlink_metadata(&target_path) {
                // If it's a directory or a symlink to a directory, we're done
                if metadata.file_type().is_dir() {
                    continue;
                }
                // If it's a symlink (to a directory), skip - the symlink target directory will be used
                if metadata.file_type().is_symlink() {
                    continue;
                }
                // It's a file, remove it
                lfs::remove_file(&target_path)?;
            }
            lfs::create_dir_all(&target_path)?;
            continue;
        }

        // Create parent directory if it doesn't exist
        // No longer necessary, since filelist.txt always show dir before files under it
        // if let Some(parent) = target_path.parent() {
        //     fs::create_dir_all(parent)
        //         .with_context(|| format!("Failed to create parent directory {}", parent.display()))?;
        // }

        mirror_file(&fs_file, &target_path, Path::new(fhs_file), fs_file_info.is_link(), link_type, can_reflink)?
    }
    Ok(())
}

/// Handle a single file (symlink or regular) in mirror_dir function
///
/// This function decides whether to call mirror_symlink_file() or mirror_regular_file()
/// based on the is_link parameter.
///
/// Parameters:
/// - fs_file: Path to the file in the store
/// - target_path: Where to create the file/symlink in the environment
/// - fhs_file: Relative path from store_fs_dir (used to determine if file is in /etc/)
/// - is_link: Whether the file is a symlink (true) or regular file (false)
/// - link_type: Link type to use (hardlink, symlink, move, runpath)
/// - can_reflink: Whether reflink (copy-on-write) is supported
pub fn mirror_file(
    fs_file: &Path,
    target_path: &Path,
    fhs_file: &Path,
    is_link: bool,
    link_type: LinkType,
    can_reflink: bool,
) -> Result<()> {
    if is_link {
        mirror_symlink_file(fs_file, target_path, link_type)
            .with_context(|| format!("Failed to handle symlink file {}", fs_file.display()))
    } else if is_config_file_path(fhs_file) {
        mirror_config_file(fs_file, target_path, can_reflink)
            .with_context(|| format!("Failed to handle config file {}", fs_file.display()))
    } else {
        mirror_regular_file(fs_file, target_path, fhs_file, link_type, can_reflink)
            .with_context(|| format!("Failed to handle regular file {}", fs_file.display()))
    }
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
                log::debug!("Cross-device hardlink not work for {} -> {}, falling back to copy",
                           source.display(), target.display());
            } else {
                // Other error, try copy as fallback
                log::info!("Hardlink failed for {} -> {}: {}, falling back to copy",
                           source.display(), target.display(), hardlink_err);
            }
            lfs::copy(source, target)?;

            // Preserve permissions if requested
            if preserve_permissions {
                utils::preserve_file_permissions(source, target)?;
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
/// - link_type: Link type to use (for Move, symlink is moved instead of copied)
fn mirror_symlink_file(fs_file: &Path, target_path: &Path, link_type: LinkType) -> Result<()> {
    utils::remove_any_existing_file(target_path, true)?;
    log::trace!("mirror_symlink_file: fs_file={}, target_path={}, link_type={:?}", fs_file.display(), target_path.display(), link_type);

    // For Move link type, rename the symlink instead of copying
    if link_type == LinkType::Move {
        // Ensure parent directory exists
        if let Some(parent) = target_path.parent() {
            lfs::create_dir_all(parent)?;
        }
        lfs::rename(fs_file, target_path)?;
        return Ok(());
    }

    // Handle regular symlink (not pointing to directory)
    copy_symlink(fs_file, target_path)
        .with_context(|| format!("Failed to copy symlink from {} to {}", fs_file.display(), target_path.display()))?;
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

/// Clean up existing target file or directory before linking
fn cleanup_existing_target(
    fs_file: &Path,
    target_path: &Path,
    _fhs_file: &Path,
) -> Result<()> {
    // Remove any existing file/dirs
    if lfs::symlink_metadata(target_path).is_ok() {
        // On upgrade, it's normal to overwrite old files from previous version
        log::trace!("File already exists, overwriting {} with {}", target_path.display(), fs_file.display());
        // Check if target path is a directory and handle accordingly
        if lfs::symlink_metadata(target_path).map(|m| m.file_type().is_dir()).unwrap_or(false) {
            lfs::remove_dir_all(target_path)?;
        } else {
            lfs::remove_file(target_path)?;
        }
    }
    Ok(())
}

fn mirror_regular_file(fs_file: &Path, target_path: &Path, fhs_file: &Path, link_type: LinkType, can_reflink: bool) -> Result<()> {
    log::trace!("mirror_regular_file: fs_file={}, target_path={}, link_type={:?}", fs_file.display(), target_path.display(), link_type);
    // Clean up existing target (remove old file/dir if needed)
    cleanup_existing_target(fs_file, target_path, fhs_file)?;

    // Apply link_type for regular files
    match link_type {
        LinkType::Hardlink => {
            // Try hardlink first, fall back to symlink on failure
            match lfs::hard_link(fs_file, target_path) {
                Ok(()) => {
                    log::trace!("Created hardlink from {} to {}", fs_file.display(), target_path.display());
                }
                Err(e) => {
                    // Fall back to symlink if hardlink fails
                    log::debug!("Hardlink not work for {} -> {}: {}, falling back to symlink",
                               fs_file.display(), target_path.display(), e);
                    symlink_or_copy(fs_file, target_path, fhs_file)?;
                }
            }
        }
        LinkType::Symlink => {
            // Current behavior: prefer symlink
            symlink_or_copy(fs_file, target_path, fhs_file)?;
        }
        LinkType::Reflink => {
            // Try reflink first, fall back to copy
            lfs::reflink_or_copy(fs_file, target_path, can_reflink)?;
        }
        LinkType::Move => {
            // Move file from store to env (will be removed from store later)
            lfs::rename(fs_file, target_path)?;
        }
        LinkType::Runpath => {
            // Move file from store to env (will be removed from store later)
            // Same as Move - requires same filesystem (checked in compute_link_type_and_reflink)
            lfs::rename(fs_file, target_path)?;
        }
    }
    Ok(())
}

pub fn symlink_or_copy(fs_file: &Path, target_path: &Path, fhs_file: &Path) -> Result<()> {
    if let Some(preserve_permissions) = needs_hard_link_or_copy(fhs_file) {
        // Certain paths (script-language trees, gconv modules, rustc driver) must be
        // materialized as real files in env_root (via hardlink or copy) so that
        // their consumers behave correctly when resolving modules or sysroots.
        hard_link_or_copy(fs_file, target_path, preserve_permissions)?;
        return Ok(());
    }

    lfs::symlink(fs_file, target_path)
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

/// Usr-merge symlinks that need relative path adjustment
/// Format: (symlink_name, symlink_target, path_prefix_to_strip)
/// When a relative symlink is placed under one of these directories,
/// the path needs adjustment because the directory is a symlink.
const USR_MERGE_SYMLINKS: &[(&str, &str)] = &[
    ("bin", "usr/bin"),
    ("sbin", "usr/sbin"),
    ("lib", "usr/lib"),
    ("lib64", "usr/lib64"),
];

/// Adjust relative symlink target for usr-merge layout.
///
/// When a package has a relative symlink like `bin/go -> ../go/bin/go`,
/// and the environment uses usr-merge layout where `bin -> usr/bin`,
/// the symlink would be created at `usr/bin/go -> ../go/bin/go`,
/// which resolves to `usr/go/bin/go` instead of `go/bin/go`.
///
/// This function detects this case and adjusts the path:
/// - `../go/bin/go` becomes `../../go/bin/go`
fn adjust_symlink_target_for_usr_merge(
    link_target: &Path,
    target_path: &Path,
    env_root: &Path,
) -> PathBuf {
    // Only adjust relative paths starting with ../
    let link_target_str = match link_target.to_str() {
        Some(s) if s.starts_with("../") => s,
        _ => return link_target.to_path_buf(),
    };

    // Check if target_path is under a usr-merge symlink directory
    // Get the relative path from env_root
    let rel_path = match target_path.strip_prefix(env_root) {
        Ok(p) => p,
        Err(_) => return link_target.to_path_buf(),
    };

    // Get the first component (should be bin, sbin, lib, or lib64)
    let first_component = match rel_path.components().next() {
        Some(c) => c.as_os_str().to_string_lossy().to_string(),
        None => return link_target.to_path_buf(),
    };

    // Check if this is a usr-merge symlink
    for &(symlink_name, _symlink_target) in USR_MERGE_SYMLINKS {
        if first_component == symlink_name {
            // Check if the directory in env is actually a symlink/junction
            let symlink_path = env_root.join(symlink_name);
            if lfs::is_symlink_or_junction(&symlink_path) {
                // The symlink is in a usr-merge directory
                // Need to add one more "../" to account for the extra level
                // For example: ../go/bin/go -> ../../go/bin/go
                log::debug!(
                    "Adjusting symlink target for usr-merge: {} -> ../../{}",
                    link_target_str,
                    &link_target_str[3..]  // Skip the "../" prefix
                );
                return PathBuf::from(format!("../../{}", &link_target_str[3..]));
            }
        }
    }

    link_target.to_path_buf()
}

// Copy symlink, adjusting relative paths for usr-merge layout
fn copy_symlink(fs_file: &Path, target_path: &Path) -> Result<()> {
    let link_target = fs::read_link(fs_file)
        .with_context(|| format!("Failed to read symlink target for {}", fs_file.display()))?;
    log::trace!("copy_symlink: fs_file={}, link_target={:?}", fs_file.display(), link_target);

    // Try to get env_root from target_path (assume it's 2 levels up from bin/ or similar)
    // target_path is like /path/to/env/bin/go or /path/to/env/usr/bin/go
    // We need to find the env_root to check for usr-merge symlinks
    let adjusted_target = if link_target.is_relative() && link_target.starts_with("..") {
        // Try to find env_root by looking for the bin/sbin/lib symlinks
        if let Some(parent) = target_path.parent() {
            if let Some(env_root) = find_env_root_from_path(parent) {
                adjust_symlink_target_for_usr_merge(&link_target, target_path, &env_root)
            } else {
                link_target
            }
        } else {
            link_target
        }
    } else {
        link_target
    };

    // Determine symlink type by checking the existing symlink in store.
    // On Windows, symlink_dir has FILE_ATTRIBUTE_DIRECTORY flag set.
    // This is reliable even when symlink target doesn't exist (cross-package or dead links).
    // We don't need to follow the symlink or check if target exists.
    let is_dir = lfs::is_directory_symlink(fs_file);

    log::debug!(
        "copy_symlink: fs_file={}, adjusted_target={}, is_directory_symlink={}",
        fs_file.display(),
        adjusted_target.display(),
        is_dir
    );

    // Use symlink_to_file or symlink_to_directory explicitly based on symlink type.
    if is_dir {
        lfs::symlink_to_directory(&adjusted_target, target_path)
    } else {
        lfs::symlink_to_file(&adjusted_target, target_path)
    }
    .with_context(|| format!("Failed to create symlink {} -> {}", target_path.display(), adjusted_target.display()))?;
    Ok(())
}

/// Find env_root from a path by looking for usr-merge symlinks (bin, sbin, lib, lib64)
fn find_env_root_from_path(path: &Path) -> Option<PathBuf> {
    // Walk up the path to find a directory containing usr-merge symlinks
    let mut current = path;
    while let Some(parent) = current.parent() {
        // Check if this could be an env root by looking for typical structure
        if parent.join("usr").is_dir() {
            // Check for usr-merge symlinks/junctions
            let has_bin_symlink = lfs::is_symlink_or_junction(&parent.join("bin"));
            let has_usr_bin = crate::dirs::path_join(parent, &["usr", "bin"]).is_dir();
            if has_bin_symlink && has_usr_bin {
                return Some(parent.to_path_buf());
            }
        }
        current = parent;
    }
    None
}

