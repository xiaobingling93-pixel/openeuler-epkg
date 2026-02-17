// ============================================================================
// DOWNLOAD ORCHESTRATION - High-Level Download Coordination
//
// This module provides high-level functions for orchestrating download operations
// across multiple URLs and packages. It coordinates the download manager, progress
// tracking, and file operations to provide a unified interface for downloading
// packages and their dependencies.
//
// Key Functions:
// - download_urls: Download multiple URLs in parallel
// - enqueue_package_downloads: Queue package downloads with dependency resolution
// - get_package_file_path: Resolve package file paths
// - Package download coordination and completion waiting
// ============================================================================

use std::{
    collections::HashMap,
    fs,
    sync::atomic::Ordering,
    thread,
    time::Duration,
};

use color_eyre::eyre::{eyre, Result, WrapErr};

use indicatif::MultiProgress;


use crate::dirs;
use crate::models::*;
use crate::mirror;

// Import AUR functions and constants
use super::aur::*;

// Import types and constants from the types module
use super::types::*;

// Import utility functions
use super::utils::*;

// Import progress functions
use super::progress::*;

// Import file operations
use super::file_ops::*;

// Import manager functions and statics
use super::manager::{DOWNLOAD_MANAGER, submit_download_task};

// Import chunk functions
use super::chunk::{create_chunk_tasks, download_chunk_task, wait_for_chunks_and_merge};
use super::file_ops::cleanup_chunk_files;


/// Download multiple URLs in parallel and wait for all downloads to complete.
///
/// This function submits download tasks for all provided URLs, starts the download manager,
/// and waits for each download to complete. It returns a vector of results where each element
/// corresponds to the URL at the same index in the input vector.
///
/// # Parameters
///
/// * `urls` - Vector of URLs to download. All downloads are performed in parallel.
///
/// # Returns
///
/// Returns `Vec<Result<String>>` where:
/// - `Ok(String)` contains the final file path of a successfully downloaded file
/// - `Err(...)` contains an error message if the download failed
///
/// The results vector has the same length as the input `urls` vector, with each result
/// corresponding to the URL at the same index.
///
/// # Behavior
///
/// - Creates the output directory (`epkg_downloads_cache`) if it doesn't exist
/// - Submits all download tasks before starting processing (parallel submission)
/// - Waits for each task to complete sequentially
/// - Each download uses the global `nr_retry` setting from `CommonOptions`
/// - All downloads are stored in `epkg_downloads_cache` directory
///
/// # Errors
///
/// Individual download failures are returned as `Err` in the results vector.
/// The function does not fail early - all downloads are attempted and all results
/// (successful or failed) are returned.
pub fn download_urls(
    urls: Vec<String>,
) -> Vec<Result<String>> {
    let output_dir = dirs().epkg_downloads_cache.clone();
    if let Err(e) = fs::create_dir_all(&output_dir)
        .with_context(|| format!("Failed to create output directory: {}", output_dir.display())) {
        let error_msg = format!("{}", e);
        return urls.into_iter().map(|_| Err(eyre!("{}", error_msg))).collect();
    }

    let mut task_urls = Vec::new();
    let mut submit_errors = Vec::new();

    for url in &urls {
        let url_for_context = url.clone();
        let task = match DownloadTask::new(url.clone()) {
            Ok(task) => task,
            Err(e) => {
                submit_errors.push(Some(e.wrap_err(format!("Failed to create download task for URL: {}", url))));
                task_urls.push(None);
                continue;
            }
        };

        // Submit the task - if URL already exists, it will just replace/reuse
        if let Err(e) = submit_download_task(task)
            .with_context(|| format!("Failed to submit download task for {}", url_for_context)) {
            submit_errors.push(Some(e));
            task_urls.push(None);
        } else {
            submit_errors.push(None);
            task_urls.push(Some(url.clone()));
        }
    }
    DOWNLOAD_MANAGER.start_processing();

    // Wait for each task using the URLs and collect results
    let mut results = Vec::new();
    for (i, task_url_opt) in task_urls.iter().enumerate() {
        if let Some(ref task_url) = task_url_opt {
            match DOWNLOAD_MANAGER.wait_for_task(task_url.clone())
                .with_context(|| format!("Failed to wait for download task {} (URL: {})", i, task_url)) {
                Ok((status, final_path_opt)) => {
                    match status {
                        DownloadStatus::Completed => {
                            if let Some(final_path) = final_path_opt {
                                results.push(Ok(final_path.to_string_lossy().to_string()));
                            } else {
                                results.push(Err(eyre!("Download completed but no final path available for {}", task_url)));
                            }
                        }
                        DownloadStatus::Failed(err_msg) => {
                            results.push(Err(eyre!("Download failed for {}: {}", task_url, err_msg)));
                        }
                        _ => {
                            results.push(Err(eyre!("Unexpected status for {}: {:?}", task_url, status)));
                        }
                    }
                }
                Err(e) => {
                    results.push(Err(e));
                }
            }
        } else {
            // Use the submit error if available
            if let Some(err) = submit_errors[i].take() {
                results.push(Err(err));
            } else {
                results.push(Err(eyre!("Failed to submit download task for URL at index {}", i)));
            }
        }
    }
    results
}

// Current Code Call Graph
// download_task()
// ├── handle_local_file()
// │   └── (file copy operations + optional size verification)
// ├── handle_aur_git_download()
// │   └── (AUR git clone/fetch flow using find_git_command())
// ├── create_pid_file()
// ├── setup_progress_bar()
// └── download_file_with_retries()
//     ├── validate_existing_file()
//     │   ├── send_file_to_channel() for existing full file
//     │   └── handle_corruption_detection()
//     ├── recover_parto_files()
//     └── download_file()
//         ├── create_chunk_tasks()
//         ├── download_chunk_task()
//         │   ├── check_existing_partfile()
//         │   │   └── send_chunk_to_channel() for master task resume
//         │   ├── resolve_mirror_and_update_task()
//         │   ├── execute_download_request()
//         │   ├── process_download_response()
//         │   │   ├── handle_304_and_extract_metadata()
//         │   │   │   ├── extract_server_metadata()
//         │   │   │   ├── handle_304_not_modified_response()
//         │   │   │   └── should_redownload() for Mutable file
//         │   │   ├── validate_range_request_response()
//         │   │   ├── validate_response_content_type()
//         │   │   ├── validate_metadata_consistency()
//         │   │   └── setup_task_progress_tracking()
//         │   ├── process_chunk_download_stream()
//         │   │   ├── read_chunk_from_stream()
//         │   │   ├── calculate_write_bytes()
//         │   │   ├── write_chunk_data()
//         │   │   ├── channel.send()
//         │   │   ├── update_download_progress()
//         │   │   └── check_ondemand_chunking()
//         │   ├── finalize_chunk_download()
//         │   │   └── validate_chunk_file_boundaries()
//         │   ├── validate_download_size()
//         │   └── log_download_completion()
//         ├── wait_for_chunks_and_merge()
//         │   ├── process_chunks_at_level()
//         │   │   ├── merge_completed_chunk()
//         │   │   │   ├── validate_chunk_file_boundaries()
//         │   │   │   ├── send_chunk_to_all_channels() for chunk tasks
//         │   │   │   └── append_file_to_file()
//         │   │   ├── handle_failed_chunk()
//         │   │   └── update_chunk_progress()
//         │   └── validate_chunk_merge_integrity()
//         └── finalize_file()
//             └── (atomic rename .part → final file)

/// Downloads a file from a URL to the output directory.
/// Uses the final_path that was calculated when the task was created.
///
/// Ensures optimal mirror configuration and comprehensive performance logging
pub(crate) fn download_task(
    task: &DownloadTask,
    multi_progress: &MultiProgress,
) -> Result<()> {
    let url = &task.url.clone();
    let final_path = &task.final_path.clone();
    let expected_size = task.file_size.load(Ordering::Relaxed);

    log::debug!("download_task starting for {}, has_channel: {}, expected_size: {:?}", url, task.get_data_channel().is_some(), expected_size);

    // Handle local files - in with_size() we already set task.final_path pointing to the original local file url
    // (detect_url_proto_path/local_url_to_path's behavior), so can avoid the extra copy
    if task.flags.contains(DownloadFlags::LOCAL) {
        // For local files, final_path already points to the canonicalized local file path
        // No download needed
        return Ok(());
    }

    // Handle AUR git downloads if URL matches AUR pattern and git is available
    if let Ok(()) = handle_aur_git_download(url) {
        return Ok(());
    }

    // Create PID file for process coordination
    let pid_file = create_pid_file(final_path)?;

    // Setup progress bar and store it in the task
    setup_progress_bar(task, multi_progress, url)?;

    // Start the download - use the old system for now until we can safely integrate the new one
    let result = download_file_with_retries(task);
    log::debug!("download_task download_file_with_retries completed for {}, result: {:?}", url, result);

    // Clean up PID file regardless of result
    let _pid_cleanup_result = cleanup_pid_file(&pid_file);

    // Update progress bar based on result
    if result.is_ok() {
        task.finish_with_message(format!("Downloaded {}", final_path.display()));
    } else {
        task.finish_with_message(format!("Error: {:?}", result));
    }

    result
}

// download_file():
//   1. Download content (including chunk merging)
//   2. Extract & store metadata from HTTP response
//   3. Verify file size on .part file
//   4. Atomic completion (.part → final file)
//   5. Set metadata (timestamp + ETag) on final file
//
// download_file_with_retries():
//   - Pure retry wrapper around download_file()
//   - Only handles retry logic and error handling
fn download_file_with_retries(
    task: &DownloadTask,
) -> Result<()> {
    let url = &task.url;
    let max_retries = task.max_retries;

    let mut retries = 0;

    // Validate existing files and determine appropriate download action
    match validate_existing_file(task)? {
        ValidationResult::SkipDownload(reason) => {
            send_file_to_channel(task)
                .with_context(|| format!("Failed to send existing file to channel: {}", task.final_path.display()))?;

            log::info!("Skipping download: {}", reason);
            return Ok(());
        }
        ValidationResult::ResumeFromPartial => {
            log::info!("Resuming from partial file at {}", task.chunk_path.display());
            // Continue with existing partial file
        }
        ValidationResult::CorruptionDetected => {
            log::warn!("Corruption detected in {}, handling...", task.chunk_path.display());
            handle_corruption_detection(task)?;
            // Continue with fresh download
        }
        ValidationResult::StartFresh => {
            log::info!("Starting fresh download for {}", task.get_resolved_url());
            // Continue with fresh download
        }
    }

    // Recover any existing part files for resumption
    match recover_parto_files(task)? {
        ValidationResult::ResumeFromPartial => {
            log::info!("Recovered partial files for resumption at {}", task.chunk_path.display());
        }
        _ => {
            // No recovery needed or recovery failed
        }
    }

    loop {
        log::debug!("download_file_with_retries calling download_file for {} (saving to {}), attempt {}", url, task.chunk_path.display(), retries + 1);

        let download_result = download_file(task);
        let resolved_url = task.get_resolved_url();

        match download_result {
            Ok(()) => {
                log::debug!("download_file_with_retries completed successfully for {}, dropping channel", &resolved_url);


                return Ok(());
            },
            Err(e) => {
                // Check if this is one of our custom download errors to avoid logging stack traces
                if let Some(download_err) = e.downcast_ref::<DownloadError>() {
                    match download_err {
                        DownloadError::Fatal { code, message } => {
                            log::debug!("download_file_with_retries got fatal error {} for {} (saving to {}): {}", code, resolved_url, task.chunk_path.display(), message);
                        },
                        DownloadError::Network { details } => {
                            log::debug!("download_file_with_retries got network error for {} (saving to {}): {}", resolved_url, task.chunk_path.display(), details);
                        },
                        // File system errors are now handled as io::Error
                        DownloadError::ContentValidation { expected, actual } => {
                            log::debug!("download_file_with_retries got content validation error for {} (saving to {}): expected {}, got {}", resolved_url, task.chunk_path.display(), expected, actual);
                        },
                        DownloadError::MirrorResolution { details } => {
                            log::debug!("download_file_with_retries got mirror resolution error for {}: {}", resolved_url, details);
                        },
                        DownloadError::UnexpectedResponse { code, details } => {
                            log::debug!("download_file_with_retries got unexpected response {} for {}: {}", code, resolved_url, details);
                        },
                        DownloadError::AlreadyComplete => {
                            log::debug!("download_file_with_retries got already complete response for {}", resolved_url);
                            return Ok(());
                        },
                        DownloadError::TooManyRequests => {
                            log::debug!("download_file_with_retries got too many requests error for {}", resolved_url);
                        },
                        DownloadError::DiskError { details } => {
                            log::debug!("download_file_with_retries got disk error for {} (saving to {}): {}", resolved_url, task.chunk_path.display(), details);
                            // Don't mark mirror as bad for disk errors - they're local issues
                            return Err(e);
                        },
                    }
                } else {
                    log::debug!("download_file_with_retries got error for {}: {}", resolved_url, e);
                }

                if retries >= max_retries {
                    return Err(eyre!("Max retries ({}) exceeded for {}: {}", max_retries, resolved_url, e));
                }

                // Reset stale chunk tasks before the next retry to avoid inconsistent state
                {
                    if let Ok(mut guard) = task.chunk_tasks.lock() {
                        if !guard.is_empty() {
                            log::debug!("Clearing {} stale chunk tasks before retry", guard.len());
                            guard.clear();
                            // Restore master expected size to full file so the next attempt
                            // resumes correctly (ondemand had reduced it to the parent range only).
                            let file_size = task.file_size.load(Ordering::Relaxed);
                            if file_size > 0 {
                                task.chunk_size.store(file_size, Ordering::Relaxed);
                            }
                        }
                    }
                    if let Err(e2) = task.set_chunk_status(ChunkStatus::NoChunk) {
                        log::warn!("Failed to reset chunk_status to NoChunk: {}", e2);
                    }
                    cleanup_chunk_files(task)?;
                }

                task.attempt_number.fetch_add(1, Ordering::SeqCst);
                retries += 1;
                if retries < max_retries / 2 {
                    // no delay, to quickly try another mirror
                    continue;
                }

                let delay = Duration::from_secs(2u64.pow(retries as u32));
                // Keep showing the original error message in pb for some time
                thread::sleep(delay);
                task.set_message(format!("Retrying (attempt {}/{} after {}s delay): {}", retries + 1, max_retries + 1, delay.as_secs(), resolved_url));
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn download_file(
    task: &DownloadTask,
) -> Result<()> {
    let url = &task.url;
    log::debug!("download_file starting for {} (chunk_path={})", url, task.chunk_path.display());

    // Try to create beforehand chunks
    create_chunk_tasks(task)?;

    download_chunk_task(task)?;

    // Wait for all chunks to complete and merge them
    wait_for_chunks_and_merge(task)?;

    log::debug!("download_file calling finalize_file for {} (chunk_path={})", url, task.chunk_path.display());
    finalize_file(task)?;
    log::info!("download_file completed: {} (chunk_path={})", task.get_resolved_url(), task.chunk_path.display());
    Ok(())
}

// ============================================================================
// PACKAGE MANAGER DOWNLOAD INTEGRATION
// ============================================================================

/// Enqueue download tasks for packages without waiting for completion
/// Returns a mapping from download URLs to their package keys for tracking
pub fn enqueue_package_downloads(
    packages: &InstalledPackagesMap,
) -> Result<HashMap<String, Vec<String>>> {
    let output_dir = dirs().epkg_downloads_cache.clone();
    let mut url_to_pkgkeys: HashMap<String, Vec<String>> = HashMap::new();

    // Create output directory
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("Failed to create output directory: {}", output_dir.display()))?;

    // Submit download tasks for each package (handles both local and remote)
    for pkgkey in packages.keys() {
        let package = crate::package_cache::load_package_info(pkgkey)
            .map_err(|e| eyre!("Failed to load package info for key: {}: {}", pkgkey, e))?;
        let url = format!(
            "{}/{}",
            package.package_baseurl,
            package.location
        );

        // Use the larger of compressed size or installed size for download prioritization
        // This helps the download manager prioritize packages that are likely to take longer
        let size = if package.size > 0 {
            Some(package.size as u64)
        } else {
            None
        };

        // Submit download task with size information (handles both local and remote files)
        let task = DownloadTask::with_size(url.clone(), size, package.repodata_name.clone(), DownloadFlags::empty())
            .with_context(|| format!("Failed to create download task for package {} (URL: {})", pkgkey, url))?;
        submit_download_task(task)
            .with_context(|| format!("Failed to submit download task for {}", url))?;
        url_to_pkgkeys.entry(url).or_default().push(pkgkey.clone());
    }

    // Start processing download tasks
    DOWNLOAD_MANAGER.start_processing();

    Ok(url_to_pkgkeys)
}

/// Get the local file path for a downloaded package
pub fn get_package_file_path(pkgkey: &str) -> Result<String> {
    let package = crate::package_cache::load_package_info(pkgkey)
        .map_err(|e| eyre!("Failed to load package info for key: {}: {}", pkgkey, e))?;
    let url = format!(
        "{}/{}",
        package.package_baseurl,
        package.location
    );

    // Check if we have a download task for this URL
    let tasks = DOWNLOAD_MANAGER.tasks.lock()
        .map_err(|e| eyre!("Failed to lock tasks mutex: {}", e))?;

    if let Some(task) = tasks.get(&url) {
        // Return the final_path from the task
        return Ok(task.final_path.to_string_lossy().to_string());
    }

    // If no task exists, calculate the path as before (fallback)
    if url.starts_with("/") {
        // Local file - return the destination path in downloads cache
        let file_name = url.split('/').last()
            .ok_or_else(|| eyre!("Failed to extract filename from URL: {}", url))?;
        let dest_path = dirs().epkg_downloads_cache.join(file_name);
        Ok(dest_path.to_string_lossy().to_string())
    } else {
        // Remote file - use the URL to cache path conversion
        let cache_path = mirror::Mirrors::url_to_cache_path(&url, &package.repodata_name)
            .map_err(|e| eyre!("Failed to convert URL to cache path: {}: {}", url, e))?;
        Ok(cache_path.to_string_lossy().to_string())
    }
}
