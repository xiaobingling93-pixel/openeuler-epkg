//! Package linking module
//!
//! This module provides functions for linking package files from the store to the environment,
//! supporting different link types (hardlink, symlink, move, runpath).

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use crate::models::{LinkType, InstalledPackageInfo, PackageFormat};
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
    if !target_path.exists() {
        return EtcFileAction::Create;
    }

    // Check if files are identical (simplified - just check if they exist and are same size)
    if let (Ok(meta1), Ok(meta2)) = (fs::metadata(target_path), fs::metadata(fs_file)) {
        if meta1.len() == meta2.len() && meta1.mode() == meta2.mode() {
            return EtcFileAction::Identical;
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
    if target_path.exists() {
        lfs::remove_file(target_path)?;
    }

    // Copy config file using reflink if supported, otherwise regular copy
    lfs::reflink_or_copy(fs_file, target_path, can_reflink)?;
    Ok(())
}

// link files from env_root to store_fs_dir
pub fn link_package(plan: &InstallationPlan, store_fs_dir: &PathBuf) -> Result<()> {
    log::debug!("link_package: link={:?} env_root={} store_fs_dir={}", plan.link, plan.env_root.display(), store_fs_dir.display());
    // Check if this is a conda package and use conda-specific linking
    if plan.package_format == PackageFormat::Conda {
        return crate::conda_link::link_conda_package(plan, store_fs_dir);
    }
    link_package_generic(plan, store_fs_dir)
}

/// Link package files using generic (non-format-specific) linking
/// This handles standard file linking without format-specific metadata processing
pub fn link_package_generic(plan: &InstallationPlan, store_fs_dir: &PathBuf) -> Result<()> {
    // Standard linking for non-conda packages
    let fs_files = utils::list_package_files_with_info(store_fs_dir.to_str().ok_or_else(|| eyre::eyre!("Invalid store_fs_dir path: {}", store_fs_dir.display()))?)
        .with_context(|| format!("Failed to list package files in {}", store_fs_dir.display()))?;
    mirror_dir(&plan.env_root, store_fs_dir, &fs_files, plan.link, plan.can_reflink)
        .with_context(|| format!("Failed to mirror directory from {} to {}", store_fs_dir.display(), plan.env_root.display()))?;

    // For link=move, remove the 'fs' dir after moving all files
    if plan.link == LinkType::Move {
        // Remove the fs directory after all files have been moved
        if store_fs_dir.exists() {
            // Try to remove the directory (should be empty or nearly empty after moves)
            if let Err(e) = lfs::remove_dir_all(store_fs_dir) {
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
        let env_file_path = env_root.join(rel_path);

        if env_file_path.exists() {
            if env_file_path.is_dir() {
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
    if fs1.fsid == 0 || fs2.fsid == 0 {
        log::warn!("Filesystem info incomplete on comparing '{}' (fsid={}) with '{}' (fsid={}), assuming different filesystems",
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

    // Use stored filesystem info from plan
    let same_fs = same_filesystem(
        &plan.store_root_fs,
        &plan.env_root_fs,
    );

    // can_hardlink: hardlinks only work on the same filesystem
    let can_hardlink = same_fs;

    // can_symlink: symlinks work across filesystems, but not if link type is Move
    let can_symlink = link_type != LinkType::Move;

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
    }

    plan.link = link_type;
    plan.can_reflink = can_reflink;
    plan.can_hardlink = can_hardlink;
    plan.can_symlink = can_symlink;
    Ok(())
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
            lfs::remove_file(&symlink1_path)?;

            // Create parent directory if it doesn't exist
            if let Some(parent) = symlink1_path.parent() {
                lfs::create_dir_all(parent)?;
            }

            lfs::symlink(fs_file, &symlink1_path)?;
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
        let fs_file = store_fs_dir.join(&fs_file_info.path);
        let fhs_file = &fs_file_info.path;
        let target_path = env_root.join(fhs_file);
        log::trace!("mirror_dir: processing fhs_file={}, is_link={}, is_dir={}", fhs_file, fs_file_info.is_link(), fs_file_info.is_dir());

        // No modify top-level directories/symlinks created by create_environment_directories()
        if matches!(fhs_file.as_str(), "sbin" | "bin" | "lib" | "lib64" | "lib32" | "usr/sbin" | "usr/lib64") {
            continue;
        }

        if fs_file_info.is_dir() {
            // Check if target path exists and is not a directory
            if target_path.exists() && !target_path.is_dir() {
                // Remove the non-directory file first
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
        mirror_symlink_file(fs_file, target_path)
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
/// - _fhs_file: Relative path from store_fs_dir (unused, kept for consistency)
fn mirror_symlink_file(fs_file: &Path, target_path: &Path) -> Result<()> {
    utils::remove_any_existing_file(target_path, true)?;
    log::trace!("mirror_symlink_file: fs_file={}, target_path={}", fs_file.display(), target_path.display());

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
        if target_path.is_dir() {
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

// Copy symlink as-is without shortcutting
fn copy_symlink(fs_file: &Path, target_path: &Path) -> Result<()> {
    let link_target = fs::read_link(fs_file)
        .with_context(|| format!("Failed to read symlink target for {}", fs_file.display()))?;
    log::trace!("copy_symlink: fs_file={}, link_target={:?}", fs_file.display(), link_target);
    lfs::symlink(&link_target, target_path)
        .with_context(|| format!("Failed to create symlink {} -> {}", target_path.display(), link_target.display()))?;
    Ok(())
}

