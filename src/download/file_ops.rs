// ============================================================================
// DOWNLOAD FILE OPS - File System Operations and Metadata Handling
//
// This module handles all file system operations for the download system,
// including partial file management, PID file coordination for process safety,
// metadata storage (.etag.json files), and chunk file operations.
//
// Key Features:
// - Partial download file management (.part files)
// - Process coordination via PID files
// - Download metadata persistence and validation
// - Chunk file creation, validation, and cleanup
// - File integrity checking and size validation
// - Atomic file operations for safety
// ============================================================================

use color_eyre::eyre::{eyre, Result, WrapErr};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::symlink;
use std::io::Seek;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use time::OffsetDateTime;
use super::types::*;
use super::chunk::{collect_and_sort_chunks, validate_chunks, adjust_and_create_chunks};
use super::mirror::{validate_mirror_metadata, fetch_server_metadata};
use super::utils::{map_io_error, send_chunk_to_channel};
use super::validation::parse_http_date;
use crate::utils;
use crate::config;
use crate::dirs;

/// Get the size of an existing partial file, or 0 if it doesn't exist
pub(crate) fn get_existing_file_size(part_path: &Path) -> Result<u64> {
    if part_path.exists() {
        log::debug!("download_file part file exists, getting metadata for {}", part_path.display());
        match fs::metadata(part_path) {
            Ok(metadata) => {
                let size = metadata.len();
                log::debug!("download_file found existing part file with {} bytes: {}", size, part_path.display());
                Ok(size)
            },
            Err(e) => {
                log::error!("download_file failed to get metadata for part file {}: {}", part_path.display(), e);
                Err(DownloadError::DiskError {
                    details: format!("Failed to get metadata for part file {}: {}", part_path.display(), e)
                }.into())
            }
        }
    } else {
        log::debug!("download_file no existing part file found: {}", part_path.display());
        Ok(0)
    }
}


// ============================================================================
// PROCESS COORDINATION
// ============================================================================

/// Helper function to generate PID file path for a given final path
fn get_pid_file_path(final_path: &Path) -> PathBuf {
    utils::append_suffix(final_path, "download.pid")
}

/// Helper function to generate temporary PID file path for a given final path
fn get_temp_pid_file_path(final_path: &Path) -> PathBuf {
    utils::append_suffix(final_path, "download.pid.tmp")
}

/// Create a PID file for download coordination and clean up stale PID files
pub(crate) fn create_pid_file(final_path: &Path) -> Result<PathBuf> {
    let pid_file = get_pid_file_path(final_path);

    // Check for existing downloads and clean up stale PID files
    check_and_cleanup_existing_downloads(final_path)?;

    // Ensure the parent directory exists
    if let Some(parent) = pid_file.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directory for {}: {}", pid_file.display(), parent.display()))?;
    }

    let pid = std::process::id();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let pid_content = format!("epkg=1\npid={}\ntime={}\n", pid, timestamp);

    // Try to create the PID file atomically
    let temp_pid_file = get_temp_pid_file_path(final_path);
    fs::write(&temp_pid_file, pid_content)?;

    // Atomic rename
    fs::rename(&temp_pid_file, &pid_file)?;

    log::debug!("Created PID file: {}", pid_file.display());
    Ok(pid_file)
}

/// Check if a process is likely an epkg download process
fn is_epkg_process(pid: u32) -> bool {
    // First try to get executable path (symlink target) - handles symlinked binaries like 'wget' -> 'epkg'
    if let Some(exe) = utils::get_process_exe(pid) {
        if let Some(name) = Path::new(&exe).file_name().and_then(|n| n.to_str()) {
            if name.to_lowercase().contains("epkg") {
                return true;
            }
        }
    }

    // Then try process name (executable basename)
    if let Some(name) = utils::get_process_name(pid) {
        if name.to_lowercase().contains("epkg") {
            return true;
        }
    }

    // If we can't determine, assume it might be epkg for safety
    // (prevents deleting PID file of a live epkg process we can't inspect)
    true
}

/// Check if a PID file represents an active download
fn is_pid_file_active(pid_file: &Path) -> bool {
    if !pid_file.exists() {
        return false;
    }

    let content = match fs::read_to_string(pid_file) {
        Ok(content) => content,
        Err(_) => return false,
    };

    // Parse the new format: epkg=1\npid=123\ntime=456\n
    let mut pid_opt = None;
    let mut has_epkg_magic = false;

    for line in content.lines() {
        if let Some(value) = line.strip_prefix("epkg=") {
            if value == "1" {
                has_epkg_magic = true;
            }
        }
        if let Some(value) = line.strip_prefix("pid=") {
            pid_opt = value.parse::<u32>().ok();
            // Continue parsing to also check for epkg= line
        }
    }

    if has_epkg_magic {
        log::debug!("PID file {} has epkg magic", pid_file.display());
    }

    let pid = match pid_opt {
        Some(pid) => pid,
        None => return false,
    };

    // Get current process ID
    let current_pid = std::process::id();

    // Check if PID in file matches current process ID
    if pid == current_pid {
        return false;
    }

    // If not our PID, check if the process is still running (Unix-like systems)
    #[cfg(unix)]
    {
        if !utils::process_exists(pid) {
            return false;
        }
        // Process exists, check if it's an epkg process
        is_epkg_process(pid)
    }

    // For Windows or if we can't check, assume it's active for safety
    #[cfg(not(unix))]
    {
        true
    }
}

/// Clean up PID file after download completion
pub(crate) fn cleanup_pid_file(pid_file: &Path) -> Result<()> {
    if pid_file.exists() {
        fs::remove_file(pid_file)?;
        log::debug!("Cleaned up PID file: {}", pid_file.display());
    }
    Ok(())
}

/// Check for existing downloads and clean up stale PID files
fn check_and_cleanup_existing_downloads(final_path: &Path) -> Result<()> {
    let pid_file = get_pid_file_path(final_path);

    if pid_file.exists() {
        if is_pid_file_active(&pid_file) {
            return Err(eyre!("Another download process is already active for: {}", final_path.display()));
        } else {
            // Clean up stale PID file
            log::info!("Cleaning up stale PID file: {}", pid_file.display());
            cleanup_pid_file(&pid_file)?;
        }
    }

    Ok(())
}

/// Recover from crashed chunked downloads
fn find_parto_files(task: &DownloadTask) -> Result<Vec<PathBuf>> {
    let mut chunk_files = Vec::new();

    // Look for chunk files in the same directory as the main file
    if let Some(parent_dir) = task.chunk_path.parent() {
        if let Ok(entries) = std::fs::read_dir(parent_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(filename) = path.file_name() {
                    if let Some(filename_str) = filename.to_str() {
                        // Check if this is a chunk file for this download
                        if filename_str.starts_with(&format!("{}-O", task.chunk_path.file_name().unwrap().to_string_lossy())) {
                            chunk_files.push(path);
                        }
                    }
                }
            }
        }
    }

    Ok(chunk_files)
}

// Extract offset from filename like "file.part-O1048576"
pub(crate) fn extract_offset(path: &Path) -> u64 {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|s| s.split("-O").nth(1))
        .and_then(|offset_str| offset_str.parse().ok())
        .unwrap_or(0)
}

fn recover_chunks_for_parto_files(
    master_task: &DownloadTask,
    chunk_files: Vec<PathBuf>,
    expected_size: u64,
) -> Result<()> {
    if expected_size == 0 {
        return Ok(());
    }

    // 1. Collect and sort chunks
    let chunks = collect_and_sort_chunks(chunk_files)?;

    // 2. Validate chunks and master file
    let valid_chunks = validate_chunks(master_task, &chunks, expected_size)?;

    if valid_chunks.is_empty() {
        log::info!("No valid chunks found, starting fresh download");
        return Ok(());
    }

    // 3. Adjust chunks and create tasks
    adjust_and_create_chunks(master_task, valid_chunks, expected_size)
}

pub(crate) fn finalize_file(task: &DownloadTask) -> Result<()> {
    log::debug!("finalize_file starting for {} -> {}", task.chunk_path.display(), task.final_path.display());

    // Final progress update
    task.set_position(task.file_size.load(Ordering::Relaxed));

    // Check if any mirror has more recent metadata than the current download
    validate_mirror_metadata(task);

    // Check if the chunk file exists before attempting to rename
    if !task.chunk_path.exists() {
        return Err(eyre!("Chunk file does not exist: {}", task.chunk_path.display()));
    }

    // Validate that the completed download size matches the expected file size.
    // This prevents prematurely finalising a partially downloaded or oversized file.
    // Use the total file size (task.file_size) when known: after on-demand chunking
    // and retry we may have cleared chunk_tasks and task.chunk_size can still be the
    // parent range only; requiring the full file size avoids accepting a truncated file.
    let file_size = task.file_size.load(Ordering::Relaxed);
    let expected_size = if file_size > 0 {
        file_size
    } else {
        task.chunk_size.load(Ordering::Relaxed)
    };
    if expected_size > 0 {
        let actual_size = fs::metadata(&task.chunk_path)?.len();
        if actual_size != expected_size {
            log::error!(
                "finalize_file: size mismatch for {} – actual {} bytes, expected {} bytes",
                task.chunk_path.display(), actual_size, expected_size
            );
            return Err(eyre!(
                "Downloaded file size {} does not match expected {} for {}",
                actual_size, expected_size, task.chunk_path.display()
            ));
        }
    }

    // Check if the final path already exists and remove it if it does
    if task.final_path.exists() {
        log::debug!("Final path already exists, removing: {}", task.final_path.display());
        let meta = fs::metadata(&task.final_path)
            .with_context(|| format!("Failed to stat final path: {}", task.final_path.display()))?;
        if meta.is_dir() {
            return Err(eyre!(
                "Final path {} is a directory; cannot replace with a single file. \
                Remove the directory or use a different output path.",
                task.final_path.display()
            ));
        }
        fs::remove_file(&task.final_path)
            .with_context(|| format!("Failed to remove existing final file: {}", task.final_path.display()))?;
    }

    if let Ok(metadata_guard) = task.serving_metadata.lock() {
        if let Some(metadata) = &*metadata_guard {
            // Apply Last-Modified timestamp from serving_metadata
            if let Some(last_modified) = &metadata.last_modified {
                if let Ok(timestamp) = time::OffsetDateTime::parse(last_modified, &time::format_description::well_known::Rfc2822) {
                    let system_time = filetime::FileTime::from_system_time(timestamp.into());
                    if let Err(e) = filetime::set_file_mtime(&task.chunk_path, system_time) {
                        log::warn!("Failed to set mtime for {}: {}", task.chunk_path.display(), e);
                    }
                }
            }
        }
    }

    // Perform the atomic rename operation
    log::debug!("Renaming {} to {}", task.chunk_path.display(), task.final_path.display());
    fs::rename(&task.chunk_path, &task.final_path)
        .with_context(|| format!("Failed to rename chunk file {} to final file {}",
                                task.chunk_path.display(), task.final_path.display()))?;

    log::debug!("Successfully finalized file: {}", task.final_path.display());
    Ok(())
}

/// Check if a chunk task is already complete and handle early completion
fn check_chunk_completion(task: &DownloadTask, existing_bytes: u64) -> Result<bool> {
    let chunk_size = task.chunk_size.load(Ordering::Relaxed);

    // A chunk is considered complete only when the on-disk size exactly matches the
    // expected chunk size. "Bigger than expected" indicates corruption and must not
    // be silently accepted.
    if chunk_size > 0 && existing_bytes == chunk_size {
        if task.is_chunk_task() {
            log::debug!("Chunk file already exists and is complete: {}", task.chunk_path.display());
        } else {
            log::debug!("Master chunk already complete (local {} == expected {}) for {}", existing_bytes, chunk_size, task.url);
        }

        // Mark bytes as reused and status as completed
        task.resumed_bytes.store(chunk_size, Ordering::Relaxed);
        task.received_bytes.store(0, Ordering::Relaxed);
        return Ok(true);
    }

    // Detect oversized files eagerly so they can be redownloaded instead of propagated
    if chunk_size > 0 && existing_bytes > chunk_size {
        log::error!(
            "Existing chunk file {} is larger than expected ({} > {}) – treating as corruption",
            task.chunk_path.display(), existing_bytes, chunk_size
        );
        // Cleanup corrupted chunk file immediately, so that the next retry starts with a pristine
        // chunk file and does not pick up invalid bytes that could cause persistent size mismatches.
        if task.chunk_path.exists() {
            match fs::remove_file(&task.chunk_path) {
                Ok(_) => log::debug!(
                    "check_chunk_completion: removed corrupt chunk file {} after size check",
                    task.chunk_path.display()
                ),
                Err(e) => log::warn!(
                    "check_chunk_completion: failed to remove corrupt chunk file {}: {}",
                    task.chunk_path.display(),
                    e
                ),
            }
            // Reset progress counters so resumed/received math is correct on retry
            task.resumed_bytes.store(0, Ordering::Relaxed);
            task.received_bytes.store(0, Ordering::Relaxed);
        }
        return Err(eyre!(
            "Corrupted chunk file: size {} exceeds expected {} for {}",
            existing_bytes, chunk_size, task.chunk_path.display()
        ));
    }
    Ok(false) // Task is not complete
}

/// Log download completion statistics

/// Try to symlink from global shared cache if we're in private mode and local file doesn't exist.
fn try_symlink_from_global_cache(task: &DownloadTask) -> bool {
    // Only check global cache when not using shared store
    if config().init.shared_store {
        return false;
    }

    let local_path = &task.final_path;
    if local_path.exists() {
        return false;
    }

    // Compute global shared cache root: /opt/epkg/cache/downloads
    let global_cache_root = dirs().opt_epkg.join("cache/downloads");
    let local_cache_root = dirs().epkg_downloads_cache.clone();

    // Get relative path from local cache root
    let relative_path = match local_path.strip_prefix(&local_cache_root) {
        Ok(rel) => rel,
        Err(_) => {
            log::debug!("Local path {} is not under cache root {}", local_path.display(), local_cache_root.display());
            return false;
        }
    };

    // Build global path
    let global_path = global_cache_root.join(relative_path);
    if !global_path.exists() {
        return false;
    }

    // Create parent directory for symlink if needed
    if let Some(parent) = local_path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            log::warn!("Failed to create parent directory for symlink {}: {}", local_path.display(), e);
            return false;
        }
    }

    // Create symlink from local path to global path
    match symlink(&global_path, local_path) {
        Ok(_) => log::debug!("Symlinked {} -> {}", local_path.display(), global_path.display()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            // File already exists (maybe created by another process)
            log::debug!("File already exists at {}, skipping symlink", local_path.display());
            return false;
        }
        Err(e) => {
            log::warn!("Failed to symlink {} -> {}: {}", local_path.display(), global_path.display(), e);
            return false;
        }
    }

    // Also symlink .etag.json file if it exists
    let global_meta_path = utils::append_suffix(&global_path, "etag.json");
    let local_meta_path = utils::append_suffix(local_path, "etag.json");
    if global_meta_path.exists() && !local_meta_path.exists() {
        match symlink(&global_meta_path, &local_meta_path) {
            Ok(_) => log::debug!("Symlinked metadata {} -> {}", local_meta_path.display(), global_meta_path.display()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                log::debug!("Metadata file already exists at {}, skipping symlink", local_meta_path.display());
            }
            Err(e) => {
                log::warn!("Failed to symlink metadata {} -> {}: {}", local_meta_path.display(), global_meta_path.display(), e);
            }
        }
    }

    true
}

/// Clean cache decision logic replacing complex nested conditionals
pub(crate) fn should_redownload(
    task: &DownloadTask,
    server_metadata: &ServerMetadata
) -> Result<CacheDecision> {
    // Try to symlink from global shared cache if local file doesn't exist
    try_symlink_from_global_cache(task);

    let local_path = &task.final_path;
    if !local_path.exists() {
        return Ok(CacheDecision::RedownloadDueTo { reason: "Local file doesn't exist".to_string() });
    }

    // Get local file metadata
    let local_metadata = map_io_error(fs::metadata(local_path), "get local file metadata", local_path)?;
    let local_size = local_metadata.len();
    let local_last_modified_sys_time = local_metadata.modified()
        .map_err(|e| eyre!("Failed to get local file modification time: {}", e))?;
    let local_last_modified: OffsetDateTime = local_last_modified_sys_time.into();

    let remote_size_opt = server_metadata.remote_size;
    let remote_size = remote_size_opt.unwrap_or(0);

    // Detect 0-byte files early - always redownload them
    if local_size == 0 {
        log::warn!("Local file {} is 0 bytes - triggering redownload", local_path.display());
        return Ok(CacheDecision::RedownloadDueTo { reason: "Local file is 0 bytes".to_string() });
    }

    // For immutable files, we already know beforehand whether to UseCache/AppendDownload
    // So these are double checks serving as validation
    if task.file_type == FileType::Immutable ||
       task.file_type == FileType::AppendOnly {
        return check_immutable_file(task, local_size, remote_size_opt);
    }

    // For mutable files, check timestamps if available
    let remote_ts_opt = server_metadata.last_modified.as_ref()
        .and_then(|s| parse_http_date(s).ok())
        .map(|st| OffsetDateTime::from(st));

    match remote_ts_opt {
        Some(remote_ts) => {
            check_mutable_file_with_timestamp(
                task,
                server_metadata,
                local_size,
                local_last_modified,
                remote_size_opt,
                remote_size,
                remote_ts
            )
        }
        None => {
            check_mutable_file_without_timestamp(remote_size_opt, remote_size, local_size, local_last_modified)
        }
    }
}

fn check_immutable_file(
    task: &DownloadTask,
    local_size: u64,
    remote_size_opt: Option<u64>
) -> Result<CacheDecision> {
    if let Some(remote_size_val) = remote_size_opt {
        if local_size == remote_size_val {
            return Ok(CacheDecision::UseCache { reason: "Immutable file size matches".to_string() });
        }
        if local_size < remote_size_val {
            if task.file_type == FileType::AppendOnly {
                return Ok(CacheDecision::AppendDownload { reason: format!("Append immutable file: local_size {} < remote_size {}", local_size, remote_size_val) });
            } else {
                return Ok(CacheDecision::RedownloadDueTo { reason: format!("Corrupt immutable file: local_size {} < remote_size {}", local_size, remote_size_val) });
            }
        }

        // local_size > remote_size is a corruption case
        return Ok(CacheDecision::RedownloadDueTo { reason: format!("Corrupt immutable file: local_size {} > remote_size {}", local_size, remote_size_val) });
    } else {
        // Remote size unknown - can't validate, so redownload
        return Ok(CacheDecision::RedownloadDueTo { reason: "Remote size unknown, cannot validate immutable file".to_string() });
    }
}

fn check_mutable_file_with_timestamp(
    task: &DownloadTask,
    server_metadata: &ServerMetadata,
    local_size: u64,
    local_last_modified: OffsetDateTime,
    remote_size_opt: Option<u64>,
    remote_size: u64,
    remote_ts: OffsetDateTime
) -> Result<CacheDecision> {
    use std::time::Duration;

    // Case 1a) If local time is more recent than remote time, assume local file is newer
    if local_last_modified > remote_ts {
        return Ok(CacheDecision::UseCache {
            reason: format!("Local file is newer than remote (local: {}, remote: {})", local_last_modified, remote_ts)
        });
    }

    // Case 1b) Compare with saved etag.json, prevent timestamp going backwards
    if let Ok(Some(loaded_metadata)) = task.load_remote_metadata() {
        if let Some(ref serving_metadata) = loaded_metadata.serving_metadata {
            if server_metadata.timestamp < serving_metadata.timestamp {
                return Ok(CacheDecision::UseCache {
                    reason: format!(
                        "Prevent timestamp going backwards. Current response timestamp: {}, Loaded timestamp: {}",
                        server_metadata.timestamp,
                        serving_metadata.timestamp
                    )
                });
            }
        }
    }

    let time_diff = if local_last_modified > remote_ts {
        (local_last_modified - remote_ts).unsigned_abs()
    } else {
        (remote_ts - local_last_modified).unsigned_abs()
    };

    let size_matches = remote_size_opt.is_some() && remote_size == local_size;

    // Case 2) If timestamps are within 10 minutes of each other, consider them the same
    if size_matches && time_diff <= Duration::from_secs(600) {
        return Ok(CacheDecision::UseCache {
            reason: format!("Size and timestamp match within 10min tolerance (remote: {}, local: {})", remote_ts, local_last_modified)
        });
    }

    // Case 3) Otherwise, collect reasons for redownload
    let mut reasons = Vec::new();
    if let Some(remote_size_val) = remote_size_opt {
        if remote_size_val != local_size {
            reasons.push(format!("size mismatch: remote {}, local {}", remote_size_val, local_size));
        }
    } else {
        reasons.push("remote size unknown".to_string());
    }
    if time_diff > Duration::from_secs(600) {
        reasons.push(format!("timestamp mismatch (tolerance: 10min): remote {}, local {}", remote_ts, local_last_modified));
    }
    Ok(CacheDecision::RedownloadDueTo { reason: reasons.join(" and ") })
}

fn check_mutable_file_without_timestamp(
    remote_size_opt: Option<u64>,
    remote_size: u64,
    local_size: u64,
    local_last_modified: OffsetDateTime
) -> Result<CacheDecision> {
    use std::time::Duration;

    // Only use cache if we actually know the remote size and it matches
    // AND the local file was modified within the last day
    if remote_size_opt.is_some() && remote_size == local_size {
        let now = OffsetDateTime::now_utc();
        let time_since_modification = (now - local_last_modified).unsigned_abs();

        if time_since_modification <= Duration::from_secs(86400) {
            return Ok(CacheDecision::UseCache { reason: "Size matches, no timestamp available".to_string() });
        } else {
            return Ok(CacheDecision::RedownloadDueTo {
                reason: format!("Size matches but local file is older than 1 day ({} seconds old)", time_since_modification.as_secs())
            });
        }
    }

    // Remote size unknown or doesn't match - redownload
    if remote_size_opt.is_none() {
        Ok(CacheDecision::RedownloadDueTo {
            reason: "Remote size unknown and no timestamp available".to_string()
        })
    } else {
        Ok(CacheDecision::RedownloadDueTo {
            reason: format!("Size differs (remote {}, local {}) and no timestamp", remote_size, local_size)
        })
    }
}

/// Setup file for download content writing
pub(crate) fn setup_download_file(task: &DownloadTask, existing_bytes: u64) -> Result<File> {
    let chunk_path = &task.chunk_path;

    if existing_bytes == 0 {
        if let Some(parent) = chunk_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| DownloadError::DiskError {
                    details: format!("Failed to create directory '{}': {}", parent.display(), e)
                })?;
        }
    };

    let mut file = map_io_error(
        OpenOptions::new()
            .create(true)
            .write(true)
            .append(false)              // Never use O_APPEND to prevent race conditions
            .open(chunk_path),
        "open file",
        chunk_path
    ).map_err(|e| DownloadError::DiskError {
        details: format!("setup_download_file failed for chunk_path {}: {}", chunk_path.display(), e)
    })?;

    // If file exists and we need to append, seek to the end to prevent overwriting
    if existing_bytes > 0 {
        file.seek(std::io::SeekFrom::Start(existing_bytes))
            .map_err(|e| DownloadError::DiskError {
                details: format!("Failed to seek to end of file {}: {}", chunk_path.display(), e)
            })?;
    }

    Ok(file)
}

/// True if saved metadata has at least one of etag, last_modified/timestamp, or remote_size,
/// so we can validate that the remote content is still consistent with a local part file.
fn can_validate_resume_from_metadata(metadata: &Option<DownloadMetadata>) -> bool {
    metadata
        .as_ref()
        .and_then(|m| m.serving_metadata.as_ref())
        .map(|s| {
            s.etag.is_some()
                || (
                    s.last_modified.is_some()
                    && s.remote_size.is_some()
                )
        })
        .unwrap_or(false)
}

/// Check existing file size and validate chunk completion
/// Returns existing bytes and whether the chunk is already complete
pub(crate) fn check_existing_partfile(task: &DownloadTask) -> Result<(u64, bool)> {
    let chunk_path = &task.chunk_path;

    // Check existing file size for resumption
    let existing_bytes = get_existing_file_size(chunk_path)?;

    // For Mutable files we must not resume unless we can validate the remote is unchanged
    // (etag, last_modified, or remote_size). Otherwise we might append to stale data.
    if existing_bytes > 0
        && task.is_master_task()
        && task.file_type == FileType::Mutable
    {
        let loaded = task.load_remote_metadata().ok().flatten();
        if !can_validate_resume_from_metadata(&loaded) {
            log::info!(
                "Dropping existing part file {} ({} bytes): no etag/last_modified to validate resume for {}",
                chunk_path.display(),
                existing_bytes,
                &task.url
            );
            if chunk_path.exists() {
                fs::remove_file(chunk_path).with_context(|| {
                    format!("Failed to remove part file {} before fresh download", chunk_path.display())
                })?;
            }
            let is_complete = check_chunk_completion(task, 0)?;
            return Ok((0, is_complete));
        }
    }

    if existing_bytes > 0 {
        task.resumed_bytes.store(existing_bytes, Ordering::Relaxed);
        log::debug!("Resuming download from {} bytes for {}", existing_bytes, &task.url);

        // Only master task has data channel
        if let Some(channel) = task.get_data_channel() {
            log::debug!("Sending master task resumed data to channel for {}", task.chunk_path.display());
            send_chunk_to_channel(&task, &task.chunk_path, &channel)?;
        }
    }

    // Check if chunk task is already complete
    let is_complete = check_chunk_completion(task, existing_bytes)?;
    Ok((existing_bytes, is_complete))
}


// ===========================
// File Validation Logic
// ===========================

/// Validate existing final_file and determine appropriate download action
pub(crate) fn validate_existing_file(task: &DownloadTask) -> Result<ValidationResult> {
    let final_path = &task.final_path;
    let file_type = &task.file_type;
    let expected_size = task.file_size.load(Ordering::Relaxed);

    // Try to symlink from global shared cache if local file doesn't exist
    try_symlink_from_global_cache(task);

    // Early return if file doesn't exist
    if !final_path.exists(){
        return Ok(ValidationResult::StartFresh);
    }

    // Get local file metadata
    let local_metadata = match fs::metadata(final_path) {
        Ok(meta) => meta,
        Err(e) => {
            log::warn!("Failed to read local file metadata for {}: {}", final_path.display(), e);
            return Ok(ValidationResult::StartFresh);
        }
    };

    let local_size = local_metadata.len();

    match file_type {
        FileType::Immutable | FileType::AppendOnly => {
            // For immutable and append-only files, we can trust size-based validation
            validate_immutable_file(task, local_size, expected_size, file_type)
        },
        FileType::Mutable => {
            // For mutable files, we need to check server metadata
            // This will be handled by download_file_with_integrity() which gets server metadata first
            log::info!("Mutable file {} exists, will validate against server metadata",
                      final_path.display());
            // the SkipDownload case will be checked after being able to resolve mirror and make request
            Ok(ValidationResult::StartFresh)
        }
    }
}

/// Handle size-based validation for immutable and append-only files
fn validate_immutable_file(
    task: &DownloadTask,
    local_size: u64,
    expected_size: u64,
    file_type: &FileType,
) -> Result<ValidationResult> {
    let final_path = &task.final_path;

    match file_type {
        FileType::Immutable => {
            if local_size == expected_size {
                log::info!("Immutable file {} already exists with correct size {}, treating as already downloaded",
                          final_path.display(), local_size);

                return Ok(ValidationResult::SkipDownload("File exists with correct size".to_string()));
            } else if local_size > expected_size {
                log::warn!("Immutable file {} has larger size than expected ({} > {}), file may be corrupt",
                          final_path.display(), local_size, expected_size);
                return Ok(ValidationResult::CorruptionDetected);
            } else {
                // local_size < expected_size - can resume from partial
                log::info!("Immutable file {} exists but incomplete ({} < {}), will resume download",
                          final_path.display(), local_size, expected_size);
                return Ok(ValidationResult::ResumeFromPartial);
            }
        }

        FileType::AppendOnly => {
            if local_size >= expected_size {
                log::info!("Append-only file {} already exists with sufficient size ({} >= {}), treating as complete",
                          final_path.display(), local_size, expected_size);

                return Ok(ValidationResult::SkipDownload("File exists with sufficient size".to_string()));
            } else {
                // local_size < expected_size - can resume from partial
                log::info!("Append-only file {} exists but incomplete ({} < {}), will resume download",
                          final_path.display(), local_size, expected_size);
                return Ok(ValidationResult::ResumeFromPartial);
            }
        }

        _ => unreachable!("This function only handles Immutable and AppendOnly file types")
    }
}

pub(crate) fn recover_parto_files(task: &DownloadTask) -> Result<ValidationResult> {
    let parto_files = find_parto_files(task)?;

    if parto_files.is_empty() {
        return Ok(ValidationResult::StartFresh);
    }

    let mut expected_size = task.file_size.load(Ordering::Relaxed);

    // SYNC DIMENSION 1: vs old download (resume scenario)
    // When resuming from part files, we need to validate:
    //   - parto_files' previous master etag.json (from disk)
    //   - master task serving_metadata (prefilled here)
    //   - master response metadata (validated later in process_download_response)
    //
    // This ensures the resumed download continues from the same snapshot
    // as the previous partial download, preventing corruption from mixing
    // different mirror versions.

    // Mutable files have no expected_size beforehand
    if task.file_type != FileType::Immutable {
        if let Ok(Some(metadata)) = task.load_remote_metadata() {
            if let Some(serving_metadata) = metadata.serving_metadata {
                match fetch_server_metadata(task, &serving_metadata.url) {
                    Ok(server_metadata) => {
                        // Validate: server_metadata (current HEAD) matches serving_metadata (from etag.json)
                        // This is the first check in the "vs old download" dimension
                        if !server_metadata.matches_with(&serving_metadata) {
                            log::info!("Server metadata conflicts with existing part files (etag.json vs current HEAD)");
                        } else {
                            expected_size = server_metadata.remote_size.unwrap_or(0);

                            // Prefill serving_metadata so that resumed master and chunk
                            // tasks share a stable baseline before any new HTTP GET starts.
                            // This establishes the baseline for both:
                            // - Dimension 1: master response will validate against this prefilled value
                            // - Dimension 2: chunk responses will validate against master's serving_metadata
                            if let Ok(mut guard) = task.serving_metadata.lock() {
                                *guard = Some(server_metadata.clone());
                            }
                        }
                    }
                    Err(e) => {
                        log::debug!("Failed to fetch server metadata for {}: {}", serving_metadata.url, e);
                        log::debug!("Will start fresh download due to metadata fetch failure");
                    }
                }
            }
        }
    }

    if expected_size == 0 {
        // cleanup_related_part_files() cleans up the pget status file together with
        // the part files, since they are tied together
        cleanup_related_part_files(task)?;
        return Ok(ValidationResult::StartFresh);
    }

    if let Err(e) = recover_chunks_for_parto_files(task, parto_files, expected_size) {
        log::info!("Cannot recover from part files: {}", e);
        log::info!("Cleaning up invalid part files and starting fresh download");
        cleanup_related_part_files(task)?;
        return Ok(ValidationResult::StartFresh);
    }
    Ok(ValidationResult::ResumeFromPartial)
}

/// Handle corruption detection by renaming corrupted files
pub(crate) fn handle_corruption_detection(task: &DownloadTask) -> Result<()> {
    utils::mark_file_bad(&task.final_path)?;
    cleanup_related_part_files(task)?;
    Ok(())
}

/// Clean up all files related to a download task (main part file, pget-status, and chunk files)
fn cleanup_related_part_files(task: &DownloadTask) -> Result<()> {
    cleanup_pget_status_file(task)?;
    cleanup_main_part_file(task)?;
    cleanup_chunk_files(task)?;
    Ok(())
}

fn cleanup_pget_status_file(task: &DownloadTask) -> Result<()> {
    let meta_path = task.meta_json_path();
    if meta_path.exists() {
        fs::remove_file(&meta_path)?;
    }
    Ok(())
}

/// Clean up the main part file and pget-status file
fn cleanup_main_part_file(task: &DownloadTask) -> Result<()> {
    // Remove .part file
    if task.chunk_path.exists() {
        fs::remove_file(&task.chunk_path)?;
    }

    Ok(())
}

/// Clean up any chunk files with -O suffix that belong to this download
pub(crate) fn cleanup_chunk_files(task: &DownloadTask) -> Result<()> {
    let part_path = &task.chunk_path;

    // Remove any chunk files (.part-O*) by globbing filesystem
    if let Some(parent) = part_path.parent() {
        let chunk_prefix = part_path.file_name()
            .and_then(|n| n.to_str())
            .map(|s| format!("{}-O", s))
            .unwrap_or_default();

        if let Ok(entries) = fs::read_dir(parent) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.starts_with(&chunk_prefix) {
                        if let Err(e) = fs::remove_file(entry.path()) {
                            log::warn!("Failed to remove file {}: {}", entry.path().display(), e);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
