//! Installation Risk management module
//!
//! This module implements simplified risk detection including disk space checking,
//! file conflict detection, and config file handling.

use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;
use color_eyre::Result;
use color_eyre::eyre::eyre;
use crate::models::InstalledPackagesMap;
use crate::models::PACKAGE_CACHE;
use crate::models::config;
use crate::plan::{InstallationPlan, FilesystemInfo};
use crate::package_cache::map_pkgline2filelist;

/// Returns true if `rel_path` (relative path under package `fs/`, POSIX separators) is a directory
/// according to `filelist.txt` entries (directory paths end with `/`).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn installed_path_is_directory_in_map(map: &HashMap<String, String>, rel_path: &str) -> bool {
    let s = rel_path.trim().trim_start_matches('/').trim_end_matches('/');
    if s.is_empty() {
        return false;
    }
    map.contains_key(&format!("{}/", s))
}

/// Calculate total download and install sizes for the installation plan
#[allow(dead_code)]
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
    #[cfg(unix)]
    {
        use std::ffi::CString;

        // Default struct with fsid = 0 (failure/default)
        let mut info = FilesystemInfo {
            path: mount_point.to_path_buf(),
            fsid: 0,
            free_space: u64::MAX,
            free_inodes: u64::MAX, // Assume unlimited by default
            block_size: 4096,      // Default block size
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
        // On Linux, f_fsid is a u64 or struct depending on architecture
        // On macOS, f_fsid is typically u32 (f_fsid_val[2])
        // We create a unique ID by combining f_fsid and f_fsid (on Linux) or use dev_t
        #[cfg(target_os = "linux")]
        {
            let fsid_bytes = u64::to_ne_bytes(statvfs_buf.f_fsid);
            info.fsid = u64::from_ne_bytes(fsid_bytes);
        }
        #[cfg(target_os = "macos")]
        {
            // On macOS, use f_fsid as part of the filesystem ID
            // f_fsid is actually u32 on macOS, combine with f_fsid for uniqueness
            info.fsid = (statvfs_buf.f_fsid as u64) | ((statvfs_buf.f_fsid as u64) << 32);
        }

        let bsize = statvfs_buf.f_bsize as u64;
        let bavail = if (statvfs_buf.f_flag & libc::ST_RDONLY) != 0 {
            0
        } else {
            statvfs_buf.f_bavail as u64
        };

        info.free_space = bavail * bsize;
        info.block_size = bsize;

        // Handle filesystems without inodes (FAT, etc.)
        info.free_inodes = if statvfs_buf.f_ffree == 0 && statvfs_buf.f_files == 0 {
            u64::MAX // No inode limit
        } else {
            statvfs_buf.f_ffree as u64
        };

        info
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceW;
        use windows::Win32::Storage::FileSystem::GetVolumeInformationW;

        // Default struct with fsid = 0 (failure/default)
        let mut info = FilesystemInfo {
            path: mount_point.to_path_buf(),
            fsid: 0,
            free_space: u64::MAX,
            free_inodes: u64::MAX,
            block_size: 4096,
        };

        // Get the root path (e.g., "C:\")
        let root_path = mount_point.ancestors().last().unwrap_or(mount_point);
        let path_wide: Vec<u16> = root_path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();

        unsafe {
            // Get volume serial number as fsid
            let mut serial_number: u32 = 0;
            let result = GetVolumeInformationW(
                windows::core::PCWSTR(path_wide.as_ptr()),
                None,
                Some(&mut serial_number),
                None,
                None,
                None,
            );

            if result.is_ok() && serial_number != 0 {
                info.fsid = serial_number as u64;
            }

            // Get free space
            let mut free_clusters: u64 = 0;
            let mut total_clusters: u64 = 0;
            let mut bytes_per_sector: u32 = 0;
            let mut sectors_per_cluster: u32 = 0;

            if GetDiskFreeSpaceW(
                windows::core::PCWSTR(path_wide.as_ptr()),
                Some(&mut sectors_per_cluster),
                Some(&mut bytes_per_sector),
                Some(&mut free_clusters as *mut u64 as *mut u32),
                Some(&mut total_clusters as *mut u64 as *mut u32),
            ).is_ok() {
                info.free_space = sectors_per_cluster as u64 * bytes_per_sector as u64 * free_clusters;
            }
        }

        info
    }
    #[cfg(not(any(unix, windows)))]
    {
        // Other platforms: return default struct with fsid=0
        FilesystemInfo {
            path: mount_point.to_path_buf(),
            fsid: 0,
            free_space: u64::MAX,
            free_inodes: u64::MAX,
            block_size: 4096,
        }
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
/// - env_root needs space for inodes (symlinks) or files (copies)
/// These may be on the same or different devices
/// If both are on the same filesystem (same fsid), check total size requirement
pub fn check_disk_space_for_plan(
    plan: &InstallationPlan,
    store_root: &Path,
    download_cache: &Path,
) -> Result<()> {
    // Check if download_cache_fs and store_root_fs are on the same filesystem
    let download_same_fs = crate::link::same_filesystem(
        &plan.download_cache_fs,
        &plan.store_root_fs,
    );

    if download_same_fs {
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

    // Check env_root space/inodes if on different filesystem from store
    let env_same_fs = crate::link::same_filesystem(
        &plan.store_root_fs,
        &plan.env_root_fs,
    );

    if !env_same_fs && plan.total_install > 0 {
        // Env is on different filesystem from store
        // Need space for inodes (symlinks) - estimate 1 inode per KB of installed size
        // This is a rough heuristic; actual inode count varies by file size distribution
        let estimated_inodes = (plan.total_install / 1024).max(1000); // At least 1000 inodes
        let env_root = &plan.env_root;

        // Check inode availability
        let available_inodes = plan.env_root_fs.free_inodes;
        let safety_margin = estimated_inodes / 20; // 5% safety margin
        let total_inodes_needed = estimated_inodes + safety_margin;

        if available_inodes < total_inodes_needed {
            let shortage = total_inodes_needed.saturating_sub(available_inodes);
            return Err(eyre!(
                "Insufficient inodes on {} for symlinks: need ~{} inodes, available {} inodes (shortage: {} inodes)",
                env_root.display(),
                total_inodes_needed,
                available_inodes,
                shortage
            ));
        }

        // Check disk space for symlink directory entries
        // Each symlink's directory entry takes one filesystem block
        // Use block_size / 2 as average (some entries share blocks)
        let block_size = plan.env_root_fs.block_size.max(4096);
        let min_env_space = total_inodes_needed * block_size / 2;
        check_space(&plan.env_root_fs, min_env_space, env_root)?;
    }

    Ok(())
}

/// Build file map from installed packages, excluding those being removed or upgraded.
/// Values are pkgkeys; keys are paths from `filelist.txt` (files and dirs; directory entries end with `/`).
#[allow(dead_code)]
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
        if let Ok(file_list) = map_pkgline2filelist(store_root, &pkg_info.pkgline) {
            for file_path in &file_list {
                installed_files.insert(file_path.clone(), pkgkey.clone());
            }
        } else {
            log::debug!("Failed to get filelist for package {}: {}", pkgkey, pkg_info.pkgline);
        }
    }

    Ok(installed_files)
}

/// Snapshot of [`build_installed_file_map`] using transaction fields from `plan`.
pub fn build_installed_file_map_from_plan(plan: &InstallationPlan) -> Result<HashMap<String, String>> {
    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
    build_installed_file_map(
        &installed,
        &plan.store_root,
        &plan.old_removes,
        &plan.upgrades_old,
    )
}

/// Check risks for all packages at once (inode space, file conflicts)
/// This is called before linking any packages to keep the environment clean
/// Validate packages before linking - check inodes and file conflicts
#[allow(dead_code)]
pub fn validate_before_linking(plan: &mut crate::plan::InstallationPlan) -> Result<()> {
    let total_inodes_needed = validate_file_conflicts(plan)?;
    validate_inode_space(plan, total_inodes_needed)?;
    Ok(())
}

/// Validate file conflicts for all packages before linking
/// Returns total number of inodes (files) needed across all packages
#[allow(dead_code)]
pub fn validate_file_conflicts(
    plan: &mut crate::plan::InstallationPlan,
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
                        if config().install.ignore_file_conflicts {
                            log::warn!(
                                "File conflict IGNORED: {} (from package {}) conflicts with installed file from package {} (--ignore-file-conflicts)",
                                file_path,
                                pkgkey,
                                existing_pkgkey
                            );
                            continue;
                        } else {
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
    }

    plan.installed_file_map = Some(Arc::new(file_map));

    Ok(total_inodes_needed)
}

/// Validate inode space for installation plan
/// Requires total_inodes_needed (count of files) to check against free inodes on env_root
#[allow(dead_code)]
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
