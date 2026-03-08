// ============================================================================
// DOWNLOAD VALIDATION - Download Integrity and Range Validation
//
// This module provides validation logic for download operations, including
// range request validation, file type classification, and integrity checking.
// It ensures downloads are performed correctly and handles various edge cases
// in HTTP responses and file handling.
//
// Key Features:
// - Range request response validation
// - File type classification for integrity handling
// - HTTP date parsing and validation
// - Content validation and corruption detection
// - Mirror conflict detection and recording
// ============================================================================

use std::{
    path::Path,
    sync::atomic::Ordering,
    time::SystemTime,
};
use crate::lfs;

use color_eyre::eyre::{eyre, Result};
use time::{OffsetDateTime, format_description::well_known::Rfc2822};
use ureq::http;

use crate::mirror;
use super::types::*;
use super::manager::DOWNLOAD_MANAGER;
use super::mirror::record_conflict_mirror;

/// Validate range request response to ensure proper partial content handling
pub fn validate_range_request_response(
    task: &DownloadTask,
    response: &http::Response<ureq::Body>,
    resolved_url: &str,
) -> Result<()> {
    let range_request_type = task.get_range_request();
    log::debug!("process_download_response: range_request={:?}, response_status={}, chunk_path={}",
               range_request_type, response.status(), task.chunk_path.display());

    if range_request_type != RangeRequest::None {
        // For chunk tasks, validate we got partial content
        if response.status() == 200 {
            // Server ignoring Range header - would corrupt chunk
            log::warn!("CORRUPTION PREVENTED: Server returned HTTP 200 instead of 206 for range request to {} (chunk: {})",
                       resolved_url, task.chunk_path.display());
            if let Err(e) = mirror::append_http_log(resolved_url, mirror::HttpEvent::NoRange) {
                log::warn!("Failed to log chunk range error: {}", e);
            }
            return Err(DownloadError::NoRangeSupport.into());
        }

        if response.status() != 206 {
            // Resume failed, restart from beginning
            if task.chunk_path.exists() {
                lfs::remove_file(&task.chunk_path)?;
            }
            task.resumed_bytes.store(0, Ordering::Relaxed);
            log::debug!("Server doesn't support resume, restarting download for {}", task.chunk_path.display());
            return Err(eyre!("Server returned {} for range request", response.status()));
        }
    } else {
        log::debug!("process_download_response: No range request validation needed for {}", task.chunk_path.display());
    }

    Ok(())
}

/// Validate metadata consistency between master and chunks
///
/// METADATA CONSISTENCY: Two synchronization dimensions
///
/// DIMENSION 1: vs old download (resume scenario)
/// When resuming from part files, we validate:
///   - parto_files' previous master etag.json (from disk, checked in recover_parto_files)
///   - master task serving_metadata (prefilled in recover_parto_files from HEAD)
///   - master response metadata (validated here against prefilled serving_metadata)
///
/// This ensures the resumed download continues from the same snapshot as the
/// previous partial download, preventing corruption from mixing different mirror versions.
///
/// DIMENSION 2: in new download (parallel tasks scenario)
/// During active download, we validate:
///   - all tasks' response metadata (master + chunks from concurrent HTTP GETs)
///   - master serving_metadata (may be prefilled if None from resume, or set by first response)
///
/// This ensures all chunks come from the same snapshot as the master, preventing
/// corruption from mixing chunks from different mirrors/versions.
///
/// RACE CONDITION HANDLING:
/// - recover_parto_files() pre-fills task.serving_metadata with HEAD metadata
///   so master/chunk share a baseline before any GET starts (Dimension 1 + Dimension 2)
/// - resolve_mirror_and_update_task() reuses that baseline URL instead of
///   picking a new mirror on resume (Dimension 1)
/// - Below we ensure:
///   * master response validates against prefilled serving_metadata (Dimension 1)
///   * first response establishes master serving_metadata baseline if None (Dimension 2)
///   * each chunk validates against master serving_metadata (Dimension 2)
pub fn validate_metadata_consistency(
    task: &DownloadTask,
    metadata: &ServerMetadata,
    resolved_url: &str,
) -> Result<()> {
    // DIMENSION 1 + DIMENSION 2: Validate metadata consistency
    if task.is_master_task() {
        // DIMENSION 1: Validate master response against prefilled serving_metadata
        // (from recover_parto_files or previous attempt_number or the first chunk).
        if let Ok(mut guard) = task.serving_metadata.lock() {
            if let Some(ref serving_metadata) = *guard {
                // Dimension 1: master response must match prefilled baseline (resume scenario)
                if !metadata.matches_with(serving_metadata) {
                    record_conflict_mirror(task, resolved_url, metadata);
                    return Err(eyre!(
                        "Master metadata conflicts with existing serving_metadata. New: {:?}, Existing: {:?}",
                        metadata,
                        serving_metadata
                    ));
                }
            }
            // Dimension 2: establish baseline for new download (no resume)
            *guard = Some(metadata.clone());
        }
    } else {
        // DIMENSION 2: Validate chunk response against master serving_metadata
        // This ensures all chunks come from the same snapshot as the master.
        if let Some(master_task) = DOWNLOAD_MANAGER.get_task(&task.url) {
            if let Ok(mut master_metadata_guard) = master_task.serving_metadata.lock() {
                if let Some(ref serving_metadata) = *master_metadata_guard {
                    // Chunk must match master baseline
                    if !metadata.matches_with(serving_metadata) {
                        record_conflict_mirror(task, resolved_url, metadata);
                        return Err(eyre!(
                            "Chunk metadata conflicts with master metadata. Chunk: {:?}, Master: {:?}",
                            metadata,
                            serving_metadata
                        ));
                    }
                } else {
                    // Edge case: chunk arrived before master response
                    // Adopt this chunk's metadata as master baseline so subsequent chunks validate
                    *master_metadata_guard = Some(metadata.clone());
                }
            }
        } else {
            log::warn!("Cannot find master task for {}", &task.url);
        }

        // Dimension 2: record for validation by validate_mirror_metadata()
        if let Ok(mut serving_metadata) = task.serving_metadata.lock() {
            *serving_metadata = Some(metadata.clone());
        }
    }

    Ok(())
}

/// Check if filename indicates an immutable file (package)
pub fn is_immutable_filename(file_path: &str) -> bool {
    file_path.ends_with(".deb") ||
    file_path.ends_with(".rpm") ||
    file_path.ends_with(".apk") ||
    file_path.ends_with(".epkg") ||
    file_path.ends_with(".conda") ||
    file_path.ends_with(".whl") ||
    file_path.contains("/by-hash/") ||
    file_path.ends_with(".bz2") ||
    file_path.ends_with(".gz") ||
    file_path.ends_with(".xz") ||
    file_path.ends_with(".zst")
}

/// Classify file type for integrity handling based on filename and path
pub fn classify_file_type(final_path: &Path, file_size: Option<u64>) -> FileType {
    let path_str = final_path.to_string_lossy();

    // These are mainly repo index files, marking them Mutable here will produce .etag.json
    // file that can be touch/checked by has_recent_download().
    if path_str.contains("/Release")                || // DEB, Release[.gpg]
       path_str.contains("/InRelease")              || // DEB
       path_str.contains("/repomd.xml")             || // RPM, repomd.xml[.asc]
       path_str.ends_with(".db.tar.gz")             || // Archlinux, (core|extra|..).db.tar.gz
       path_str.ends_with(".files.tar.gz")          || // Archlinux, (core|extra|..).files.tar.gz, superset of .db.tar.gz
       path_str.contains("/APKINDEX")               || // Alpine, APKINDEX.tar.gz[.sig]
       path_str.contains("/repodata.json")          || // Conda, repodata.json.(zst|bz2)
       path_str.contains("/current_repodata.json")  || // Conda, current_repodata.json.gz
       path_str.contains("/elf-loader")             || // epkg elf-loader[.sig]
       path_str.ends_with("/packages-meta-ext-v1.json.gz") { // AUR metadata
        return FileType::Mutable;
    }

    // Immutable files (packages) - require known size
    if file_size.is_some() && is_immutable_filename(&path_str) {
        return FileType::Immutable;
    }

    // Append-only files (future extension)
    // if path_str.contains("/epkg-index") {
    //     return FileType::AppendOnly;
    // }

    // Default classification based on size availability
    // Files with known size are more likely to be immutable packages
    if file_size.is_some() {
        FileType::Immutable
    } else {
        FileType::Mutable
    }
}

/// Parse HTTP date string into SystemTime
pub fn parse_http_date(date_str: &str) -> Result<SystemTime> {
    log::debug!("Parsing HTTP date: {}", date_str);

    // Try parsing RFC 2822 format (most common HTTP date format)
    if let Ok(datetime) = OffsetDateTime::parse(date_str, &Rfc2822) {
        return Ok(datetime.into());
    }

    // Try parsing ISO format as fallback
    if let Ok(datetime) = OffsetDateTime::parse(date_str, &time::format_description::well_known::Iso8601::DEFAULT) {
        return Ok(datetime.into());
    }

    // Try parsing simple date formats
    let formats = [
        "%a, %d %b %Y %H:%M:%S GMT",
        "%A, %d-%b-%y %H:%M:%S GMT",
        "%a %b %d %H:%M:%S %Y",
    ];

    for format in &formats {
        if let Ok(parsed) = time::PrimitiveDateTime::parse(date_str, &time::format_description::parse(format).unwrap_or_default()) {
            let offset_dt = parsed.assume_utc();
            return Ok(offset_dt.into());
        }
    }

    Err(eyre!("Failed to parse HTTP date: {}", date_str))
}
