// ============================================================================
// DOWNLOAD MIRROR - Mirror Selection and Synchronization Management
//
// This module addresses mirror synchronization issues that occur when different
// mirrors have different rsync times and periods. It provides intelligent mirror
// selection, conflict resolution, and metadata consistency validation to ensure
// reliable package downloads across distributed mirror networks.
//
// Key Features:
// - Mirror metadata consistency validation
// - Conflict resolution for out-of-sync mirrors
// - ADB (Alpine Database) file special handling
// - Server response metadata extraction and comparison
// ============================================================================

use color_eyre::eyre::{eyre, Result, WrapErr};

use crate::mirror;
use crate::models::channel_config;
use super::types::*;
use super::utils::log_http_event_safe;
use super::http::{get_remote_size, parse_etag};
use super::validation::parse_http_date;
use ureq::http;
use std::time::UNIX_EPOCH;

/*
 * ============================================================================
 * MIRROR SYNCHRONIZATION PROBLEM AND SOLUTIONS
 * ============================================================================
 *
 * PROBLEM BACKGROUND:
 *
 * Mirror sites have different rsync times and periods, leading to newer/older
 * mismatches among index files and package files. This causes two main issues:
 *
 * 1. **Index File Mismatches**: Different mirrors may serve different versions
 *    of index files (e.g., core.db vs extra.db).  When these mismatched indexes
 *    are combined, the resolver may not find dependencies with specific versions,
 *    leading to missing "$depend=$version" errors.
 *
 * 2. **Index vs Package Mismatches**: A mirror may serve an index file that
 *    references package files that don't exist on another mirror (404 errors).
 *    This is especially common with Arch Linux, which doesn't keep old files.
 *    For example:
 *    - Index file from mirror A references package X version 1.2.3
 *    - Package file download from mirror B returns 404 because it only has 1.2.4
 *    - This happens because mirrors sync at different times
 *
 * EXAMPLES FROM REAL-WORLD CURL OUTPUTS:
 *
 * The same index file (extra.db.tar.gz) from different mirrors shows:
 * - mirrors.huaweicloud.com: Last-Modified: Fri, 12 Dec 2025 19:38:05 GMT, ETag: "693c6f1d-80cd4d"
 * - mirrors.163.com:         Last-Modified: Sat, 13 Dec 2025 00:06:58 GMT, ETag: "693cae22-80cc8f"
 * - mirrors.nju.edu.cn:      Last-Modified: Sat, 13 Dec 2025 09:22:31 GMT, ETag: "693d3057-80cc10"
 * - mirrors.aliyun.com:      Last-Modified: Sat, 13 Dec 2025 11:30:36 GMT, ETag: "693d4e5c-810249"
 * - mirrors.ustc.edu.cn:     Last-Modified: Sat, 13 Dec 2025 11:30:36 GMT, ETag: "693d4e5c-81026e"
 *
 * These differences in Last-Modified and ETag indicate different snapshot versions
 * of the repository, which can lead to inconsistent combined indexes and 404 errors.
 *
 * SOLUTIONS IMPLEMENTED:
 *
 * 1. **ADB File Special Handling**: Files downloaded via sync_from_package_database()
 *    (Alpine APKINDEX.tar.gz, Arch Linux core.files.tar.gz, etc.) are marked with
 *    DownloadFlags::ADB. These files use a special mirror selection algorithm.
 *
 * 2. **Multi-Mirror Metadata Checking**: For ADB files, we:
 *    - Call select_mirror_with_usage_tracking() up to 6 times to get 3 unique mirrors
 *    - Fetch server metadata (HEAD request) from each mirror to get Last-Modified/ETag
 *    - Select the mirror with the most recent last_modified timestamp
 *    - Add mismatched mirrors to skip list to avoid using them for this file
 *    - Note: 3 tries are good enough to find either the (maybe most) recent or
 *      most common repodata (which reduces 404 risk on fetching package files)
 *
 * 3. **Metadata Tracking**: All metadata responses from different mirrors are stored
 *    in DownloadTask.servers_metadata for debugging and consistency checking.
 *
 * 4. **Post-Download Validation**: After download completes, we check if any mirror
 *    has more recent metadata than what was downloaded and warn the user.
 *
 * 5. **404 Error Handling**: On 404 errors, we add the URL to the mirror's skip list
 *    since it's likely only missing the current file on that mirror.
 *
 * 6. **Mirror Health Tracking**: MirrorStats.no_content changed from bool to u32 counter.
 *    Mirrors are excluded when no_content >= 3, allowing temporary 404s without
 *    permanently blacklisting mirrors.
 *
 * 7. **Unified Metadata Storage**: Extended .etag file to .etag.json format that stores
 *    both serving_metadata (from the serving mirror) and servers_metadata (from all
 *    probed mirrors) for easier debugging.
 *
 * FUTURE IMPROVEMENTS (Possible):
 *
 * - Snapshot-aware mirror grouping: Group mirrors by matching ETags/Last-Modified
 *   and ensure all index files come from the same snapshot group
 * - Merge old/new index versions: To discover all possible available package files
 * - Keep old versions index and packages: server side improvements
 */

/*
 * ============================================================================
 * DOWNLOAD ORCHESTRATION WITH MIRROR OPTIMIZATION
 * ============================================================================
 *
 * PERFORMANCE LOGGING INTEGRATION:
 *
 * Every download operation contributes to the mirror performance database:
 *
 * 1. **Latency Tracking**: HTTP request timing for all call() operations
 * 2. **Throughput Measurement**: Actual transfer speeds during content download
 * 3. **Error Classification**: Distinguishes server errors from network issues
 * 4. **Capability Detection**: Tracks range request support and content availability
 *
 * This comprehensive logging enables the mirror selection system to make
 * increasingly intelligent decisions over time.
 */

/// Helper function to format mirror URL and resolve $mirror placeholder
/// Returns the resolved URL with $mirror replaced by the formatted mirror URL
fn format_and_resolve_mirror_url(
    mirror: &mirror::Mirror,
    repodata_name: &str,
    url: &str,
) -> Result<String> {
    let distro = &channel_config().distro;
    let arch = &channel_config().arch;
    let distro_dir = mirror::Mirrors::find_distro_dir(mirror, distro, arch, repodata_name);
    let final_distro_dir = if distro_dir.is_empty() { distro.to_string() } else { distro_dir };

    let url_formatted = {
        let mirrors = mirror::MIRRORS.lock()
            .map_err(|e| eyre!("Failed to lock mirrors: {}", e))?;
        mirrors.format_mirror_url(&mirror.url, mirror.top_os.is_some(), &final_distro_dir)?
    };

    Ok(url.replace("$mirror", &url_formatted))
}

/// Resolve mirror URL and update task with resolved URL and mirror
pub(crate) fn resolve_mirror_and_update_task(task: &DownloadTask) -> Result<String> {
    let url = &task.url;
    let need_range = task.get_range_request() != RangeRequest::None;

    // If URL doesn't contain $mirror, just update resolved URL
    if !url.contains("$mirror") {
        log::debug!("resolve_mirror_and_update_task: URL {} doesn't contain $mirror, using as-is", url);
        if let Ok(mut resolved) = task.resolved_url.lock() {
            *resolved = url.to_string();
        }
        return Ok(url.to_string());
    }

    log::debug!("resolve_mirror_and_update_task: Resolving mirror for URL {}", url);

    // DIMENSION 1: vs old download (resume scenario)
    // If this task already has serving_metadata with a fully-resolved URL
    // (prefilled from recover_parto_files() on resume), reuse that URL
    // instead of selecting a new mirror. This ensures the resumed download
    // continues from the same mirror snapshot as the previous partial download,
    // maintaining consistency across the "vs old download" dimension.
    if let Ok(guard) = task.serving_metadata.lock() {
        if let Some(ref sm) = *guard {
            if sm.url.starts_with("http://") || sm.url.starts_with("https://") {
                log::debug!(
                    "resolve_mirror_and_update_task: Reusing resolved URL from serving_metadata (resume): {}",
                    sm.url
                );
                if let Ok(mut resolved) = task.resolved_url.lock() {
                    *resolved = sm.url.clone();
                }
                // Best-effort: set mirror_inuse by matching site key. Even if
                // this fails, the resolved URL is enough for correctness.
                if let Ok(mirrors) = mirror::MIRRORS.lock() {
                    let site = mirror::url2site(&sm.url);
                    if let Some(m) = mirrors.mirrors.get(&site) {
                        if let Ok(mut mirror_guard) = task.mirror_inuse.lock() {
                            *mirror_guard = Some(m.clone());
                        }
                    }
                }
                return Ok(sm.url.clone());
            }
        }
    }

    // For ADB files, use special mirror selection that checks metadata from multiple mirrors
    if task.flags.contains(DownloadFlags::ADB) && task.is_master_task() {
        return resolve_mirror_for_adb(task, url, need_range);
    }

    // Select mirror with usage tracking
    let selected_mirror = {
        let mut mirrors = mirror::MIRRORS.lock()
            .map_err(|e| eyre!("Failed to lock mirrors: {}", e))?;

        let mirror = mirrors.select_mirror_with_usage_tracking(need_range, Some(&task.url), &task.repodata_name)
            .map_err(|e| DownloadError::MirrorResolution {
                details: format!("{}", e)
            })?;

        log::debug!("resolve_mirror_and_update_task: Selected mirror {} for URL {} {}", mirror.url, url, &task.repodata_name);
        mirror
    };

    // Get distro directory and format mirror URL
    let resolved_url = format_and_resolve_mirror_url(
        &selected_mirror,
        &task.repodata_name,
        url,
    )?;

    // Store the selected mirror in the task
    if let Ok(mut mirror_guard) = task.mirror_inuse.lock() {
        *mirror_guard = Some(selected_mirror);
    }

    // Update resolved URL in task
    if let Ok(mut resolved) = task.resolved_url.lock() {
        *resolved = resolved_url.clone();
    }

    Ok(resolved_url)
}

/// Collect unique mirrors for ADB files by calling select_mirror_with_usage_tracking up to 6 times
fn collect_unique_mirrors_for_adb(
    task: &DownloadTask,
    url: &str,
    need_range: bool,
) -> Result<Vec<crate::mirror::Mirror>> {
    let mut unique_mirrors = Vec::new();
    let mut seen_sites = std::collections::HashSet::new();
    let mut attempts = 0;
    const MAX_ATTEMPTS: usize = 6;
    const TARGET_UNIQUE_MIRRORS: usize = 3;

    while unique_mirrors.len() < TARGET_UNIQUE_MIRRORS && attempts < MAX_ATTEMPTS {
        attempts += 1;
        let mut mirrors = mirror::MIRRORS.lock()
            .map_err(|e| eyre!("Failed to lock mirrors: {}", e))?;

        match mirrors.select_mirror_with_usage_tracking(need_range, Some(url), &task.repodata_name) {
            Ok(mirror) => {
                let site = mirror::url2site(&mirror.url);
                if seen_sites.insert(site.clone()) {
                    unique_mirrors.push(mirror);
                    log::debug!("resolve_mirror_for_adb: Collected unique mirror {} (attempt {}/{})", site, attempts, MAX_ATTEMPTS);
                } else {
                    log::debug!("resolve_mirror_for_adb: Skipping duplicate mirror {} (attempt {}/{})", site, attempts, MAX_ATTEMPTS);
                }
            }
            Err(e) => {
                log::debug!("resolve_mirror_for_adb: Failed to select mirror on attempt {}: {}", attempts, e);
            }
        }
    }

    if unique_mirrors.is_empty() {
        return Err(eyre!("Failed to find any mirrors for ADB file {}", url));
    }

    Ok(unique_mirrors)
}

/// Fetch metadata from each mirror for ADB files
fn fetch_mirror_metadata_for_adb(
    task: &DownloadTask,
    unique_mirrors: Vec<crate::mirror::Mirror>,
    url: &str,
) -> Result<Vec<(crate::mirror::Mirror, String, ServerMetadata)>> {
    log::debug!("resolve_mirror_for_adb: Collected {} unique mirrors, fetching metadata", unique_mirrors.len());

    let mut mirror_metadata = Vec::new();
    let total_mirrors = unique_mirrors.len();
    for (idx, mirror) in unique_mirrors.iter().enumerate() {
        let test_url = format_and_resolve_mirror_url(
            mirror,
            &task.repodata_name,
            url,
        )?;

        // Surface progress to the user while we probe mirrors
        task.set_message(format!(
            "Probing mirror {}/{}: {}",
            idx + 1,
            total_mirrors,
            mirror.url
        ));

        match fetch_server_metadata(task, &test_url) {
            Ok(metadata) => {
                log::debug!("resolve_mirror_for_adb: Got metadata from {}: last_modified={:?}, timestamp={}",
                           mirror.url, metadata.last_modified, metadata.timestamp);
                mirror_metadata.push((mirror.clone(), test_url, metadata));
            }
            Err(e) => {
                task.set_message(format!("Probe failed for {}: {}", mirror.url, e));
                log::debug!("resolve_mirror_for_adb: Failed to fetch metadata from {}: {}", mirror.url, e);
            }
        }
    }

    if mirror_metadata.is_empty() {
        return Err(eyre!("Failed to fetch metadata from any mirror for ADB file {}", url));
    }

    Ok(mirror_metadata)
}

/// Select the best mirror based on metadata and update the task
fn select_and_update_mirror_for_adb(
    task: &DownloadTask,
    mirror_metadata: Vec<(crate::mirror::Mirror, String, ServerMetadata)>,
) -> Result<String> {
    // Select mirror with most recent last_modified
    let (selected_mirror, selected_url, selected_metadata) = mirror_metadata.iter()
        .max_by_key(|(_, _, metadata)| metadata.timestamp)
        .ok_or_else(|| eyre!("No metadata available"))?;

    log::debug!("resolve_mirror_for_adb: Selected mirror {} with timestamp {} (most recent)",
               selected_mirror.url, selected_metadata.timestamp);

    // Add URLs to skip list for mirrors whose metadata doesn't match the selected one
    for (mirror, test_url, metadata) in &mirror_metadata {
        if !metadata.matches_with(selected_metadata) {
            log::debug!("resolve_mirror_for_adb: Mirror {} has mismatched metadata, adding to skip list", mirror.url);
            record_conflict_mirror(&task, test_url, &metadata);
        }
    }

    // Store the selected mirror in the task
    if let Ok(mut mirror_guard) = task.mirror_inuse.lock() {
        *mirror_guard = Some(selected_mirror.clone());
    }

    // Update resolved URL in task
    if let Ok(mut resolved) = task.resolved_url.lock() {
        *resolved = selected_url.clone();
    }

    Ok(selected_url.clone())
}

/// Resolve mirror for ADB (Alpine/Arch Database) files by checking metadata from multiple mirrors
///
/// This function:
/// 1. Calls select_mirror_with_usage_tracking() up to 6 times to get 3 unique mirrors
/// 2. For each mirror, calls fetch_server_metadata() to get Last-Modified/ETag
/// 3. Selects the mirror with the most recent last_modified
/// 4. Calls add_url_to_mirror_skip_list() for mirrors whose metadata doesn't match the selected one
///
/// Comment: 3 tries are good enough to find either the (maybe most) recent or most common repodata
/// (which reduces 404 risk on fetching package files)
fn resolve_mirror_for_adb(task: &DownloadTask, url: &str, need_range: bool) -> Result<String> {
    log::debug!("resolve_mirror_for_adb: Resolving mirror for ADB file {}", url);

    let unique_mirrors = collect_unique_mirrors_for_adb(task, url, need_range)?;
    let mirror_metadata = fetch_mirror_metadata_for_adb(task, unique_mirrors, url)?;
    select_and_update_mirror_for_adb(task, mirror_metadata)
}


/// Add URL to mirror skip list to avoid using it again
pub fn add_url_to_mirror_skip_list(url: &str, resolved_url: &str) {
    if url == resolved_url || resolved_url.contains("$mirror") {
        log::debug!("add_url_to_mirror_skip_list: No resolved URL found for task URL {}", resolved_url);
        return;
    }

    // Extract mirror site from resolved URL
    let site_key = mirror::url2site(&resolved_url);
    log::debug!("add_url_to_mirror_skip_list: resolved_url={}, site_key={}", resolved_url, site_key);

    // Add URL to mirror skip list
    if let Ok(mut mirrors) = mirror::MIRRORS.lock() {
        if let Some(mirror_in_collection) = mirrors.mirrors.get_mut(&site_key) {
            mirror_in_collection.add_skip_url(url);
            log::debug!("Successfully added {} to skip_urls for mirror site {}", url, site_key);
        } else {
            log::warn!("Mirror site {} not found in mirrors collection for URL {}", site_key, url);
        }
    } else {
        log::warn!("Failed to lock mirrors collection for URL {}", url);
    }
}

/// Apply stored metadata (timestamp and ETag) to the final downloaded file
/// Check if any mirror has more recent metadata than the current download
/// This is useful for detecting when mirrors are out of sync
/// Validates all level tasks' servers_metadata against master task's serving_metadata
pub(crate) fn validate_mirror_metadata(task: &DownloadTask) {
    if !task.flags.contains(DownloadFlags::ADB) || !task.is_master_task() {
        return;
    }

    // Get master task's serving_metadata for comparison
    let master_serving_metadata = if let Ok(guard) = task.serving_metadata.lock() {
        guard.clone()
    } else {
        return;
    };

    let master_serving_metadata = match master_serving_metadata {
        Some(ref metadata) => metadata,
        None => return,
    };

    // Use iterate_task_levels to validate all level tasks
    task.iterate_task_levels(|level_task, _level| {
        // 1) Compare each level task's serving_metadata with the master's serving_metadata
        if let Ok(level_serving_guard) = level_task.serving_metadata.lock() {
            if let Some(ref level_serving_metadata) = *level_serving_guard {
                if !level_serving_metadata.matches_with(master_serving_metadata) {
                    log::warn!(
                        "Level task {}: serving_metadata is inconsistent with master. \
                         Level: remote_size={:?}, etag={:?}, last_modified={:?}, timestamp={}; \
                         Master: remote_size={:?}, etag={:?}, last_modified={:?}, timestamp={}",
                        level_task.url,
                        level_serving_metadata.remote_size,
                        level_serving_metadata.etag,
                        level_serving_metadata.last_modified,
                        level_serving_metadata.timestamp,
                        master_serving_metadata.remote_size,
                        master_serving_metadata.etag,
                        master_serving_metadata.last_modified,
                        master_serving_metadata.timestamp
                    );
                }
            }
        }

        // 2) Detect mirrors that appear more recent than the master
        if let Ok(servers_metadata_guard) = level_task.servers_metadata.lock() {
            for server_metadata in servers_metadata_guard.iter() {
                if server_metadata.timestamp > master_serving_metadata.timestamp {
                    log::warn!(
                        "Level task {}: Mirror {} has more recent last-modified ({:?}, timestamp {}) than the master task's serving metadata ({:?}, timestamp {})",
                        level_task.url,
                        server_metadata.url,
                        server_metadata.last_modified,
                        server_metadata.timestamp,
                        master_serving_metadata.last_modified,
                        master_serving_metadata.timestamp
                    );
                }
            }
        }
    });
}

pub(crate) fn fetch_server_metadata(task: &DownloadTask, url: &str) -> Result<ServerMetadata> {
    let request_start = std::time::Instant::now();

    let client = task.get_client()?;
    let response = client.head(url).call()
        .with_context(|| format!("Failed to make HEAD request to {}", url))?;

    let latency = request_start.elapsed().as_millis() as u64;
    log_http_event_safe(url, mirror::HttpEvent::Latency(latency));

    if let Ok(mut guard) = task.range_request.lock() {
        *guard = RangeRequest::None;  // reset for correct get_remote_size()
    }
    Ok(extract_server_metadata(task, &response, url))
}

/// Get server metadata from HTTP response headers
pub(crate) fn extract_server_metadata(task: &DownloadTask, response: &http::Response<ureq::Body>, resolved_url: &str) -> ServerMetadata {
    let remote_size = get_remote_size(task, response);
    let last_modified = response.headers().get("last-modified").map(|s| s.to_str().unwrap_or("").to_string());
    let etag = parse_etag(response);

    // Parse timestamp from last_modified, or use 0 if not available
    let timestamp = if let Some(ref lm) = last_modified {
        parse_http_date(lm)
            .map(|st| st.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())
            .unwrap_or(0)
    } else {
        0
    };

    ServerMetadata {
        url: resolved_url.to_string(),
        remote_size,
        last_modified,
        timestamp,
        etag,
    }
}


pub(crate) fn record_conflict_mirror(task: &DownloadTask, resolved_url: &str, metadata: &ServerMetadata) {
    add_url_to_mirror_skip_list(&task.url, resolved_url);

    // Record different metadata in servers_metadata for validation in validate_mirror_metadata()
    if let Ok(mut servers_metadata) = task.servers_metadata.lock() {
        servers_metadata.push(metadata.clone());
    }
}

