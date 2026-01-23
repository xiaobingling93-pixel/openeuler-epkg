//! # Performance Logging and Analytics System
//!
//! This module implements the optimized performance tracking system for mirror analytics.
//! It provides comprehensive logging of download performance, HTTP events, and mirror
//! health metrics to enable intelligent mirror selection decisions.
//!
//! ## Core Features
//!
//! - **Bulk Performance Loading**: Efficiently loads 6 months of historical performance data
//! - **Key-Value Logging Format**: Future-proof extensible logging with automatic parsing
//! - **Date-Based Rotation**: Monthly log rotation using YYYY-MM format for organization
//! - **Dual Logging**: Both file-based persistent logs and in-memory mirror state updates
//! - **HTTP Event Tracking**: Comprehensive monitoring of non-download HTTP interactions
//! - **Connection Limit Learning**: Adaptive limits based on HTTP 429 "Too Many Requests" responses
//!
//! ## Log Entry Types
//!
//! - **PerformanceLog**: Download performance metrics (throughput, latency, success rate)
//! - **HttpLog**: HTTP status codes, latency events, range support detection, network errors
//! - **Connection Limits**: Learned per-mirror connection limits from 429 error responses
//!
//! ## Log Processing
//!
//! - **Single-Pass Loading**: All log files processed once at startup for efficiency
//! - **Intelligent Distribution**: Log entries automatically routed to corresponding mirrors
//! - **Online Status Management**: Automatic mirror status updates based on success/failure patterns
//! - **Global Limits**: Prevents excessive offline mirror marking to maintain availability

use std::collections::HashMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use color_eyre::eyre::{Result, WrapErr};
use time::{OffsetDateTime, UtcOffset};
use time::macros::format_description;
use crate::dirs;
use crate::mirror::types::*;
use crate::mirror::url::url2site;

/*
 * ============================================================================
 * OPTIMIZED PERFORMANCE TRACKING SYSTEM
 * ============================================================================
 *
 * EFFICIENT BULK LOADING STRATEGY:
 *
 * Performance logs are now loaded using an optimized bulk processing approach:
 *
 * 1. **6-Month Historical Window**: Loads performance data from the last 6 months
 *    instead of just 2 months for better performance insights
 *
 * 2. **Single-Pass Processing**: All log files are processed once, with each log
 *    entry automatically distributed to its corresponding mirror
 *
 * 3. **Date-Based Rotation**: Log files use YYYY-MM format for automatic monthly
 *    rotation (e.g., mirror-2024-03.log)
 *
 * 4. **Key=Value Format**: Future-proof logging format that supports easy
 *    extension without breaking compatibility:
 *    [1234567890] https://... bytes=1024 dur=500 lat=100 ok=1
 *
 * 5. **Intelligent Mirror Matching**: Each log entry finds its mirror using
 *    URL pattern matching, eliminating the need for per-mirror log loading
 *
 * This approach provides comprehensive performance data immediately at startup
 * while maintaining optimal performance through efficient bulk processing.
 */

/// Append download performance log both to file and in-memory structures
/// This should only be called for actual downloads with bytes > 0
pub fn append_download_log(
    url: &str,
    offset: u64,
    bytes_transferred: u64,
    duration_ms: u64,
    success: bool,
) -> Result<()> {
    // Only log actual downloads with bytes > 0
    if bytes_transferred == 0 {
        return Ok(());
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let throughput_bps = if duration_ms > 0 && bytes_transferred > 0 {
        (bytes_transferred * 1024) / duration_ms
    } else {
        0
    };

    let log_entry = PerformanceLog {
        timestamp,
        url: url.to_string(),
        offset,
        bytes_transferred,
        duration_ms,
        throughput_bps,
        success,
    };

    // Log to file
    append_log_to_file(&log_entry)?;

    // Update in-memory mirror data
    update_mirror_performance(&log_entry)?;

    // Debug output for informative dumps as requested
    if log::log_enabled!(log::Level::Debug) {
        let kbps = throughput_bps / 1024;
        log::debug!(
            "Mirror performance: {} | {} KB/s | {}ms total | {} bytes | offset: {} | success: {}",
            url2site(url),
            kbps,
            duration_ms,
            bytes_transferred,
            offset,
            success,
        );
    }

    Ok(())
}

/// Append HTTP event log for non-download operations
pub fn append_http_log(url: &str, event: HttpEvent) -> Result<()> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let http_log = HttpLog {
        timestamp,
        url: url.to_string(),
        event: event.clone(),
    };

    // Log to file
    append_http_log_to_file(&http_log)?;

    // Update in-memory mirror data based on event
    update_mirror_http_event(&http_log)?;

    // Debug output
    if log::log_enabled!(log::Level::Debug) {
        log::debug!(
            "Mirror HTTP event: {} | {:?}",
            url2site(url),
            event,
        );
    }

    Ok(())
}

/// Append log entry to the performance log file with date-based rotation
fn append_log_to_file(log_entry: &PerformanceLog) -> Result<()> {
    use std::io::Write;

    // Generate log file name using proper date formatting
    let log_file_name = generate_log_file_name(log_entry.timestamp);
    let log_file_path = dirs().epkg_downloads_cache.join("log").join(log_file_name);

    // Ensure parent directory exists
    if let Some(parent) = log_file_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create log directory: {}", parent.display()))?;
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
        .with_context(|| format!("Failed to open log file: {}", log_file_path.display()))?;

    // Use key=value format for better compatibility and extensibility
    let log_line = format!("{} {} offset={} bytes={} dur={} tput={} ok={}\n",
        format_timestamp_to_local_datetime(log_entry.timestamp),
        log_entry.url,
        log_entry.offset,
        log_entry.bytes_transferred,
        log_entry.duration_ms,
        log_entry.throughput_bps,
        if log_entry.success { "1" } else { "0" },
    );

    file.write_all(log_line.as_bytes())
        .with_context(|| "Failed to write to log file")?;

    Ok(())
}

/// Append HTTP log entry to the log file with date-based rotation
fn append_http_log_to_file(http_log: &HttpLog) -> Result<()> {
    use std::io::Write;

    // Generate log file name using proper date formatting
    let log_file_name = generate_log_file_name(http_log.timestamp);
    let log_file_path = dirs().epkg_downloads_cache.join("log").join(log_file_name);

    // Ensure parent directory exists
    if let Some(parent) = log_file_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create log directory: {}", parent.display()))?;
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
        .with_context(|| format!("Failed to open log file: {}", log_file_path.display()))?;

    // Use key=value format for HTTP events
    let event_str = match &http_log.event {
        HttpEvent::Latency(ms) => format!("latency={}", ms),
        HttpEvent::NoRange => "no_range=1".to_string(),
        HttpEvent::NetError(err) => format!("net_error={}", err),
        HttpEvent::HttpStatus(code) => format!("http_status={}", code),
        HttpEvent::TooManyRequests(count) => format!("too_many_requests={}", count),
        HttpEvent::OldContent => "old_content=1".to_string(),
    };

    let log_line = format!("{} {} {}\n",
        format_timestamp_to_local_datetime(http_log.timestamp),
        http_log.url,
        event_str,
    );

    file.write_all(log_line.as_bytes())
        .with_context(|| "Failed to write HTTP log to file")?;

    Ok(())
}

/// Update in-memory mirror performance data
fn update_mirror_performance(log_entry: &PerformanceLog) -> Result<()> {
    let site = url2site(&log_entry.url);

    if let Ok(mut mirrors_guard) = MIRRORS.lock() {
        if let Some(mirror) = mirrors_guard.mirrors.get_mut(&site) {
            // Update performance data if successful
            if log_entry.success && log_entry.bytes_transferred > 0 {
                mirror.record_performance(
                    log_entry.throughput_bps as u32,
                    0
                );
                mirror.calculate_performance_score();
            }
        }
    }

    Ok(())
}

/*
 * ============================================================================
 * MAX_PARALLEL_CONNS MANAGEMENT FLOW
 * ============================================================================
 *
 * This system implements adaptive connection limiting to prevent HTTP 429
 * "Too Many Requests" errors by learning from server responses and adjusting
 * per-site connection limits accordingly.
 *
 * FLOW OVERVIEW:
 *
 * 1. **Initial State**: All mirrors start with max_parallel_conns = None
 *    (no learned limit, use adaptive_max_concurrent instead)
 *
 * 2. **HTTP 429 Detection**: When a download receives HTTP 429:
 *    - The active connection count is captured from mirror.stats.active_downloads
 *    - HttpEvent::TooManyRequests(conn_count) is logged to file
 *    - update_mirror_http_event() is called immediately
 *
 * 3. **Limit Calculation**: In update_mirror_http_event():
 *    - new_limit = min(conn_count - 1, old_limit)
 *    - This ensures we never exceed the limit that caused 429
 *    - The limit can only decrease, never increase
 *
 * 4. **Persistent Storage**: The limit is saved to log files as:
 *    - Format: "too_many_requests=5" (where 5 is the connection count)
 *    - parse_and_distribute_log_entries() reads this on startup
 *    - Applies the same min(conn_count-1, old_limit) logic
 *
 * 5. **Mirror Selection**: select_best_mirror() respects both limits:
 *    - adaptive_max_concurrent (calculated from performance)
 *    - max_parallel_conns (learned from 429 errors)
 *    - Uses the more restrictive of the two
 *
 * 6. **Automatic Recovery**: The system automatically:
 *    - Skips mirrors that are at their learned limits
 *    - Distributes load to other available mirrors
 *    - Prevents repeated 429 errors from the same mirror
 *
 * BENEFITS:
 * - Prevents cascading 429 errors across multiple downloads
 * - Maintains optimal performance while respecting server limits
 * - Provides persistent learning across application restarts
 * - Enables graceful degradation when servers have strict limits
 *
 * EXAMPLE SCENARIO:
 * - Mirror A has 5 active connections and receives 429
 * - System learns: max_parallel_conns = 4 (5-1)
 * - Future downloads to Mirror A are limited to 4 concurrent connections
 * - If Mirror A receives another 429 with 4 connections, limit becomes 3
 * - System automatically distributes excess load to other mirrors
 */

/// Check if a mirror should be marked as NoOnline based on success rate and global limit
///
/// A mirror should be marked as NoOnline only if:
/// - It is not already marked as NoOnline, AND
/// - It meets the failure criteria (no throughputs with errors, or failure rate > 2/3), AND
/// - The total number of NoOnline mirrors would not exceed 1/MAX_NOONLINE_FRACTION_DENOM of all mirrors
///
/// This prevents mirrors from being marked offline too easily and ensures we don't exclude
/// too many mirrors (max 1/3 of all mirrors can be NoOnline).
fn should_mark_no_online(stats: &MirrorStats, total_mirrors: usize, current_noonline_count: usize) -> bool {
    // If already marked as NoOnline, don't mark again
    if stats.no_online {
        return false;
    }

    let successes = stats.throughputs.len();
    let total_errors: usize = stats.http_errors.values().sum::<u32>() as usize + stats.other_errors as usize;
    let total_attempts = successes + total_errors;

    // Check if mirror meets failure criteria
    let meets_failure_criteria = if successes == 0 && total_errors > 0 {
        // New mirror that failed immediately
        true
    } else if total_attempts >= MIN_ATTEMPTS_FOR_NOONLINE && total_attempts > 0 {
        // Only mark as NoOnline if failure rate > 2/3 (i.e., success rate < 1/3)
        // Using integer comparison: total_errors * 3 > total_attempts * 2
        total_errors * 3 > total_attempts * 2
    } else {
        false
    };

    if !meets_failure_criteria {
        return false;
    }

    // Check global limit: don't mark as NoOnline if we've already excluded 1/3 of mirrors
    if total_mirrors > 0 {
        return (current_noonline_count + 1) * MAX_NOONLINE_FRACTION_DENOM <= total_mirrors;
    }

    false
}

/// Update in-memory mirror data based on HTTP events
fn update_mirror_http_event(http_log: &HttpLog) -> Result<()> {
    let site = url2site(&http_log.url);

    if let Ok(mut mirrors_guard) = MIRRORS.lock() {
        let total_mirrors = mirrors_guard.mirrors.len();
        // Count NoOnline mirrors, excluding the current one if it exists
        let current_noonline_count = mirrors_guard.mirrors.iter()
            .filter(|(s, m)| *s != &site && m.stats.no_online)
            .count();

        if let Some(mirror) = mirrors_guard.mirrors.get_mut(&site) {
            let stats = &mut mirror.stats;
            stats.last_check = Some(http_log.timestamp);

            match &http_log.event {
                HttpEvent::Latency(ms) => {
                    stats.latencies.push(*ms as u32);
                },
                HttpEvent::NoRange => {
                    stats.no_range = true;
                },
                HttpEvent::NetError(_) => {
                    stats.other_errors += 1;
                    if should_mark_no_online(stats, total_mirrors, current_noonline_count) {
                        stats.no_online = true;
                    }
                },
                HttpEvent::HttpStatus(code) => {
                    // Record the error first
                    *stats.http_errors.entry(*code).or_insert(0) += 1;

                    if *code == 404 {
                        stats.no_content += 1;
                    } else if *code == HTTP_FORBIDDEN || *code >= HTTP_SERVER_ERROR_START {
                        if should_mark_no_online(stats, total_mirrors, current_noonline_count) {
                            stats.no_online = true;
                        }
                    }
                },
                HttpEvent::TooManyRequests(conn_count_val) => {
                    let conn_count = conn_count_val.clone();
                    // Handle TooManyRequests event: set max_parallel_conns to min(conn_count-1, old_value)
                    let new_limit = if conn_count > 1 { conn_count - 1 } else { 1 };
                    let final_limit = if let Some(old_limit) = stats.max_parallel_conns {
                        new_limit.min(old_limit)
                    } else {
                        new_limit
                    };
                    stats.max_parallel_conns = Some(final_limit);
                    log::debug!("Learned new connection limit for {}: {} (from {} connections) when requesting {}",
                              mirror.url, final_limit, conn_count, http_log.url);
                    // Also record the 429 error in stats
                    *stats.http_errors.entry(429).or_insert(0) += 1;
                },
                HttpEvent::OldContent => {
                    // Mark mirror as having old/inconsistent content for integrity system
                    stats.old_content = true;
                    log::debug!("Mirror {} marked as having old/inconsistent content", mirror.url);
                }
            }
        }
    }

    Ok(())
}


/// Load performance logs from recent log files at once
pub(crate) fn load_performance_logs(mirrors: &mut HashMap<String, Mirror>) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Generate the last 6 months (including current month) using proper date formatting
    let months_to_check = generate_recent_month_strings(now, 6);

    for month in months_to_check {
        let log_file_path = dirs().epkg_downloads_cache
            .join(format!("log/mirror-{}.log", month));

        if log_file_path.exists() {
            if let Err(e) = parse_and_distribute_log_entries(&log_file_path, mirrors) {
                log::debug!("Failed to parse log file {}: {}", log_file_path.display(), e);
            }
        }
    }
}

/// Parse log file and distribute entries to appropriate mirrors
fn parse_and_distribute_log_entries(
    log_file_path: &std::path::Path,
    mirrors: &mut HashMap<String, Mirror>,
) -> Result<()> {
    let contents = fs::read_to_string(log_file_path)?;

    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let mut bytes_transferred = 0u64;
        let mut throughput_bps = 0u64;
        let mut success = false;
        let mut latency_ms = None;
        let mut no_range = None;
        let mut net_error = None;
        let mut http_status = None;
        let mut too_many_requests: Option<u32> = None;
        let mut old_content = None;

        // Split the line into tokens
        let tokens: Vec<&str> = line.split_whitespace().collect();

        if tokens.len() < 2 {
            continue;
        }

        // Format: time url key=value key=value...
        let url = tokens[1].to_string();

        // Parse the remaining key=value pairs
        for &token in tokens.iter() {
            if let Some((key, value)) = token.split_once('=') {
                match key {
                    "bytes" => bytes_transferred = value.parse().unwrap_or(0),
                    "tput" => throughput_bps = value.parse().unwrap_or(0),
                    "ok" => success = value == "1" || value == "true",
                    "lat" | "latency" => latency_ms = Some(value.parse().unwrap_or(0)),
                    "no_range" => no_range = Some(value == "1" || value == "true"),
                    "net_error" => net_error = Some(value.to_string()),
                    "http_status" => http_status = Some(value.parse().unwrap_or(0)),
                    "too_many_requests" => too_many_requests = value.parse().ok(),
                    "old_content" => old_content = Some(value == "1" || value == "true"),
                    "dur" => {}, // Duration field, ignored for now
                    _ => {} // Ignore unknown keys for forward compatibility
                }
            }
        }

        // Find the mirror this log entry belongs to and update its online status
        //
        // Mirror online status logic:
        // 1. Mark mirror as online (no_online = false) on any successful download with throughput data
        // 2. Mark mirror as offline (no_online = true) only when:
        //    - New mirror with no throughputs and has errors (failed immediately), OR
        //    - Has >= MIN_ATTEMPTS_FOR_NOONLINE attempts AND failure rate > 2/3 (success rate < 1/3)
        //    - AND total NoOnline mirrors would not exceed 1/MAX_NOONLINE_FRACTION_DENOM of all mirrors
        //    - This prevents mirrors from being marked offline too easily and ensures we don't exclude
        //      too many mirrors (max 1/3 of all mirrors can be NoOnline).
        let site = url2site(&url);
        let total_mirrors = mirrors.len();
        // Count NoOnline mirrors, excluding the current one if it exists
        let current_noonline_count = mirrors.iter()
            .filter(|(s, m)| *s != &site && m.stats.no_online)
            .count();

        if let Some(mirror) = mirrors.get_mut(&site) {
            // Update mirror attributes based on the log entry
            if bytes_transferred > 0 && success {
                // This is a download log entry
                mirror.record_performance(throughput_bps as u32, 0);
                mirror.calculate_performance_score();
                mirror.stats.no_online = false;
            } else if let Some(latency) = latency_ms {
                // This is a latency event
                mirror.record_performance(0, latency as u32);
                mirror.calculate_performance_score();
            } else if let Some(true) = no_range {
                // Server doesn't support range requests (permanent attribute)
                mirror.stats.no_range = true;
            } else if net_error.is_some() {
                mirror.stats.other_errors += 1;
                if should_mark_no_online(&mirror.stats, total_mirrors, current_noonline_count) {
                    mirror.stats.no_online = true;
                }
            } else if let Some(code) = http_status {
                // Record the error first
                *mirror.stats.http_errors.entry(code).or_insert(0) += 1;

                if code == HTTP_FORBIDDEN || code >= HTTP_SERVER_ERROR_START {
                    if should_mark_no_online(&mirror.stats, total_mirrors, current_noonline_count) {
                        mirror.stats.no_online = true;
                    }
                }
                // DO NOT set no_content on 404 history logs: it may well be temp
                // issue due to rsync delays, or error in a different distro/version
            } else if let Some(conn_count_val) = too_many_requests {
                let conn_count = conn_count_val;
                // Handle TooManyRequests event: set max_parallel_conns to min(conn_count-1, old_value)
                let new_limit = if conn_count > 1 { conn_count - 1 } else { 1 };
                let final_limit = if let Some(old_limit) = mirror.stats.max_parallel_conns {
                    new_limit.min(old_limit)
                } else {
                    new_limit
                };
                mirror.stats.max_parallel_conns = Some(final_limit);
                log::trace!("Learned new connection limit for {}: {} (from {} connections) when requesting {}",
                          mirror.url, final_limit, conn_count, url);
                // Also record the 429 error in stats
                *mirror.stats.http_errors.entry(429).or_insert(0) += 1;
            } else if let Some(true) = old_content {
                // Handle OldContent event: mark mirror as having old/inconsistent content
                mirror.stats.old_content = true;
                log::trace!("Mirror {} marked as having old/inconsistent content (from log)", mirror.url);
            }
        }
    }

    Ok(())
}

/// Convert Unix timestamp to formatted datetime string using time crate
fn format_timestamp_to_local_datetime(timestamp: u64) -> String {
    // Convert timestamp to OffsetDateTime in UTC first
    match OffsetDateTime::from_unix_timestamp(timestamp as i64) {
        Ok(utc_datetime) => {
            // Try to get local offset and convert to local time
            let local_datetime = if let Ok(local_offset) = UtcOffset::current_local_offset() {
                utc_datetime.to_offset(local_offset)
            } else {
                // Fallback to UTC if we can't get local offset (e.g., in multi-threaded environment)
                utc_datetime
            };

            // Use the same format as in history.rs for consistency
            local_datetime.format(&format_description!("[year]-[month]-[day].[hour repr:24]:[minute]:[second]"))
                .unwrap_or_else(|_| format!("{}", timestamp))
        },
        Err(_) => format!("{}", timestamp), // Fallback to timestamp if conversion fails
    }
}

/// Generate log file name from timestamp using proper date formatting
fn generate_log_file_name(timestamp: u64) -> String {
    match OffsetDateTime::from_unix_timestamp(timestamp as i64) {
        Ok(utc_datetime) => {
            // Try to get local offset and convert to local time, fallback to UTC
            let datetime = if let Ok(local_offset) = UtcOffset::current_local_offset() {
                utc_datetime.to_offset(local_offset)
            } else {
                // Fallback to UTC if we can't get local offset
                utc_datetime
            };

            // Use YYYY-MM format for monthly log rotation
            datetime.format(&format_description!("[year]-[month padding:zero]"))
                .map(|date_str| format!("mirror-{}.log", date_str))
                .unwrap_or_else(|_| {
                    // Final fallback - use UTC formatting directly
                    utc_datetime.format(&format_description!("[year]-[month padding:zero]"))
                        .map(|date_str| format!("mirror-{}.log", date_str))
                        .unwrap_or_else(|_| format!("mirror-{}.log", timestamp))
                })
        },
        Err(_) => {
            // Last resort fallback if timestamp conversion completely fails
            format!("mirror-{}.log", timestamp)
        }
    }
}

/// Generate a list of month strings for the last N months using proper date formatting
fn generate_recent_month_strings(timestamp: u64, months_back: usize) -> Vec<String> {
    match OffsetDateTime::from_unix_timestamp(timestamp as i64) {
        Ok(utc_datetime) => {
            // Try to get local offset and convert to local time, fallback to UTC
            let current_datetime = if let Ok(local_offset) = UtcOffset::current_local_offset() {
                utc_datetime.to_offset(local_offset)
            } else {
                // Fallback to UTC if we can't get local offset
                utc_datetime
            };

            let mut month_strings = Vec::new();

            for i in 0..months_back {
                // Calculate the date for i months ago (approximate with 30 days per month)
                let days_to_subtract = i as i64 * DAYS_PER_MONTH;
                let target_date = if let Some(target) = current_datetime.checked_sub(time::Duration::days(days_to_subtract)) {
                    target
                } else {
                    // Try UTC fallback if local time calculation fails
                    if let Some(target) = utc_datetime.checked_sub(time::Duration::days(days_to_subtract)) {
                        target
                    } else {
                        continue;
                    }
                };

                if let Ok(month_str) = target_date.format(&format_description!("[year]-[month padding:zero]")) {
                    month_strings.push(month_str);
                }
            }

            month_strings
        },
        Err(_) => {
            // Last resort fallback if timestamp conversion completely fails
            vec![format!("{}", timestamp / SECONDS_PER_MONTH)] // Very rough month approximation
        }
    }
}
