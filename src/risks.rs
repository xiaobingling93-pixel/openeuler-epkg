//! Installation Risk management module
//!
//! This module implements simplified risk detection including disk space checking,
//! file conflict detection, and config file handling.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use color_eyre::Result;
use color_eyre::eyre::eyre;
use crate::models::InstalledPackagesMap;
use crate::models::PACKAGE_CACHE;
use crate::models::config;
use crate::plan::{InstallationPlan, FilesystemInfo};
use crate::package_cache::map_pkgline2filelist;

// ============================================================================
// Progressive Disk Space Estimation for Store Data Integrity
// ============================================================================
//
// Problem: Concurrent download+unpack means packages occupy disk space BEFORE
// we get accurate file counts from filelist.txt. The original one-time risk
// evaluation happens AFTER all packages are unpacked, which is too late.
//
// Solution: Progressive estimation per-package granularity:
//
// 1. At installation start: Calculate TOTAL estimate for ALL packages once
// 2. After each unpack: Replace that package's estimate with actual value
// 3. predicted_final_free = INITIAL_FREE_SPACE - BATCH_TOTAL_ESTIMATE
//
// As more packages are unpacked, BATCH_TOTAL_ESTIMATE becomes more accurate,
// and predicted_final_free converges to the true final free space.
//
// Why INITIAL_FREE_SPACE is set ONLY ONCE at the start:
// ======================================================
// We do concurrent download+unpack. At any moment during the process:
// - Some packages are fully unpacked
// - Some packages are partially unpacked (files being written)
// - Some packages are being downloaded
// - Some packages haven't started yet
//
// During this period, a real-time 'df' query returns free space that is in an
// indeterminate state - it doesn't correspond to any clean "checkpoint" in the
// installation process. The free space is a moving target that can't be used
// for meaningful calculations.
//
// Therefore, we can only reliably measure free space at the START (before any
// concurrent unpacking begins) and use that as our baseline. All subsequent
// calculations are based on this initial snapshot plus our progressive estimation.
//
// Constants derived from real /usr data analysis:
// - 23GB total, 480,995 files, 45,681 dirs, 42,861 symlinks
// - AVG_FILE_SIZE: 50KB (23GB / 480,995 files)
// - AVG_FILES_PER_DIR: 10 (480,995 / 45,681)
// - AVG_FILES_PER_LINK: 11 (480,995 / 42,861)
//

const AVG_FILE_SIZE: u64 = 50 * 1024; // 50KB
const AVG_FILES_PER_DIR: u64 = 10;
const AVG_FILES_PER_LINK: u64 = 11;
const MIN_FREE_SPACE: u64 = 100 * 1024 * 1024; // 100MB safety margin

static INITIAL_FREE_SPACE: AtomicU64 = AtomicU64::new(0);
static BATCH_TOTAL_ESTIMATE: AtomicU64 = AtomicU64::new(0);
static PACKAGE_ESTIMATES: RwLock<Option<HashMap<String, u64>>> = RwLock::new(None);

/// Initialize estimation for entire batch (call once at installation start)
/// Sets INITIAL_FREE_SPACE once - see module comment for why this is only
/// done at the start and not during concurrent unpack operations.
pub fn init_batch_estimation(pkgkeys: &[String]) -> Result<()> {
    let store_root = crate::models::dirs().epkg_store.clone();
    let store_fs_info = get_filesystem_info(&store_root);
    INITIAL_FREE_SPACE.store(store_fs_info.free_space, Ordering::Relaxed);

    let mut total_estimate: u64 = 0;
    let mut pkg_estimates = HashMap::new();

    for pkgkey in pkgkeys {
        match estimate_package_disk_space(pkgkey) {
            Ok(est) => {
                pkg_estimates.insert(pkgkey.clone(), est);
                total_estimate += est;
            }
            Err(e) => {
                log::warn!("Failed to estimate disk space for {}: {}", pkgkey, e);
            }
        }
    }

    BATCH_TOTAL_ESTIMATE.store(total_estimate, Ordering::Relaxed);
    *PACKAGE_ESTIMATES.write().unwrap() = Some(pkg_estimates);

    log::debug!(
        "init_batch_estimation: {} packages, initial_free={}, total_estimate={}",
        pkgkeys.len(),
        crate::utils::format_size(store_fs_info.free_space),
        crate::utils::format_size(total_estimate)
    );

    Ok(())
}

/// Reset estimation counters (call before starting new installation batch)
pub fn reset_estimation_counters() {
    INITIAL_FREE_SPACE.store(0, Ordering::Relaxed);
    BATCH_TOTAL_ESTIMATE.store(0, Ordering::Relaxed);
    *PACKAGE_ESTIMATES.write().unwrap() = None;
}

/// Get predicted final free space (converges to true value as packages are unpacked)
pub fn get_predicted_final_free() -> u64 {
    let initial_free = INITIAL_FREE_SPACE.load(Ordering::Relaxed);
    let total_estimate = BATCH_TOTAL_ESTIMATE.load(Ordering::Relaxed);
    initial_free.saturating_sub(total_estimate)
}

/// Estimate disk space needed for a package (before unpack)
pub fn estimate_package_disk_space(pkgkey: &str) -> Result<u64> {
    let package = crate::package_cache::load_package_info(pkgkey)
        .map_err(|e| eyre!("Failed to load package info for {}: {}", pkgkey, e))?;

    let installed_size = package.installed_size as u64;
    let file_count_estimate = if installed_size > 0 {
        installed_size / AVG_FILE_SIZE
    } else {
        10
    };

    let dir_count_estimate = file_count_estimate / AVG_FILES_PER_DIR + 1;
    let link_count_estimate = file_count_estimate / AVG_FILES_PER_LINK;

    let store_root = crate::models::dirs().epkg_store.clone();
    let store_fs_info = get_filesystem_info(&store_root);
    let block_size = store_fs_info.block_size.max(4096);

    let file_block_overhead = file_count_estimate * block_size * 3 / 4;
    let dir_overhead = dir_count_estimate * block_size;

    const INFO_BASE_OVERHEAD: u64 = 2 * 1024;
    const FILELIST_BYTES_PER_ENTRY: u64 = 80;
    let total_entries = file_count_estimate + dir_count_estimate + link_count_estimate;
    let info_overhead = INFO_BASE_OVERHEAD + total_entries * FILELIST_BYTES_PER_ENTRY;

    let total_estimate = installed_size + file_block_overhead + dir_overhead + info_overhead;

    log::trace!(
        "estimate_package_disk_space: pkgkey={}, installed_size={}, total_estimate={}",
        pkgkey, crate::utils::format_size(installed_size), crate::utils::format_size(total_estimate)
    );

    Ok(total_estimate)
}

/// Check if there's enough disk space before unpacking
pub fn check_store_space_before_unpack(pkgkey: &str) -> Result<()> {
    let predicted_final_free = get_predicted_final_free();

    log::trace!(
        "check_store_space_before_unpack: pkgkey={}, predicted_final_free={}",
        pkgkey,
        crate::utils::format_size(predicted_final_free)
    );

    if predicted_final_free < MIN_FREE_SPACE {
        let initial_free = INITIAL_FREE_SPACE.load(Ordering::Relaxed);
        let batch_total = BATCH_TOTAL_ESTIMATE.load(Ordering::Relaxed);

        return Err(eyre!(
            "Insufficient disk space predicted: {} final free space. \
             Initial free: {}, current batch estimate: {}. \
             Abort early to prevent partial/corrupted store.",
            crate::utils::format_size(predicted_final_free),
            crate::utils::format_size(initial_free),
            crate::utils::format_size(batch_total)
        ));
    }

    Ok(())
}

/// Adjust batch estimate after unpack by replacing estimate with actual
pub fn adjust_batch_estimate(pkgkey: &str, store_tmp_dir: &Path) -> Result<()> {
    let pkg_estimate = {
        let estimates = PACKAGE_ESTIMATES.read().unwrap();
        match estimates.as_ref() {
            Some(map) => map.get(pkgkey).copied().unwrap_or(0),
            None => 0,
        }
    };

    let filelist_path = store_tmp_dir.join("info").join("filelist.txt");

    if !crate::lfs::exists_on_host(&filelist_path) {
        log::warn!("filelist.txt not found at {}, skipping adjustment", filelist_path.display());
        return Ok(());
    }

    let content = std::fs::read_to_string(&filelist_path)
        .map_err(|e| eyre!("Failed to read filelist.txt: {}", e))?;

    let file_infos = crate::mtree::parse_simplified_mtree(&content)?;

    let actual_file_count = file_infos.iter().filter(|f| f.is_file()).count() as u64;
    let actual_dir_count = file_infos.iter().filter(|f| f.is_dir()).count() as u64;
    let actual_link_count = file_infos.iter().filter(|f| f.is_link()).count() as u64;

    let store_root = crate::models::dirs().epkg_store.clone();
    let store_fs_info = get_filesystem_info(&store_root);
    let block_size = store_fs_info.block_size.max(4096);

    let file_block_overhead = actual_file_count * block_size * 3 / 4;
    let dir_overhead = actual_dir_count * block_size;

    const INFO_BASE_OVERHEAD: u64 = 2 * 1024;
    const FILELIST_BYTES_PER_ENTRY: u64 = 80;
    let actual_entries = actual_file_count + actual_dir_count + actual_link_count;
    let info_overhead = INFO_BASE_OVERHEAD + actual_entries * FILELIST_BYTES_PER_ENTRY;

    let package_txt_path = store_tmp_dir.join("info").join("package.txt");
    let installed_size = if crate::lfs::exists_on_host(&package_txt_path) {
        let pkg_content = std::fs::read_to_string(&package_txt_path)?;
        pkg_content.lines()
            .find(|line| line.starts_with("installedSize: "))
            .and_then(|line| line.split(": ").nth(1))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
    } else {
        0
    };

    let actual_size = installed_size + file_block_overhead + dir_overhead + info_overhead;

    let prev_total = BATCH_TOTAL_ESTIMATE.fetch_sub(pkg_estimate, Ordering::Relaxed);
    let new_total = BATCH_TOTAL_ESTIMATE.fetch_add(actual_size, Ordering::Relaxed);

    log::trace!(
        "adjust_batch_estimate: pkgkey={}, files={}, dirs={}, links={}, actual={}, estimate={}. \
         Batch estimate: {} - {} + {} = {} (predicted_final_free: {})",
        pkgkey, actual_file_count, actual_dir_count, actual_link_count,
        crate::utils::format_size(actual_size),
        crate::utils::format_size(pkg_estimate),
        crate::utils::format_size(prev_total),
        crate::utils::format_size(pkg_estimate),
        crate::utils::format_size(actual_size),
        crate::utils::format_size(new_total + actual_size),
        crate::utils::format_size(get_predicted_final_free())
    );

    Ok(())
}

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
/// Skips packages already in store (plan.pkgs_in_store)
#[allow(dead_code)]
pub fn calculate_plan_sizes(plan: &mut InstallationPlan) -> Result<()> {
    let pkgs_in_store = &plan.pkgs_in_store;
    if !pkgs_in_store.is_empty() {
        log::debug!("calculate_plan_sizes: {} packages already in store, skipping size calculation", pkgs_in_store.len());
    }

    let mut total_download: u64 = 0;
    let mut total_install: u64 = 0;

    for pkgkey in plan.fresh_installs.iter().chain(plan.upgrades_new.iter()) {
        // Skip packages already in store
        if pkgs_in_store.contains(pkgkey) {
            continue;
        }

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
            total_space: 0,
            used_space: 0,
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
            log::trace!("get_filesystem_info: {} f_fsid={} computed_fsid={}",
                       mount_point.display(), statvfs_buf.f_fsid, info.fsid);
        }

        // Use f_frsize (fragment/block allocation size) for actual block size.
        // On macOS APFS, f_bsize is "optimal transfer size" (2MB) which is wrong for
        // block alignment calculations. f_frsize is the actual allocation unit.
        let frsize = statvfs_buf.f_frsize as u64;
        let blocks = statvfs_buf.f_blocks as u64;  // Total blocks
        let bavail = if (statvfs_buf.f_flag & libc::ST_RDONLY) != 0 {
            0
        } else {
            statvfs_buf.f_bavail as u64
        };

        // Calculate total and used space
        // f_blocks: total blocks
        // f_bavail: blocks available to non-root user
        // f_bfree: blocks free (including reserved for root)
        // Use frsize (fragment/block allocation size) for all calculations.
        // On macOS APFS, f_bsize is "optimal transfer size" (2MB) which is wrong.
        // f_frsize is the actual allocation unit (typically 4KB).
        let total_space = blocks * frsize;
        let free_space = bavail * frsize;
        // Used space: total - free to non-root
        // Note: This includes reserved blocks for root
        let used_space = total_space.saturating_sub(statvfs_buf.f_bfree as u64 * frsize);

        info.free_space = free_space;
        info.total_space = total_space;
        info.used_space = used_space;
        info.block_size = frsize; // Use frsize for allocation unit

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
            total_space: 0,
            used_space: 0,
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
                let bytes_per_cluster = sectors_per_cluster as u64 * bytes_per_sector as u64;
                info.free_space = bytes_per_cluster * free_clusters;
                info.total_space = bytes_per_cluster * total_clusters;
                info.used_space = info.total_space.saturating_sub(info.free_space);
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
            total_space: 0,
            used_space: 0,
            free_inodes: u64::MAX,
            block_size: 4096,
        }
    }
}

/// Helper function to check disk space with safety margin
/// Returns error if insufficient space available
/// Safety margin: 5% of required space or 100MB minimum, whichever is larger
pub fn check_space(fs_info: &FilesystemInfo, required: u64, location: &Path) -> Result<()> {
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
    log::debug!("check_disk_space_for_plan: download_cache={} (fsid={}), store_root={} (fsid={}), download_same_fs={}",
               download_cache.display(), plan.download_cache_fs.fsid,
               store_root.display(), plan.store_root_fs.fsid,
               download_same_fs);

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
/// Also adds block alignment overhead to total_install for accurate space estimation
#[allow(dead_code)]
pub fn validate_before_linking(plan: &mut crate::plan::InstallationPlan) -> Result<()> {
    let (file_count, dir_count, link_count) = validate_file_conflicts(plan)?;
    // total_inodes_needed includes all filesystem entries (files + dirs + links)
    // Each entry needs one inode in the environment
    plan.total_inodes_needed = file_count + dir_count + link_count;

    let block_size = plan.store_root_fs.block_size.max(4096);

    // Add block alignment overhead for regular files only.
    // APK installedSize only counts file content, not filesystem overhead.
    // Each file wastes ~block_size * 0.75 on average due to block alignment.
    // Using 0.75 instead of 0.5 to be more conservative and avoid underestimation.
    // Note: Symlinks are NOT counted here - they don't occupy data blocks.
    let file_block_overhead = file_count * block_size * 3 / 4;
    plan.total_install += file_block_overhead;

    // Add directory overhead.
    // Each directory occupies at least one block for its metadata.
    let dir_overhead = dir_count * block_size;
    plan.total_install += dir_overhead;

    // Symlinks don't need data block overhead - they only occupy directory entries
    // which is negligible compared to file/directory overhead.

    log::trace!(
        "validate_before_linking: file_count={}, dir_count={}, link_count={}, block_size={}, file_block_overhead={}, dir_overhead={}, total_install before info={}",
        file_count, dir_count, link_count, block_size, file_block_overhead, dir_overhead, plan.total_install
    );

    // Add info/ directory overhead for each NEW package (not already in store).
    // The info/ directory contains:
    // - filelist.txt: one line per file/dir/link (~60-100 bytes each)
    // - package.txt: ~1 KB
    // - Other metadata: ~1-5 KB
    //
    // For packages with many files (e.g., breeze-icons with 40K entries),
    // filelist.txt alone can be several MB. Estimate based on entry count.
    let pkgs_in_store = &plan.pkgs_in_store;
    let new_pkg_count = plan.batch.new_pkgkeys.iter()
        .filter(|pkgkey| !pkgs_in_store.contains(*pkgkey))
        .count() as u64;

    // Base overhead: package.txt and other small files (~2 KB)
    const INFO_BASE_OVERHEAD: u64 = 2 * 1024;
    // filelist.txt overhead: ~80 bytes per entry (path + type + metadata)
    const FILELIST_BYTES_PER_ENTRY: u64 = 80;

    // Calculate info overhead based on number of filesystem entries
    let total_entries = file_count + dir_count + link_count;
    let info_overhead = new_pkg_count * INFO_BASE_OVERHEAD
        + total_entries.saturating_mul(FILELIST_BYTES_PER_ENTRY);
    plan.total_install += info_overhead;

    log::trace!(
        "validate_before_linking: new_pkg_count={}, total_entries={}, info_overhead={}, total_install={}",
        new_pkg_count, total_entries, info_overhead, plan.total_install
    );

    validate_inode_space(plan, plan.total_inodes_needed)?;
    Ok(())
}

/// Validate file conflicts for all packages before linking
/// Returns (file_count, dir_count, link_count) for disk space estimation
/// - file_count: regular files (need block alignment overhead)
/// - dir_count: directories (need 1 block each for metadata)
/// - link_count: symlinks (no data block overhead, only directory entry)
/// Skips packages that already exist in store (plan.pkgs_in_store)
#[allow(dead_code)]
pub fn validate_file_conflicts(
    plan: &mut crate::plan::InstallationPlan,
) -> color_eyre::Result<(u64, u64, u64)> {
    let store_root = &plan.store_root;

    // Build file map from installed packages (excluding those being removed or upgraded)
    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
    let mut file_map = build_installed_file_map(
        &installed,
        store_root,
        &plan.old_removes,
        &plan.upgrades_old,
    )?;
    drop(installed);

    // Packages already in store don't need new disk space
    let pkgs_in_store = &plan.pkgs_in_store;
    if !pkgs_in_store.is_empty() {
        log::debug!("validate_file_conflicts: {} packages already in store, skipping inode count", pkgs_in_store.len());
    }

    // Process each package
    // Track files, directories, and symlinks separately for disk space estimation
    let mut unique_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut unique_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut unique_links: std::collections::HashSet<String> = std::collections::HashSet::new();

    for pkgkey in plan.batch.new_pkgkeys.iter() {
        if let Some(package_info) = crate::plan::pkgkey2new_pkg_info(plan, pkgkey) {
            // Use file list with type info to distinguish files/dirs/links
            let file_infos = crate::package_cache::map_pkgline2filelist_with_info(store_root, &package_info.pkgline)?;

            for file_info in &file_infos {
                // Only count for packages NOT already in store
                if !pkgs_in_store.contains(pkgkey) {
                    if file_info.is_dir() {
                        unique_dirs.insert(file_info.path.clone());
                    } else if file_info.is_link() {
                        unique_links.insert(file_info.path.clone());
                    } else {
                        unique_files.insert(file_info.path.clone());
                    }
                }

                // Check for file conflicts (only for files and links, not directories)
                // Multiple packages can share the same directory
                if !file_info.is_dir() {
                    if let Some(existing_pkgkey) = file_map.insert(file_info.path.clone(), pkgkey.clone()) {
                        if plan.batch.new_pkgkeys.contains(&existing_pkgkey) {
                            log::warn!(
                                "Transaction file conflict: {} is provided by multiple packages: {} and {}",
                                file_info.path,
                                existing_pkgkey,
                                pkgkey
                            );
                        } else if !config().install.ignore_file_conflicts {
                            return Err(eyre!(
                                "File conflict: {} (from package {}) conflicts with installed file from package {}",
                                file_info.path,
                                pkgkey,
                                existing_pkgkey
                            ));
                        }
                    }
                }
            }
        }
    }

    // Count unique files, directories, and links for disk space estimation
    let file_count = unique_files.len() as u64;
    let dir_count = unique_dirs.len() as u64;
    let link_count = unique_links.len() as u64;

    plan.installed_file_map = Some(Arc::new(file_map));

    Ok((file_count, dir_count, link_count))
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

/// Compare estimated vs actual disk space usage and display the error.
/// Uses filesystem-level free_space change measurement (NOT file-by-file traverse).
///
/// IMPORTANT: File-by-file traversal is FORBIDDEN due to performance cost.
/// Must use filesystem-level df-style measurement via free_space delta.
///
/// Parameters:
/// - download_before: download cache filesystem info captured BEFORE installation
/// - store_before: store filesystem info captured BEFORE installation
/// - env_before: env filesystem info captured BEFORE installation
/// - estimated_download: the pre-installation download size estimate
/// - estimated_install: the pre-installation install size estimate (with block alignment overhead)
///
/// Calculates separate errors for download cache and store.
/// Store and env are typically on the same filesystem.
///
/// Note: filesystem-level measurement may be affected by other processes writing to
/// the same filesystem during installation. For short installations, this interference
/// is usually minimal and acceptable for estimation validation purposes.
#[cfg(unix)]
pub fn compare_disk_space_estimate(
    download_before: &FilesystemInfo,
    store_before: &FilesystemInfo,
    env_before: &FilesystemInfo,
    estimated_download: u64,
    estimated_install: u64,
) {
    // Check if download cache is on different filesystem from store
    let download_same_fs = download_before.fsid != 0
        && download_before.fsid == store_before.fsid;

    // Check if store and env are on the same filesystem
    let env_same_fs = store_before.fsid != 0
        && store_before.fsid == env_before.fsid;

    // Query after states
    let download_after = get_filesystem_info(&download_before.path);
    let store_after = get_filesystem_info(&store_before.path);

    // Calculate download cache delta
    let download_actual = download_before.free_space.saturating_sub(download_after.free_space);

    // Calculate store delta (env is typically on same fs as store)
    let store_actual = store_before.free_space.saturating_sub(store_after.free_space);

    // Report download cache error (only if download was needed and on different fs)
    if !download_same_fs && estimated_download > 0 && download_actual > 0 {
        let diff = if estimated_download > download_actual {
            estimated_download.saturating_sub(download_actual)
        } else {
            download_actual.saturating_sub(estimated_download)
        };
        let sign = if estimated_download >= download_actual { "+" } else { "-" };
        let error_pct = format!("{}{:.1}%", sign, (diff as f64 / download_actual as f64) * 100.0);

        log::info!(
            "Download cache disk space: actual Δ {} (free: {} -> {}), estimated {}, error {}",
            crate::utils::format_size(download_actual),
            crate::utils::format_size(download_before.free_space),
            crate::utils::format_size(download_after.free_space),
            crate::utils::format_size(estimated_download),
            error_pct
        );
    }

    // Report store/env error
    // Use combined label when store and env are on the same filesystem
    let label = if env_same_fs { "Store/Env" } else { "Store" };

    // When download cache is on the same filesystem as store, the actual delta
    // includes BOTH downloaded files AND installed files. Add estimated_download
    // to estimated_install for accurate comparison.
    let estimated_total = if download_same_fs {
        estimated_download + estimated_install
    } else {
        estimated_install
    };

    log::trace!(
        "compare_disk_space_estimate: download_same_fs={}, estimated_download={}, estimated_install={}, estimated_total={}, store_actual={}",
        download_same_fs, estimated_download, estimated_install, estimated_total, store_actual
    );

    // Store/Env disk space comparison is not shown to user because:
    // - Concurrent download+unpack means packages already occupy most disk space
    //   when info/filelist.txt is obtained, making file-count-based adjustment meaningless
    // - Store data integrity is now checked in unpack_mv_package_with_format()
    // Keep the calculation for internal debugging only
    log::trace!(
        "compare_disk_space_estimate: store_actual={}, estimated_total={}, label={}",
        store_actual, estimated_total, label
    );

    if store_actual > 0 && estimated_total > 0 {
        let diff = if estimated_total > store_actual {
            estimated_total.saturating_sub(store_actual)
        } else {
            store_actual.saturating_sub(estimated_total)
        };
        let sign = if estimated_total >= store_actual { "+" } else { "-" };
        let error_pct = format!("{}{:.1}%", sign, (diff as f64 / store_actual as f64) * 100.0);
        log::trace!(
            "{} disk space: actual Δ {} (free: {} -> {}), estimated {}, error {}",
            label,
            crate::utils::format_size(store_actual),
            crate::utils::format_size(store_before.free_space),
            crate::utils::format_size(store_after.free_space),
            crate::utils::format_size(estimated_total),
            error_pct
        );
    } else if estimated_total > 0 && store_actual == 0 {
        log::trace!(
            "{} disk space: actual Δ {} (hardlink reuse), estimated {} (over-estimated)",
            label,
            crate::utils::format_size(store_actual),
            crate::utils::format_size(estimated_total)
        );
    }

    // Report env delta if on different filesystem
    if !env_same_fs {
        let env_after = get_filesystem_info(&env_before.path);
        let env_actual = env_before.free_space.saturating_sub(env_after.free_space);

        if env_actual > 0 {
            log::info!(
                "Env disk space: actual Δ {} (free: {} -> {})",
                crate::utils::format_size(env_actual),
                crate::utils::format_size(env_before.free_space),
                crate::utils::format_size(env_after.free_space)
            );
        }
    }
}
