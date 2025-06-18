use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::sync::mpsc::Sender;
use std::sync::LazyLock;

use color_eyre::{eyre::eyre, eyre::WrapErr, Result};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use ureq::{Agent, Proxy};
use ureq::http;
use crate::dirs;
use crate::models::*;
use crate::mirror::{append_download_log, track_mirror_usage_start, track_mirror_usage_end, MIRRORS, Mirrors};
use time::{OffsetDateTime, format_description::well_known::Rfc2822};
use filetime::set_file_mtime;

#[derive(Debug, Clone)]
pub struct DownloadTask {
    pub url: String,
    #[allow(dead_code)]
    pub output_dir: PathBuf,
    pub max_retries: usize,
    pub data_channel: Option<Sender<Vec<u8>>>,
    pub status: Arc<std::sync::Mutex<DownloadStatus>>,
    pub final_path: PathBuf, // Store the final download path
    pub size: Option<u64>, // Expected file size for prioritization and verification
    pub attempt_number: Arc<std::sync::atomic::AtomicUsize>, // Track which attempt number this is (0 = first attempt)

    // New fields for chunking
    pub chunk_tasks: Arc<std::sync::Mutex<Vec<Arc<DownloadTask>>>>,
    pub chunk_path: PathBuf, // Full path to the chunk file (for master: .part, for chunks: .part-O{offset})
    pub chunk_offset: u64, // Starting byte offset for this chunk
    pub chunk_size: u64, // Size of this chunk in bytes
    pub start_time: Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    pub received_bytes: Arc<std::sync::atomic::AtomicU64>, // Bytes actually received from network
    pub resumed_bytes: Arc<std::sync::atomic::AtomicU64>, // Bytes reused from local partial files
}

#[derive(Debug, Clone)]
pub enum DownloadStatus {
    Pending,
    Downloading,
    Completed,
    Failed(String),
}

/// Helper function to update a download task's status
///
/// This function handles the common pattern of updating a task's status
/// while properly handling mutex locks and error reporting.
pub fn update_download_status(task: &DownloadTask, new_status: DownloadStatus) -> Result<()> {
    let mut status = task.status.lock()
        .map_err(|e| eyre!("Failed to lock download status mutex: {}", e))?;
    *status = new_status;
    Ok(())
}

impl DownloadTask {
    pub fn new(url: String, output_dir: PathBuf, max_retries: usize) -> Self {
        Self::with_size(url, output_dir, max_retries, None)
    }

    pub fn with_size(url: String, output_dir: PathBuf, max_retries: usize, size: Option<u64>) -> Self {
        let final_path = Mirrors::resolve_mirror_path(&url, &output_dir);
        // Initialize chunk_path to the standard .part file for master tasks
        let chunk_path = final_path.with_extension("part");

        Self {
            url,
            output_dir,
            max_retries,
            data_channel: None,
            status: Arc::new(std::sync::Mutex::new(DownloadStatus::Pending)),
            final_path,
            size,
            attempt_number: Arc::new(std::sync::atomic::AtomicUsize::new(0)), // Initialize to 0 (first attempt)
            chunk_tasks: Arc::new(std::sync::Mutex::new(Vec::new())),
            chunk_path,
            chunk_offset: 0,
            chunk_size: 0,
            start_time: Arc::new(std::sync::Mutex::new(None)),
            received_bytes: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            resumed_bytes: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    pub fn with_data_channel(mut self, channel: Sender<Vec<u8>>) -> Self {
        self.data_channel = Some(channel);
        self
    }

    pub fn get_status(&self) -> DownloadStatus {
        self.status.lock()
            .unwrap_or_else(|e| panic!("Failed to lock download status mutex: {}", e))
            .clone()
    }

    /// Check if this is a master task (has chunk tasks)
    pub fn is_master_task(&self) -> bool {
        !self.is_chunk_task()
    }

    /// Check if this is a chunk task (has non-zero offset or is explicitly a chunk)
    pub fn is_chunk_task(&self) -> bool {
        self.chunk_path.to_string_lossy().contains(".part-O")
    }

    /// Check if this is the first download attempt for this task
    /// This is more reliable than checking the retries parameter in download_file
    pub fn is_first_attempt(&self) -> bool {
        self.attempt_number.load(std::sync::atomic::Ordering::SeqCst) == 0
    }

    /// Increment the attempt number when a retry is needed
    pub fn increment_attempt(&self) {
        self.attempt_number.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Reset the attempt number to zero
    pub fn reset_attempt(&self) {
        self.attempt_number.store(0, std::sync::atomic::Ordering::SeqCst);
    }



    /// Create a chunk task for a specific byte range
    pub fn create_chunk_task(&self, offset: u64, size: u64) -> Arc<DownloadTask> {
        // Create a chunk task with a specific offset and size
        // The chunk file will be named .part-O{offset}
        let chunk_path = format!("{}-O{}", self.chunk_path.to_string_lossy(), offset);

        Arc::new(DownloadTask {
            url: self.url.clone(),
            output_dir: self.output_dir.clone(),
            max_retries: self.max_retries,
            data_channel: None, // Chunks don't need data channels
            status: Arc::new(std::sync::Mutex::new(DownloadStatus::Pending)),
            final_path: self.final_path.clone(),
            size: Some(size),
            attempt_number: Arc::new(std::sync::atomic::AtomicUsize::new(0)), // Initialize to 0 (first attempt)

            chunk_tasks: Arc::new(std::sync::Mutex::new(Vec::new())),
            chunk_path: PathBuf::from(chunk_path),
            chunk_offset: offset,
            chunk_size: 0, // Will be set later if chunking is used
            start_time: Arc::new(std::sync::Mutex::new(None)),
            received_bytes: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            resumed_bytes: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        })
    }

    /// Get total progress bytes across all chunks (reused + network bytes)
    /// This represents the total download progress for display purposes
    pub fn get_total_progress_bytes(&self) -> u64 {
        let mut total_received = self.received_bytes.load(std::sync::atomic::Ordering::Relaxed);
        let mut total_reused = self.resumed_bytes.load(std::sync::atomic::Ordering::Relaxed);

        if let Ok(chunks) = self.chunk_tasks.lock() {
            for chunk in chunks.iter() {
                total_received += chunk.received_bytes.load(std::sync::atomic::Ordering::Relaxed);
                total_reused += chunk.resumed_bytes.load(std::sync::atomic::Ordering::Relaxed);
            }
        }

        total_received + total_reused
    }

    /// Get total bytes actually received from network (excluding reused local files)
    /// This is used for accurate rate calculation and time estimation
    pub fn get_network_bytes(&self) -> u64 {
        let mut total = self.received_bytes.load(std::sync::atomic::Ordering::Relaxed);

        if let Ok(chunks) = self.chunk_tasks.lock() {
            for chunk in chunks.iter() {
                total += chunk.received_bytes.load(std::sync::atomic::Ordering::Relaxed);
            }
        }

        total
    }
}

pub struct DownloadManager {
    client: Agent,
    multi_progress: MultiProgress,
    tasks: Arc<std::sync::Mutex<HashMap<String, DownloadTask>>>,
    nr_parallel: usize,
    task_handles: Arc<std::sync::Mutex<Vec<thread::JoinHandle<()>>>>,
    chunk_handles: Arc<std::sync::Mutex<Vec<thread::JoinHandle<()>>>>,
    is_processing: Arc<std::sync::atomic::AtomicBool>,
    current_task_count: Arc<std::sync::atomic::AtomicUsize>,
}

impl DownloadManager {
    pub fn new(nr_parallel: usize, proxy: &str) -> Result<Self> {
        let mut config_builder = Agent::config_builder()
            .user_agent("curl/8.13.0") // necessary to avoid download error for some URLs
            .timeout_connect(Some(Duration::from_secs(5)));

        if !proxy.is_empty() {
            match Proxy::new(proxy) {
                Ok(p) => {
                    config_builder = config_builder.proxy(Some(p));
                }
                Err(e) => {
                    log::error!("Failed to create proxy from {}: {}", proxy, e);
                    panic!("Failed to create proxy: {}", e);
                }
            }
        }
        // If proxy.is_empty(), .proxy() is not called on config_builder.
        // This allows ureq::Agent to use its default proxy detection (e.g., from environment variables).

        let client = config_builder.build().into();
        let multi_progress = MultiProgress::new();

        Ok(Self {
            client,
            multi_progress,
            tasks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            nr_parallel,
            task_handles: Arc::new(std::sync::Mutex::new(Vec::new())),
            chunk_handles: Arc::new(std::sync::Mutex::new(Vec::new())),
            is_processing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            current_task_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    pub fn submit_task(&self, task: DownloadTask) -> Result<()> {
        let mut tasks = self.tasks.lock()
            .map_err(|e| eyre!("Failed to lock tasks mutex: {}", e))?;
        if !tasks.contains_key(&task.url) {
            tasks.insert(task.url.clone(), task);
        }
        Ok(())
    }

    pub fn wait_for_task(&self, task_url: String) -> Result<DownloadStatus> {
        loop {
            let tasks = self.tasks.lock()
                .map_err(|e| eyre!("Failed to lock tasks mutex: {}", e))?;
            if let Some(task) = tasks.get(&task_url) {
                let status = task.get_status();
                match status {
                    DownloadStatus::Completed | DownloadStatus::Failed(_) => return Ok(status),
                    _ => {}
                }
            } else {
                drop(tasks);
                return Err(eyre!("Task with URL {} not found", task_url));
            }
            drop(tasks);
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Wait for any download task to complete and return the completed task's URL
    pub fn wait_for_any_task(&self, task_urls: &[String]) -> Result<Option<String>> {
        if task_urls.is_empty() {
            return Ok(None);
        }

        loop {
            let tasks = self.tasks.lock()
                .map_err(|e| eyre!("Failed to lock tasks mutex: {}", e))?;

            // Check if any of the specified tasks have completed
            for task_url in task_urls {
                if let Some(task) = tasks.get(task_url) {
                    match task.get_status() {
                        DownloadStatus::Completed => {
                            return Ok(Some(task_url.clone()));
                        }
                        DownloadStatus::Failed(err) => {
                            return Err(eyre!("Download failed for {}: {}", task_url, err));
                        }
                        _ => {} // Still downloading or pending
                    }
                }
            }

            drop(tasks);
            thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn start_processing(&self) {
        if self.is_processing.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }

        self.is_processing.store(true, std::sync::atomic::Ordering::Relaxed);
        let tasks = Arc::clone(&self.tasks);
        let client = self.client.clone();
        let multi_progress = self.multi_progress.clone();
        let is_processing = Arc::clone(&self.is_processing);
        let task_handles = Arc::clone(&self.task_handles);
        let chunk_handles = Arc::clone(&self.chunk_handles);
        let nr_parallel = self.nr_parallel;
        let current_task_count_arc = Arc::clone(&self.current_task_count);

        // Spawn the main processing thread
        thread::spawn(move || {
            loop {
                // Clean up finished task handles
                Self::cleanup_finished_handles(&task_handles);
                Self::cleanup_finished_handles(&chunk_handles);

                let mut tasks_guard = match tasks.lock() {
                    Ok(guard) => guard,
                    Err(e) => {
                        log::error!("Failed to lock tasks mutex: {}", e);
                        is_processing.store(false, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                };

                let pending_tasks: Vec<_> = tasks_guard.iter_mut()
                    .filter(|(_, t)| matches!(t.get_status(), DownloadStatus::Pending))
                    .collect();

                if pending_tasks.is_empty() {
                    // Check if all tasks are completed or failed
                    let all_done = tasks_guard.iter()
                        .all(|(_, t)| matches!(t.get_status(), DownloadStatus::Completed | DownloadStatus::Failed(_)));
                    if all_done {
                        is_processing.store(false, std::sync::atomic::Ordering::Relaxed);
                        break;
                    }
                    drop(tasks_guard);
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }

                // Sort pending tasks by size (largest first)
                let mut sorted_pending = pending_tasks;
                sorted_pending.sort_by(|(_, a), (_, b)| {
                    let size_a = a.size.unwrap_or(0);
                    let size_b = b.size.unwrap_or(0);
                    size_b.cmp(&size_a) // Descending order (largest first)
                });

                // Check how many task threads are currently running
                let mut current_task_count = {
                    let handles_guard = task_handles.lock().unwrap();
                    let count = handles_guard.len();
                    is_processing.load(std::sync::atomic::Ordering::Relaxed); // Ensure memory ordering
                    count
                };

                // Spawn new task threads if we have capacity
                for (_task_url, task) in sorted_pending {
                    current_task_count_arc.store(current_task_count, std::sync::atomic::Ordering::Relaxed);
                    if current_task_count >= nr_parallel {
                        break; // We've reached our task thread limit
                    }

                    let client = client.clone();
                    let multi_progress = multi_progress.clone();
                    let task_clone = task.clone();
                    let _task_handles_clone = Arc::clone(&task_handles);

					/*
					 * CRITICAL: We must take() the data_channel here to prevent recv() side from blocking forever.
					 *
					 * Problem: The data_channel sender is stored in the DownloadTask which lives in self.tasks HashMap.
					 * Since tasks are stored permanently (for deduplication), the sender side of the channel
					 * remains alive even after download completes. This means any recv() calls on the receiver
					 * side will block indefinitely waiting for more data, because the channel is never closed.
					 *
					 * Solution: By calling take() here, we move the sender out of the task and into the download
					 * thread. When the download thread exits (successfully or with error), the sender is
					 * automatically dropped, which closes the channel and unblocks any recv() calls.
					 *
					 * This is especially important for async submission patterns where the caller submits a
					 * download task and immediately starts reading from the receiver without waiting for
					 * task completion. Without take(), the receiver would hang forever even after the
					 * download finishes.
					 *
					 * Note: We need iter_mut() above to get &mut DownloadTask so we can call take().
					 */
                    task.data_channel.take();

                    // Mark task as downloading
                    match task.status.lock() {
                        Ok(mut status) => *status = DownloadStatus::Downloading,
                        Err(e) => {
                            log::error!("Failed to lock task status mutex: {}", e);
                            continue;
                        }
                    };

                    // Spawn task thread
                    let handle = thread::spawn(move || {
                        if let Err(e) = download_task(
                            &client,
                            &task_clone,
                            &multi_progress,
                        ) {
                            log::error!("Download task failed for {}: {}", task_clone.url, e);
                        }

                        // Remove self from task handles when done
                        // Note: We can't remove by handle since we're inside the thread
                        // The cleanup will happen in the next iteration
                    });

                    // Store the handle
                    if let Ok(mut handles_guard) = task_handles.lock() {
                        handles_guard.push(handle);
                        current_task_count += 1;
                    }
                }

                drop(tasks_guard);

                // Start chunk processing for existing tasks
                Self::start_chunks_processing(&tasks, &chunk_handles, nr_parallel);

                thread::sleep(Duration::from_millis(100));
            }
        });
    }

    /// Clean up finished thread handles
    fn cleanup_finished_handles(handles: &Arc<std::sync::Mutex<Vec<thread::JoinHandle<()>>>>) {
        if let Ok(mut handles_guard) = handles.lock() {
            let mut unfinished_handles = Vec::new();

            // Drain all handles and separate finished from unfinished
            for handle in handles_guard.drain(..) {
                if handle.is_finished() {
                    // Handle is finished, join it to clean up
                    let _ = handle.join();
                } else {
                    // Keep unfinished handles
                    unfinished_handles.push(handle);
                }
            }

            // Put back the unfinished handles
            *handles_guard = unfinished_handles;
        }
    }

    /// Internal chunk processing that doesn't require external chunk list
    fn start_chunks_processing(
        tasks: &Arc<std::sync::Mutex<HashMap<String, DownloadTask>>>,
        chunk_handles: &Arc<std::sync::Mutex<Vec<thread::JoinHandle<()>>>>,
        nr_parallel: usize
    ) {
        let max_chunk_threads = nr_parallel * 2;

        // Get current chunk thread count
        let current_chunk_count = {
            let handles_guard = match chunk_handles.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };
            handles_guard.len()
        };

        if current_chunk_count >= max_chunk_threads {
            return; // Already at capacity
        }

        // Collect all chunk tasks from all download tasks
        let mut all_chunks = Vec::new();
        if let Ok(tasks_guard) = tasks.lock() {
            for (_url, download_task) in tasks_guard.iter() {
                if let Ok(chunks) = download_task.chunk_tasks.lock() {
                    for chunk in chunks.iter() {
                        if matches!(chunk.get_status(), DownloadStatus::Pending) {
                            let priority = chunk.chunk_offset as f64 / chunk.size.unwrap_or(1) as f64;
                            all_chunks.push((priority, Arc::clone(chunk)));
                        }
                    }
                }
            }
        }

        // Sort chunks by priority (chunk_offset / size)
        all_chunks.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        // Spawn chunk threads up to the limit
        let threads_to_spawn = std::cmp::min(
            max_chunk_threads - current_chunk_count,
            all_chunks.len()
        );

        for (_, chunk_task) in all_chunks.into_iter().take(threads_to_spawn) {
            let chunk_clone = Arc::clone(&chunk_task);
            let _chunk_handles_clone = Arc::clone(chunk_handles);

            // Resolve mirror URL before spawning thread to ensure consistency
            let resolved_url = match resolve_mirror_in_url(&chunk_clone.url, 0, &chunk_clone.output_dir) {
                Ok((resolved_url, final_path)) => (resolved_url, final_path),
                Err(e) => {
                    log::error!("Failed to resolve mirror for chunk {}: {}", chunk_clone.url, e);
                    continue; // Skip this chunk if mirror resolution fails
                }
            };

            let handle = thread::spawn(move || {
                let client = Agent::config_builder()
                    .user_agent("curl/8.13.0")
                    .timeout_connect(Some(Duration::from_secs(5)))
                    .build()
                    .into();

                // Mark chunk as started
                if let Ok(mut start_time) = chunk_clone.start_time.lock() {
                    *start_time = Some(std::time::Instant::now());
                }

                let chunk_result = download_chunk_task(
                    &client,
                    &chunk_clone,
                    &resolved_url.0,
                );

                // Ensure mirror usage tracking is ended using the same resolved URL
                track_mirror_end_from_url(&resolved_url.0);

                if let Err(e) = chunk_result {
                    log::error!("Chunk task failed for {} at offset {}: {}",
                               chunk_clone.url, chunk_clone.chunk_offset, e);

                    // Mark chunk as failed
                    if let Ok(mut status) = chunk_clone.status.lock() {
                        *status = DownloadStatus::Failed(format!("{}", e));
                    }
                }
            });

            // Store the chunk handle
            if let Ok(mut handles_guard) = chunk_handles.lock() {
                handles_guard.push(handle);
            }
        }
    }

    #[allow(dead_code)]
    pub fn wait_for_all_tasks(&self) -> Result<()> {
        while self.is_processing.load(std::sync::atomic::Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(100));
        }

        // Check for any failed tasks
        let tasks = match self.tasks.lock() {
            Ok(guard) => guard,
            Err(e) => {
                log::error!("Failed to lock tasks mutex: {}", e);
                return Err(eyre!("Failed to lock tasks mutex: {}", e));
            }
        };
        let errors: Vec<String> = tasks.iter()
            .filter_map(|(_, t)| {
                if let DownloadStatus::Failed(e) = t.get_status() {
                    Some(format!("Failed to download {}: {}", t.url, e))
                } else {
                    None
                }
            })
            .collect();

        if !errors.is_empty() {
            let error_count = errors.len();
            let error_details = errors.join("\n");
            return Err(eyre!(
                "{} downloads failed:\n{}",
                error_count,
                error_details
            ));
        }

        Ok(())
    }
}

// Main Features:
//
// 1. Concurrent Downloads
//   - Supports parallel downloads with configurable concurrency limit
//   - Uses custom thread management with JoinHandle tracking
//
// 2. Resumable Downloads
//   - Creates .part files during download
//   - Uses HTTP Range headers to resume interrupted downloads
//   - Automatically handles servers that don't support resuming
//
// 3. Error Handling
//   - Distinguishes between fatal (4xx) and transient errors
//   - Implements exponential backoff for retries
//   - Configurable maximum retry count
//
// 4. Progress Tracking
//   - Shows download progress with indicatif progress bars
//   - Tracks downloaded bytes across retries
//   - Displays ETA and transfer speed
//
// 5. File Management
//   - Downloads to .part files first
//   - Renames to final filename only after successful completion
//   - Cleans up partial files on failure
//
// 6. Robustness Features
//   - Verifies downloaded file size matches Content-Length
//   - Handles network interruptions gracefully
//   - Implements proper timeouts
//
// 7. User Feedback
//   - Provides clear status messages
//   - Shows retry attempts and delays
//   - Indicates when downloads are complete
//
// 8. Safety Features
//   - Skips already downloaded files
//   - Ensures atomic completion with file renaming
//   - Properly cleans up resources on errors
//
// 9. Blocking I/O
//   - Uses blocking I/O instead of async/await
//   - Relies on custom thread management for parallelism
//   - Avoids tokio async runtime dependencies
//
// 10. Cross-Platform
//   - Uses rustls for TLS instead of OpenSSL
//   - Works on all major platforms (Linux, macOS, Windows)
//
// 11. Lightweight
//   - Minimal dependencies
//   - No async runtime overhead
//   - Efficient resource usage

#[derive(Debug)]
struct FatalError(String);

impl std::fmt::Display for FatalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for FatalError {}



/// Helper function to extract distro and format info from URL for mirror selection


/*
 * ============================================================================
 * DOWNLOAD-TIME MIRROR RESOLUTION SYSTEM
 * ============================================================================
 *
 * INTELLIGENT RESOLUTION STRATEGY:
 *
 * This system implements mirror resolution at download time rather than at
 * repo metadata preparation time. This provides several key advantages:
 *
 * 1. **Failure Recovery**: Can switch mirrors on download failures
 * 2. **Load Balancing**: Distributes downloads across multiple mirrors
 * 3. **Performance Optimization**: Uses real-time performance data
 * 4. **Retry Intelligence**: Selects different mirrors for retry attempts
 *
 * RETRY LOGIC:
 *
 * - First attempt: Use optimal mirror with normal concurrent limits
 * - Retry attempts: Reduce concurrent limits to encourage different selection
 * - This naturally provides fallback to less-loaded mirrors on failures
 *
 * DISTRO-AGNOSTIC DESIGN:
 *
 * Since mirrors are pre-filtered by distro at initialization, this function
 * no longer needs to extract distro information from URLs or pass it through
 * the selection chain. This eliminates heuristic parsing and simplifies
 * the resolution logic significantly.
 */

/// Resolve mirror placeholder in URL with smart mirror selection
///
/// Uses pre-filtered mirrors and intelligent retry logic for optimal performance
fn resolve_mirror_in_url(url: &str, retry_count: usize, output_dir: &Path) -> Result<(String, PathBuf)> {
    if !url.contains("$mirror") {
        // For URLs without mirror placeholders, return as-is
        return Ok((url.to_string(), Mirrors::resolve_mirror_path(&url, output_dir)));
    }

    let mirrors = MIRRORS.lock()
        .map_err(|e| eyre!("Failed to lock mirrors: {}", e))?;

    // For retries, try different mirrors by adjusting the max concurrent limit
    // This encourages selection of different mirrors on failures
    let max_concurrent = if retry_count == 0 { 10 } else { 5 };

    let selected_mirror = mirrors.select_mirror_with_usage_tracking(max_concurrent)?;

    // Get distro directory for the selected mirror
    let distro = &crate::models::channel_config().distro;
    let arch = &crate::models::channel_config().arch;
    let distro_dir = crate::mirror::Mirrors::find_distro_dir(&selected_mirror, distro, arch);
    let final_distro_dir = if distro_dir.is_empty() { distro } else { &distro_dir };

    let url_formatted = mirrors.format_mirror_url(&selected_mirror.url, selected_mirror.top_level, final_distro_dir)?;

    let resolved_url = url.replace("$mirror", &url_formatted);

    let final_path = Mirrors::resolve_mirror_path(&url, output_dir);

    log::debug!("Mirror resolution: {} -> {} (retry: {})", url, resolved_url, retry_count);

    Ok((resolved_url, final_path))
}

/// Track mirror usage for a URL (extract base mirror and increment)
fn track_mirror_start_from_url(url: &str) {
    if let Some(mirror_part) = extract_mirror_base_from_url(url) {
        track_mirror_usage_start(&mirror_part);
    }
}

/// Stop tracking mirror usage for a URL
fn track_mirror_end_from_url(url: &str) {
    if let Some(mirror_part) = extract_mirror_base_from_url(url) {
        track_mirror_usage_end(&mirror_part);
    }
}

/// Extract mirror base URL from a resolved URL
fn extract_mirror_base_from_url(url: &str) -> Option<String> {
    if let Some(triple_slash_pos) = url.find("///") {
        Some(url[..triple_slash_pos].to_string())
    } else if let Some(scheme_end) = url.find("://") {
        let after_scheme = &url[scheme_end + 3..];
        if let Some(path_start) = after_scheme.find('/') {
            Some(format!("{}://{}", &url[..scheme_end], &after_scheme[..path_start]))
        } else {
            Some(url.to_string())
        }
    } else {
        None
    }
}

/*
 * ============================================================================
 * SIMPLIFIED MIRROR SYSTEM
 * ============================================================================
 *
 * STREAMLINED INITIALIZATION:
 *
 * The mirror system is now initialized directly with distro filtering at startup,
 * eliminating the need for complex runtime initialization logic:
 *
 * 1. **Immediate Filtering**: Uses channel_config().distro at LazyLock time
 * 2. **Performance Data Loading**: Historical logs loaded during initialization
 * 3. **No Runtime Setup**: Mirrors are ready for use immediately
 * 4. **Simplified Code Path**: Removes initialization complexity from download path
 *
 * This provides better performance and reliability while simplifying the codebase.
 */

pub static DOWNLOAD_MANAGER: LazyLock<DownloadManager> = LazyLock::new(|| {
    DownloadManager::new(config().common.nr_parallel, &config().common.proxy)
        .expect("Failed to initialize download manager")
});

pub fn submit_download_task(task: DownloadTask) -> Result<()> {
    DOWNLOAD_MANAGER.submit_task(task)
}

/// Wait for any of the specified download tasks to complete
pub fn wait_for_any_download_task(task_urls: &[String]) -> Result<Option<String>> {
    DOWNLOAD_MANAGER.wait_for_any_task(task_urls)
}

pub fn download_urls(
    urls: Vec<String>,
    output_dir: &Path,
    max_retries: usize,
    async_mode: bool,
) -> Result<Vec<DownloadTask>> {
    let mut submitted_tasks = Vec::new();
    let mut task_urls = Vec::new();
    for url in urls {
        let url_for_context = url.clone();
        let task = DownloadTask::new(url.clone(), output_dir.to_path_buf(), max_retries);

        // Submit the task - if URL already exists, it will just replace/reuse
        submit_download_task(task.clone())
            .with_context(|| format!("Failed to submit download task for {}", url_for_context))?;
        submitted_tasks.push(task);
        task_urls.push(url);
    }
    fs::create_dir_all(output_dir)
        .with_context(|| format!("Failed to create output directory: {}", output_dir.display()))?;
    DOWNLOAD_MANAGER.start_processing();

    if !async_mode {
        // Wait for each task using the URLs
        for (i, task_url) in task_urls.iter().enumerate() {
            DOWNLOAD_MANAGER.wait_for_task(task_url.clone())
                .with_context(|| format!("Failed to wait for download task {} (URL: {})", i, task_url))?;
        }
        Ok(Vec::new())
    } else {
        Ok(submitted_tasks)
    }
}

/// Checks if a package file exists with matching size and can be considered already downloaded
fn check_existing_package_file(task: &DownloadTask) -> Result<Option<()>> {
    let final_path = &task.final_path;

    if final_path.exists() && task.size.is_some() {
        let file_path = final_path.to_string_lossy();
        let is_package_file = file_path.ends_with(".deb") ||
                              file_path.ends_with(".rpm") ||
                              file_path.ends_with(".apk") ||
                              file_path.ends_with(".conda") ||
                              file_path.ends_with(".pkg.tar.zst");

        if is_package_file {
            if let Ok(metadata) = fs::metadata(final_path) {
                let actual_size = metadata.len();
                if actual_size == task.size.unwrap() {
                    log::info!("File {} already exists with correct size {}, treating as already downloaded",
                              final_path.display(), actual_size);

                    // Mark task as completed
                    update_download_status(task, DownloadStatus::Completed)?;
                    return Ok(Some(()));
                }
            }
        }
    }

    Ok(None)
}

/// Prepare the download environment (rename existing file, create directories)
fn prepare_download_environment(final_path: &Path, part_path: &Path) -> Result<()> {
    if final_path.exists() {
        fs::rename(final_path, part_path)
            .with_context(|| format!("Failed to rename file: {} to {}", final_path.display(), part_path.display()))?;
    }

    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directory for {}: {}", final_path.display(), parent.display()))?;
    }

    Ok(())
}

/// Setup progress bar for download
fn setup_progress_bar(multi_progress: &MultiProgress, url: &str) -> Result<ProgressBar> {
    let pb = multi_progress.add(ProgressBar::new(0));
    pb.set_style(ProgressStyle::default_bar()
        .template("[{elapsed_precise}] [{bar:10}] {bytes_per_sec:12} ({eta}) {msg}")
        .map_err(|e| eyre!("Failed to parse HTTP response: {}", e))?
        .progress_chars("=> "));
    pb.set_message(url.to_string());

    Ok(pb)
}

/// Verify downloaded file size against expected size
fn verify_file_size(part_path: &Path, expected_size: Option<u64>, url: &str) -> Result<()> {
    if let Some(expected) = expected_size {
        if let Ok(metadata) = fs::metadata(part_path) {
            let actual_size = metadata.len();
            if actual_size != expected {
                let error_msg = format!(
                    "Downloaded file size mismatch: expected {} bytes, got {} bytes",
                    expected, actual_size
                );
                log::warn!("{} for {}", error_msg, url);
                // Note: We could make this a hard error, but for now just warn
                // since size information might not always be accurate
            } else {
                log::debug!("Downloaded file size verified: {} bytes for {}", actual_size, url);
            }
        }
    }

    Ok(())
}

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

/// Downloads a file from a URL to the output directory.
/// Uses the final_path that was calculated when the task was created.
///
/// Ensures optimal mirror configuration and comprehensive performance logging
fn download_task(
    client: &Agent,
    task: &DownloadTask,
    multi_progress: &MultiProgress,
) -> Result<()> {
    let url = &task.url;
    let final_path = &task.final_path;
    let data_channel = &task.data_channel;
    let max_retries = task.max_retries;
    let expected_size = task.size;

    log::debug!("download_task starting for {}, has_channel: {}, expected_size: {:?}", url, data_channel.is_some(), expected_size);

    // Handle local file URLs (file:// or starting with /)
    if url.starts_with("file://") || url.starts_with("/") {
        return handle_local_file(url, final_path, task);
    }

    // Check for existing downloads and clean up stale PID files
    check_and_cleanup_existing_downloads(final_path)?;

    // Create PID file for process coordination
    let pid_file = create_pid_file(final_path)?;

    let part_path = final_path.with_extension("part");

    // Check if we can skip download for existing package files
    if let Some(()) = check_existing_package_file(task)? {
        cleanup_pid_file(&pid_file)?;
        return Ok(());
    }

    // Try to recover from previous chunked downloads
    let _recovered_chunks = recover_chunked_download(task)?;

    // Prepare download environment
    prepare_download_environment(final_path, &part_path)?;

    // Setup progress bar
    let pb = setup_progress_bar(multi_progress, url)?;

    // Start the download - download_file_with_retries handles mirror resolution
    log::debug!("download_task calling download_file_with_retries for {}", url);
    let result = download_file_with_retries(
        client,
        url, // Pass original URL, mirror resolution happens in download_file_with_retries
        &part_path,
        &pb,
        max_retries,
        data_channel.clone(),
        task,
    );
    log::debug!("download_task download_file_with_retries completed for {}, result: {:?}", url, result);

    // Clean up PID file regardless of result
    let _pid_cleanup_result = cleanup_pid_file(&pid_file);

    // Update progress bar based on result
    if result.is_ok() {
        pb.finish_with_message(format!("Downloaded {}", final_path.display()));
    } else {
        pb.finish_with_message(format!("Error: {:?}", result));
    }

    // Handle download result
    match result {
        Ok(()) => {
            // Verify file size
            verify_file_size(&part_path, expected_size, url)?;

            // Finalize download atomically
            atomic_file_completion(&part_path, final_path)?;

            // Mark task as completed
            update_download_status(task, DownloadStatus::Completed)?;

            Ok(())
        },
        Err(e) => update_download_status(task, DownloadStatus::Failed(format!("{}", e))),
    }
}

/// Handles local file URLs by copying them to the destination
///
/// - Supports file:// URLs and absolute paths starting with /
/// - Creates parent directories as needed
/// - Marks the download task as completed
/// - Verifies file size if expected size is provided
fn handle_local_file(url: &str, final_path: &Path, task: &DownloadTask) -> Result<()> {
    let source_path = if url.starts_with("file://") {
        Path::new(&url[7..])
    } else {
        Path::new(url)
    };

    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directory for {}: {}", final_path.display(), parent.display()))?;
    }

    fs::copy(source_path, final_path)
        .with_context(|| format!("Failed to copy file from {} to {}", source_path.display(), final_path.display()))?;

    // Verify file size if expected size is provided
    if let Some(expected_size) = task.size {
        if let Ok(metadata) = fs::metadata(final_path) {
            let actual_size = metadata.len();
            if actual_size != expected_size {
                let error_msg = format!(
                    "Local file size mismatch: expected {} bytes, got {} bytes",
                    expected_size, actual_size
                );
                log::warn!("{} for {}", error_msg, url);
                // Note: We could make this a hard error, but for now just warn
                // since size information might not always be accurate
            } else {
                log::debug!("Local file size verified: {} bytes for {}", actual_size, url);
            }
        }
    }

    // Mark task as completed
    update_download_status(task, DownloadStatus::Completed)?;

    Ok(())
}

fn download_file_with_retries(
    client: &Agent,
    url: &str,
    part_path: &Path,
    pb: &ProgressBar,
    max_retries: usize,
    data_channel: Option<Sender<Vec<u8>>>,
    task: &DownloadTask,
) -> Result<()> {
    log::debug!("download_file_with_retries starting for {}, has_channel: {}", url, data_channel.is_some());
    let mut retries = 0;
    let mut current_mirror_url: Option<String> = None;

    loop {
        // Resolve mirror for this attempt (try different mirrors on retries)
        let resolved_url = match resolve_mirror_in_url(url, retries, &task.output_dir) {
            Ok((resolved, final_path)) => {
                log::debug!("Mirror resolved for attempt {}: {} -> {}", retries + 1, url, resolved);
                if resolved != url {
                    current_mirror_url = Some(resolved.clone());
                }
                resolved
            }
            Err(e) => {
                log::warn!("Failed to resolve mirror for {}: {}", url, e);
                return Err(eyre!("Mirror resolution failed: {}", e));
            }
        };

        // Track mirror usage start
        if current_mirror_url.is_some() {
            track_mirror_start_from_url(&resolved_url);
        }

        log::debug!("download_file_with_retries calling download_file for {}, attempt {}", resolved_url, retries + 1);
        log::debug!("About to call download_file with data_channel.is_some() = {}", data_channel.is_some());

        let download_result = download_file(client, &resolved_url, &part_path, pb, retries, &data_channel, task);

        // Track mirror usage end
        if current_mirror_url.is_some() {
            track_mirror_end_from_url(&resolved_url);
        }

        match download_result {
            Ok(()) => {
                log::debug!("download_file_with_retries completed successfully for {}, dropping channel", resolved_url);

                return Ok(());
            },
            Err(e) => {
                log::debug!("download_file_with_retries got error for {}: {:?}", resolved_url, e);

                // Check if this is a fatal error (like 404) that shouldn't be retried
                if e.downcast_ref::<FatalError>().is_some() {
                    log::info!("Skipping retries for fatal error (client error 4xx) for {}", resolved_url);
                    return Err(e);
                }

                if retries >= max_retries {
                    return Err(eyre!("Max retries ({}) exceeded for {}: {}", max_retries, resolved_url, e));
                }

                retries += 1;

                let delay = Duration::from_secs(2u64.pow(retries as u32));
                pb.println(format!("Retrying {} (attempt {}/{}) after {}s delay...", url, retries + 1, max_retries + 1, delay.as_secs()));
                thread::sleep(delay);
            }
        }
    }
}

pub fn send_file_to_channel(
    part_path: &Path,
    data_channel: &Sender<Vec<u8>>,
) -> Result<()> {
    // The channel receivers process_packages_content()/process_filelist_content() expect full file
    // to decompress and compute hash, so send the existing file content first. This fixes bug
    // "Decompression error: stream/file format not recognized"
    log::debug!("Sending file to channel: {}", part_path.display());

    // Check if file exists and get its size
    let file_metadata = match std::fs::metadata(part_path) {
        Ok(metadata) => {
            let size = metadata.len();
            log::debug!("File size: {} bytes", size);
            if size == 0 {
                log::warn!("File is empty: {}", part_path.display());
            }
            metadata
        },
        Err(e) => {
            let err_msg = format!("Failed to get metadata for file {}: {}", part_path.display(), e);
            log::error!("{}", err_msg);
            return Err(eyre!(err_msg));
        }
    };

    // Open the file
    let mut file = match std::fs::File::open(part_path) {
        Ok(file) => file,
        Err(e) => {
            let err_msg = format!("Failed to open file {}: {}", part_path.display(), e);
            log::error!("{}", err_msg);
            return Err(eyre!(err_msg));
        }
    };

    // Use a reasonably sized buffer for reading chunks
    // 1MB is a good balance between memory usage and number of channel sends
    const CHUNK_SIZE: usize = 64 * 1024; // 64KB chunks
    let mut buffer = vec![0; CHUNK_SIZE];
    let mut total_bytes_read = 0;
    let mut chunks_sent = 0;

    loop {
        // Read a chunk from the file
        match file.read(&mut buffer) {
            Ok(0) => {
                // End of file
                log::debug!("Reached end of file after reading {} bytes in {} chunks",
                          total_bytes_read, chunks_sent);
                break;
            },
            Ok(bytes_read) => {
                total_bytes_read += bytes_read;
                chunks_sent += 1;

                // Create a new buffer with just the bytes we read
                let chunk = buffer[..bytes_read].to_vec();

                // Send the chunk through the channel
                match data_channel.send(chunk) {
                    Ok(_) => {
                        if chunks_sent % 10 == 0 || bytes_read < CHUNK_SIZE {
                            log::trace!("Sent chunk {} ({} bytes, total {} bytes) for {}",
                                      chunks_sent, bytes_read, total_bytes_read, part_path.display());
                        }
                    },
                    Err(e) => {
                        let err_msg = format!("Failed to send chunk {} to channel: {}", chunks_sent, e);
                        log::error!("{}", err_msg);
                        return Err(eyre!(err_msg));
                    }
                }

                // If we read less than the buffer size, we've reached the end
                if bytes_read < CHUNK_SIZE {
                    log::debug!("Reached end of file (last chunk was smaller than buffer)");
                    break;
                }
            },
            Err(e) => {
                let err_msg = format!("Error reading chunk from file {}: {}", part_path.display(), e);
                log::error!("{}", err_msg);
                return Err(eyre!(err_msg));
            }
        }
    }

    // Verify we read the expected number of bytes
    if total_bytes_read != file_metadata.len() as usize {
        log::warn!("Read {} bytes but file size is {} bytes",
                 total_bytes_read, file_metadata.len());
    }

    log::debug!("Successfully sent file data to channel in {} chunks: {}",
              chunks_sent, part_path.display());
    Ok(())
}

// create_and_setup_chunks has been removed - use create_chunk_tasks directly

fn download_file(
    client: &Agent,
    url: &str,
    part_path: &Path,
    pb: &ProgressBar,
    _retries: usize,
    data_channel: &Option<Sender<Vec<u8>>>,
    task: &DownloadTask,
) -> Result<()> {
    log::debug!("download_file starting for {}, part_path: {}", url, part_path.display());

    // Step 1: Get existing file size
    let existing_bytes = get_existing_file_size(part_path)?;
    let mut chunks = Vec::new();

    // Step 2: If this is the first attempt and we have a known size, try to create chunks before HTTP request
    if task.is_first_attempt() && task.is_master_task() {
        task.increment_attempt(); // Mark this as no longer the first attempt

        // Try to create chunks based on known size before HTTP request
        if let Some(size) = task.size {
            if size >= MIN_FILE_SIZE_FOR_CHUNKING {
                log::debug!("Using known size {} bytes to create chunks before HTTP request", size);
                chunks = create_chunk_tasks(task, size, existing_bytes)?;

                if !chunks.is_empty() {
                    log::debug!("Created {} chunk tasks for {} byte file", chunks.len(), size);
                }
            }
        }
    }

    // Step 3: Make the HTTP request with range header if we have partial download
    let mut response = match make_download_request_with_416_handling(client, url, task, part_path, pb, data_channel) {
        Ok(response) => response,
        Err(e) => {
            // Check if this is the special case where download was skipped due to file unchanged
            if e.to_string().contains("Download skipped - file unchanged") {
                return Ok(());
            }
            return Err(e);
        }
    };
    let status = response.status();

    // Step 4: Validate response and handle resume logic
    validate_response_content_type(&response, url, pb)?;
    let new_existing_bytes = handle_resume_logic(part_path, pb, url, existing_bytes, status.as_u16())?;

    // Step 5: If resume failed (existing_bytes was reset to 0), reset chunk tasks and attempt counter
    if existing_bytes > 0 && new_existing_bytes == 0 {
        // Clear our local chunks vector
        if !chunks.is_empty() {
            log::debug!("Resume failed, clearing {} local chunk tasks", chunks.len());
            chunks.clear();
        }

        // Reset the master task's state
        if task.is_master_task() {
            log::debug!("Resume failed, clearing master chunk tasks and resetting attempt counter");
            task.reset_attempt();
        }

        // Return an error to let the caller try a different mirror
        return Err(eyre!("Server doesn't support resume for {}, try a different mirror", url));
    }

    // Update existing_bytes to the new value and set resumed_bytes for resumed downloads
    let existing_bytes = new_existing_bytes;

    // Set resumed_bytes to track bytes from existing partial file
    if existing_bytes > 0 {
        task.resumed_bytes.store(existing_bytes, std::sync::atomic::Ordering::Relaxed);
        log::debug!("Set resumed_bytes to {} bytes for resumed download", existing_bytes);
    }

    // Step 6: Setup progress tracking and get total file size
    let total_size = setup_progress_tracking(&response, pb, existing_bytes);
    if task.size.is_none() {
        // Use unsafe to modify the immutable task's size field
        unsafe {
            let task_mut = task as *const DownloadTask as *mut DownloadTask;
            (*task_mut).size = Some(total_size);
        }
    }

    if task.is_first_attempt() && task.is_master_task() {
        task.increment_attempt();
        // Step 7: Send existing file content to channel if resuming
        if existing_bytes > 0 {
            if let Some(channel) = data_channel {
                send_file_to_channel(part_path, channel).map_err(|e|
                    eyre!("Failed to send file '{}' to channel: {}", part_path.display(), e)
                )?;
            }
        }

        // Step 8: If we haven't created chunks yet and have total size, try to create them now
        if chunks.is_empty() {
            chunks = create_chunk_tasks(task, total_size, existing_bytes)?;

            if !chunks.is_empty() {
                log::debug!("Created {} chunk tasks for {} byte file using HTTP response size", chunks.len(), total_size);
            }
        }
    }

    // Step 9: Add all chunk tasks to the master task
    if !chunks.is_empty() {
        // Add all chunk tasks to the master task in one go
        if let Ok(mut master_chunks) = task.chunk_tasks.lock() {
            // Clear any existing chunks first to avoid duplicates
            master_chunks.clear();

            // Add all our local chunks to the master task
            for chunk in &chunks {
                master_chunks.push(Arc::clone(chunk));
            }
            log::info!("Added {} chunk tasks to master task for download {}", chunks.len(), task.url);
        }
    }

    // Set start time for estimation if not already set
    if let Ok(mut start_time) = task.start_time.lock() {
        if start_time.is_none() {
            *start_time = Some(std::time::Instant::now());
        }
    }

        let download_start = std::time::Instant::now();
    let final_downloaded = download_content(&mut response, part_path, pb, existing_bytes, data_channel, task)?;
    let total_duration = download_start.elapsed().as_millis() as u64;

    // Calculate network bytes transferred (excluding resumed bytes)
    let network_bytes = task.get_network_bytes();

    // Log comprehensive download performance
    if let Err(e) = append_download_log(
        url,
        network_bytes,
        total_duration,
        0, // We don't have the initial latency here, it was logged separately
        true,
        None,
        Some(!task.chunk_tasks.lock().unwrap().is_empty() || response.status().as_u16() == 206), // Range support if chunked or got 206
        Some(true),
    ) {
        log::warn!("Failed to log download completion: {}", e);
    }

    // If this is a master task with chunks, wait for all chunks to complete
    if task.is_master_task() {
        log::debug!("Master task waiting for chunks to complete");
        wait_for_chunks_and_merge(task, pb)?;

        // Update progress with final total
        let total_received = task.get_total_progress_bytes();
        pb.set_position(total_received);
        log::info!("Chunked download completed: {} bytes total for {}", total_received, url);
    }

    validate_download_size(final_downloaded, total_size, part_path)?;
    set_file_timestamp(&response, part_path);

    let filename = part_path.file_name()
        .ok_or_else(|| eyre!("Invalid filename in path: {}", part_path.display()))?;
    pb.finish_with_message(format!("Downloaded {}", filename.to_string_lossy()));

    Ok(())
}

/// Get the size of an existing partial file, or 0 if it doesn't exist
fn get_existing_file_size(part_path: &Path) -> Result<u64> {
    if part_path.exists() {
        log::debug!("download_file part file exists, getting metadata");
        match fs::metadata(part_path) {
            Ok(metadata) => {
                let size = metadata.len();
                log::debug!("download_file found existing part file with {} bytes", size);
                Ok(size)
            },
            Err(e) => {
                log::error!("download_file failed to get metadata for part file {}: {}", part_path.display(), e);
                Err(eyre!("Failed to get metadata for part file {}: {}", part_path.display(), e))
            }
        }
    } else {
        log::debug!("download_file no existing part file found");
        Ok(0)
    }
}

/// Make the HTTP download request with special handling for 416 errors
/// The 416 logic is kept inline as it needs access to part_path and data_channel
fn make_download_request_with_416_handling(
    client: &Agent,
    url: &str,
    task: &DownloadTask,
    part_path: &Path,
    pb: &ProgressBar,
    data_channel: &Option<Sender<Vec<u8>>>,
) -> Result<http::Response<ureq::Body>> {
    let mut request = client.get(url.replace("///", "/"))
        .config()
        .max_redirects(3)
        .build();

    // Get the download offset from task's chunk_offset or use the downloaded size
    let downloaded = task.chunk_offset;

    if downloaded > 0 {
        // If we have a chunk size, use a specific range
        if task.chunk_size > 0 {
            let end = downloaded + task.chunk_size - 1; // HTTP ranges are inclusive
            log::debug!("download_file setting Range header: bytes={}-{}", downloaded, end);
            request = request.header("Range", &format!("bytes={}-{}", downloaded, end));
        } else {
            // Otherwise request from offset to the end
            log::debug!("download_file setting Range header: bytes={}-", downloaded);
            request = request.header("Range", &format!("bytes={}-", downloaded));
        }
    }

    let request_start = std::time::Instant::now();
    match request.call() {
        Ok(response) => {
            let latency = request_start.elapsed().as_millis() as u64;
            // Log latency info for successful requests
            if let Err(e) = append_download_log(
                url,
                0, // No bytes transferred yet for this call
                latency,
                latency,
                true,
                None,
                None,
                Some(true), // Content was available since we got a response
            ) {
                log::warn!("Failed to log download latency: {}", e);
            }
            Ok(response)
        },
        Err(ureq::Error::StatusCode(code)) => {
            let latency = request_start.elapsed().as_millis() as u64;
            log::debug!("download_file got HTTP error code: {}", code);

            // Log the error for performance tracking
            if let Err(e) = append_download_log(
                url,
                0,
                latency,
                latency,
                false,
                Some(format!("HTTP {}", code)),
                None,
                Some(code != 404), // Content available unless it's 404
            ) {
                log::warn!("Failed to log download error: {}", e);
            }

            if code == 416 && downloaded > 0 {
                // The requested byte range is outside the size of the file
                log::debug!("download_file handling HTTP 416 with downloaded={}", downloaded);
                return handle_416_range_error(client, url, task, part_path, pb, data_channel);
            }
            let result = handle_non_416_http_error(code, url, pb);

            // Log the error handling
            if let Err(e) = append_download_log(
                url,
                0,
                latency,
                latency,
                false,
                Some(format!("HTTP {}", code)),
                None,
                Some(code != 404),
            ) {
                log::warn!("Failed to log HTTP error: {}", e);
            }

            result
        }
        Err(ureq::Error::Io(e)) => {
            let error_msg = format!("Network error: {} - {}", e, url);
            pb.finish_with_message(error_msg.clone());
            Err(eyre!("Download error: {}", error_msg))
        }
        Err(e) => {
            let error_msg = format!("Error downloading: {} - {}", e, url);
            pb.finish_with_message(error_msg.clone());
            Err(eyre!("Download error: {}", error_msg))
        }
    }
}

/// Handle 416 Range Not Satisfiable error with full access to required context
fn handle_416_range_error(
    client: &Agent,
    url: &str,
    task: &DownloadTask,
    part_path: &Path,
    pb: &ProgressBar,
    data_channel: &Option<Sender<Vec<u8>>>,
) -> Result<http::Response<ureq::Body>> {
    let downloaded = task.chunk_offset; // Use task's chunk_offset instead of downloaded parameter
    // Send a request to check remote size and time, then compare with local
    let metadata_request_start = std::time::Instant::now();
    let remote_metadata = client.get(url.replace("///", "/"))
        .config()
        .max_redirects(3)
        .build()
        .call()
        .with_context(|| format!("Failed to make HTTP request for {}", url))?;
    let metadata_latency = metadata_request_start.elapsed().as_millis() as u64;

    // Log metadata request latency
    if let Err(e) = append_download_log(
        url,
        0,
        metadata_latency,
        metadata_latency,
        true,
        None,
        None,
        Some(true),
    ) {
        log::warn!("Failed to log metadata request latency: {}", e);
    }

    let remote_size = parse_content_length(&remote_metadata);
    log::debug!("download_file remote_size: {}, local_size: {}", remote_size, downloaded);

    let remote_timestamp_opt = parse_remote_timestamp(&remote_metadata);

    let local_metadata = fs::metadata(part_path).map_err(|e| eyre!("Failed to get local file metadata: {}", e))?;
    let local_size = local_metadata.len();
    let local_last_modified_sys_time = local_metadata.modified().map_err(|e| eyre!("Failed to get local file modification time: {}", e))?;
    let local_last_modified: OffsetDateTime = local_last_modified_sys_time.into();

    if let Some(remote_ts) = remote_timestamp_opt {
        // A remote timestamp was successfully parsed from headers
        if remote_size == local_size && remote_ts == local_last_modified {
            log::debug!("download_file sizes and timestamps match (remote_ts: {}, local_ts: {}), skipping download.", remote_ts, local_last_modified);
            let message = format!("Remote file unchanged (size and timestamp match), skipping download");
            pb.finish_with_message(message.clone());
            if let Some(channel) = data_channel {
                send_file_to_channel(part_path, &channel).map_err(|e| eyre!("Failed to send file to channel: {}", e))?;
            }
            log::debug!("download_file returning Ok after skipping download due to matching size and timestamp.");
            // Return a dummy response since we're skipping the download
            return Err(eyre!("Download skipped - file unchanged"));
        } else {
            let mut reason = String::from("Remote file differs");
            if remote_size != local_size {
                reason.push_str(&format!(" (size mismatch: remote {}, local {})", remote_size, local_size));
            }
            if remote_ts != local_last_modified {
                reason.push_str(&format!(" (timestamp mismatch: remote {}, local {})", remote_ts, local_last_modified));
            }
            log::info!("{}, restarting download from 0.", reason);
            let error_msg = format!("{}, restarting download from 0.", reason);
            pb.finish_with_message(error_msg.clone());
            fs::remove_file(part_path).map_err(|e| eyre!("Failed to remove part file '{}': {}", part_path.display(), e))?;
            return Err(std::io::Error::new(std::io::ErrorKind::Other, error_msg).into());
        }
    } else {
        // No valid remote timestamp header found, or parsing failed. Download for safety.
        log::info!("No valid remote timestamp found or failed to parse. Re-downloading for safety (remote size: {}, local size: {}).", remote_size, local_size);
        let error_msg = format!("No remote timestamp, re-downloading for safety (current local size: {}, path: {})", local_size, part_path.display());
        pb.finish_with_message(error_msg.clone());
        fs::remove_file(part_path).map_err(|e| eyre!("Failed to remove part file '{}': {}", part_path.display(), e))?;
        return Err(std::io::Error::new(std::io::ErrorKind::Other, error_msg).into());
    }
}

/// Handle non-416 HTTP errors
fn handle_non_416_http_error(code: u16, url: &str, pb: &ProgressBar) -> Result<http::Response<ureq::Body>> {
    let error_msg = if code >= 400 && code < 500 {
        format!("HTTP {} error: {} - {}", code, "Client Error", url)
    } else {
        format!("HTTP {} error: {} - {}", code, "Server Error", url)
    };
    pb.finish_with_message(error_msg.clone());

    if code >= 400 && code < 500 {
        // For client errors (like 404), create a simple FatalError without verbose backtrace
        log::info!("Client error {} for {}, will not retry", code, url);
        Err(eyre!(FatalError(error_msg)))
    } else {
        Err(eyre!("HTTP error: {}", error_msg))
    }
}

/// Validate response content type to detect HTML login pages
fn validate_response_content_type(
    response: &http::Response<ureq::Body>,
    url: &str,
    pb: &ProgressBar,
) -> Result<()> {
    if let Some(content_type) = response.headers().get("Content-Type").and_then(|v| v.to_str().ok()) {
        if content_type.contains("text/html") {
            let error_msg = "Received HTML page instead of file. This may indicate an authentication issue with the server.";
            pb.finish_with_message(error_msg);
            return Err(eyre!("Fatal error while downloading from {}: {}", url, error_msg.to_string()));
        }
    }
    Ok(())
}

/// Handle resume logic - check if server supports resuming and adjust downloaded bytes accordingly
fn handle_resume_logic(
    part_path: &Path,
    pb: &ProgressBar,
    url: &str,
    existing_bytes: u64,
    status: u16,
) -> Result<u64> {
    if existing_bytes > 0 && status != 206 {
        // Log that server doesn't support range requests
        if let Err(e) = append_download_log(
            url,
            0,
            0,
            0,
            false,
            Some("No range support".to_string()),
            Some(false), // Server doesn't support range requests
            Some(true),
        ) {
            log::warn!("Failed to log range support info: {}", e);
        }

        fs::remove_file(part_path).map_err(|e| eyre!("Failed to remove part file '{}': {}", part_path.display(), e))?;
        pb.println(format!("Server doesn't support resume, restarting: {}", url));
        Ok(0)
    } else {
        Ok(existing_bytes)
    }
}

/// Set up progress tracking with total size and current position
fn setup_progress_tracking(response: &http::Response<ureq::Body>, pb: &ProgressBar, existing_bytes: u64) -> u64 {
    let content_length = parse_content_length(response);
    let total_size = content_length + existing_bytes; // Total file size

    // Set progress bar length to total file size
    pb.set_length(total_size);
    // Set position to already downloaded bytes (reused from partial file)
    pb.set_position(existing_bytes);

    total_size
}

/// Download the actual content from the response to the file
fn download_content(
    response: &mut http::Response<ureq::Body>,
    part_path: &Path,
    pb: &ProgressBar,
    mut existing_bytes: u64,
    data_channel: &Option<Sender<Vec<u8>>>,
    task: &DownloadTask,
) -> Result<u64> {
    // Open the file in append mode to resume partial downloads
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(true)
        .open(part_path).map_err(|e| eyre!("Failed to open file '{}' for writing (existing_bytes={}): {}", part_path.display(), existing_bytes, e))?;

    let mut reader = response.body_mut().as_reader();
    let mut buffer = vec![0; 8192];
    let mut last_update = std::time::Instant::now();
    let mut last_ondemand_check = std::time::Instant::now();

    // Track network bytes separately from total downloaded bytes
    let mut network_bytes = 0u64; // Bytes actually received from network

    // For master tasks, limit download to chunk size if chunking is active
    let is_master = task.is_master_task();

    loop {
        let bytes_read = reader.read(&mut buffer)
            .map_err(|e| eyre!("Failed to read from response (existing_bytes={}, buffer_size={}): {}", existing_bytes, buffer.len(), e))?;

        if bytes_read == 0 {
            break;
        }

        if is_master && task.chunk_size > 0 {
            let boundary = task.chunk_offset + task.chunk_size;
            if existing_bytes >= boundary {
                log::debug!("Master task reached chunk boundary at {} bytes, stopping", existing_bytes);
                break;
            }

            // Adjust bytes to read if we're approaching the limit
            let bytes_to_write = if existing_bytes + bytes_read as u64 > boundary {
                (boundary - existing_bytes) as usize
            } else {
                bytes_read
            };

            file.write_all(&buffer[..bytes_to_write])
                .map_err(|e| eyre!("Failed to write {} bytes to file '{}' (existing_bytes={}): {}", bytes_to_write, part_path.display(), existing_bytes, e))?;

            existing_bytes += bytes_to_write as u64;
            network_bytes += bytes_to_write as u64;

            // Store only network bytes in received_bytes
            task.received_bytes.store(network_bytes, std::sync::atomic::Ordering::Relaxed);

            if let Some(channel) = &data_channel {
                if let Err(_) = channel.send(buffer[..bytes_to_write].to_vec()) {
                    // Channel was closed, but we continue downloading
                }
            }

            if bytes_to_write < bytes_read {
                // Reached chunk boundary
                break;
            }
        } else {
            file.write_all(&buffer[..bytes_read])
                .map_err(|e| eyre!("Failed to write {} bytes to file '{}' (existing_bytes={}): {}", bytes_read, part_path.display(), existing_bytes, e))?;

            existing_bytes += bytes_read as u64;
            network_bytes += bytes_read as u64;

            // Store only network bytes in received_bytes
            task.received_bytes.store(network_bytes, std::sync::atomic::Ordering::Relaxed);

            if let Some(channel) = &data_channel {
                if let Err(_) = channel.send(buffer[..bytes_read].to_vec()) {
                    // Channel was closed, but we continue downloading
                }
            }
        }

        // Update progress bar more frequently for master tasks
        let now = std::time::Instant::now();
        if now.duration_since(last_update) > Duration::from_millis(300) {
            if task.is_master_task() {
                // For master tasks, show total progress across all chunks (reused + network bytes)
                let total_received = task.get_total_progress_bytes();
                pb.set_position(total_received);
            } else {
                // For chunk tasks, show total downloaded (reused + network bytes)
                pb.set_position(existing_bytes);
            }
            last_update = now;
        }

        // Check for on-demand chunking opportunity
        if is_master && task.chunk_size == 0 &&
            // The 1s delay serves two critical purposes:
            // 1. Data collection: Ensures we've downloaded enough bytes to accurately calculate
            //    download speed and estimate_remaining_time(), preventing premature chunking
            // 2. Task stabilization: Allows DOWNLOAD_MANAGER.current_task_count to stabilize.
            //    During 'epkg install', multiple downloads are submitted in quick succession.
            //    Without this delay, early tasks might see current_task_count = 1 when it should
            //    actually be much higher, leading to excessive chunking and thread creation.
            now.duration_since(last_ondemand_check) > Duration::from_secs(1) &&
            DOWNLOAD_MANAGER.current_task_count.load(std::sync::atomic::Ordering::Relaxed) <= DOWNLOAD_MANAGER.nr_parallel / 2 {

            let estimated_time = estimate_remaining_time(task);
            let remaining_size = task.size.unwrap() - existing_bytes;

            // Check conditions for on-demand chunking
            if estimated_time > Duration::from_secs(5) && remaining_size >= 2 * ONDEMAND_CHUNK_SIZE {
                log::debug!("On-demand chunking opportunity: estimated {}s remaining, {} bytes left",
                           estimated_time.as_secs(), remaining_size);

                // Create on-demand chunks for remaining data
                if let Ok(chunk_count) = create_ondemand_chunks(task, existing_bytes, remaining_size) {
                    log::info!("Created {} on-demand chunks for {} bytes remaining", chunk_count, remaining_size);
                }
            }

            last_ondemand_check = now;
        }
    }

    // Final progress update
    if task.is_master_task() {
        let total_received = task.get_total_progress_bytes();
        pb.set_position(total_received);
    } else {
        pb.set_position(existing_bytes);
    }

    Ok(existing_bytes)
}

/// Validate that the downloaded size matches the expected Content-Length
fn validate_download_size(downloaded: u64, total_size: u64, part_path: &Path) -> Result<()> {
    if total_size > 0 && downloaded != total_size {
        return Err(eyre!("Download size mismatch: Downloaded size ({}) does not match expected size ({}) for {}", downloaded, total_size, part_path.display()));
    }
    Ok(())
}

/// Set file timestamp from response headers (Last-Modified or Date)
fn set_file_timestamp(response: &http::Response<ureq::Body>, part_path: &Path) {
    if let Some(timestamp_str) = response.headers().get("Last-Modified")
        .or_else(|| response.headers().get("Date"))
        .and_then(|s| s.to_str().ok())
    {
        match OffsetDateTime::parse(timestamp_str, &Rfc2822) {
            Ok(timestamp) => {
                let system_time = filetime::FileTime::from_system_time(timestamp.into());
                if let Err(e) = set_file_mtime(part_path, system_time) {
                     log::warn!("Failed to set mtime for {}: {}", part_path.display(), e);
                }
            }
            Err(e) => {
                log::warn!("Failed to parse timestamp header value '{}' for mtime: {}", timestamp_str, e);
            }
        }
    } else {
        log::debug!("No Last-Modified or Date header found for mtime for {}", part_path.display());
    }
}

/// Parse Content-Length header from response
fn parse_content_length(response: &http::Response<ureq::Body>) -> u64 {
    response.headers().get("Content-Length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            if let Err(e) = s.parse::<u64>() {
                log::warn!("Failed to parse Content-Length header value '{}': {}", s, e);
                None
            } else {
                s.parse::<u64>().ok()
            }
        })
        .unwrap_or(0)
}

/// Parse remote timestamp from Last-Modified or Date headers
fn parse_remote_timestamp(response: &http::Response<ureq::Body>) -> Option<OffsetDateTime> {
    response.headers().get("Last-Modified")
        .or_else(|| response.headers().get("Date"))
        .and_then(|s| {
            s.to_str().ok().and_then(|s_val| {
                match OffsetDateTime::parse(s_val, &Rfc2822) {
                    Ok(dt) => Some(dt),
                    Err(e) => {
                        log::warn!("Failed to parse timestamp header value '{}': {}", s_val, e);
                        None
                    }
                }
            })
        })
}

impl PackageManager {
    /// Submit download tasks for packages without waiting for completion
    /// Returns a mapping from download URLs to their package keys for tracking
    pub fn submit_download_tasks(&mut self, packages: &HashMap<String, InstalledPackageInfo>) -> Result<HashMap<String, String>> {
        let output_dir = dirs().epkg_downloads_cache.clone();
        let mut url_to_pkgkey = HashMap::new();

        // Create output directory
        fs::create_dir_all(&output_dir)
            .with_context(|| format!("Failed to create output directory: {}", output_dir.display()))?;

        // Submit download tasks for each package (handles both local and remote)
        for pkgkey in packages.keys() {
            let package = self.load_package_info(pkgkey)
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
            let task = DownloadTask::with_size(url.clone(), output_dir.clone(), 6, size);
            submit_download_task(task)
                .with_context(|| format!("Failed to submit download task for {}", url))?;
            url_to_pkgkey.insert(url, pkgkey.clone());
        }

        // Start processing download tasks
        DOWNLOAD_MANAGER.start_processing();

        Ok(url_to_pkgkey)
    }

    /// Get the local file path for a downloaded package
    pub fn get_package_file_path(&mut self, pkgkey: &str) -> Result<String> {
        let package = self.load_package_info(pkgkey)
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
            let cache_path = crate::mirror::Mirrors::url_to_cache_path(&url)
                .map_err(|e| eyre!("Failed to convert URL to cache path: {}: {}", url, e))?;
            Ok(cache_path.to_string_lossy().to_string())
        }
    }

    // Download packages specified by their pkgkey strings.
    #[allow(dead_code)]
    pub fn download_packages(&mut self, packages: &HashMap<String, InstalledPackageInfo>, async_mode: bool) -> Result<Vec<String>> {
        let output_dir = dirs().epkg_downloads_cache.clone();

        // Step 1: Compose URLs for each pkgkey
        let mut urls = Vec::new();
        let mut local_files = Vec::new();
        for pkgkey in packages.keys() {
            let package = self.load_package_info(pkgkey)
                .map_err(|e| eyre!("Failed to load package info for key: {}: {}", pkgkey, e))?;
            let url = format!(
                "{}/{}",
                package.package_baseurl,
                package.location
            );
            urls.push(url.clone());
            let cache_path = crate::mirror::Mirrors::url_to_cache_path(&url)
                .map_err(|e| eyre!("Failed to convert URL to cache path: {}: {}", url, e))?
                .to_string_lossy().to_string();
            local_files.push(cache_path);
        }

        // Step 2: Call download_urls function (handles both local and remote files)
        download_urls(urls, &output_dir, 6, async_mode)
            .map_err(|e| eyre!("Failed to download URLs to {}: {}", output_dir.display(), e))?;
        Ok(local_files)
    }

    // Wait for all pending downloads to complete
    #[allow(dead_code)]
    pub fn wait_for_downloads(&self) -> Result<()> {
        DOWNLOAD_MANAGER.wait_for_all_tasks()
            .map_err(|e| eyre!("Failed to wait for download tasks to complete: {}", e))?;
        Ok(())
    }
}

// ============================================================================
// CHUNKED DOWNLOAD IMPLEMENTATION
// ============================================================================
/// Constants for chunking configuration
// Using power of 2 for efficient bit operations
const MIN_CHUNK_SIZE: u64 = 1 << 20;                        // 1MB chunks
const MIN_CHUNK_SIZE_MASK: u64 = MIN_CHUNK_SIZE - 1;        // Mask for modulo operations
const MIN_FILE_SIZE_FOR_CHUNKING: u64 = 3 * 1024 * 1024;    // 3MB
                                                            //
const ONDEMAND_CHUNK_SIZE: u64 = 256 * 1024;                // 256KB chunks
const ONDEMAND_CHUNK_SIZE_MASK: u64 = ONDEMAND_CHUNK_SIZE - 1;

/// Create and setup chunk tasks for large files
///
/// This function handles creating chunk tasks based on the total file size and
/// how much has already been downloaded. It directly modifies the master task's
/// chunk_offset and chunk_size properties, and returns a vector of chunk tasks.
///
/// Cases handled:
/// 1. If the file is smaller than MIN_FILE_SIZE_FOR_CHUNKING, no chunks are created
/// 2. If downloaded > 0, we skip chunks that are already downloaded
/// 3. The master task handles from current offset to next chunk boundary
///
/// Returns a vector of chunk tasks if chunks were created, empty vector otherwise
fn create_chunk_tasks(task: &DownloadTask, total_size: u64, downloaded: u64) -> Result<Vec<Arc<DownloadTask>>> {
    // Don't chunk small files or chunk tasks themselves
    if task.is_chunk_task() || total_size < downloaded + MIN_FILE_SIZE_FOR_CHUNKING {
        log::debug!("Skipping chunking: is_chunk_task={}, size={} bytes",
                  task.is_chunk_task(), total_size);
        return Ok(Vec::new());
    }

    log::debug!("Creating chunks for {} byte file with {} bytes already downloaded",
              total_size, downloaded);

    // Calculate the master task's chunk offset and the next chunk boundary
    let master_chunk_offset = downloaded;

    // Calculate the next chunk boundary after downloaded bytes
    let next_boundary = if downloaded > 0 {
        // Round up to the next chunk boundary using bit operations
        (downloaded + MIN_CHUNK_SIZE_MASK) & !MIN_CHUNK_SIZE_MASK
    } else {
        MIN_CHUNK_SIZE
    };

    // Master task will handle from current offset to next chunk boundary
    let master_chunk_size = std::cmp::min(next_boundary - downloaded, total_size - downloaded);

    // Update master task's chunk information
    unsafe {
        // These fields aren't normally mutable, but we need to update them
        // This is safe because we're only modifying the master task's view of its own chunk
        let task_mut = task as *const DownloadTask as *mut DownloadTask;
        (*task_mut).chunk_offset = master_chunk_offset;
        (*task_mut).chunk_size = master_chunk_size;
    }

    log::debug!("Master task will handle {} bytes starting from offset {}",
              master_chunk_size, master_chunk_offset);

    // Starting offset for additional chunks is the next boundary
    let mut offset = next_boundary;

    // Create chunk tasks for the remaining parts of the file
    let mut chunk_tasks = Vec::new();
    while offset < total_size {
        let chunk_size = std::cmp::min(MIN_CHUNK_SIZE, total_size - offset);
        let chunk_task = task.create_chunk_task(offset, chunk_size);
        chunk_tasks.push(chunk_task);
        offset += chunk_size;
    }

    if chunk_tasks.is_empty() {
        log::debug!("No additional chunks needed for {} byte file with {} bytes already downloaded",
                 total_size, downloaded);
    }

    Ok(chunk_tasks)
}


/// Download a specific chunk of a file
fn download_chunk_task(
    client: &Agent,
    task: &DownloadTask,
    resolved_url: &str,
) -> Result<()> {
    let url = resolved_url;
    let chunk_path = &task.chunk_path;

    // Track mirror usage for chunk
    track_mirror_start_from_url(url);
    let chunk_offset = task.chunk_offset;
    let chunk_size = task.chunk_size;

    log::debug!("Starting chunk download: {} bytes at offset {} for {}",
               chunk_size, chunk_offset, url);

    // Check if chunk file already exists
    let mut adjusted_offset = chunk_offset;

    if let Ok(metadata) = fs::metadata(chunk_path) {
        let existing_size = metadata.len() as u64;

        // If the file is complete, we're done
        if existing_size >= chunk_size {
            log::debug!("Chunk file already exists and is complete: {}", chunk_path.display());
            // Set resumed_bytes to the full chunk size since we're reusing the entire file
            task.resumed_bytes.store(chunk_size, std::sync::atomic::Ordering::Relaxed);
            task.received_bytes.store(0, std::sync::atomic::Ordering::Relaxed); // No network bytes needed

            // Mark task as completed
            update_download_status(task, DownloadStatus::Completed)?;
            return Ok(());
        }

        // If the file is partially downloaded, adjust the offset and size
        if existing_size > 0 {
            adjusted_offset = chunk_offset + existing_size;
            log::debug!("Resuming chunk download from offset {} ({} bytes already downloaded)",
                      adjusted_offset, existing_size);
            // Set resumed_bytes to existing bytes, received_bytes will track new network bytes
            task.resumed_bytes.store(existing_size, std::sync::atomic::Ordering::Relaxed);
            task.received_bytes.store(0, std::sync::atomic::Ordering::Relaxed); // Reset network bytes
        }
    }

    // Create directories if needed
    if let Some(parent) = chunk_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Prepare the download request with Range header
    let end_offset = chunk_offset + chunk_size - 1; // Keep the original end offset
    let range_header = format!("bytes={}-{}", adjusted_offset, end_offset);

    let chunk_request_start = std::time::Instant::now();
    let response = client.get(url)
        .header("Range", &range_header)
        .config()
        .max_redirects(3)
        .build()
        .call()
        .map_err(|e| eyre!("Failed to make chunk request for {}: {}", url, e))?;
    let chunk_latency = chunk_request_start.elapsed().as_millis() as u64;

    // Log chunk request latency
    if let Err(e) = append_download_log(
        url,
        0,
        chunk_latency,
        chunk_latency,
        true,
        None,
        Some(response.status().as_u16() == 206), // Server supports range requests
        Some(true),
    ) {
        log::warn!("Failed to log chunk request latency: {}", e);
    }

    if response.status().as_u16() != 206 {
        // Log that server doesn't support range requests for chunks
        if let Err(e) = append_download_log(
            url,
            0,
            chunk_latency,
            chunk_latency,
            false,
            Some(format!("No chunk range support (got {})", response.status())),
            Some(false),
            Some(true),
        ) {
            log::warn!("Failed to log chunk range error: {}", e);
        }
        return Err(eyre!("Server doesn't support range requests (got {})", response.status()));
    }

    // Download the chunk content
    let chunk_download_start = std::time::Instant::now();
    download_chunk_content(response, chunk_path, task)?;
    let chunk_download_duration = chunk_download_start.elapsed().as_millis() as u64;

    // Log chunk download completion
    let chunk_bytes = task.received_bytes.load(std::sync::atomic::Ordering::Relaxed);
    if let Err(e) = append_download_log(
        url,
        chunk_bytes,
        chunk_download_duration + chunk_latency, // Total time including latency
        chunk_latency,
        true,
        None,
        Some(true), // Range requests work for chunks
        Some(true),
    ) {
        log::warn!("Failed to log chunk download completion: {}", e);
    }

    log::debug!("Completed chunk download: {} bytes at offset {} for {}",
               chunk_size, chunk_offset, url);

    // Track mirror usage end for chunk
    track_mirror_end_from_url(url);

    // Mark task as completed
    update_download_status(task, DownloadStatus::Completed)?;

    Ok(())
}

/// Download content for a chunk task
fn download_chunk_content(
    mut response: http::Response<ureq::Body>,
    chunk_path: &Path,
    task: &DownloadTask,
) -> Result<()> {
    // Check if we need to append to an existing file
    let existing_bytes = if chunk_path.exists() {
        fs::metadata(chunk_path)
            .map(|m| m.len() as u64)
            .unwrap_or(0)
    } else {
        0
    };

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(existing_bytes > 0) // Append if file exists with content
        .truncate(existing_bytes == 0) // Only truncate if file is empty or doesn't exist
        .open(chunk_path)
        .map_err(|e| eyre!("Failed to open chunk file '{}': {}", chunk_path.display(), e))?;

    let mut reader = response.body_mut().as_reader();
    let mut buffer = vec![0; 8192];
    let mut total_downloaded = existing_bytes; // Total bytes in file (reused + network)
    let mut network_bytes = 0u64; // Only bytes received from network

    loop {
        let bytes_read = reader.read(&mut buffer)
            .map_err(|e| eyre!("Failed to read chunk data: {}", e))?;

        if bytes_read == 0 {
            break;
        }

        file.write_all(&buffer[..bytes_read])
            .map_err(|e| eyre!("Failed to write {} bytes to chunk file '{}': {}",
                               bytes_read, chunk_path.display(), e))?;

        total_downloaded += bytes_read as u64;
        network_bytes += bytes_read as u64;

        // Store only network bytes in received_bytes
        task.received_bytes.store(network_bytes, std::sync::atomic::Ordering::Relaxed);
    }

    log::debug!("Chunk download completed: {} total bytes ({} network bytes) written to {}",
               total_downloaded, network_bytes, chunk_path.display());

    Ok(())
}

/// Wait for chunks to complete and stream their data to the data channel one by one
///
/// CRITICAL STREAMING REQUIREMENT:
/// This function MUST process chunks one by one as they complete, NOT wait for all chunks
/// to finish before processing. This enables real-time streaming of chunk data to the
/// data channel as each chunk becomes available. The streaming behavior is essential for
/// memory efficiency and responsiveness.
///
/// FAILURE/RETRY COORDINATION LOGIC:
/// When a chunk fails, we don't immediately return an error. Instead:
/// 1. If attempt_number < max_retries: increment attempt_number and reset status to Pending
///    This allows start_chunks_processing() to retry the chunk automatically
/// 2. If max_retries exceeded: record the failure but continue processing other chunks
///    This prevents the next master task retry from conflicting with in-progress chunk downloads
/// 3. After all chunks are processed, if any failures occurred, return an error
///    This triggers download_file_with_retries() to retry the entire master task,
///    which will restart all failed chunks from scratch
///
/// This coordination ensures that:
/// - Individual chunk failures don't immediately abort the entire download
/// - Failed chunks get proper retry attempts within the chunking system
/// - Only after all retry attempts are exhausted does the master task retry
/// - No conflicts occur between chunk retries and master task retries
fn wait_for_chunks_and_merge(master_task: &DownloadTask, pb: &ProgressBar) -> Result<()> {
    // Get a mutable reference to the chunk tasks
    let mut chunks = {
        let chunks_guard = master_task.chunk_tasks.lock()
            .map_err(|e| eyre!("Failed to lock chunk tasks: {}", e))?;
        chunks_guard.clone()
    };

    if chunks.is_empty() {
        return Ok(()); // No chunks to wait for
    }

    log::debug!("Processing {} chunks for {}", chunks.len(), master_task.url);

    // Check if we have a data channel to stream to
    let data_channel = master_task.data_channel.as_ref();
    let mut any_fail = false; // Track if any chunks failed after exhausting retries

    // Process chunks one by one in order until all are complete
    // STREAMING BEHAVIOR: We process each chunk as soon as it's ready, not all at once
    while !chunks.is_empty() {
        // Always process the first chunk in the list (they're already in order)
        let chunk = &chunks[0];
        let chunk_index = chunks.len(); // For logging (chunks.len() - index from end)

        // Check chunk status instead of using thread handles
        match chunk.get_status() {
            DownloadStatus::Completed => {
                log::debug!("Chunk {} at offset {} completed", chunk_index, chunk.chunk_offset);

                // Process the completed chunk immediately (STREAMING)
                if chunk.chunk_path.exists() {
                    // If we have a data channel, stream the chunk data
                    if let Some(channel) = data_channel {
                        log::debug!("Streaming chunk {} data from {}", chunk_index, chunk.chunk_path.display());
                        send_file_to_channel(&chunk.chunk_path, channel)?;
                    }

                    // Concatenate this chunk to the master file
                    if let Err(e) = append_file_to_file(&chunk.chunk_path, &master_task.chunk_path) {
                        log::warn!("Failed to append chunk {} to master file: {}", chunk_index, e);
                    } else {
                        log::debug!("Appended chunk {} to master file", chunk_index);
                    }

                    // Clean up this chunk file after processing
                    if let Err(e) = fs::remove_file(&chunk.chunk_path) {
                        log::warn!("Failed to clean up chunk file {}: {}", chunk.chunk_path.display(), e);
                    } else {
                        log::debug!("Cleaned up chunk file: {}", chunk.chunk_path.display());
                    }
                } else {
                    log::warn!("Chunk file not found: {}", chunk.chunk_path.display());
                }

                // Remove this chunk from the list
                chunks.remove(0);
            },
            DownloadStatus::Failed(ref err) => {
                let current_attempt = chunk.attempt_number.load(std::sync::atomic::Ordering::SeqCst);

                if current_attempt < master_task.max_retries {
                    // Retry the chunk: increment attempt number and reset status to Pending
                    chunk.attempt_number.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                    if let Ok(mut status) = chunk.status.lock() {
                        *status = DownloadStatus::Pending;
                        log::info!("Retrying chunk {} at offset {} (attempt {}/{}): {}",
                                 chunk_index, chunk.chunk_offset, current_attempt + 1, master_task.max_retries, err);
                    }

                    // Don't remove the chunk - let start_chunks_processing() retry it
                } else {
                    // Max retries exceeded - record failure and continue with other chunks
                    log::error!("Chunk {} at offset {} failed after {} attempts: {}",
                              chunk_index, chunk.chunk_offset, master_task.max_retries, err);
                    any_fail = true;

                    // Remove this failed chunk from the list to continue processing others
                    chunks.remove(0);
                }
            },
            DownloadStatus::Pending | DownloadStatus::Downloading => {
                // Chunk is not ready yet, continue waiting

                // Update progress with current total
                let total_received = master_task.get_total_progress_bytes();
                pb.set_position(total_received);
                log::trace!("Chunk progress update: {} bytes received", total_received);

                // Sleep a bit before checking again
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
    }

    // Handle any failures that occurred after exhausting retries
    // This triggers download_file_with_retries() to retry the entire master task
    if any_fail {
        return Err(eyre!("One or more chunks failed after exhausting retries - master task will retry"));
    }

    log::debug!("All chunks processed for {}", master_task.url);
    Ok(())
}

/// Append the contents of one file to another
fn append_file_to_file(source_path: &Path, target_path: &Path) -> Result<()> {
    // Create target file if it doesn't exist
    if !target_path.exists() {
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }
        File::create(target_path)?
            .sync_all()?;
    }

    // Open source file for reading
    let mut source = File::open(source_path)
        .map_err(|e| eyre!("Failed to open source file {}: {}", source_path.display(), e))?;

    // Open target file for appending
    let mut target = OpenOptions::new()
        .write(true)
        .append(true)
        .open(target_path)
        .map_err(|e| eyre!("Failed to open target file {}: {}", target_path.display(), e))?;

    // Copy data from source to target
    std::io::copy(&mut source, &mut target)
        .map_err(|e| eyre!("Failed to append file {}: {}", source_path.display(), e))?;

    // Ensure data is written to disk
    target.sync_all()
        .map_err(|e| eyre!("Failed to sync target file {}: {}", target_path.display(), e))?;

    Ok(())
}

/// Estimate remaining download time for on-demand chunking
fn estimate_remaining_time(task: &DownloadTask) -> Duration {
    let start_time = {
        if let Ok(start_guard) = task.start_time.lock() {
            start_guard.clone()
        } else {
            return Duration::from_secs(0);
        }
    };

    if let Some(start) = start_time {
        let elapsed = start.elapsed();
        // Use only network received bytes for accurate rate calculation
        let network_downloaded = task.get_network_bytes();
        let total_size = if task.chunk_size > 0 { task.chunk_size } else { task.size.unwrap_or(0) };
        let total_downloaded = task.get_total_progress_bytes(); // includes reused bytes

        if network_downloaded > 0 && total_downloaded < total_size {
            let rate = network_downloaded as f64 / elapsed.as_secs_f64();
            let remaining_bytes = total_size - total_downloaded;
            let estimated_seconds = remaining_bytes as f64 / rate;
            return Duration::from_secs_f64(estimated_seconds);
        }
    }

    Duration::from_secs(0)
}

/// Create on-demand chunk tasks during download
///
/// This function modifies the master task's chunk range and creates additional 256KB chunk tasks
/// when a download is slow and we want to parallelize it further. The master task is modified to
/// cover from current position to the next 256KB boundary, then additional chunks are created
/// for the remaining data.
///
/// Returns the number of chunk tasks created.
// Input: existing_bytes=400KB, remaining_size=900KB
// Output: Master task modified + 4 chunks created
//
// Master Task (Modified):  400KB → 512KB  (112KB)
// Chunk 1:                 512KB → 768KB  (256KB)
// Chunk 2:                 768KB → 1024KB (256KB)
// Chunk 3:                 1024KB → 1280KB (256KB)
// Chunk 4:                 1280KB → 1300KB (20KB final)
//
// Result: 5 parallel downloads (1 master + 4 chunks)
//
// Step 1: Modify master task to cover existing_bytes → next_boundary
// Step 2: Create 256KB chunks from next_boundary → end
// Step 3: Add all chunks to master task atomically
fn create_ondemand_chunks(master_task: &DownloadTask, existing_bytes: u64, remaining_size: u64) -> Result<usize> {
    // Calculate the next 256KB boundary after current position
    let next_boundary = (existing_bytes + ONDEMAND_CHUNK_SIZE_MASK) & !ONDEMAND_CHUNK_SIZE_MASK;
    let total_size = existing_bytes + remaining_size;

    // Modify master task to cover from current position to next 256KB boundary
    let master_chunk_size = std::cmp::min(next_boundary - existing_bytes, remaining_size);

    // Update master task's chunk information using unsafe (similar to how it's done in create_chunk_tasks)
    unsafe {
        let task_mut = master_task as *const DownloadTask as *mut DownloadTask;
        (*task_mut).chunk_offset = existing_bytes;
        (*task_mut).chunk_size = master_chunk_size;
    }

    log::debug!("Modified master task to handle {} bytes from offset {} to boundary {}",
               master_chunk_size, existing_bytes, next_boundary);

    // Create additional 256KB chunks from next boundary to end of file
    let mut chunk_tasks = Vec::new();
    let mut offset = next_boundary;

    while offset < total_size {
        let chunk_size = std::cmp::min(ONDEMAND_CHUNK_SIZE, total_size - offset);
        let chunk_task = master_task.create_chunk_task(offset, chunk_size);
        chunk_tasks.push(chunk_task);
        offset += chunk_size;
    }

    // Add all chunk tasks to the master task's chunk list
    if let Ok(mut chunks) = master_task.chunk_tasks.lock() {
        for chunk_task in &chunk_tasks {
            chunks.push(Arc::clone(chunk_task));
        }

        log::info!("Created {} on-demand chunks (256KB each) for {} bytes remaining, master covers {}→{} bytes",
                  chunk_tasks.len(), remaining_size, existing_bytes, next_boundary);
    } else {
        return Err(eyre!("Failed to lock master task's chunk list"));
    }

    Ok(chunk_tasks.len())
}

// ============================================================================
// PROCESS COORDINATION
// ============================================================================

/// Create a PID file for download coordination
fn create_pid_file(final_path: &Path) -> Result<PathBuf> {
    let pid_file = final_path.with_extension("download.pid");
    let pid = std::process::id();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let pid_content = format!("{}:{}", pid, timestamp);

    // Try to create the PID file atomically
    let temp_pid_file = pid_file.with_extension("download.pid.tmp");
    fs::write(&temp_pid_file, pid_content)?;

    // Atomic rename
    fs::rename(&temp_pid_file, &pid_file)?;

    log::debug!("Created PID file: {}", pid_file.display());
    Ok(pid_file)
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

    let parts: Vec<&str> = content.trim().split(':').collect();
    if parts.len() != 2 {
        return false;
    }

    let pid: u32 = match parts[0].parse() {
        Ok(pid) => pid,
        Err(_) => return false,
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
        use std::process::Command;
        match Command::new("kill").args(&["-0", &pid.to_string()]).output() {
            Ok(output) => output.status.success(),
            Err(_) => false,
        }
    }

    // For Windows or if we can't check, assume it's active for safety
    #[cfg(not(unix))]
    {
        true
    }
}

/// Clean up PID file after download completion
fn cleanup_pid_file(pid_file: &Path) -> Result<()> {
    if pid_file.exists() {
        fs::remove_file(pid_file)?;
        log::debug!("Cleaned up PID file: {}", pid_file.display());
    }
    Ok(())
}

/// Check for existing downloads and clean up stale PID files
fn check_and_cleanup_existing_downloads(final_path: &Path) -> Result<()> {
    let pid_file = final_path.with_extension("download.pid");

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

/// Atomic file operations for crash recovery
fn atomic_file_completion(temp_path: &Path, final_path: &Path) -> Result<()> {
    if temp_path.exists() {
        fs::rename(temp_path, final_path)?;
        log::debug!("Atomically completed file: {}", final_path.display());
    }
    Ok(())
}

/// Recover from crashed chunked downloads
fn recover_chunked_download(task: &DownloadTask) -> Result<Vec<PathBuf>> {
    let mut recovered_chunks = Vec::new();
    let base_name = task.final_path.file_stem()
        .ok_or_else(|| eyre!("Invalid file path: {}", task.final_path.display()))?
        .to_string_lossy();

    // Look for existing chunk files
    if let Some(parent) = task.final_path.parent() {
        if let Ok(entries) = fs::read_dir(parent) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(filename) = path.file_name() {
                    let filename_str = filename.to_string_lossy();
                    if filename_str.starts_with(&format!("{}.part-O", base_name)) {
                        // Validate chunk file integrity
                        if let Ok(metadata) = fs::metadata(&path) {
                            if metadata.len() > 0 {
                                log::debug!("Recovered chunk file: {}", path.display());
                                recovered_chunks.push(path);
                            }
                        }
                    }
                }
            }
        }
    }

    if !recovered_chunks.is_empty() {
        log::info!("Recovered {} chunk files for {}", recovered_chunks.len(), task.url);
    }

    Ok(recovered_chunks)
}
