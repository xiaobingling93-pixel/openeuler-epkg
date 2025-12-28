//! Transaction management module for RPM-compatible package transactions
//!
//! This module implements transaction handling similar to RPM's transaction.cc,
//! including disk space checking, file conflict detection, config file handling,
//! and problem reporting.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use color_eyre::Result;
use color_eyre::eyre::{WrapErr, eyre};
use crate::models::InstalledPackageInfo;

/// File state types (matching RPM's RPMFILE_STATE_*)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    Normal,             // RPMFILE_STATE_NORMAL
    #[allow(dead_code)] // Replaced state - tracked but not actively used in current implementation
    Replaced,           // RPMFILE_STATE_REPLACED
    NotInstalled,       // RPMFILE_STATE_NOTINSTALLED
    #[allow(dead_code)] // ELF 32/64-bit conflict - not applicable to epkg's architecture
    WrongColor,         // RPMFILE_STATE_WRONGCOLOR (ELF 32/64-bit conflict resolved)
}

/// File action types (matching RPM's FA_*)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    #[allow(dead_code)] // Unknown action - placeholder for future use
    Unknown,            // FA_UNKNOWN
    Create,             // FA_CREATE
    #[allow(dead_code)] // Erase action - used during removal, not currently tracked in transaction
    Erase,              // FA_ERASE
    Skip,               // FA_SKIP
    Backup,             // FA_BACKUP
    AltName,            // FA_ALTNAME (for NOREPLACE configs - creates .rpmnew file)
    #[allow(dead_code)] // Save action - for config file handling during removal (future enhancement)
    Save,               // FA_SAVE (save modified config before removal)
    #[allow(dead_code)] // minimize_writes optimization - not currently used in epkg
    Touch,              // FA_TOUCH (minimize_writes optimization)
    #[allow(dead_code)] // ELF color conflicts - not applicable to epkg
    SkipColor,          // FA_SKIPCOLOR
    #[allow(dead_code)] // Network shared files - not applicable to epkg
    SkipNetShared,      // FA_SKIPNETSHARED
    #[allow(dead_code)] // Install policy - not currently used in epkg
    SkipNState,         // FA_SKIPNSTATE (install policy)
}

impl FileAction {
    /// Check if action is a skipping action
    #[allow(dead_code)] // Useful utility method for future use
    pub fn is_skipping(&self) -> bool {
        matches!(
            self,
            FileAction::Skip
                | FileAction::SkipColor
                | FileAction::SkipNetShared
                | FileAction::SkipNState
        )
    }
}

/// Problem types (matching RPM's RPMPROB_*)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProblemType {
    DiskSpace,          // RPMPROB_DISKSPACE
    DiskNodes,          // RPMPROB_DISKNODES
    FileConflict,       // RPMPROB_FILE_CONFLICT
    NewFileConflict,    // RPMPROB_NEW_FILE_CONFLICT
    #[allow(dead_code)] // Old package detection - not currently implemented
    OldPackage,         // RPMPROB_OLDPACKAGE
    #[allow(dead_code)] // Architecture validation - not currently implemented
    BadArch,            // RPMPROB_BADARCH
    #[allow(dead_code)] // OS validation - not currently implemented
    BadOs,              // RPMPROB_BADOS
    #[allow(dead_code)] // Package already installed - not currently implemented
    PkgInstalled,       // RPMPROB_PKG_INSTALLED
    #[allow(dead_code)] // Package verification - not currently implemented
    Verify,             // RPMPROB_VERIFY
}

/// Problem reported during transaction
#[derive(Debug, Clone)]
pub struct Problem {
    pub problem_type: ProblemType,
    pub pkgkey:        String,
    pub alt_pkgkey:    Option<String>, // For conflicts, the conflicting package
    pub file_path:     Option<String>, // For file-related problems
    pub mount_point:   Option<String>, // For disk space problems
    pub amount:        Option<u64>,    // For disk space (bytes) or inodes
    pub message:       Option<String>,
}

/// Disk space information for a device (matching RPM's diskspaceInfo)
#[derive(Debug, Clone)]
pub struct DiskSpaceInfo {
    #[allow(dead_code)]             // Device ID - kept for debugging but not actively used
    pub dev:        u64,            // Device ID
    pub bsize:      u64,            // Block size
    pub bneeded:    i64,            // Blocks needed
    pub bavail:     i64,            // Blocks available
    pub bdelta:     i64,            // Temporary -> final delta
    pub ineeded:    i64,            // Inodes needed
    pub iavail:     i64,            // Inodes available (-1 if filesystem has no inodes)
    pub idelta:     i64,            // Inode delta
    pub mnt_point:  PathBuf,        // Mount point
    #[allow(dead_code)]             // Rotational detection - not currently used in epkg
    pub rotational: Option<bool>,   // Whether device is rotational (None = not checked)
    pub obneeded:   i64,            // Old bneeded (for problem reporting)
    pub oineeded:   i64,            // Old ineeded (for problem reporting)
}

impl DiskSpaceInfo {
    /// Create new disk space info
    pub fn new(dev: u64, bsize: u64, bavail: i64, iavail: i64, mnt_point: PathBuf) -> Self {
        Self {
            dev,
            bsize,
            bneeded: 0,
            bavail,
            bdelta: 0,
            ineeded: 0,
            iavail,
            idelta: 0,
            mnt_point,
            rotational: None,
            obneeded: 0,
            oineeded: 0,
        }
    }
}

/// File fingerprint for conflict detection
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileFingerprint {
    pub dev:      u64,
    pub dir:      PathBuf,
    pub filename: String,
    pub size:     u64,
    pub mode:     u32,
    pub mtime:    Option<u64>,
    pub sha256:   Option<String>, // Optional for faster comparison
}

/// File information for transaction
/// Reserved for future file state management and detailed transaction tracking
#[derive(Debug, Clone)]
#[allow(dead_code)] // Reserved for future file state management enhancement
pub struct TransactionFile {
    pub path:           PathBuf,
    pub fingerprint:    FileFingerprint,
    pub action:         FileAction,
    pub state:          FileState,
    pub replaced_size:  u64,            // Size of file being replaced
    pub fixup_size:     u64,            // Size of overlapped file in transaction
    pub is_config:      bool,           // RPMFILE_CONFIG flag
    pub is_noreplace:   bool,           // RPMFILE_NOREPLACE flag
    pub hardlink_index: Option<usize>,  // Index in hardlink set (None if not hardlinked)
    pub hardlink_count: usize,          // Total number of hardlinks
}

/// Transaction context
pub struct Transaction {
    pub dsi:                HashMap<u64, DiskSpaceInfo>,                     // Device -> disk space info
    pub problems:           Vec<Problem>,
    pub file_fingerprints:  HashMap<FileFingerprint, Vec<(String, usize)>>,  // fingerprint -> (pkgkey, file_index)
    pub installed_files:    HashMap<PathBuf, (String, FileState)>,           // path -> (pkgkey, state)
    #[allow(dead_code)] // Transaction files tracking - reserved for future file state management
    pub transaction_files:  HashMap<String, Vec<TransactionFile>>,           // pkgkey -> files
    pub filter_flags:       ProblemFilterFlags,
    #[allow(dead_code)] // minimize_writes optimization - not currently used in epkg
    pub minimize_writes:    bool,
}

/// Problem filter flags (matching RPM's RPMPROB_FILTER_*)
#[derive(Debug, Clone, Copy, Default)]
pub struct ProblemFilterFlags {
    pub ignore_arch:       bool,
    pub ignore_os:         bool,
    pub replace_pkg:       bool,
    #[allow(dead_code)] // Force relocate - not applicable to epkg's architecture
    pub force_relocate:    bool,
    pub replace_new_files: bool,
    pub replace_old_files: bool,
    pub old_package:       bool,
    pub diskspace:         bool,
    pub disknodes:         bool,
    pub verify:            bool,
}

impl Transaction {
    /// Create a new transaction
    pub fn new(filter_flags: ProblemFilterFlags) -> Self {
        Self {
            dsi: HashMap::new(),
            problems: Vec::new(),
            file_fingerprints: HashMap::new(),
            installed_files: HashMap::new(),
            transaction_files: HashMap::new(),
            filter_flags,
            minimize_writes: false,
        }
    }

    /// Initialize disk space info
    pub fn init_dsi(&mut self) {
        self.dsi.clear();
    }

    /// Get or create disk space info for a device
    pub fn get_dsi(&mut self, dev: u64, dir_name: &Path) -> Result<Option<&mut DiskSpaceInfo>> {
        // Check if it already exists
        if self.dsi.contains_key(&dev) {
            return Ok(self.dsi.get_mut(&dev));
        }

        // Create new DSI
        let stat = fs::metadata(dir_name)
            .wrap_err_with(|| format!("Failed to stat {}", dir_name.display()))?;

        if stat.dev() != dev {
            return Ok(None);
        }

        #[cfg(target_os = "linux")]
        let (bsize, bavail, iavail) = {
            use std::ffi::CString;
            let c_path = CString::new(dir_name.as_os_str().as_bytes())
                .map_err(|e| eyre!("Invalid path: {}", e))?;

            let mut statvfs_buf: libc::statvfs = unsafe { std::mem::zeroed() };
            let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut statvfs_buf) };

            if rc != 0 {
                return Err(eyre!("statvfs failed for {}", dir_name.display()));
            }

            let bsize = statvfs_buf.f_bsize as u64;
            let bavail = if (statvfs_buf.f_flag & libc::ST_RDONLY) != 0 {
                0
            } else {
                statvfs_buf.f_bavail as i64
            };

            // Handle filesystems without inodes (FAT, etc.)
            let iavail = if statvfs_buf.f_ffree == 0 && statvfs_buf.f_files == 0 {
                -1
            } else {
                statvfs_buf.f_ffree as i64
            };

            (bsize, bavail, iavail)
        };

        #[cfg(not(target_os = "linux"))]
        let (bsize, bavail, iavail) = {
            // Fallback for non-Linux: use 4096 block size, no availability info
            (4096, -1, -1)
        };

        let bsize = if bsize == 0 { 512 } else { bsize };

        // Normalize block size to 4096 if too big
        let (bsize, bavail) = if bsize > 4096 {
            let old_size = bavail as u64 * bsize;
            log::debug!("Normalizing blocksize {} on {} to 4096", bsize, dir_name.display());
            (4096, (old_size / 4096) as i64)
        } else {
            (bsize, bavail)
        };

        let mnt_point = get_mount_point(dir_name, dev)?;

        let dsi = DiskSpaceInfo::new(dev, bsize, bavail, iavail, mnt_point);
        self.dsi.insert(dev, dsi);

        Ok(self.dsi.get_mut(&dev))
    }

    /// Update disk space info for a file action
    pub fn update_dsi(
        &mut self,
        dev: u64,
        dir_name: &Path,
        file_size: u64,
        prev_size: u64,
        fixup_size: u64,
        action: FileAction,
    ) -> Result<()> {
        let dsi = match self.get_dsi(dev, dir_name)? {
            Some(dsi) => dsi,
            None => return Ok(()),
        };

        let bneeded = block_round(file_size, dsi.bsize) as i64;

        match action {
            FileAction::Backup | FileAction::Save | FileAction::AltName => {
                dsi.ineeded += 1;
                dsi.bneeded += bneeded;
            }
            FileAction::Create => {
                dsi.bneeded += bneeded;
                dsi.ineeded += 1;
                if prev_size > 0 {
                    dsi.bdelta += block_round(prev_size.saturating_sub(1), dsi.bsize) as i64;
                    dsi.idelta += 1;
                }
                if fixup_size > 0 {
                    dsi.bdelta += block_round(fixup_size.saturating_sub(1), dsi.bsize) as i64;
                    dsi.idelta += 1;
                }
            }
            FileAction::Erase => {
                dsi.ineeded -= 1;
                dsi.bneeded -= bneeded;
            }
            _ => {}
        }

        // Adjust bookkeeping when requirements shrink
        if dsi.bneeded < dsi.obneeded {
            dsi.obneeded = dsi.bneeded;
        }
        if dsi.ineeded < dsi.oineeded {
            dsi.oineeded = dsi.ineeded;
        }

        Ok(())
    }

    /// Check disk space problems for a package
    pub fn check_dsi_problems(&mut self, pkgkey: &str) {
        // Adjust for temporary -> final disk consumption
        for dsi in self.dsi.values_mut() {
            dsi.bneeded -= dsi.bdelta;
            dsi.bdelta = 0;
            dsi.ineeded -= dsi.idelta;
            dsi.idelta = 0;
        }

        if self.filter_flags.diskspace && self.filter_flags.disknodes {
            return; // Filtered out
        }

        // Collect problems first to avoid borrow conflicts
        let mut problems = Vec::new();

        for dsi in self.dsi.values() {
            // Check block space (with 5% reserved space adjustment)
            if dsi.bavail >= 0 && !self.filter_flags.diskspace {
                let adj_needed = adj_fs_blocks(dsi.bneeded);
                if adj_needed > dsi.bavail {
                    if dsi.bneeded > dsi.obneeded {
                        let shortage = (adj_needed - dsi.bavail) * dsi.bsize as i64;
                        problems.push(Problem {
                            problem_type: ProblemType::DiskSpace,
                            pkgkey: pkgkey.to_string(),
                            alt_pkgkey: None,
                            file_path: None,
                            mount_point: Some(dsi.mnt_point.display().to_string()),
                            amount: Some(shortage as u64),
                            message: Some(format!(
                                "Insufficient disk space on {}: need {} bytes",
                                dsi.mnt_point.display(),
                                shortage
                            )),
                        });
                    }
                }
            }

            // Check inode space (with 5% reserved space adjustment)
            if dsi.iavail >= 0 && !self.filter_flags.disknodes {
                let adj_needed = adj_fs_blocks(dsi.ineeded);
                if adj_needed > dsi.iavail {
                    if dsi.ineeded > dsi.oineeded {
                        let shortage = adj_needed - dsi.iavail;
                        problems.push(Problem {
                            problem_type: ProblemType::DiskNodes,
                            pkgkey: pkgkey.to_string(),
                            alt_pkgkey: None,
                            file_path: None,
                            mount_point: Some(dsi.mnt_point.display().to_string()),
                            amount: Some(shortage as u64),
                            message: Some(format!(
                                "Insufficient inodes on {}: need {} inodes",
                                dsi.mnt_point.display(),
                                shortage
                            )),
                        });
                    }
                }
            }
        }

        // Add all collected problems
        for problem in problems {
            self.add_problem(problem);
        }
    }

    /// Add a problem to the transaction
    pub fn add_problem(&mut self, problem: Problem) {
        self.problems.push(problem);
    }

    /// Get all problems
    #[allow(dead_code)] // Useful for debugging and future enhancements
    pub fn get_problems(&self) -> &[Problem] {
        &self.problems
    }

    /// Check if transaction has unfiltered problems
    #[allow(dead_code)] // Useful for debugging and future enhancements
    pub fn has_problems(&self) -> bool {
        !self.problems.is_empty()
    }

    /// Load installed files from installed packages
    /// This populates the installed_files map for conflict detection
    pub fn load_installed_files(
        &mut self,
        packages: &HashMap<String, InstalledPackageInfo>,
        store_root: &Path,
        env_root: &Path,
    ) -> Result<()> {
        self.installed_files.clear();

        for (pkgkey, pkg_info) in packages {
            // Read filelist.txt from package store (at store_root/pkgline/info/filelist.txt)
            let filelist_path = store_root
                .join(&pkg_info.pkgline)
                .join("info")
                .join("filelist.txt");

            if !filelist_path.exists() {
                log::debug!("Filelist not found for package {}: {}", pkgkey, filelist_path.display());
                continue;
            }

            // Read and parse filelist (mtree format: path type=file mode=...)
            if let Ok(content) = fs::read_to_string(&filelist_path) {
                for line in content.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }

                    // Parse mtree format: path [type=file] [mode=...] [sha256=...]
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.is_empty() {
                        continue;
                    }

                    // First part is the relative path
                    let relative_path = parts[0];
                    let file_path = PathBuf::from(relative_path);
                    let env_file_path = env_root.join(&file_path);

                    // Skip directories - we only track files for conflict detection
                    let is_dir = parts.iter().any(|p| p.starts_with("type=dir"));
                    if is_dir {
                        continue;
                    }

                    // Check if file actually exists in environment
                    let state = if env_file_path.exists() {
                        FileState::Normal
                    } else {
                        FileState::NotInstalled
                    };

                    self.installed_files.insert(file_path, (pkgkey.clone(), state));
                }
            }
        }

        Ok(())
    }

    /// Check for conflicts with installed files and within transaction
    /// Returns list of conflicts found
    pub fn check_file_conflicts(
        &mut self,
        file_path: &Path,
        pkgkey: &str,
        file_index: usize,
        store_file: &Path,
    ) -> Result<Vec<Problem>> {
        let mut conflicts = Vec::new();

        // Check conflict with installed files
        if let Some((installed_pkgkey, state)) = self.installed_files.get(file_path) {
            if *state != FileState::NotInstalled && installed_pkgkey != pkgkey {
                // Check if files are identical (if so, no real conflict)
                let conflict_exists = if store_file.exists() && file_path.exists() {
                    match (create_file_fingerprint(store_file), create_file_fingerprint(file_path)) {
                        (Ok(fp1), Ok(fp2)) => !files_identical(&fp1, &fp2),
                        _ => true, // If we can't compare, assume conflict
                    }
                } else {
                    true
                };

                if conflict_exists {
                    conflicts.push(Problem {
                        problem_type: ProblemType::FileConflict,
                        pkgkey: pkgkey.to_string(),
                        alt_pkgkey: Some(installed_pkgkey.clone()),
                        file_path: Some(file_path.display().to_string()),
                        mount_point: None,
                        amount: None,
                        message: Some(format!(
                            "File {} is already provided by package {}",
                            file_path.display(),
                            installed_pkgkey
                        )),
                    });
                }
            }
        }

        // Check conflict within transaction
        if let Ok(Some(transaction_conflicts)) = check_transaction_file_conflicts(
            self,
            file_path,
            pkgkey,
            file_index,
        ) {
            for (conflict_pkgkey, _) in transaction_conflicts {
                conflicts.push(Problem {
                    problem_type: ProblemType::NewFileConflict,
                    pkgkey: pkgkey.to_string(),
                    alt_pkgkey: Some(conflict_pkgkey.clone()),
                    file_path: Some(file_path.display().to_string()),
                    mount_point: None,
                    amount: None,
                    message: Some(format!(
                        "File {} is provided by multiple packages in transaction: {} and {}",
                        file_path.display(),
                        pkgkey,
                        conflict_pkgkey
                    )),
                });
            }
        }

        // Add file to transaction fingerprint cache
        add_file_to_transaction(self, file_path, pkgkey, file_index)?;

        Ok(conflicts)
    }
}

/// Adjust for filesystem reserved space (5% on ext2/3/4)
fn adj_fs_blocks(blocks: i64) -> i64 {
    ((blocks * 21) / 20).max(0)
}

/// Round size to blocks
fn block_round(size: u64, block_size: u64) -> u64 {
    if block_size == 0 {
        return size;
    }
    (size + block_size - 1) / block_size
}

/// Get mount point for a directory
fn get_mount_point(dir_name: &Path, dev: u64) -> Result<PathBuf> {
    let mut current = dir_name.canonicalize()
        .unwrap_or_else(|_| dir_name.to_path_buf());

    loop {
        let parent = current.parent();
        if parent.is_none() || parent == Some(Path::new("/")) {
            // Check if we're at root or different device
            let stat = fs::metadata(&current)?;
            if stat.dev() != dev {
                return Ok(current);
            }
            return Ok(PathBuf::from("/"));
        }

        let parent_path = parent.unwrap();
        let parent_stat = fs::metadata(parent_path)?;
        if parent_stat.dev() != dev {
            return Ok(current);
        }

        current = parent_path.to_path_buf();
    }
}

/// Create file fingerprint from path
pub fn create_file_fingerprint(path: &Path) -> Result<FileFingerprint> {
    let metadata = fs::symlink_metadata(path)?;
    let dev = metadata.dev();

    let dir = path.parent()
        .unwrap_or_else(|| Path::new("/"))
        .to_path_buf();

    let filename = path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.display().to_string());

    let size = metadata.len();
    let mode = metadata.mode();
    let mtime = metadata.mtime();

    // Optionally compute SHA256 for more accurate comparison
    let sha256 = if size > 0 && size < 1024 * 1024 {
        // Only compute for small files
        crate::store::calculate_file_sha256(path).ok()
    } else {
        None
    };

    Ok(FileFingerprint {
        dev,
        dir,
        filename,
        size,
        mode,
        mtime: Some(mtime as u64),
        sha256,
    })
}

/// Check if two files are identical (for conflict detection)
pub fn files_identical(fp1: &FileFingerprint, fp2: &FileFingerprint) -> bool {
    // Quick checks first
    if fp1.size != fp2.size {
        return false;
    }

    if fp1.mode != fp2.mode {
        return false;
    }

    // Use SHA256 if available
    if let (Some(h1), Some(h2)) = (&fp1.sha256, &fp2.sha256) {
        return h1 == h2;
    }

    // Fallback to size and mode comparison
    true
}

/// Handle config file disposition (matching RPM's rpmfilesDecideFate)
pub fn decide_config_fate(
    existing_path: &Path,
    new_path: &Path,
    is_noreplace: bool,
    skip_missing: bool,
) -> FileAction {
    // Check if existing file exists
    if !existing_path.exists() {
        if skip_missing {
            return FileAction::Skip;
        }
        return FileAction::Create;
    }

    // Check if files are identical
    if let (Ok(fp1), Ok(fp2)) = (
        create_file_fingerprint(existing_path),
        create_file_fingerprint(new_path),
    ) {
        if files_identical(&fp1, &fp2) {
            if is_noreplace {
                return FileAction::Skip;
            }
            return FileAction::Create; // Overwrite identical file
        }
    }

    // Files differ - handle based on flags
    if is_noreplace {
        FileAction::AltName // Create .rpmnew file
    } else {
        FileAction::Backup // Backup existing, then create new
    }
}

/// Check if file is last in hardlink set (for disk space accounting)
/// TODO: Integrate hardlink accounting in disk space calculations
#[allow(dead_code)] // Reserved for future hardlink accounting enhancement
pub fn is_last_hardlink(hardlink_index: Option<usize>, hardlink_count: usize) -> bool {
    match hardlink_index {
        Some(idx) => idx == hardlink_count - 1,
        None => true, // Not hardlinked, so it's the "last" (only) one
    }
}

/// Get file size accounting for hardlinks (only count last file)
/// TODO: Integrate hardlink accounting in disk space calculations
#[allow(dead_code)] // Reserved for future hardlink accounting enhancement
pub fn get_accounted_file_size(
    file_size: u64,
    hardlink_index: Option<usize>,
    hardlink_count: usize,
) -> u64 {
    if is_last_hardlink(hardlink_index, hardlink_count) {
        file_size
    } else {
        0 // Don't count non-last hardlinks
    }
}

/// Handle removal conflict (directory -> file, symlink -> directory)
/// Returns true if conflict exists
/// This is used during file installation to detect type conflicts
/// TODO: Integrate into mirror_regular_file for better conflict detection
#[allow(dead_code)] // Reserved for future conflict detection enhancement
pub fn handle_removal_conflict(
    new_file_type: &str,
    existing_file_type: &str,
    path: &Path,
) -> Result<bool> {
    // Check if existing is directory and new is not
    if existing_file_type == "dir" && new_file_type != "dir" {
        // Check if directory still exists on disk
        if path.exists() {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.file_type().is_dir() {
                return Ok(true); // Conflict: can't change directory to non-directory
            }
        }
    }

    // Check if existing is symlink and new is directory
    if existing_file_type == "link" && new_file_type == "dir" {
        if path.exists() {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.file_type().is_dir() {
                return Ok(true); // Conflict: symlink points to directory, can't change
            }
        }
    }

    Ok(false) // No conflict
}

/// Determine if a file path is a config file (in /etc/)
pub fn is_config_file_path(file_path: &Path) -> bool {
    file_path.to_string_lossy().starts_with("etc/") ||
    file_path.to_string_lossy().starts_with("/etc/")
}

/// Get config file action based on existing file and flags
/// This is a simplified version that works with epkg's current architecture
pub fn get_config_file_action(
    existing_path: &Path,
    new_path: &Path,
    is_noreplace: bool,
) -> FileAction {
    decide_config_fate(existing_path, new_path, is_noreplace, false)
}

/// Check for file conflicts with installed packages
/// NOTE: This function is superseded by Transaction::check_file_conflicts()
/// which provides a more integrated approach
#[allow(dead_code)] // Kept for backwards compatibility, but use Transaction::check_file_conflicts instead
pub fn check_installed_file_conflicts(
    _transaction: &mut Transaction,
    file_path: &Path,
    pkgkey: &str,
    installed_files: &HashMap<PathBuf, (String, FileState)>,
) -> Result<Option<(String, FileState)>> {
    if let Some((installed_pkgkey, state)) = installed_files.get(file_path) {
        // Skip if file is not installed
        if *state == FileState::NotInstalled {
            return Ok(None);
        }

        // Skip if it's the same package (reinstall)
        if installed_pkgkey == pkgkey {
            return Ok(None);
        }

        // Check if files are identical
        let _existing_fp = create_file_fingerprint(file_path)?;
        // For new file, we'd need to get fingerprint from package, but for now
        // we'll report the conflict

        return Ok(Some((installed_pkgkey.clone(), *state)));
    }

    Ok(None)
}

/// Check for file conflicts within transaction (overlapped files)
pub fn check_transaction_file_conflicts(
    transaction: &mut Transaction,
    file_path: &Path,
    pkgkey: &str,
    file_index: usize,
) -> Result<Option<Vec<(String, usize)>>> {
    let fingerprint = create_file_fingerprint(file_path)?;

    if let Some(conflicting_files) = transaction.file_fingerprints.get(&fingerprint) {
        // Filter out same package/file
        let conflicts: Vec<_> = conflicting_files
            .iter()
            .filter(|(pk, idx)| *pk != pkgkey || *idx != file_index)
            .cloned()
            .collect();

        if !conflicts.is_empty() {
            return Ok(Some(conflicts));
        }
    }

    Ok(None)
}

/// Add file to transaction fingerprint cache
pub fn add_file_to_transaction(
    transaction: &mut Transaction,
    file_path: &Path,
    pkgkey: &str,
    file_index: usize,
) -> Result<()> {
    let fingerprint = create_file_fingerprint(file_path)?;
    transaction
        .file_fingerprints
        .entry(fingerprint)
        .or_insert_with(Vec::new)
        .push((pkgkey.to_string(), file_index));
    Ok(())
}

/// Prepare transaction: initialize DSI and load installed files
pub fn prepare_transaction(
    transaction: &mut Transaction,
    store_root: &Path,
    env_root: &Path,
    packages: &HashMap<String, InstalledPackageInfo>,
) -> Result<()> {
    transaction.init_dsi();

    // Initialize DSI for store and env roots
    let store_meta = fs::metadata(store_root)?;
    let env_meta = fs::metadata(env_root)?;

    transaction.get_dsi(store_meta.dev(), store_root)?;
    if store_meta.dev() != env_meta.dev() {
        transaction.get_dsi(env_meta.dev(), env_root)?;
    }

    // Load installed files from installed packages
    transaction.load_installed_files(packages, store_root, env_root)?;

    Ok(())
}

/// Validate transaction before execution
pub fn validate_transaction(transaction: &Transaction) -> Result<Vec<Problem>> {
    let mut critical_problems = Vec::new();

    for problem in &transaction.problems {
        // Check if problem is critical (not filtered)
        let is_critical = match problem.problem_type {
            ProblemType::DiskSpace       => !transaction.filter_flags.diskspace,
            ProblemType::DiskNodes       => !transaction.filter_flags.disknodes,
            ProblemType::FileConflict    => !transaction.filter_flags.replace_old_files,
            ProblemType::NewFileConflict => !transaction.filter_flags.replace_new_files,
            ProblemType::BadArch         => !transaction.filter_flags.ignore_arch,
            ProblemType::BadOs           => !transaction.filter_flags.ignore_os,
            ProblemType::OldPackage      => !transaction.filter_flags.old_package,
            ProblemType::PkgInstalled    => !transaction.filter_flags.replace_pkg,
            ProblemType::Verify          => !transaction.filter_flags.verify,
        };

        if is_critical {
            critical_problems.push(problem.clone());
        }
    }

    Ok(critical_problems)
}

/// Format problem for display
pub fn format_problem(problem: &Problem) -> String {
    match &problem.problem_type {
        ProblemType::DiskSpace => {
            format!(
                "{}: Insufficient disk space on {}: need {} bytes",
                problem.pkgkey,
                problem.mount_point.as_deref().unwrap_or("unknown"),
                problem.amount.unwrap_or(0)
            )
        }
        ProblemType::DiskNodes => {
            format!(
                "{}: Insufficient inodes on {}: need {} inodes",
                problem.pkgkey,
                problem.mount_point.as_deref().unwrap_or("unknown"),
                problem.amount.unwrap_or(0)
            )
        }
        ProblemType::FileConflict => {
            format!(
                "{}: File conflict: {} (from {})",
                problem.pkgkey,
                problem.file_path.as_deref().unwrap_or("unknown"),
                problem.alt_pkgkey.as_deref().unwrap_or("unknown")
            )
        }
        ProblemType::NewFileConflict => {
            format!(
                "{}: New file conflict: {} (with {})",
                problem.pkgkey,
                problem.file_path.as_deref().unwrap_or("unknown"),
                problem.alt_pkgkey.as_deref().unwrap_or("unknown")
            )
        }
        _ => {
            problem
                .message
                .clone()
                .unwrap_or_else(|| format!("{:?}", problem.problem_type))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adj_fs_blocks() {
        assert_eq!(adj_fs_blocks(100), 105); // 100 * 21 / 20 = 105
        assert_eq!(adj_fs_blocks(0), 0);
    }

    #[test]
    fn test_block_round() {
        assert_eq!(block_round(1000, 512), 2);
        assert_eq!(block_round(512, 512), 1);
        assert_eq!(block_round(513, 512), 2);
    }

    #[test]
    fn test_is_last_hardlink() {
        assert!(is_last_hardlink(Some(2), 3)); // Index 2 of 3 (0,1,2)
        assert!(!is_last_hardlink(Some(0), 3)); // Index 0 of 3
        assert!(is_last_hardlink(None, 1)); // Not hardlinked
    }

    #[test]
    fn test_get_accounted_file_size() {
        assert_eq!(get_accounted_file_size(1000, Some(2), 3), 1000); // Last hardlink
        assert_eq!(get_accounted_file_size(1000, Some(0), 3), 0); // Not last
        assert_eq!(get_accounted_file_size(1000, None, 1), 1000); // Not hardlinked
    }
}

