// src/rpm_verify.rs
// Only compile this module in debug builds for verification purposes
// Also only available on Linux systems (uses Linux-specific metadata extensions)
#![cfg(all(target_os = "linux", debug_assertions))]

use std::collections::HashMap;
use std::fs::{self, File, Metadata as StdMetadata};
use std::io::{BufReader, Read};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use color_eyre::eyre::{eyre, WrapErr};
use color_eyre::Result;
use libc;
use log;
use walkdir::{DirEntry, WalkDir};

// Assuming utils::find_command_in_paths exists and is accessible
// If utils is a module in src, then crate::utils should work.
// If it's a submodule of rpm_pkg, this might need adjustment after moving.
// For now, assuming it's generally available via crate::utils
use crate::utils;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ComparisonMismatchDetail {
    MissingInOfficial(PathBuf),
    MissingInEpkg(PathBuf),
    TypeMismatch { path: PathBuf, official_type: String, epkg_type: String },
    ContentMismatch(PathBuf),
    SymlinkTargetMismatch { path: PathBuf, official_target: PathBuf, epkg_target: PathBuf },
    PermissionsMismatch { path: PathBuf, official_mode: u32, epkg_mode: u32 },
    OwnerMismatch { path: PathBuf, official_uid: u32, epkg_uid: u32 },
    GroupMismatch { path: PathBuf, official_gid: u32, epkg_gid: u32 },
    SizeMismatch { path: PathBuf, official_size: u64, epkg_size: u64 },
    // Extended stat comparison fields
    #[allow(dead_code)]
    MtimeMismatch { path: PathBuf, official_mtime: i64, epkg_mtime: i64 },
    DevMismatch { path: PathBuf, official_dev: u64, epkg_dev: u64 },
    // Device file specific fields
    RdevMismatch { path: PathBuf, official_rdev: u64, epkg_rdev: u64 },
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ComparisonResult {
    pub are_identical: bool,
    pub mismatches: Vec<ComparisonMismatchDetail>,
}

fn are_files_equal(path1: &Path, path2: &Path) -> Result<bool> {
    let f1 = match File::open(path1) {
        Ok(f) => f,
        Err(e) => {
            // Log the error but don't fail the entire comparison for permission issues
            log::warn!("Failed to open file for comparison: {} (error: {})", path1.display(), e);
            return Ok(false);
        }
    };
    let f2 = match File::open(path2) {
        Ok(f) => f,
        Err(e) => {
            // Log the error but don't fail the entire comparison for permission issues
            log::warn!("Failed to open file for comparison: {} (error: {})", path2.display(), e);
            return Ok(false);
        }
    };

    let meta1: StdMetadata = match f1.metadata() {
        Ok(m) => m,
        Err(e) => {
            log::warn!("Failed to get metadata for: {} (error: {})", path1.display(), e);
            return Ok(false);
        }
    };
    let meta2: StdMetadata = match f2.metadata() {
        Ok(m) => m,
        Err(e) => {
            log::warn!("Failed to get metadata for: {} (error: {})", path2.display(), e);
            return Ok(false);
        }
    };

    if meta1.len() != meta2.len() {
        return Ok(false);
    }

    // If files are empty and sizes match, they are equal.
    if meta1.len() == 0 {
        return Ok(true);
    }

    let mut reader1 = BufReader::new(f1);
    let mut reader2 = BufReader::new(f2);

    let mut buf1 = [0; 8192];
    let mut buf2 = [0; 8192];

    loop {
        let n1 = match reader1.read(&mut buf1) {
            Ok(n) => n,
            Err(e) => {
                log::warn!("Failed to read from: {} (error: {})", path1.display(), e);
                return Ok(false);
            }
        };
        let n2 = match reader2.read(&mut buf2) {
            Ok(n) => n,
            Err(e) => {
                log::warn!("Failed to read from: {} (error: {})", path2.display(), e);
                return Ok(false);
            }
        };

        if n1 == 0 && n2 == 0 { // Both EOF
            return Ok(true);
        }
        if n1 == 0 || n2 == 0 { // One EOF, other not (shouldn't happen if sizes are equal)
            return Ok(false);
        }
        if buf1[..n1] != buf2[..n2] {
            return Ok(false);
        }
    }
}

fn get_entry_type_as_string(entry: &DirEntry) -> String {
    let ft = entry.file_type();
    if ft.is_dir() { "directory".to_string() }
    else if ft.is_file() { "file".to_string() }
    else if ft.is_symlink() { "symlink".to_string() }
    else { "other".to_string() }
}

fn get_file_type_from_metadata(metadata: &StdMetadata) -> String {
    let mode = metadata.mode();

    if metadata.is_dir() {
        "directory".to_string()
    } else if metadata.is_file() {
        "file".to_string()
    } else if metadata.file_type().is_symlink() {
        "symlink".to_string()
    } else if (mode & libc::S_IFMT) == libc::S_IFBLK {
        "block_device".to_string()
    } else if (mode & libc::S_IFMT) == libc::S_IFCHR {
        "char_device".to_string()
    } else if (mode & libc::S_IFMT) == libc::S_IFIFO {
        "fifo".to_string()
    } else if (mode & libc::S_IFMT) == libc::S_IFSOCK {
        "socket".to_string()
    } else {
        "unknown".to_string()
    }
}

fn get_metadata_or_log(p: &Path, _path_for_log: &PathBuf) -> Option<StdMetadata> {
    match fs::symlink_metadata(p) {
        Ok(meta) => Some(meta),
        Err(e) => {
            log::warn!("Failed to get metadata for {}: {}. Skipping some checks for this entry.", p.display(), e);
            None
        }
    }
}

fn compare_one_path_pair(
    path: &PathBuf,
    official_entry: &DirEntry,
    epkg_entry: &DirEntry,
    mismatches: &mut Vec<ComparisonMismatchDetail>,
) -> Result<()> {
    let official_meta_opt = get_metadata_or_log(official_entry.path(), path);
    let epkg_meta_opt = get_metadata_or_log(epkg_entry.path(), path);

    if let (Some(official_meta), Some(epkg_meta)) = (official_meta_opt.as_ref(), epkg_meta_opt.as_ref()) {
        let official_type = get_file_type_from_metadata(official_meta);
        let epkg_type = get_file_type_from_metadata(epkg_meta);

        if official_type != epkg_type {
            mismatches.push(ComparisonMismatchDetail::TypeMismatch {
                path: path.clone(),
                official_type,
                epkg_type,
            });
        }

        if official_meta.mode() != epkg_meta.mode() {
            mismatches.push(ComparisonMismatchDetail::PermissionsMismatch {
                path: path.clone(),
                official_mode: official_meta.mode(),
                epkg_mode: epkg_meta.mode(),
            });
        }

        if official_meta.uid() != epkg_meta.uid() {
            mismatches.push(ComparisonMismatchDetail::OwnerMismatch {
                path: path.clone(),
                official_uid: official_meta.uid(),
                epkg_uid: epkg_meta.uid(),
            });
        }

        if official_meta.gid() != epkg_meta.gid() {
            mismatches.push(ComparisonMismatchDetail::GroupMismatch {
                path: path.clone(),
                official_gid: official_meta.gid(),
                epkg_gid: epkg_meta.gid(),
            });
        }

        if official_meta.len() != epkg_meta.len() {
            mismatches.push(ComparisonMismatchDetail::SizeMismatch {
                path: path.clone(),
                official_size: official_meta.len(),
                epkg_size: epkg_meta.len(),
            });
        }

        // Extended stat comparisons

        // if official_meta.mtime() != epkg_meta.mtime() {
        //     mismatches.push(ComparisonMismatchDetail::MtimeMismatch {
        //         path: path.clone(),
        //         official_mtime: official_meta.mtime(),
        //         epkg_mtime: epkg_meta.mtime(),
        //     });
        // }

        if official_meta.dev() != epkg_meta.dev() {
            mismatches.push(ComparisonMismatchDetail::DevMismatch {
                path: path.clone(),
                official_dev: official_meta.dev(),
                epkg_dev: epkg_meta.dev(),
            });
        }

        // For device files, compare rdev (device ID)
        let official_mode = official_meta.mode();
        let epkg_mode = epkg_meta.mode();
        if ((official_mode & libc::S_IFMT) == libc::S_IFBLK || (official_mode & libc::S_IFMT) == libc::S_IFCHR)
            && ((epkg_mode & libc::S_IFMT) == libc::S_IFBLK || (epkg_mode & libc::S_IFMT) == libc::S_IFCHR)
        {
            if official_meta.rdev() != epkg_meta.rdev() {
                mismatches.push(ComparisonMismatchDetail::RdevMismatch {
                    path: path.clone(),
                    official_rdev: official_meta.rdev(),
                    epkg_rdev: epkg_meta.rdev(),
                });
            }
        }

        // Content and symlink target comparison
        if official_meta.is_file() && epkg_meta.is_file() {
            // Only compare content if sizes match
            if official_meta.len() == epkg_meta.len() {
                if !are_files_equal(official_entry.path(), epkg_entry.path())? {
                    mismatches.push(ComparisonMismatchDetail::ContentMismatch(path.clone()));
                }
            }
        } else if official_meta.file_type().is_symlink() && epkg_meta.file_type().is_symlink() {
            let official_target = fs::read_link(official_entry.path())
                .wrap_err_with(|| format!("Failed to read link: {}", official_entry.path().display()))?;
            let epkg_target =
                fs::read_link(epkg_entry.path()).wrap_err_with(|| format!("Failed to read link: {}", epkg_entry.path().display()))?;
            if official_target != epkg_target {
                mismatches.push(ComparisonMismatchDetail::SymlinkTargetMismatch {
                    path: path.clone(),
                    official_target,
                    epkg_target,
                });
            }
        }
    } else {
        // Fallback comparison using DirEntry if metadata is not available
        let official_type = official_entry.file_type();
        let epkg_type = epkg_entry.file_type();

        if official_type.is_dir() != epkg_type.is_dir()
            || official_type.is_file() != epkg_type.is_file()
            || official_type.is_symlink() != epkg_type.is_symlink()
        {
            mismatches.push(ComparisonMismatchDetail::TypeMismatch {
                path: path.clone(),
                official_type: get_entry_type_as_string(official_entry),
                epkg_type: get_entry_type_as_string(epkg_entry),
            });
        }
    }
    Ok(())
}

pub fn compare_directories(official_dir: &Path, epkg_dir: &Path) -> Result<ComparisonResult> {
    let mut mismatches = Vec::new();
    let mut official_entries = HashMap::new();
    let mut epkg_entries = HashMap::new();

    for entry_result in WalkDir::new(official_dir).min_depth(1).sort_by_file_name() {
        let entry = entry_result.wrap_err_with(|| format!("Failed to walk official_dir: {}", official_dir.display()))?;
        let relative_path = entry.path().strip_prefix(official_dir).unwrap().to_path_buf();
        official_entries.insert(relative_path.clone(), entry);
    }

    for entry_result in WalkDir::new(epkg_dir).min_depth(1).sort_by_file_name() {
        let entry = entry_result.wrap_err_with(|| format!("Failed to walk epkg_dir: {}", epkg_dir.display()))?;
        let relative_path = entry.path().strip_prefix(epkg_dir).unwrap().to_path_buf();
        epkg_entries.insert(relative_path.clone(), entry);
    }

    for (path, official_entry) in &official_entries {
        match epkg_entries.get(path) {
            Some(epkg_entry) => {
                compare_one_path_pair(path, official_entry, epkg_entry, &mut mismatches)?;
            }
            None => {
                mismatches.push(ComparisonMismatchDetail::MissingInEpkg(path.clone()));
            }
        }
    }

    for (path, _epkg_entry) in &epkg_entries {
        if !official_entries.contains_key(path) {
            mismatches.push(ComparisonMismatchDetail::MissingInOfficial(path.clone()));
        }
    }

    Ok(ComparisonResult {
        are_identical: mismatches.is_empty(),
        mismatches,
    })
}

/// Filters out known false positive mismatches
fn filter_known_false_positives(mismatches: Vec<ComparisonMismatchDetail>, epkg_dir: &Path) -> Vec<ComparisonMismatchDetail> {
    mismatches.into_iter().filter(|mismatch| {
        match mismatch {
            // Ignore directory size mismatches - these are often artifacts of extraction differences
            ComparisonMismatchDetail::SizeMismatch { path, .. } => {
                !is_directory_from_disk(epkg_dir, path)
            },

            // Ignore permission mismatches caused by epkg's hardcoded permission modifications
            ComparisonMismatchDetail::PermissionsMismatch { official_mode, epkg_mode, .. } => {
                // Check if this looks like epkg's permission modification (0o750 for dirs, 0o640 for files)
                let official_base = official_mode & 0o777;
                let epkg_base = epkg_mode & 0o777;

                // Directory permission fix: epkg adds 0o750 minimum
                let is_dir_perm_fix = (epkg_base & 0o750) == 0o750 && official_base != epkg_base;
                // File permission fix: epkg adds 0o640 minimum
                let is_file_perm_fix = (epkg_base & 0o640) == 0o640 && official_base != epkg_base;

                !(is_dir_perm_fix || is_file_perm_fix)
            },

            // Ghost files are now properly skipped during extraction, so this workaround is no longer needed
            _ => true, // Keep all other mismatches
        }
    }).collect()
}

/// Check if a path is actually a directory by examining the file system
fn is_directory_from_disk(base_dir: &Path, relative_path: &Path) -> bool {
    let full_path = base_dir.join(relative_path);
    match fs::metadata(&full_path) {
        Ok(metadata) => metadata.is_dir(),
        Err(_) => false, // If we can't read metadata, assume it's not a directory
    }
}

/// Handles directory mismatch by renaming directories for debug investigation
/// Returns true if the temp directory should be kept, false otherwise
fn handle_directory_mismatch(
    epkg_extracted_fs_dir: &Path,
    official_outdir_path: &Path,
) -> Result<bool> {
    // Find files that aren't readable and make them readable for verification
    let output = Command::new("find")
        .arg(official_outdir_path)
        .arg("-type")
        .arg("f")
        .arg("!")
        .arg("-readable")
        .arg("-exec")
        .arg("chmod")
        .arg("u+rw")
        .arg("{}")
        .arg(";")
        .output()
        .wrap_err_with(|| format!("Failed to run find command on {}", official_outdir_path.display()))?;

    if !output.status.success() {
        log::warn!("Failed to set permissions on some files: {}", String::from_utf8_lossy(&output.stderr));
    }

    // 1. Rename epkg_extracted_fs_dir for debug investigations
    let debug_dir = epkg_extracted_fs_dir.with_extension("debug_epkg_extracted");
    if let Err(e) = fs::rename(epkg_extracted_fs_dir, &debug_dir) {
        log::error!("Failed to rename epkg_extracted_fs_dir to debug directory {}: {}", debug_dir.display(), e);
    } else {
        log::info!("Renamed epkg_extracted_fs_dir to {} for debug investigations", debug_dir.display());
    }

    // 2. Rename official_outdir_path to epkg_extracted_fs_dir to use the good rpm2archive output
    if let Err(e) = fs::rename(official_outdir_path, epkg_extracted_fs_dir) {
        log::error!("Failed to rename official extraction directory {} to {}: {}",
                   official_outdir_path.display(), epkg_extracted_fs_dir.display(), e);
        log::warn!("The official extraction directory {} has been preserved for manual inspection.", official_outdir_path.display());
        Ok(true) // Keep the temp directory since rename failed
    } else {
        log::info!("Renamed official extraction directory {} to {} to use the good rpm2archive output",
                  official_outdir_path.display(), epkg_extracted_fs_dir.display());
        Ok(false) // Don't keep the temp directory since we've moved it successfully
    }
}

pub fn verify_rpm_extraction(rpm_file_path: &Path, epkg_extracted_fs_dir: &Path) -> Result<()> {
    log::debug!("Starting RPM extraction verification for: {}", rpm_file_path.display());

    // if !utils::is_running_as_root() {
    //     log::debug!("Verification skipped: Not running as root.");
    //     return Ok(());
    // }
    // log::debug!("Root check passed.");

    match std::env::var("RUST_LOG") {
        Ok(val) if val.to_lowercase().contains("debug") => {
            log::debug!("RUST_LOG contains 'debug' check passed.");
        }
        _ => {
            log::debug!("Verification skipped: RUST_LOG does not contain 'debug' (case-insensitive).");
            return Ok(());
        }
    }

    if !utils::command_exists("rpm2archive") {
        log::info!("Verification skipped: 'rpm2archive' command not found in PATH.");
        return Ok(());
    }
    if !utils::command_exists("tar") {
        log::info!("Verification skipped: 'tar' command not found in PATH.");
        return Ok(());
    }
    log::debug!("rpm2archive and tar found.");

    let official_outdir_path = epkg_extracted_fs_dir.parent()
        .ok_or_else(|| eyre!("Failed to get parent directory for epkg_extracted_fs_dir: {}", epkg_extracted_fs_dir.display()))?
        .join("rpm2archive");
    std::fs::create_dir_all(&official_outdir_path)
        .wrap_err_with(|| format!("Failed to create directory for official RPM extraction: {}", official_outdir_path.display()))?;

    log::debug!("Official extraction directory: {}", official_outdir_path.display());

    let mut rpm2archive_cmd = Command::new("rpm2archive")
        .arg(rpm_file_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .wrap_err_with(|| format!("Failed to spawn rpm2archive for {}", rpm_file_path.display()))?;

    let rpm2archive_stdout = rpm2archive_cmd.stdout.take().ok_or_else(|| eyre!("Failed to capture stdout from rpm2archive"))?;

    let tar_cmd = Command::new("tar")
        .arg("-xzf")
        .arg("-")
        .arg("-C")
        .arg(&official_outdir_path)
        .stdin(rpm2archive_stdout)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .wrap_err("Failed to execute tar command")?;

    // Wait for tar to finish first
    let tar_output = tar_cmd.wait_with_output().wrap_err("Failed to wait for tar process")?;

    // Then wait for rpm2archive to finish
    let rpm2archive_output = rpm2archive_cmd.wait_with_output().wrap_err("Failed to wait for rpm2archive process")?;

    if !tar_output.status.success() {
        let tar_stderr_str = String::from_utf8_lossy(&tar_output.stderr);
        log::error!("tar command failed with status: {}. Stderr:\n{}", tar_output.status, tar_stderr_str);
        if !rpm2archive_output.status.success() {
            let rpm2archive_stderr_str = String::from_utf8_lossy(&rpm2archive_output.stderr);
            log::error!("rpm2archive also failed with status: {}. Stderr:\n{}", rpm2archive_output.status, rpm2archive_stderr_str);
        }
        // Preserve directory on failure
        return Err(eyre!(
            "rpm2archive | tar pipeline failed. tar exit: {}, rpm2archive exit: {}. Official dir: {}",
            tar_output.status, rpm2archive_output.status, official_outdir_path.display()
        ));
    }

    if !rpm2archive_output.status.success() {
        // tar might succeed even if rpm2archive had non-fatal errors (e.g., warnings to stderr)
        let rpm2archive_stderr_str = String::from_utf8_lossy(&rpm2archive_output.stderr);

        // Check if it's a SIGPIPE error, which is expected when tar finishes reading
        if rpm2archive_output.status.signal() == Some(13) { // SIGPIPE = 13
            log::debug!(
                "rpm2archive received SIGPIPE (signal 13) - this is normal when tar finishes reading. Stderr:\n{}",
                rpm2archive_stderr_str
            );
        } else if rpm2archive_stderr_str.contains("Write error") {
            // This is expected when tar finishes reading before rpm2archive finishes writing
            log::debug!(
                "rpm2archive write error - this is normal when tar finishes reading first. Stderr:\n{}",
                rpm2archive_stderr_str
            );
        } else {
            log::warn!(
                "rpm2archive command finished with non-success status: {} (but tar succeeded). Stderr:\n{}",
                rpm2archive_output.status, rpm2archive_stderr_str
            );
        }
    }

    log::debug!("Official extraction via rpm2archive and tar completed.");
    log::info!("Comparing epkg extraction at {} with official extraction at {}", epkg_extracted_fs_dir.display(), official_outdir_path.display());

    match compare_directories(&official_outdir_path, epkg_extracted_fs_dir) {
        Ok(comp_result) => {
            // Filter out known false positives
            let filtered_mismatches = filter_known_false_positives(comp_result.mismatches, epkg_extracted_fs_dir);
            let are_identical_after_filtering = filtered_mismatches.is_empty();

            if are_identical_after_filtering {
                log::info!("Verification successful: epkg extraction matches official extraction for {}.", rpm_file_path.display());
                log::debug!("Removing successfully verified official extraction directory: {}", official_outdir_path.display());

                // Fix directory permissions before removal to avoid "Permission denied" errors
                let output = Command::new("find")
                    .arg(&official_outdir_path)
                    .arg("-type")
                    .arg("d")
                    .arg("-exec")
                    .arg("chmod")
                    .arg("u+rwx")
                    .arg("{}")
                    .arg(";")
                    .output()
                    .wrap_err_with(|| format!("Failed to run find command on {}", official_outdir_path.display()))?;

                if !output.status.success() {
                    log::warn!("Failed to set permissions on some files: {}", String::from_utf8_lossy(&output.stderr));
                }

                if let Err(e) = fs::remove_dir_all(&official_outdir_path) {
                    log::warn!("Failed to remove official extraction directory {}: {}. Manual cleanup may be required.", official_outdir_path.display(), e);
                }
            } else {
                log::warn!("Verification FAILED for {}: Mismatches found between epkg and official extraction.", rpm_file_path.display());
                for mismatch in filtered_mismatches {
                    log::warn!("  Mismatch: {:?}", mismatch);
                }

                // Handle directory mismatch as requested
                match handle_directory_mismatch(epkg_extracted_fs_dir, &official_outdir_path) {
                    Ok(_should_keep_temp) => {
                    }
                    Err(e) => {
                        log::error!("Error handling directory mismatch: {}", e);
                    }
                }
            }
        }
        Err(e) => {
            log::error!("Error during directory comparison for {}: {}", rpm_file_path.display(), e);
            log::warn!("The official extraction directory {} might be incomplete or problematic. Preserving for inspection.", official_outdir_path.display());
            return Err(e.wrap_err("Directory comparison failed"));
        }
    }
    Ok(())
}
