//! Installation Risk management module
//!
//! This module implements simplified risk detection including disk space checking,
//! file conflict detection, and config file handling.

use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use color_eyre::Result;
use color_eyre::eyre::eyre;
use crate::models::InstalledPackagesMap;
use crate::plan::{InstallationPlan, FilesystemInfo};
use crate::models::PACKAGE_CACHE;
use crate::package_cache::map_pkgline2filelist;

/// Calculate total download and install sizes for the installation plan
pub fn calculate_plan_sizes(plan: &mut InstallationPlan) -> Result<()> {
    let mut total_download: u64 = 0;
    let mut total_install: u64 = 0;

    for pkgkey in plan.fresh_installs.iter().chain(plan.upgrades_new.iter()) {
        if let Ok(pkginfo) = crate::package_cache::load_package_info(pkgkey) {
            total_download += pkginfo.size as u64;
            total_install += pkginfo.installed_size as u64;
        }
    }

    plan.total_download = total_download;
    plan.total_install = total_install;

    Ok(())
}

/// Get filesystem information for a mount point
/// Returns FilesystemInfo with filesystem ID, free space, and free inodes
/// Always returns a FilesystemInfo struct, fsid=0 if statvfs failed
pub fn get_filesystem_info(mount_point: &Path) -> FilesystemInfo {
    #[cfg(not(target_os = "linux"))]
    {
        // Non-Linux platforms: return default struct with fsid=0
        FilesystemInfo {
            path: mount_point.to_path_buf(),
            fsid: 0,
            free_space: u64::MAX,
            free_inodes: u64::MAX,
        }
    }
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;

        // Default struct with fsid = 0 (failure/default)
        let mut info = FilesystemInfo {
            path: mount_point.to_path_buf(),
            fsid: 0,
            free_space: u64::MAX,
            free_inodes: u64::MAX, // Assume unlimited by default
        };

        // Try to convert path to C string
        let c_path = match CString::new(mount_point.as_os_str().as_bytes()) {
            Ok(c) => c,
            Err(_) => {
                // CString conversion failed, return default (fsid=0)
                log::warn!("CString conversion failed for path: {}", mount_point.display());
                return info;
            }
        };

        let mut statvfs_buf: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut statvfs_buf) };

        if rc != 0 {
            // statvfs failed, return default (fsid=0)
            let io_err = std::io::Error::last_os_error();
            log::warn!("statvfs failed for {}: {} (errno: {})", mount_point.display(), io_err, io_err.raw_os_error().unwrap_or(-1));
            return info;
        }

        // Get filesystem ID from statvfs
        // f_fsid may be u64 or a struct depending on the system
        // Convert to bytes using native endianness
        let fsid_bytes = u64::to_ne_bytes(statvfs_buf.f_fsid);
        info.fsid = u64::from_ne_bytes(fsid_bytes);

        let bsize = statvfs_buf.f_bsize as u64;
        let bavail = if (statvfs_buf.f_flag & libc::ST_RDONLY) != 0 {
            0
        } else {
            statvfs_buf.f_bavail as u64
        };

        info.free_space = bavail * bsize;

        // Handle filesystems without inodes (FAT, etc.)
        info.free_inodes = if statvfs_buf.f_ffree == 0 && statvfs_buf.f_files == 0 {
            u64::MAX // No inode limit
        } else {
            statvfs_buf.f_ffree as u64
        };

        info
    }
}

/// Helper function to check disk space with safety margin
/// Returns error if insufficient space available
/// Safety margin: 5% of required space or 100MB minimum, whichever is larger
fn check_space(fs_info: &FilesystemInfo, required: u64, location: &Path) -> Result<()> {
    // Calculate safety margin: 5% of required or 100MB minimum
    let safety_margin_5pct = required / 20; // 5% = 1/20
    const MIN_SAFETY_MARGIN: u64 = 100 * 1024 * 1024; // 100MB
    let safety_margin = safety_margin_5pct.max(MIN_SAFETY_MARGIN);
    let total_needed = required + safety_margin;

    if fs_info.free_space < total_needed {
        let shortage = total_needed.saturating_sub(fs_info.free_space);
        return Err(eyre!(
            "Insufficient disk space on {}: need {} bytes ({} + {} safety margin), available {} bytes (shortage: {} bytes)",
            location.display(),
            total_needed,
            crate::utils::format_size(required),
            crate::utils::format_size(safety_margin),
            crate::utils::format_size(fs_info.free_space),
            crate::utils::format_size(shortage)
        ));
    }
    Ok(())
}

/// Check disk space for installation plan
/// - plan.total_download needs space on download cache filesystem
/// - plan.total_install needs space on store filesystem
/// These may be on the same or different devices
/// If both are on the same filesystem (same fsid), check total size requirement
pub fn check_disk_space_for_plan(
    plan: &InstallationPlan,
    store_root: &Path,
    download_cache: &Path,
) -> Result<()> {
    // Check if download_cache_fs and store_root_fs are on the same filesystem
    let same_fs = crate::link::same_filesystem(
        &plan.download_cache_fs,
        &plan.store_root_fs,
    );

    if same_fs {
        // Both on same filesystem - check total requirement
        let total_required = plan.total_download + plan.total_install;
        check_space(
            &plan.store_root_fs,
            total_required,
            store_root,
        )?;
    } else {
        // Different filesystems - check separately
        check_space(&plan.download_cache_fs, plan.total_download, download_cache)?;
        check_space(&plan.store_root_fs, plan.total_install, store_root)?;
    }

    Ok(())
}

/// Build file map from installed packages, excluding those being removed or upgraded
pub fn build_installed_file_map(
    packages: &InstalledPackagesMap,
    store_root: &Path,
    old_removes: &std::collections::HashSet<String>,
    upgrades_old: &std::collections::HashSet<String>,
) -> Result<HashMap<String, String>> {
    let mut installed_files = HashMap::new();

    for (pkgkey, pkg_info) in packages.iter() {
        // Skip packages that are being removed or upgraded
        if old_removes.contains(pkgkey) || upgrades_old.contains(pkgkey) {
            continue;
        }
        // Get filelist using the cached function (already filters out dirs)
        if let Ok(file_list) = map_pkgline2filelist(store_root, &pkg_info.pkgline) {
            // Process file list - all entries are files (dirs already filtered)
            for file_path in &file_list {
                installed_files.insert(file_path.clone(), pkgkey.clone());
            }
        } else {
            log::debug!("Failed to get filelist for package {}: {}", pkgkey, pkg_info.pkgline);
        }
    }

    Ok(installed_files)
}

/// Check risks for all packages at once (inode space, file conflicts)
/// This is called before linking any packages to keep the environment clean
/// Validate packages before linking - check inodes and file conflicts
pub fn validate_before_linking(
    plan: &crate::plan::InstallationPlan,
) -> Result<()> {
    let total_inodes_needed = validate_file_conflicts(plan)?;
    validate_inode_space(plan, total_inodes_needed)?;
    Ok(())
}

/// Validate file conflicts for all packages before linking
/// Returns total number of inodes (files) needed across all packages
pub fn validate_file_conflicts(
    plan: &crate::plan::InstallationPlan,
) -> color_eyre::Result<u64> {
    let store_root = &plan.store_root;

    // Count total files (inodes) needed across all packages
    let mut total_inodes_needed: u64 = 0;

    // Build file map from installed packages (excluding those being removed or upgraded)
    // This map will also track files from new packages to detect all conflicts
    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
    let mut file_map = build_installed_file_map(
        &installed,
        store_root,
        &plan.old_removes,
        &plan.upgrades_old,
    )?;
    drop(installed);

    // Process each package
    for pkgkey in plan.batch.new_pkgkeys.iter() {
        if let Some(package_info) = crate::plan::pkgkey2new_pkg_info(plan, pkgkey) {
            // Get filelist from cache or store (already filters out dirs)
            let file_list = map_pkgline2filelist(store_root, &package_info.pkgline)?;
            total_inodes_needed += file_list.len() as u64;

            // Count files (inodes) needed and check conflicts
            for file_path in &file_list {
                // Skip directories for conflict checking (they end with /)
                if file_path.ends_with('/') {
                    continue;
                }

                // Check for file conflicts at insertion time
                if let Some(existing_pkgkey) = file_map.insert(file_path.clone(), pkgkey.clone()) {
                    // File already exists in map - conflict detected
                    // Check if conflict is with another new package (transaction conflict) or installed package
                    if plan.batch.new_pkgkeys.contains(&existing_pkgkey) {
                        // Transaction conflict: file provided by multiple new packages.
                        // Downgrade transaction-time conflicts to warnings so that package
                        // sets that legitimately share some files can still be installed.
                        log::warn!(
                            "Transaction file conflict: {} is provided by multiple packages: {} and {}",
                            file_path,
                            existing_pkgkey,
                            pkgkey
                        );
                        continue;
                    } else {
                        // Conflict with installed package
                        return Err(eyre!(
                            "File conflict: {} (from package {}) conflicts with installed file from package {}",
                            file_path,
                            pkgkey,
                            existing_pkgkey
                        ));
                    }
                }
            }
        }
    }

    Ok(total_inodes_needed)
}

/// Validate inode space for installation plan
/// Requires total_inodes_needed (count of files) to check against free inodes on env_root
pub fn validate_inode_space(
    plan: &crate::plan::InstallationPlan,
    total_inodes_needed: u64,
) -> color_eyre::Result<()> {
    let env_root = &plan.env_root;
    // Get free inodes on env_root mount point and compare to total file count
    let env_fs = &plan.env_root_fs;
    let available = env_fs.free_inodes;
    if available < total_inodes_needed + total_inodes_needed / 20 {
        let shortage = total_inodes_needed.saturating_sub(available);
        return Err(eyre!(
            "Insufficient inodes on {}: need {} inodes, available {} inodes (shortage: {} inodes)",
            env_root.display(),
            total_inodes_needed,
            available,
            shortage
        ));
    }
    Ok(())
}
