//! Installation Risk management module
//!
//! This module implements simplified risk detection including disk space checking,
//! file conflict detection, and config file handling.

use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use color_eyre::Result;
use color_eyre::eyre::eyre;
use crate::models::{InstalledPackagesMap, InstalledPackageInfo};
use crate::plan::{InstallationPlan, FilesystemInfo};
use crate::models::PACKAGE_CACHE;
use crate::package_cache::map_package2filelist;
use crate::link::same_filesystem;

/// Calculate total download and install sizes for the installation plan
pub fn calculate_plan_sizes(plan: &mut InstallationPlan) -> Result<()> {
    let mut total_download: u64 = 0;
    let mut total_install: u64 = 0;

    for pkgkey in plan.fresh_installs.keys().chain(plan.upgrades_new.keys()) {
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
pub fn get_filesystem_info(mount_point: &Path) -> Result<FilesystemInfo> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        let c_path = CString::new(mount_point.as_os_str().as_bytes())
            .map_err(|e| eyre!("Invalid path: {}", e))?;

        let mut statvfs_buf: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut statvfs_buf) };

        if rc != 0 {
            return Err(eyre!("statvfs failed for {}", mount_point.display()));
        }

        // Get filesystem ID from statvfs
        // f_fsid may be u64 or a struct depending on the system
        // We'll use unsafe to access it as bytes and convert to u64
        let fsid_bytes = unsafe { std::mem::transmute::<_, [u8; 8]>(statvfs_buf.f_fsid) };
        let fsid = u64::from_le_bytes(fsid_bytes);

        let bsize = statvfs_buf.f_bsize as u64;
        let bavail = if (statvfs_buf.f_flag & libc::ST_RDONLY) != 0 {
            0
        } else {
            statvfs_buf.f_bavail as u64
        };

        let free_space = bavail * bsize;

        // Handle filesystems without inodes (FAT, etc.)
        let free_inodes = if statvfs_buf.f_ffree == 0 && statvfs_buf.f_files == 0 {
            u64::MAX // No inode limit
        } else {
            statvfs_buf.f_ffree as u64
        };

        Ok(FilesystemInfo {
            fsid,
            free_space,
            free_inodes,
        })
    }
}

/// Helper function to check disk space with safety margin
/// Returns error if insufficient space available
/// Safety margin: 5% of required space or 100MB minimum, whichever is larger
fn check_space(fs_info: Option<&FilesystemInfo>, required: u64, location: &Path) -> Result<()> {
    if let Some(fs_info) = fs_info {
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
    let same_fs = same_filesystem(
        plan.download_cache_fs.as_ref(),
        plan.store_root_fs.as_ref(),
    ).unwrap_or(false);

    if same_fs {
        // Both on same filesystem - check total requirement
        let total_required = plan.total_download + plan.total_install;
        check_space(
            plan.store_root_fs.as_ref(),
            total_required,
            store_root,
        )?;
    } else {
        // Different filesystems - check separately
        check_space(plan.download_cache_fs.as_ref(), plan.total_download, download_cache)?;
        check_space(plan.store_root_fs.as_ref(), plan.total_install, store_root)?;
    }

    Ok(())
}


/// Check for file conflicts with installed packages
/// Returns list of conflicting file paths with their owning packages
pub fn check_file_conflicts(
    file_path: &str,
    pkgkey: &str,
    installed_files: &HashMap<String, String>, // relative path -> pkgkey
) -> Result<Vec<(String, String)>> {
    let mut conflicts = Vec::new();

    if let Some(installed_pkgkey) = installed_files.get(file_path) {
        if installed_pkgkey != pkgkey {
            conflicts.push((file_path.to_string(), installed_pkgkey.clone()));
        }
    }

    Ok(conflicts)
}


/// Load installed files from installed packages for conflict detection
pub fn load_installed_files(
    packages: &InstalledPackagesMap,
    store_root: &Path,
) -> Result<HashMap<String, String>> {
    let mut installed_files = HashMap::new();

    for (pkgkey, pkg_info) in packages.iter() {
        let store_fs_dir = store_root.join(&pkg_info.pkgline).join("fs");

        // Get filelist using the cached function
        if let Ok(file_list) = map_package2filelist(pkgkey, &store_fs_dir) {
            // Process file list - skip directories, only track files for conflict detection
            for file_info in &file_list {
                if file_info.is_dir() {
                    continue;
                }

                installed_files.insert(file_info.path.clone(), pkgkey.clone());
            }
        } else {
            log::debug!("Failed to get filelist for package {}: {}", pkgkey, store_fs_dir.display());
        }
    }

    Ok(installed_files)
}

/// Check risks for all packages at once (inode space, file conflicts)
/// This is called before linking any packages to keep the environment clean
/// Validate packages before linking - check inodes and file conflicts
pub fn validate_before_linking(
    packages_to_link: &[(String, InstalledPackageInfo)],
    store_root: &Path,
    env_root: &Path,
    plan: &crate::plan::InstallationPlan,
) -> Result<()> {
    // Count total files (inodes) needed across all packages
    let mut total_inodes_needed: u64 = 0;
    let mut all_transaction_files: HashMap<String, String> = HashMap::new();

    // Load installed files once for all packages
    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
    let installed_files = load_installed_files(&installed, store_root)?;
    drop(installed);

    // Process each package
    for (pkgkey, package_info) in packages_to_link {
        let store_fs_dir = store_root.join(&package_info.pkgline).join("fs");

        // Get filelist from cache or store
        let file_list = map_package2filelist(pkgkey, &store_fs_dir)?;

        // Count files (inodes) needed
        for file_info in &file_list {
            if file_info.is_dir() {
                continue;
            }

            total_inodes_needed += 1;

            // Check conflicts with installed files
            if let Ok(conflicts) = check_file_conflicts(&file_info.path, pkgkey, &installed_files) {
                for (conflict_path, conflict_pkgkey) in conflicts {
                    return Err(eyre!(
                        "File conflict: {} (from package {}) conflicts with installed file from package {}",
                        conflict_path,
                        pkgkey,
                        conflict_pkgkey
                    ));
                }
            }

            // Track files in transaction for conflict detection
            if let Some(existing_pkgkey) = all_transaction_files.insert(file_info.path.clone(), pkgkey.clone()) {
                // Conflict detected: file is provided by multiple packages
                return Err(eyre!(
                    "Transaction file conflict: {} is provided by multiple packages: {} and {}",
                    file_info.path,
                    existing_pkgkey,
                    pkgkey
                ));
            }
        }
    }

    // Get free inodes on env_root mount point and compare to total file count
    if let Some(ref env_fs) = plan.env_root_fs {
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
    }

    Ok(())
}
