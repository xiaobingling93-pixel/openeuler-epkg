// src/rpm_verify.rs
// Only compile this module in debug builds for verification purposes
#![cfg(debug_assertions)]

use std::collections::HashMap;
use std::fs::{self, File, Metadata as StdMetadata};
use std::io::{BufReader, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use color_eyre::eyre::{eyre, WrapErr};
use color_eyre::Result;
use log;
use tempfile::TempDir;
use walkdir::{DirEntry, WalkDir};

// Assuming utils::find_command_in_paths exists and is accessible
// If utils is a module in src, then crate::utils should work.
// If it's a submodule of rpm_pkg, this might need adjustment after moving.
// For now, assuming it's generally available via crate::utils
use crate::utils;

#[derive(Debug, PartialEq, Eq, Clone)]
pub(crate) enum ComparisonMismatchDetail {
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
    MtimeMismatch { path: PathBuf, official_mtime: i64, epkg_mtime: i64 },
    DevMismatch { path: PathBuf, official_dev: u64, epkg_dev: u64 },
    // Device file specific fields
    RdevMismatch { path: PathBuf, official_rdev: u64, epkg_rdev: u64 },
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub(crate) struct ComparisonResult {
    pub(crate) are_identical: bool,
    pub(crate) mismatches: Vec<ComparisonMismatchDetail>,
}

fn are_files_equal(path1: &Path, path2: &Path) -> Result<bool> {
    let f1 = File::open(path1).wrap_err_with(|| format!("Failed to open file for comparison: {}", path1.display()))?;
    let f2 = File::open(path2).wrap_err_with(|| format!("Failed to open file for comparison: {}", path2.display()))?;

    let meta1: StdMetadata = f1.metadata().wrap_err_with(|| format!("Failed to get metadata for: {}", path1.display()))?;
    let meta2: StdMetadata = f2.metadata().wrap_err_with(|| format!("Failed to get metadata for: {}", path2.display()))?;

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
        let n1 = reader1.read(&mut buf1).wrap_err_with(|| format!("Failed to read from: {}", path1.display()))?;
        let n2 = reader2.read(&mut buf2).wrap_err_with(|| format!("Failed to read from: {}", path2.display()))?;

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

pub(crate) fn compare_directories(official_dir: &Path, epkg_dir: &Path) -> Result<ComparisonResult> {
    fn get_metadata_or_log(p: &Path, _path_for_log: &PathBuf) -> Option<StdMetadata> {
        match fs::symlink_metadata(p) {
            Ok(meta) => Some(meta),
            Err(e) => {
                log::warn!("Failed to get metadata for {}: {}. Skipping some checks for this entry.", p.display(), e);
                None
            }
        }
    }

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
                let official_meta_opt = get_metadata_or_log(official_entry.path(), path);
                let epkg_meta_opt = get_metadata_or_log(epkg_entry.path(), path);

                if let (Some(official_meta), Some(epkg_meta)) = (official_meta_opt.as_ref(), epkg_meta_opt.as_ref()) {
                    // Compare file types using metadata instead of DirEntry file_type for better accuracy
                    let official_type = get_file_type_from_metadata(official_meta);
                    let epkg_type = get_file_type_from_metadata(epkg_meta);

                    if official_type != epkg_type {
                        mismatches.push(ComparisonMismatchDetail::TypeMismatch {
                            path: path.clone(),
                            official_type,
                            epkg_type,
                        });
                    }

                    // Compare all metadata fields
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
                    if ((official_mode & libc::S_IFMT) == libc::S_IFBLK || (official_mode & libc::S_IFMT) == libc::S_IFCHR) &&
                       ((epkg_mode & libc::S_IFMT) == libc::S_IFBLK || (epkg_mode & libc::S_IFMT) == libc::S_IFCHR) {
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
                        let official_target = fs::read_link(official_entry.path()).wrap_err_with(|| format!("Failed to read link: {}", official_entry.path().display()))?;
                        let epkg_target = fs::read_link(epkg_entry.path()).wrap_err_with(|| format!("Failed to read link: {}", epkg_entry.path().display()))?;
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

                    if official_type.is_dir() != epkg_type.is_dir() ||
                       official_type.is_file() != epkg_type.is_file() ||
                       official_type.is_symlink() != epkg_type.is_symlink() {
                        mismatches.push(ComparisonMismatchDetail::TypeMismatch {
                            path: path.clone(),
                            official_type: get_entry_type_as_string(official_entry),
                            epkg_type: get_entry_type_as_string(epkg_entry),
                        });
                    }
                }
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

pub(crate) fn verify_rpm_extraction(rpm_file_path: &Path, epkg_extracted_fs_dir: &Path) -> Result<()> {
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

    if !utils::command_exists("rpm2cpio") {
        log::warn!("Verification skipped: 'rpm2cpio' command not found in PATH.");
        return Ok(());
    }
    if !utils::command_exists("cpio") {
        log::warn!("Verification skipped: 'cpio' command not found in PATH.");
        return Ok(());
    }
    log::debug!("rpm2cpio and cpio found.");

    let official_outdir_temp = TempDir::new_in(epkg_extracted_fs_dir.parent().unwrap_or_else(|| Path::new("/tmp")))
        .wrap_err("Failed to create temporary directory for official RPM extraction")?;
    let official_outdir_path = official_outdir_temp.path().to_path_buf();

    log::debug!("Official extraction directory: {}", official_outdir_path.display());

    let mut rpm2cpio_cmd = Command::new("rpm2cpio")
        .arg(rpm_file_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .wrap_err_with(|| format!("Failed to spawn rpm2cpio for {}", rpm_file_path.display()))?;

    let rpm2cpio_stdout = rpm2cpio_cmd.stdout.take().ok_or_else(|| eyre!("Failed to capture stdout from rpm2cpio"))?;

    let cpio_cmd_output = Command::new("cpio")
        .arg("-idm")
        .arg("--no-absolute-filenames")
        .stdin(rpm2cpio_stdout)
        .current_dir(&official_outdir_path)
        .output() // Use output() to get status, stdout, and stderr
        .wrap_err("Failed to execute cpio command")?;

    // Wait for rpm2cpio to finish and get its output (especially stderr)
    let rpm2cpio_output = rpm2cpio_cmd.wait_with_output().wrap_err("Failed to wait for rpm2cpio process")?;

    if !cpio_cmd_output.status.success() {
        let cpio_stderr_str = String::from_utf8_lossy(&cpio_cmd_output.stderr);
        log::error!("cpio command failed with status: {}. Stderr:\n{}", cpio_cmd_output.status, cpio_stderr_str);
        if !rpm2cpio_output.status.success() {
            let rpm2cpio_stderr_str = String::from_utf8_lossy(&rpm2cpio_output.stderr);
            log::error!("rpm2cpio also failed with status: {}. Stderr:\n{}", rpm2cpio_output.status, rpm2cpio_stderr_str);
        }
        // Preserve directory on failure
        let _persisted_dir = official_outdir_temp.keep();
        return Err(eyre!(
            "rpm2cpio | cpio pipeline failed. cpio exit: {}, rpm2cpio exit: {}. Official dir: {}",
            cpio_cmd_output.status, rpm2cpio_output.status, official_outdir_path.display()
        ));
    }

    if !rpm2cpio_output.status.success() {
        // cpio might succeed even if rpm2cpio had non-fatal errors (e.g., warnings to stderr)
        let rpm2cpio_stderr_str = String::from_utf8_lossy(&rpm2cpio_output.stderr);
        log::warn!(
            "rpm2cpio command finished with non-success status: {} (but cpio succeeded). Stderr:\n{}",
            rpm2cpio_output.status, rpm2cpio_stderr_str
        );
    }

    log::debug!("Official extraction via rpm2cpio and cpio completed.");
    log::info!("Comparing epkg extraction at {} with official extraction at {}", epkg_extracted_fs_dir.display(), official_outdir_path.display());

    match compare_directories(&official_outdir_path, epkg_extracted_fs_dir) {
        Ok(comp_result) => {
            if comp_result.are_identical {
                log::info!("Verification successful: epkg extraction matches official extraction for {}.", rpm_file_path.display());
                // Temp dir will be cleaned up automatically on drop
            } else {
                log::warn!("Verification FAILED for {}: Mismatches found between epkg and official extraction.", rpm_file_path.display());
                for mismatch in comp_result.mismatches {
                    log::warn!("  Mismatch: {:?}", mismatch);
                }
                log::warn!("The official extraction directory {} has been preserved for manual inspection.", official_outdir_path.display());
                let _persisted_dir = official_outdir_temp.keep(); // Persist the directory
            }
        }
        Err(e) => {
            log::error!("Error during directory comparison for {}: {}", rpm_file_path.display(), e);
            log::warn!("The official extraction directory {} might be incomplete or problematic. Preserving for inspection.", official_outdir_path.display());
            let _ = official_outdir_temp.keep(); // Persist the directory
            return Err(e.wrap_err("Directory comparison failed"));
        }
    }
    Ok(())
}
