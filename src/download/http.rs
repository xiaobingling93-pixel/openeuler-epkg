// ============================================================================
// DOWNLOAD HTTP - HTTP Client Operations and Response Handling
//
// This module handles all HTTP client operations for downloads, including request
// execution, response processing, range request handling, and error management.
// It provides the low-level HTTP interface used by the download system.
//
// Key Features:
// - HTTP request execution with comprehensive error handling
// - ETag and conditional request support for caching
// - Range request handling for resumable downloads
// - Response validation and content processing
// - Network timeout and retry logic
// ============================================================================

// =====================================================================================
// NOTICE: DO NOT change `http::Response<ureq::Body>` to `ureq::Response` or
//           `response.headers().get(...)` to `response.header(...)` in this file!
//
// ureq's public API returns `http::Response<ureq::Body>` and uses `.headers().get()`.
// Changing these will break the build. This is the correct, working usage for ureq.
//
// Also, we use `std::sync::mpsc` for channels throughout the codebase instead of
// `crossbeam_channel` for consistency and to avoid extra dependencies.
// =====================================================================================

use color_eyre::eyre::{eyre, Result};
use std::fs;
use std::path::Path;
use std::sync::atomic::Ordering;
use ureq::http;
use crate::mirror;
use crate::config;
use super::types::*;
use super::utils::*;
use super::validation::*;
use super::mirror::add_url_to_mirror_skip_list;
use super::aur::AUR_BASE_URL;
use super::file_ops::setup_download_file;
use super::progress::update_download_progress;
use super::chunk::{check_ondemand_chunking, validate_chunk_file_boundaries};
use crate::download::setup_task_progress_tracking;
use crate::download::extract_server_metadata;
use crate::download::should_redownload;

/// Execute HTTP download request with comprehensive error handling
///
/// This function handles:
/// - ETag conditional requests (304 Not Modified)
/// - Range request errors (416 Range Not Satisfiable)
/// - Network and timeout errors
/// - Request logging and metrics
pub(crate) fn execute_download_request(
    task: &DownloadTask,
    resolved_url: &str,
    existing_bytes: u64,
) -> Result<http::Response<ureq::Body>> {
    // Build HTTP request with appropriate headers
    let client = task.get_client()?;
    let mut request = client.get(resolved_url.replace("///", "/"));

    // Add ETag conditional headers for cache validation
    let part_path = &task.chunk_path;
    let file_size = task.file_size.load(Ordering::Relaxed);
    let chunk_size = task.chunk_size.load(Ordering::Relaxed);
    let chunk_offset = task.chunk_offset.load(Ordering::Relaxed);

    // Add Range headers for partial content requests
    let resumed_bytes = task.resumed_bytes.load(Ordering::Relaxed);

    match task.get_range_request() {
        RangeRequest::Chunk => {
            let end = chunk_offset + chunk_size - 1;
            let start = chunk_offset + resumed_bytes;
            if start >= end {
                log::warn!("Invalid range detected: start={} >= end={} for {} (chunk_offset={}, chunk_size={}, resumed_bytes={}, file_size={}, chunk_path={})",
                          start, end, resolved_url, chunk_offset, chunk_size, resumed_bytes, file_size, part_path.display());
                return Err(eyre!("Invalid range calculation: start > end"));
            }
            log::debug!("Setting Range header: bytes={}-{} (chunk_offset={}, chunk_size={}, resumed_bytes={}, chunk_path={})",
                       start, end, chunk_offset, chunk_size, resumed_bytes, part_path.display());
            request = request.header("Range", &format!("bytes={}-{}", start, end));
        }
        RangeRequest::Resume => {
            if resumed_bytes >= file_size && file_size > 0 {
                log::warn!("Invalid range detected: resumed_bytes={} >= file_size={} for {} (chunk_offset={}, chunk_size={}, chunk_path={})",
                          resumed_bytes, file_size, resolved_url, chunk_offset, chunk_size, part_path.display());
                return Err(eyre!("Invalid range calculation: resumed_bytes >= file_size"));
            }
            log::debug!("Setting Range header: bytes={}- (resume from existing bytes, chunk_path={})", resumed_bytes, part_path.display());
            request = request.header("Range", &format!("bytes={}-", resumed_bytes));
        }
        RangeRequest::None => {
            // is master task w/o chunking
            // For mutable files, check final_path; for others, check part_path
            let file_to_check: Option<&Path> = if matches!(task.file_type, FileType::Mutable) && task.final_path.exists() {
                Some(&task.final_path)
            } else if part_path.exists() {
                Some(part_path)
            } else {
                log::debug!("Local file {} doesn't exist, skipping ETag header", part_path.display());
                None
            };

            if let Some(file_to_check) = file_to_check {
                // Check file size - don't use ETag for 0-byte files
                let file_size = fs::metadata(file_to_check)
                    .map(|m| m.len())
                    .unwrap_or(0);

                if file_size == 0 {
                    log::debug!("File {} is 0 bytes, skipping ETag header to force fresh download", file_to_check.display());
                } else {
                    // Load ETag from .etag.json format
                    if let Ok(Some(metadata)) = task.load_remote_metadata() {
                        if let Some(etag) = metadata.serving_metadata.and_then(|m| m.etag) {
                            log::debug!("Adding If-None-Match header with ETag '{}' for conditional request (file={})", etag, file_to_check.display());
                            request = request.header("If-None-Match", &format!("\"{}\"", etag));
                        }
                    }
                }
            }
        }
    }

    // Execute the request and handle all possible outcomes
    let request_start = std::time::Instant::now();
    let call_result = request.call();
    let latency = request_start.elapsed().as_millis() as u64;
    log_http_event_safe(resolved_url, mirror::HttpEvent::Latency(latency));

    match call_result {
        Ok(response) => Ok(response),
        Err(ureq::Error::StatusCode(code)) => handle_http_status_error(code, task, resolved_url, existing_bytes),
        Err(ureq::Error::Io(e)) => handle_network_io_error(e, task, resolved_url),
        Err(e) => handle_general_request_error(e, task, resolved_url),
    }
}

/// Handle HTTP status code errors (4xx, 5xx responses)
/// Level 6: Error Handling - processes HTTP status code errors
// ### HTTP Standards & Headers
// #### `Range` (Request Header)
// - Requests a specific part of a resource:
//   ```http
//   Range: bytes=0-499
//   ```
// - Server responds with:
//   - `206 Partial Content` (success) + `Content-Range`.
//   - `416 Range Not Satisfiable` (invalid range).
//
// #### `Accept-Ranges` (Response Header)
// - Indicates if the server supports range requests:
//   ```http
//   Accept-Ranges: bytes
//   ```
//   (or `none` if unsupported).
//
// ### Example Flow
// 1. Client requests a range:
//    ```http
//    GET /largefile.zip HTTP/1.1
//    Range: bytes=0-999
//    ```
// 2. Server responds:
//    ```http
//    HTTP/1.1 206 Partial Content
//    Content-Range: bytes 0-999/5000
//    Content-Length: 1000
//    [...data...]
fn handle_http_status_error(
    code: u16,
    task: &DownloadTask,
    resolved_url: &str,
    _existing_bytes: u64,
) -> Result<http::Response<ureq::Body>> {
    log::debug!("HTTP error code {} for chunk_path={}", code, task.chunk_path.display());

    // Log latency even for errors
    log_http_event_safe(resolved_url, mirror::HttpEvent::HttpStatus(code));

    let error_msg = format!("HTTP {}", code);
    task.set_message(format!("{} - {}", error_msg, resolved_url));

    if code == 429 {
        // Get the active connection count for this mirror **before** logging for better diagnostics
        let active_conns = {
            let site = mirror::url2site(&resolved_url);
            if let Ok(mirrors_guard) = mirror::MIRRORS.lock() {
                mirrors_guard.mirrors.get(&site)
                    .map(|mirror| mirror.shared_usage.active_downloads.load(std::sync::atomic::Ordering::Relaxed))
                    .unwrap_or(0)
            } else {
                0
            }
        };

        log::debug!("Received HTTP 429 Too Many Requests ({} active connections) for {} (chunk_path={})", active_conns, resolved_url, task.chunk_path.display());

        // Log the TooManyRequests event with the connection count
        log_http_event_safe(&resolved_url, mirror::HttpEvent::TooManyRequests(active_conns as u32));

        return Err(DownloadError::TooManyRequests.into());
    }

    if code == 416 {
        // Special handling for 416 Range Not Satisfiable errors
        let chunk_offset = task.chunk_offset.load(Ordering::Relaxed);
        let chunk_size = task.chunk_size.load(Ordering::Relaxed);
        let resumed_bytes = task.resumed_bytes.load(Ordering::Relaxed);
        let file_size = task.file_size.load(Ordering::Relaxed);

        log::warn!("HTTP 416 Range Not Satisfiable for {} (chunk_path={}) - Range calculation details:", resolved_url, task.chunk_path.display());
        log::warn!("  chunk_offset={}, chunk_size={}, resumed_bytes={}, file_size={}",
                  chunk_offset, chunk_size, resumed_bytes, file_size);

        if chunk_offset > 0 || chunk_size != file_size {
            let start = chunk_offset + resumed_bytes;
            let end = chunk_offset + chunk_size - 1;
            log::warn!("  Attempted range: bytes={}-{} (start={}, end={})", start, end, start, end);

            if start > end {
                log::error!("  INVALID RANGE: start > end - this is the root cause of the 416 error");
            } else if end >= file_size && file_size > 0 {
                log::warn!("  Range extends beyond file size: end={} >= file_size={}", end, file_size);
            }
        }

        // For 416 errors, we should try a different mirror or restart the download
        log::warn!("HTTP 416 error indicates invalid range request - will retry with different mirror or restart");
        Err(DownloadError::UnexpectedResponse { code, details: format!("HTTP 416 Range Not Satisfiable: {}", error_msg) }.into())
    } else if code == 502 {
        // 502 Bad Gateway - server is temporarily unavailable (common with unreliable servers like AUR)
        log::warn!("HTTP 502 Bad Gateway for {} (chunk_path={}) - server may be unreliable, will retry", resolved_url, task.chunk_path.display());
        Err(DownloadError::UnexpectedResponse { code, details: format!("HTTP 502 Bad Gateway - server temporarily unavailable: {}", error_msg) }.into())
    } else if code >= HTTP_CLIENT_ERROR_START && code < HTTP_SERVER_ERROR_START {
        // For client errors (like 403, 404), create a simple DownloadError without verbose backtrace
        log::debug!("Client error {} for {} (chunk_path={})", code, resolved_url, task.chunk_path.display());
        // On 404, add URL to mirror skip list since it's likely only missing the current file
        if code == 404 {
            add_url_to_mirror_skip_list(&task.url, resolved_url);
        }
        Err(DownloadError::Fatal { code, message: error_msg }.into())
    } else {
        log::debug!("Server error {} for {} (chunk_path={})", code, resolved_url, task.chunk_path.display());
        Err(DownloadError::UnexpectedResponse { code, details: format!("HTTP error: {}", error_msg) }.into())
    }
}

/// Handle network I/O errors
/// Level 6: Error Handling - processes network I/O errors
fn handle_network_io_error(
    e: std::io::Error,
    task: &DownloadTask,
    resolved_url: &str,
) -> Result<http::Response<ureq::Body>> {
    log_http_event_safe(resolved_url, mirror::HttpEvent::NetError(e.to_string()));

    log::debug!("Network I/O error for {} (chunk_path={}): {}", resolved_url, task.chunk_path.display(), e);

    let error_msg = format!("Network error: {} - {}", e, resolved_url);
    task.set_message(error_msg.clone());
    Err(DownloadError::Network { details: error_msg }.into())
}

/// Handle general request errors (timeouts, DNS failures, etc.)
/// Level 6: Error Handling - processes general request errors
fn handle_general_request_error(
    e: ureq::Error,
    task: &DownloadTask,
    resolved_url: &str,
) -> Result<http::Response<ureq::Body>> {
    let error_str = e.to_string();
    let error_msg = format!("Error downloading: {} - {}", error_str, resolved_url);

    // Log general error as network error
    log_http_event_safe(resolved_url, mirror::HttpEvent::NetError(error_str.clone()));

    log::debug!("General request error for {} (chunk_path={}): {}", resolved_url, task.chunk_path.display(), error_str);

    task.set_message(error_msg.clone());

    Err(DownloadError::Network { details: error_msg }.into())
}

/// Validate response content type to detect HTML login pages
fn validate_response_content_type(
    response: &http::Response<ureq::Body>,
    url: &str,
    task: &DownloadTask,
) -> Result<()> {
    log::debug!("Validating response content type for {} (chunk_path={})", url, task.chunk_path.display());
    if let Some(content_type) = response.headers().get("content-type").and_then(|v| v.to_str().ok()) {
        if content_type.contains("text/html") {
            // Allow HTML for directory listings (URLs ending with /) and HTML files (.html)
            // These are legitimate HTML downloads for index_html.rs
            if url.ends_with('/') || url.ends_with(".html") {
                log::debug!("Allowing HTML content for directory listing or HTML file: {}", url);
                return Ok(());
            }

            // Check if this is an AUR URL that might need git
            // AUR downloads via HTTP are unreliable due to bot protection (Anubis),
            // so git is recommended for AUR packages
            if url.starts_with(AUR_BASE_URL) {
                eprintln!("\nError: Received HTML page (likely bot protection) instead of file from {}", url);
                eprintln!("AUR downloads via HTTP are unreliable due to bot protection systems.");
                eprintln!("Git is recommended for downloading AUR packages.");
                eprintln!("\nPlease retry after installing git in either:");
                eprintln!("  - Host OS: Install git using your system package manager (e.g., 'apt-get install git')");
                eprintln!("  - Environment: Run 'epkg -e {} install git' to install git in current environment", config().common.env_name);
                let error_msg = format!(
                    "AUR download failed: received HTML page (bot protection) instead of file. \
                    AUR downloads via HTTP are unreliable. Please install git (in host OS or environment) and retry."
                );
                task.set_message(error_msg.clone());
                return Err(eyre!("Fatal error while downloading from {}: {}", url, error_msg));
            }

            if task.file_type == FileType::Immutable ||
               task.file_type == FileType::AppendOnly {
                // Reject HTML content for known file types
                let error_msg = "Received HTML page instead of file. This may indicate an authentication issue with the server.";
                task.set_message(error_msg.to_string());
                return Err(eyre!("Fatal error while downloading from {}: {}", url, error_msg.to_string()));
            }
        }
    }
    Ok(())
}

/// Process the main download stream with chunked reading and progress tracking
/// Level 5: Stream Processing - handles the core download loop with boundaries
pub(crate) fn process_chunk_download_stream(
    response: &mut http::Response<ureq::Body>,
    task: &DownloadTask,
    existing_bytes: u64,
) -> Result<u64> {
    // Check if content is compressed - if so, we can't trust content-length for validation
    let has_compression = is_content_compressed(response);

    // Get expected response size from Content-Length header for validation
    // Only use content-length for validation if there's no compression
    let expected_response_size = if !has_compression {
        parse_content_length(response)
    } else {
        log::debug!("Content-encoding detected, skipping content-length validation for {}", task.url);
        None
    };

    let mut reader = response.body_mut().as_reader();
    let mut buffer = vec![0; 8192];
    let mut chunk_append_offset = existing_bytes;
    let mut network_bytes = 0u64;
    let mut last_update = std::time::Instant::now();
    let mut last_ondemand_check = std::time::Instant::now();
    let data_channel = task.get_data_channel();

    // Setup file for writing
    let mut file = setup_download_file(task, existing_bytes)?;

    log::debug!("process_chunk_download_stream: Starting to read response body for {} (existing_bytes={})",
               task.chunk_path.display(), existing_bytes);

    loop {
        // Read data from network stream
        let bytes_read = read_chunk_from_stream(&mut reader, &mut buffer, task, chunk_append_offset)?;

        if bytes_read == 0 {
            // EOF reached - validate against expected size if available
            if let Some(expected_size) = expected_response_size {
                if network_bytes < expected_size {
                    log::error!(
                        "Premature EOF: received {} bytes but expected {} bytes for {}",
                        network_bytes, expected_size, task.chunk_path.display()
                    );
                    return Err(DownloadError::Network {
                        details: format!("Premature EOF: received {} bytes but expected {} bytes for {}",
                                       network_bytes, expected_size, task.chunk_path.display())
                    }.into());
                }
            }
            break; // EOF reached
        }

        // Calculate bytes to write based on chunk boundaries
        let bytes_to_write = calculate_write_bytes(task, bytes_read, chunk_append_offset);

        // Write data to file with boundary checks
        let written_bytes = write_chunk_data(&mut file, &buffer, bytes_to_write, task, chunk_append_offset)?;

        if written_bytes == 0 {
            break; // Chunk boundary reached
        }

        // Update download counters
        chunk_append_offset += written_bytes as u64;
        network_bytes += written_bytes as u64;
        task.received_bytes.store(network_bytes, Ordering::Relaxed);

        // Send data to channel for master tasks
        if let Some(ref channel) = data_channel {
            match channel.send(buffer[..written_bytes].to_vec()) {
                Ok(_) => {
                    // Successfully sent data
                }
                Err(e) => {
                    // Channel is disconnected (receiver dropped)
                    log::warn!("Data channel disconnected for {}: {}", task.url, e);
                    // Don't retry since channel is broken - receiver is gone
                }
            }
        }

        if written_bytes < bytes_read {
            break; // Reached chunk boundary for master task
        }

        update_download_progress(task, &mut last_update);
        check_ondemand_chunking(task, chunk_append_offset, &mut last_ondemand_check);
    }

    Ok(chunk_append_offset)
}

/// Read chunk data from network stream with error handling
/// Level 6: Network I/O - handles reading from response stream
fn read_chunk_from_stream(
    reader: &mut dyn std::io::Read,
    buffer: &mut [u8],
    task: &DownloadTask,
    chunk_append_offset: u64,
) -> Result<usize> {
    match reader.read(buffer) {
        Ok(0) => {
            log::debug!("read_chunk_from_stream: EOF reached at offset {} for {}", chunk_append_offset, task.chunk_path.display());
            Ok(0) // EOF reached
        },
        Ok(n) => {
            log::trace!("read_chunk_from_stream: Read {} bytes at offset {} for {}", n, chunk_append_offset, task.chunk_path.display());
            Ok(n)
        },
        Err(e) => {
            log::error!("read_chunk_from_stream: Read error at offset {} for {}: {}", chunk_append_offset, task.chunk_path.display(), e);
            if task.is_master_task() {
                let error_msg = format!("Read error at {} bytes: {}", chunk_append_offset,
                    task.resolved_url.lock().map(|r| r.clone()).unwrap_or_else(|_| task.url.clone()));
                task.set_message(error_msg);
            }
            Err(eyre!("Failed to read from response (chunk_append_offset={}, buffer_size={}): {}", chunk_append_offset, buffer.len(), e))
        }
    }
}

/// Finalize chunk download with progress updates and completion logging
/// Level 5: Download Finalization - completes download with final updates
pub(crate) fn finalize_chunk_download(
    task: &DownloadTask,
    chunk_append_offset: u64,
    existing_bytes: u64,
) -> Result<u64> {
    let network_bytes = chunk_append_offset - existing_bytes;

    log::debug!("download_content completed: {} total bytes ({} network bytes) written to {}",
               chunk_append_offset, network_bytes, task.chunk_path.display());

    // Detect 0-byte downloads for mutable files (like AUR packages) - this indicates server issues
    // Check BEFORE finalization so we can retry
    if task.is_master_task() && chunk_append_offset == 0 && matches!(task.file_type, FileType::Mutable) {
        log::info!("Download resulted in 0 bytes for {} - likely server issue (unreliable server like AUR), cleaning up and will retry", task.url);
        // Clean up the 0-byte file before returning error to trigger retry
        if task.chunk_path.exists() {
            if let Err(e) = fs::remove_file(&task.chunk_path) {
                log::warn!("Failed to remove 0-byte file {}: {}", task.chunk_path.display(), e);
            } else {
                log::debug!("Cleaned up 0-byte file: {}", task.chunk_path.display());
            }
        }
        return Err(DownloadError::Network {
            details: format!("Download resulted in 0 bytes for {} - server may be unreliable", task.url)
        }.into());
    }

    // Validate that the chunk file respects its designated boundaries
    validate_chunk_file_boundaries(task, chunk_append_offset)?;

    Ok(chunk_append_offset)
}

/// Validate that the downloaded size matches the expected Content-Length
pub(crate) fn validate_download_size(downloaded: u64, total_size: u64, part_path: &Path) -> Result<()> {
    if total_size > 0 && downloaded != total_size {
        // Escalate to ERROR so that mismatches are clearly visible in logs
        log::error!(
            "Download size mismatch: Downloaded size ({}) does not match expected size ({}) for {}",
            downloaded,
            total_size,
            part_path.display()
        );
        return Err(DownloadError::ContentValidation {
            expected: format!("{} bytes", total_size),
            actual: format!("{} bytes", downloaded)
        }.into());
    }
    Ok(())
}

/// Parse Content-Length header from response
///
/// This function tries multiple approaches to extract the content size:
/// 1. Standard Content-Length header (but only if content is not compressed)
/// 2. Content-Range header (e.g., "bytes 0-1023/4096")
/// 3. X-Content-Length header (some servers use this)
///
/// Note: When content-encoding is present (e.g., gzip), Content-Length refers to
/// the compressed size, not the final uncompressed size. In such cases, we return
/// None since we can't reliably predict the final size.
/// Get remote file size from HTTP response headers, taking into account range requests
///
/// This function computes the total remote file size by considering:
/// - task.resumed_bytes: bytes already downloaded locally
/// - task.get_range_request(): type of range request made
/// - response Content-Length: size of current response
/// - response Content-Range: total file size from range response
///
/// For range requests, it properly calculates the total file size by adding
/// the resumed bytes to the response size or using Content-Range total.
pub(crate) fn get_remote_size(task: &DownloadTask, response: &http::Response<ureq::Body>) -> Option<u64> {

    // 1. Try Content-Range header first (most reliable for range requests)
    // Format: "bytes START-END/TOTAL" or "bytes */TOTAL"
    if let Some(content_range) = response.headers().get("content-range") {
        if let Ok(s) = content_range.to_str() {
            if let Some(total_size) = parse_content_range_total(s) {
                log::debug!("Got total size {} from Content-Range header: {}", total_size, s);
                return Some(total_size);
            }
        }
    }

    // Check if content is compressed - if so, Content-Length is unreliable for full file size
    let is_compressed = is_content_compressed(response);

    if is_compressed {
        log::debug!(
            "Content is compressed with '{}', Content-Length refers to compressed size, not final size",
            response.headers().get("content-encoding")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown")
        );
    }

    // 2. Calculate total size from Content-Length + resumed bytes
    if task.get_range_request() != RangeRequest::Chunk && !is_compressed {
        if let Some(response_size) = parse_content_length(response) {
            let resumed_bytes = task.resumed_bytes.load(Ordering::Relaxed);
            let total_size = resumed_bytes + response_size;
            log::debug!(
                "Range request: Content-Length {} + resumed_bytes {} = total size {}",
                response_size, resumed_bytes, total_size
            );
            return Some(total_size);
        }
    }

    // 3. Try X-Content-Length header (some servers use this)
    if let Some(x_content_length) = response.headers().get("x-content-length") {
        if let Ok(s) = x_content_length.to_str() {
            if let Ok(size) = s.parse::<u64>() {
                log::debug!("Got size {} from X-Content-Length header", size);
                return Some(size);
            }
        }
    }

    None
}

/// Parse the total size from a Content-Range header
///
/// Examples:
/// - "bytes 0-1023/4096" -> Some(4096)
/// - "bytes 1024-2047/4096" -> Some(4096)
/// - "bytes */4096" -> Some(4096)
fn parse_content_range_total(range_str: &str) -> Option<u64> {
    // Match patterns like "bytes 0-1023/4096" or "bytes */4096"
    if let Some(slash_pos) = range_str.rfind('/') {
        let total_part = &range_str[slash_pos + 1..];
        if let Ok(size) = total_part.parse::<u64>() {
            return Some(size);
        }
    }
    None
}

/// Check if HTTP response content is compressed
///
/// Detects common compression types that make Content-Length unreliable:
/// - gzip, deflate, compress (standard HTTP compression)
/// - br (Brotli), zstd, xz (modern compression)
/// - identity (explicitly uncompressed)
///
/// Returns true if content is compressed, false if uncompressed or unknown
fn is_content_compressed(response: &http::Response<ureq::Body>) -> bool {
    response.headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .map(|encoding| {
            let encoding_lower = encoding.to_lowercase();
            // Check for common compression types
            encoding_lower.contains("gzip") ||
            encoding_lower.contains("deflate") ||
            encoding_lower.contains("compress") ||
            encoding_lower.contains("br") ||
            encoding_lower.contains("zstd") ||
            encoding_lower.contains("xz") ||
            // Some servers use non-standard compression names
            encoding_lower.contains("bzip2") ||
            encoding_lower.contains("lzma") ||
            encoding_lower.contains("lz4")
        })
        .unwrap_or(false)
}

/// Get Content-Length from HTTP response headers
///
/// This function safely extracts and parses the Content-Length header.
/// It handles various edge cases:
/// - Missing Content-Length header
/// - Invalid UTF-8 in header value
/// - Non-numeric header value
/// - Multiple Content-Length headers (uses first one)
///
/// Returns Some(size) if valid Content-Length found, None otherwise
fn parse_content_length(response: &http::Response<ureq::Body>) -> Option<u64> {
    response.headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
}

/// Parse ETag from response headers
///
/// ETags can be in different formats:
/// - etag: "67736fc6-dee" (lowercase with quotes)
/// - ETag: "67736fc6-dee" (mixed case with quotes)
/// - etag: 67736fc6-dee (without quotes)
/// - ETag: W/"67736fc6-dee" (weak ETag)
///
/// This function normalizes the ETag by removing quotes and the W/ prefix for weak ETags
pub(crate) fn parse_etag(response: &http::Response<ureq::Body>) -> Option<String> {
    // Try both lowercase and mixed case headers
    let etag_value = response.headers().get("etag")
        .or_else(|| response.headers().get("ETag"))
        .and_then(|v| v.to_str().ok())?;

    // Remove W/ prefix for weak ETags and quotes
    let cleaned_etag = etag_value.trim()
        .strip_prefix("W/").unwrap_or(etag_value.trim())  // Remove weak ETag prefix
        .trim_matches('"');  // Remove surrounding quotes

    if cleaned_etag.is_empty() {
        None
    } else {
        Some(cleaned_etag.to_string())
    }
}

/// Handle 304 Not Modified response
fn handle_304_not_modified_response(
    task: &DownloadTask,
) -> Result<()> {
    log::debug!("Received 304 Not Modified - file unchanged on server");
    task.set_message(format!("File unchanged, checking local copy - {}", task.final_path.display()));

    send_file_to_channel(task)
        .map_err(|e| eyre!("Failed to send cached file to channel: {}", e))?;

    Err(DownloadError::AlreadyComplete.into())
}

pub(crate) fn process_download_response(
    task: &DownloadTask,
    response: &http::Response<ureq::Body>,
    resolved_url: &str,
    existing_bytes: u64
) -> Result<()> {
    // Extract metadata and handle 304 responses
    let metadata = match handle_304_and_extract_metadata(task, response, resolved_url)? {
        None => return Ok(()), // 304 handled, early return
        Some(m) => m,
    };

    // Validate range request response
    validate_range_request_response(task, response, resolved_url)?;

    // Validate response and handle resume logic for master tasks
    validate_response_content_type(response, resolved_url, task)?;

    // Validate metadata consistency
    validate_metadata_consistency(task, &metadata, resolved_url)?;

    // Setup progress tracking
    setup_task_progress_tracking(task, response, existing_bytes)?;

    Ok(())
}

/// Process HTTP response and execute content download
/// Level 4: Response Processing - handles HTTP response validation and content download
/// Extract metadata and handle 304 Not Modified responses
fn handle_304_and_extract_metadata(
    task: &DownloadTask,
    response: &http::Response<ureq::Body>,
    resolved_url: &str,
) -> Result<Option<ServerMetadata>> {
    let metadata = extract_server_metadata(task, response, resolved_url);

    log::debug!("process_download_response for {} chunk: {}, metadata: remote_size={:?}, etag={:?}, last_modified={:?}, response: {:?}",
               resolved_url, task.chunk_path.display(), metadata.remote_size, metadata.etag, metadata.last_modified, response);

    // Check for unchanged file case
    // Handle 304 Not Modified responses for ETag conditional requests
    if task.is_master_task() {
        if response.status() == 304 {
            handle_304_not_modified_response(task)?;
            return Ok(None);
        }
        if matches!(task.file_type, FileType::Mutable) {
            let decision = should_redownload(task, &metadata)?;
            if matches!(decision, CacheDecision::UseCache { .. }) {
                handle_304_not_modified_response(task)?;
                return Ok(None);
            }
        }
    }

    Ok(Some(metadata))
}
