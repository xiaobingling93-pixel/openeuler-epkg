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
use std::sync::atomic::{AtomicU64, AtomicUsize, AtomicBool};
use std::sync::Mutex;
use std::sync::atomic::Ordering;

use color_eyre::{eyre::eyre, eyre::WrapErr, Result};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use ureq::{Agent, Proxy};
use ureq::http;
use crate::dirs;
use crate::models::*;
use crate::mirror::{append_download_log, append_http_log, HttpEvent, MirrorUsageGuard, MIRRORS, Mirrors};
use time::{OffsetDateTime, format_description::well_known::Rfc2822};
use filetime::set_file_mtime;


#[derive(Debug)]
pub struct DownloadTask {
    pub url: String,
    pub resolved_url: Mutex<String>,
    #[allow(dead_code)]
    pub output_dir: PathBuf,
    pub max_retries: usize,
    pub client: Arc<Mutex<Option<Agent>>>, // HTTP client created on-demand
    pub data_channel: Arc<Mutex<Option<Sender<Vec<u8>>>>>,  // will never change, but need take()
                                                            // to avoid blocking the consumer side
    pub status: Arc<Mutex<DownloadStatus>>,
    pub final_path: PathBuf, // Store the final download path
    pub file_size: AtomicU64, // Expected file size for prioritization and verification (0 = unknown)
    pub attempt_number: AtomicUsize, // Track which attempt number this is (0 = first attempt)
    pub is_immutable_file: bool, // True for files whose content won't change over time (filename == some id)

    // New fields for chunking
    pub chunk_tasks: Arc<Mutex<Vec<Arc<DownloadTask>>>>,
    pub chunk_path: PathBuf, // Full path to the chunk file (for master: .part, for chunks: .part-O{offset})
    pub chunk_offset: AtomicU64, // Starting byte offset for this chunk
    pub chunk_size: AtomicU64, // Size of this chunk in bytes
    pub start_time: Mutex<Option<std::time::Instant>>,
    pub received_bytes: AtomicU64, // Bytes actually received from network
    pub resumed_bytes: AtomicU64, // Bytes reused from local partial files

    // Progress bar for this download task
    pub progress_bar: Mutex<Option<ProgressBar>>,           // will never change
}

#[derive(Debug, Clone, PartialEq)]
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

    pub fn with_size(url: String, output_dir: PathBuf, max_retries: usize, file_size: Option<u64>) -> Self {
        let final_path = Mirrors::resolve_mirror_path(&url, &output_dir);
        // Initialize chunk_path to the standard .part file for master tasks
        let chunk_path = final_path.with_extension("part");

        // Determine if this is an immutable file based on the file path
        let file_path = final_path.to_string_lossy();
        let is_immutable_file = file_path.ends_with(".deb") ||
                               file_path.ends_with(".rpm") ||
                               file_path.ends_with(".apk") ||
                               file_path.ends_with(".conda") ||
                               file_path.contains("/by-hash/") ||
                               file_path.ends_with(".gz") ||
                               file_path.ends_with(".xz") ||
                               file_path.ends_with(".zst");

        Self {
            url: url.clone(),
            resolved_url: Mutex::new(url), // Initialize resolved_url with the original url
            output_dir,
            max_retries,
            client: Arc::new(Mutex::new(None)), // Initialize with no client
            data_channel: Arc::new(Mutex::new(None)),
            status: Arc::new(Mutex::new(DownloadStatus::Pending)),
            final_path,
            file_size: AtomicU64::new(file_size.unwrap_or(0)),
            attempt_number: AtomicUsize::new(0), // Initialize to 0 (first attempt)
            is_immutable_file,
            chunk_tasks: Arc::new(Mutex::new(Vec::new())),
            chunk_path,
            chunk_offset: AtomicU64::new(0),
            chunk_size: AtomicU64::new(0),
            start_time: Mutex::new(None),
            received_bytes: AtomicU64::new(0),
            resumed_bytes: AtomicU64::new(0),
            progress_bar: Mutex::new(None),
        }
    }

    pub fn with_data_channel(mut self, channel: Sender<Vec<u8>>) -> Self {
        self.data_channel = Arc::new(Mutex::new(Some(channel)));
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
    #[allow(dead_code)]
    pub fn is_first_attempt(&self) -> bool {
        self.attempt_number.load(Ordering::SeqCst) == 0
    }

    /// Increment the attempt number when a retry is needed
    #[allow(dead_code)]
    pub fn increment_attempt(&self) {
        self.attempt_number.fetch_add(1, Ordering::SeqCst);
    }

    /// Reset the attempt number to zero
    #[allow(dead_code)]
    pub fn reset_attempt(&self) {
        self.attempt_number.store(0, Ordering::SeqCst);
    }

    /// Get the resolved URL, falling back to the original URL if resolution failed
    pub fn get_resolved_url(&self) -> String {
        if let Ok(resolved) = self.resolved_url.lock() {
            if resolved.is_empty() {
                self.url.clone()
            } else {
                resolved.clone()
            }
        } else {
            self.url.clone()
        }
    }

    /// Get or create the HTTP client on-demand with configuration from config().common
    pub fn get_client(&self) -> Result<Agent> {
        let mut client_guard = self.client.lock()
            .map_err(|e| eyre!("Failed to lock client mutex: {}", e))?;

        if client_guard.is_none() {
            // Create client with proxy configuration from config
            let mut config_builder = Agent::config_builder()
                .user_agent("curl/8.13.0")
                .timeout_connect(Some(Duration::from_secs(5)))
                .timeout_recv_response(Some(Duration::from_secs(9)));

            let proxy_config = &crate::models::config().common.proxy;
            if !proxy_config.is_empty() {
                match ureq::Proxy::new(proxy_config) {
                    Ok(p) => {
                        config_builder = config_builder.proxy(Some(p));
                    }
                    Err(e) => {
                        log::error!("Failed to create proxy from {}: {}", proxy_config, e);
                        return Err(eyre!("Failed to create proxy: {}", e));
                    }
                }
            }

            *client_guard = Some(config_builder.build().into());
        }

        Ok(client_guard.as_ref().unwrap().clone())
    }

    /// Create a chunk task for a specific byte range
    pub fn create_chunk_task(&self, offset: u64, size: u64) -> Arc<DownloadTask> {
        // Create a chunk task with a specific offset and size
        // The chunk file will be named .part-O{offset}
        let chunk_path = format!("{}-O{}", self.chunk_path.to_string_lossy(), offset);

        Arc::new(DownloadTask {
            url: self.url.clone(),
            resolved_url: Mutex::new(self.get_resolved_url()), // Use helper method
            output_dir: self.output_dir.clone(),
            max_retries: self.max_retries,
            client: Arc::new(Mutex::new(None)), // Initialize with no client
            data_channel: Arc::new(Mutex::new(None)), // Chunks don't need data channels
            status: Arc::new(Mutex::new(DownloadStatus::Pending)),
            final_path: self.final_path.clone(),
            file_size: AtomicU64::new(self.file_size.load(Ordering::Relaxed)),
            attempt_number: AtomicUsize::new(0), // Initialize to 0 (first attempt)
            is_immutable_file: self.is_immutable_file, // Copy immutable file flag

            chunk_tasks: Arc::new(Mutex::new(Vec::new())),
            chunk_path: PathBuf::from(chunk_path),
            chunk_offset: AtomicU64::new(offset),
            chunk_size: AtomicU64::new(size), // <-- set correct chunk size here
            start_time: Mutex::new(None),
            received_bytes: AtomicU64::new(0),
            resumed_bytes: AtomicU64::new(0),
            progress_bar: Mutex::new(None),
        })
    }

    /// Get total progress bytes across all chunks (reused + network bytes)
    /// This represents the total download progress for display purposes
    pub fn get_total_progress_bytes(&self) -> (u64, usize) {
        let mut total_received = self.received_bytes.load(Ordering::Relaxed);
        let mut total_reused = self.resumed_bytes.load(Ordering::Relaxed);
        let mut downloading_chunks = 0;

        if let Ok(chunks) = self.chunk_tasks.lock() {
            for chunk in chunks.iter() {
                total_received += chunk.received_bytes.load(Ordering::Relaxed);
                total_reused += chunk.resumed_bytes.load(Ordering::Relaxed);

                // Count chunks with Downloading status
                if let Ok(status) = chunk.status.lock() {
                    if *status == DownloadStatus::Downloading {
                        downloading_chunks += 1;
                    }
                }
            }
        }

        (total_received + total_reused, downloading_chunks)
    }

    /// Get total bytes actually received from network (excluding reused local files)
    /// This is used for accurate rate calculation and time estimation
    pub fn get_network_bytes(&self) -> u64 {
        let mut total = self.received_bytes.load(Ordering::Relaxed);

        if let Ok(chunks) = self.chunk_tasks.lock() {
            for chunk in chunks.iter() {
                total += chunk.received_bytes.load(Ordering::Relaxed);
            }
        }

        total
    }

    /// Set progress bar message
    pub fn set_message(&self, message: String) {
        if let Ok(pb_guard) = self.progress_bar.lock() {
            if let Some(ref pb) = *pb_guard {
                pb.set_message(message);
            }
        }
    }

    /// Set progress bar length
    pub fn set_length(&self, length: u64) {
        if let Ok(pb_guard) = self.progress_bar.lock() {
            if let Some(ref pb) = *pb_guard {
                pb.set_length(length);
            }
        }
    }

    /// Set progress bar position
    pub fn set_position(&self, position: u64) {
        if let Ok(pb_guard) = self.progress_bar.lock() {
            if let Some(ref pb) = *pb_guard {
                pb.set_position(position);
            }
        }
    }

    /// Finish progress bar with message
    pub fn finish_with_message(&self, message: String) {
        if let Ok(pb_guard) = self.progress_bar.lock() {
            if let Some(ref pb) = *pb_guard {
                pb.finish_with_message(message);
            }
        }
    }
}

pub struct DownloadManager {
    multi_progress: MultiProgress,
    tasks: Arc<Mutex<HashMap<String, Arc<DownloadTask>>>>,
    nr_parallel: usize,
    task_handles: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
    chunk_handles: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
    is_processing: Arc<AtomicBool>,
    current_task_count: Arc<AtomicUsize>,
}

impl DownloadManager {
    pub fn new(nr_parallel: usize) -> Result<Self> {
        // Note: Proxy configuration is now handled per-task via on-demand client creation from config().common.proxy

        let multi_progress = MultiProgress::new();

        Ok(Self {
            multi_progress,
            tasks: Arc::new(Mutex::new(HashMap::new())),
            nr_parallel,
            task_handles: Arc::new(Mutex::new(Vec::new())),
            chunk_handles: Arc::new(Mutex::new(Vec::new())),
            is_processing: Arc::new(AtomicBool::new(false)),
            current_task_count: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn submit_task(&self, task: DownloadTask) -> Result<()> {
        let mut tasks = self.tasks.lock()
            .map_err(|e| eyre!("Failed to lock tasks mutex: {}", e))?;
        if !tasks.contains_key(&task.url) {
            tasks.insert(task.url.clone(), Arc::new(task));
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
        if self.is_processing.load(Ordering::Relaxed) {
            return;
        }

        self.is_processing.store(true, Ordering::Relaxed);
        let tasks = Arc::clone(&self.tasks);

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

                let tasks_guard = match tasks.lock() {
                    Ok(guard) => guard,
                    Err(e) => {
                        log::error!("Failed to lock tasks mutex: {}", e);
                        is_processing.store(false, Ordering::Relaxed);
                        return;
                    }
                };

                let pending_tasks: Vec<_> = tasks_guard.iter()
                    .filter(|(_, t)| matches!(t.get_status(), DownloadStatus::Pending))
                    .map(|(url, task)| (url.clone(), Arc::clone(task)))
                    .collect();

                if pending_tasks.is_empty() {
                    // Check if all tasks are completed or failed
                    let all_done = tasks_guard.iter()
                        .all(|(_, t)| matches!(t.get_status(), DownloadStatus::Completed | DownloadStatus::Failed(_)));
                    if all_done {
                        is_processing.store(false, Ordering::Relaxed);
                        break;
                    }
                    drop(tasks_guard);

                    // No new master tasks to start, but we may still need to process chunks
                    Self::start_chunks_processing(&tasks, &chunk_handles, nr_parallel);

                    thread::sleep(Duration::from_millis(100));
                    continue;
                }

                // Sort pending tasks by size (largest first)
                let mut sorted_pending = pending_tasks;
                sorted_pending.sort_by(|(_, a), (_, b)| {
                    let size_a = a.file_size.load(Ordering::Relaxed);
                    let size_b = b.file_size.load(Ordering::Relaxed);
                    size_b.cmp(&size_a) // Descending order (largest first)
                });

                // Check how many task threads are currently running
                let mut current_task_count = {
                    let handles_guard = task_handles.lock().unwrap();
                    let count = handles_guard.len();
                    is_processing.load(Ordering::Relaxed); // Ensure memory ordering
                    count
                };

                // Spawn new task threads if we have capacity
                for (_task_url, task) in sorted_pending {
                    current_task_count_arc.store(current_task_count, Ordering::Relaxed);
                    if current_task_count >= nr_parallel {
                        break; // We've reached our task thread limit
                    }


                    let multi_progress = multi_progress.clone();
                    let task_clone = Arc::clone(&task);
                    let _task_handles_clone = Arc::clone(&task_handles);

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
                        // Now we can work directly with the Arc since critical mutable operations
                        // (like data_channel.take()) have been handled above
                        if let Err(e) = download_task(
                            &task_clone,
                            &multi_progress,
                        ) {
                            log::error!("Download task failed for {}: {}", task_clone.url, e);
                        }

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
                         * Note: With Arc<DownloadTask> and Arc<Mutex<Option<Sender<...>>>>, we can now call take()
                         * even through the Arc since data_channel uses interior mutability.
                         */
                        let _data_channel = {
                            match task_clone.data_channel.lock() {
                                Ok(mut dc) => dc.take(),
                                Err(_) => None,
                            }
                        };
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
    fn cleanup_finished_handles(handles: &Arc<Mutex<Vec<thread::JoinHandle<()>>>>) {
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
        tasks: &Arc<Mutex<HashMap<String, Arc<DownloadTask>>>>,
        chunk_handles: &Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
        nr_parallel: usize
    ) {
        let max_chunk_threads = Self::calculate_max_chunk_threads(nr_parallel);

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

        let pending_chunks = Self::collect_pending_chunks(tasks);

        // Spawn chunk threads up to the limit
        let threads_to_spawn = std::cmp::min(
            max_chunk_threads - current_chunk_count,
            pending_chunks.len()
        );

        if threads_to_spawn <= 0 {
            return;
        }

        log::debug!("pending_chunks={} max_threads={} active_chunks={} to_spawn={}",
            pending_chunks.len(), max_chunk_threads, current_chunk_count, threads_to_spawn);

        Self::spawn_chunk_threads(&pending_chunks, threads_to_spawn, chunk_handles);
    }

    /// Calculate the maximum number of chunk threads based on parallel limit and available mirrors
    fn calculate_max_chunk_threads(nr_parallel: usize) -> usize {
        // Get available mirrors count
        let available_mirrors_count = {
            if let Ok(mirrors) = MIRRORS.lock() {
                mirrors.available_mirrors.len()
            } else {
                1
            }
        };

        std::cmp::max(
            nr_parallel,
            std::cmp::min(nr_parallel * 8, available_mirrors_count)
        ) * 2
    }

    /// Collect all pending chunk tasks from all download tasks
    fn collect_pending_chunks(
        tasks: &Arc<Mutex<HashMap<String, Arc<DownloadTask>>>>
    ) -> Vec<(f64, Arc<DownloadTask>)> {
        let mut nr_master = 0;
        let mut nr_chunks = 0;
        let mut collected = Vec::new();

        if let Ok(tasks_guard) = tasks.lock() {
            for (_url, download_task) in tasks_guard.iter() {
                if let Ok(chunks) = download_task.chunk_tasks.lock() {
                    nr_master = nr_master + 1;
                    for chunk in chunks.iter() {
                        nr_chunks = nr_chunks + 1;
                        if matches!(chunk.get_status(), DownloadStatus::Pending) {
                            let chunk_offset = chunk.chunk_offset.load(Ordering::Relaxed);
                            let file_size = chunk.file_size.load(Ordering::Relaxed);
                            let priority = if file_size > 0 { chunk_offset as f64 / file_size as f64 } else { 0.0 };
                            collected.push((priority, Arc::clone(chunk)));
                        }
                    }
                }
            }
        }

        // Sort chunks by priority (chunk_offset / size)
        collected.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        if !collected.is_empty() {
            log::debug!("master_tasks={} chunk_tasks={} pending_chunks={}",
                       nr_master, nr_chunks, collected.len());
        }

        collected
    }

    /// Spawn chunk download threads for the given pending chunks
    fn spawn_chunk_threads(
        pending_chunks: &[(f64, Arc<DownloadTask>)],
        threads_to_spawn: usize,
        chunk_handles: &Arc<Mutex<Vec<thread::JoinHandle<()>>>>
    ) {
        for (_, chunk_task) in pending_chunks.iter().take(threads_to_spawn) {
            let chunk_clone = Arc::clone(chunk_task);
            let _chunk_handles_clone = Arc::clone(chunk_handles);

            // Mark chunk as downloading NOW to avoid duplicate scheduling in the next scheduler tick
            if let Err(e) = update_download_status(&chunk_clone, DownloadStatus::Downloading) {
                log::error!("Failed to set chunk status to Downloading for {}: {}", chunk_clone.chunk_path.display(), e);
                continue; // Skip spawning thread if we cannot update status
            }

            // Resolve mirror URL before spawning thread to ensure consistency
            let (resolved_url, _final_path) = match resolve_mirror_in_url(&chunk_clone.url, &chunk_clone.output_dir, true) {
                Ok((resolved_url, final_path)) => (resolved_url, final_path),
                Err(e) => {
                    log::error!("Failed to resolve mirror for chunk {}: {}", chunk_clone.url, e);
                    continue; // Skip this chunk if mirror resolution fails
                }
            };

            // Update resolved_url using mutex
            if let Ok(mut resolved) = chunk_clone.resolved_url.lock() {
                *resolved = resolved_url.clone();
            }

            let handle = thread::spawn(move || {

                // Mark chunk as started
                if let Ok(mut start_time) = chunk_clone.start_time.lock() {
                    *start_time = Some(std::time::Instant::now());
                }

                let chunk_result = download_chunk_task(&chunk_clone);

                if let Err(e) = chunk_result {
                    log::debug!("Chunk task failed for {} at offset {}: {}",
                               chunk_clone.url, chunk_clone.chunk_offset.load(Ordering::Relaxed), e);

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
        while self.is_processing.load(Ordering::Relaxed) {
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
enum DownloadError {
    Fatal(String),
    Timeout(String),
    TimestampMismatch(String),
    NetworkError(String),
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::Fatal(msg) => write!(f, "Fatal: {}", msg),
            DownloadError::Timeout(msg) => write!(f, "Timeout: {}", msg),
            DownloadError::TimestampMismatch(msg) => write!(f, "Timestamp mismatch: {}", msg),
            DownloadError::NetworkError(msg) => write!(f, "Network error: {}", msg),
        }
    }
}

impl std::error::Error for DownloadError {}

#[derive(Debug)]
#[allow(dead_code)]
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
fn resolve_mirror_in_url(url: &str, output_dir: &Path, need_range: bool) -> Result<(String, PathBuf)> {
    if !url.contains("$mirror") {
        return Ok((url.to_string(), Mirrors::resolve_mirror_path(&url, output_dir)));
    }

    let mut mirrors = MIRRORS.lock()
        .map_err(|e| eyre!("Failed to lock mirrors: {}", e))?;

    let (selected_mirror_url, selected_mirror_top_level, final_distro_dir) = {
        // Use adaptive mirror selection with load balancing
        // The selection algorithm automatically handles concurrent limits per mirror
        let selected_mirror = mirrors.select_mirror_with_usage_tracking(need_range)?;

        // Get distro directory for the selected mirror
        let distro = &crate::models::channel_config().distro;
        let arch = &crate::models::channel_config().arch;

        let distro_dir = crate::mirror::Mirrors::find_distro_dir(&selected_mirror, distro, arch);
        let final_distro_dir = if distro_dir.is_empty() { distro.to_string() } else { distro_dir };

        (selected_mirror.url.clone(), selected_mirror.top_level, final_distro_dir)
    };

    let url_formatted = mirrors.format_mirror_url(&selected_mirror_url, selected_mirror_top_level, &final_distro_dir)?;

    let resolved_url = url.replace("$mirror", &url_formatted);

    let final_path = Mirrors::resolve_mirror_path(&url, output_dir);

    log::debug!("resolve_mirror_in_url: need_range={} {} -> {}", need_range, resolved_url, final_path.display());

    Ok((resolved_url, final_path))
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
    DownloadManager::new(config().common.nr_parallel)
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
    let mut task_urls = Vec::new();
    for url in urls {
        let url_for_context = url.clone();
        let task = DownloadTask::new(url.clone(), output_dir.to_path_buf(), max_retries);

        // Submit the task - if URL already exists, it will just replace/reuse
        submit_download_task(task)
            .with_context(|| format!("Failed to submit download task for {}", url_for_context))?;
        task_urls.push(url);
    }
    fs::create_dir_all(output_dir)
        .with_context(|| format!("Failed to create output directory: {}", output_dir.display()))?;
    DOWNLOAD_MANAGER.start_processing();

    if !async_mode {
        // Wait for each task using the URLs
        for (i, task_url) in task_urls.iter().enumerate() {
            let status = DOWNLOAD_MANAGER.wait_for_task(task_url.clone())
                .with_context(|| format!("Failed to wait for download task {} (URL: {})", i, task_url))?;
            if let DownloadStatus::Failed(err_msg) = status {
                return Err(eyre!("Download failed for {}: {}", task_url, err_msg));
            }
        }
        Ok(Vec::new())
    } else {
        Ok(Vec::new()) // Return empty vec in async mode since tasks are managed by DownloadManager
    }
}

/// Checks if an immutable file exists with matching size and can be considered already downloaded
/// Immutable files are files whose content won't change over time (filename == some id),
/// so as long as our local size == remote size, we can trust it's the same content.
fn check_existing_immutable_file(task: &DownloadTask) -> Result<Option<()>> {
    let final_path = &task.final_path;

    // Early return if file doesn't exist or we don't know the expected size
    if !final_path.exists() {
        return Ok(None);
    }

    let file_size_val = task.file_size.load(Ordering::Relaxed); if file_size_val == 0 {
        return Ok(None);
    };

    // Only check immutable files
    if !task.is_immutable_file {
        return Ok(None);
    }

    if let Ok(metadata) = fs::metadata(final_path) {
        let actual_size = metadata.len();
        if actual_size == file_size_val {
            log::info!("Immutable file {} already exists with correct size {}, treating as already downloaded",
                      final_path.display(), actual_size);

            // Send file content to channel if needed for hash verification
            if let Ok(data_channel_guard) = task.data_channel.lock() {
                if let Some(ref data_channel) = *data_channel_guard {
                    send_file_to_channel(final_path, data_channel)
                        .with_context(|| format!("Failed to send existing file to channel: {}", final_path.display()))?;
                }
            }

            // Mark task as completed
            update_download_status(task, DownloadStatus::Completed)?;
            return Ok(Some(()));
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
    task: &DownloadTask,
    multi_progress: &MultiProgress,
) -> Result<()> {
    let url = &task.url.clone();
    let final_path = &task.final_path.clone();
    let data_channel = match task.data_channel.lock() {
        Ok(dc) => dc.clone(),
        Err(_) => None,
    };
    let expected_size = task.file_size.load(Ordering::Relaxed);

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
    if let Some(()) = check_existing_immutable_file(task)? {
        cleanup_pid_file(&pid_file)?;
        return Ok(());
    }

    // Try to recover from previous chunked downloads
    let _recovered_chunks = recover_chunked_download(task)?;

    // Prepare download environment
    prepare_download_environment(final_path, &part_path)?;

    // Setup progress bar and store it in the task
    let pb = setup_progress_bar(multi_progress, url)?;
    if let Ok(mut pb_guard) = task.progress_bar.lock() {
        *pb_guard = Some(pb);
    }

    // Start the download - download_file_with_retries handles mirror resolution
    log::debug!("download_task calling download_file_with_retries for {}", url);
    let result = download_file_with_retries(
        task,
    );
    log::debug!("download_task download_file_with_retries completed for {}, result: {:?}", url, result);

    // Clean up PID file regardless of result
    let _pid_cleanup_result = cleanup_pid_file(&pid_file);

    // Update progress bar based on result
    if result.is_ok() {
        task.finish_with_message(format!("Downloaded {}", final_path.display()));
    } else {
        task.finish_with_message(format!("Error: {:?}", result));
    }

    // Handle download result
    match result {
        Ok(_metadata) => {
            // Mark task as completed (metadata handling is now done in download_file_with_retries)
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
    let file_size_val = task.file_size.load(Ordering::Relaxed);
    if file_size_val > 0 {
        if let Ok(metadata) = fs::metadata(final_path) {
            let actual_size = metadata.len();
            if actual_size != file_size_val {
                let error_msg = format!(
                    "Local file size mismatch: expected {} bytes, got {} bytes",
                    file_size_val, actual_size
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
    let data_channel = match task.data_channel.lock() {
        Ok(dc) => dc.clone(),
        Err(_) => None,
    };

    log::debug!("download_file_with_retries starting for {}, has_channel: {}", url, data_channel.is_some());
    let mut retries = 0;

    loop {
        log::debug!("download_file_with_retries calling download_file for {}, attempt {}", url, retries + 1);
        log::debug!("About to call download_file with data_channel.is_some() = {}", data_channel.is_some());

        let download_result = download_file(task);
        let resolved_url = task.get_resolved_url();

        match download_result {
            Ok(()) => {
                log::debug!("download_file_with_retries completed successfully for {}, dropping channel", &resolved_url);

                log::debug!("download_file_with_retries completed successfully for {}, dropping channel", &resolved_url);

                return Ok(());
            },
            Err(e) => {
                // Check if this is one of our custom download errors to avoid logging stack traces
                if let Some(download_err) = e.downcast_ref::<DownloadError>() {
                    match download_err {
                        DownloadError::Fatal(msg) => {
                            log::debug!("download_file_with_retries got fatal error for {}: {}", resolved_url, msg);
                        },
                        DownloadError::Timeout(msg) => {
                            log::debug!("download_file_with_retries got timeout error for {}: {}", resolved_url, msg);
                        },
                        DownloadError::TimestampMismatch(msg) => {
                            log::debug!("download_file_with_retries got timestamp mismatch for {}: {}", resolved_url, msg);
                        },
                        DownloadError::NetworkError(msg) => {
                            log::debug!("download_file_with_retries got network error for {}: {}", resolved_url, msg);
                        },
                    }
                } else {
                    log::debug!("download_file_with_retries got error for {}: {}", resolved_url, e);
                }

                if retries >= max_retries {
                    return Err(eyre!("Max retries ({}) exceeded for {}: {}", max_retries, resolved_url, e));
                }

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

fn download_file(
    task: &DownloadTask,
) -> Result<()> {
    let url = &task.url;
    log::debug!("download_file starting for {}", url);

    if task.is_master_task() {
        // Master task: handle chunk creation and coordination
        log::debug!("Processing master task for {}", url);

        // Step 1: Get existing file size
        let existing_bytes = get_existing_file_size(&task.chunk_path)?;
        task.chunk_offset.store(existing_bytes, Ordering::Relaxed);

        // Step 2: Try to create chunks before HTTP request
        let chunks = create_chunk_tasks(task)?;

        // Step 3: Add chunk tasks to the master task
        if !chunks.is_empty() {
            if let Ok(mut master_chunks) = task.chunk_tasks.lock() {
                master_chunks.clear();
                for chunk in &chunks {
                    master_chunks.push(Arc::clone(chunk));
                }
                log::info!("Added {} chunk tasks to master task for download {}", chunks.len(), task.url);
            }
        }

        // Step 4: Send existing file content to channel if resuming
        if existing_bytes > 0 {
            let data_channel = match task.data_channel.lock() {
                Ok(dc) => dc.clone(),
                Err(_) => None,
            };
            if let Some(channel) = data_channel {
                send_file_to_channel(&task.chunk_path, &channel).map_err(|e|
                    eyre!("Failed to send file '{}' to channel: {}", task.chunk_path.display(), e)
                )?;
            }
        }
    }

    // Use the unified download_chunk_task for both master and chunk tasks
    download_chunk_task(task)?;

    // Master task post-processing
    if task.is_master_task() {
        // Wait for all chunks to complete and merge them
        log::debug!("Master task waiting for chunks to complete");
        wait_for_chunks_and_merge(task)?;

        // Verify file size if known
        let file_size_val = task.file_size.load(Ordering::Relaxed);
        let expected_size = if file_size_val > 0 { Some(file_size_val) } else { None };
        verify_file_size(&task.chunk_path, expected_size, &task.get_resolved_url())?;

        // Finalize download atomically
        atomic_file_completion(&task.chunk_path, &task.final_path)?;

        // Apply metadata (timestamp and ETag) to the final file
        if let Some(etag) = load_etag(&task.chunk_path.with_extension("etag")) {
            // Move ETag to final location and set metadata
            set_file_metadata("", &etag, &task.final_path, task);

            // Move ETag file to final location
            let final_etag_path = task.final_path.with_extension("etag");
            if let Err(e) = std::fs::rename(task.chunk_path.with_extension("etag"), &final_etag_path) {
                log::warn!("Failed to move ETag file to final location: {}", e);
            }
        }

        log::info!("download_file completed: {}", task.get_resolved_url());
    }

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
fn make_download_request_with_416_handling(
    task: &DownloadTask,
) -> Result<http::Response<ureq::Body>> {
    let url = task.get_resolved_url();
    let part_path = &task.chunk_path;
    let data_channel = match task.data_channel.lock() {
        Ok(dc) => dc.clone(),
        Err(_) => None,
    };

    let client = task.get_client()?;
    let mut request = client.get(url.replace("///", "/"))
        .config()
        .max_redirects(3)
        .build();

    // Get the download offset from task's chunk_offset or use the downloaded size
    let downloaded = task.chunk_offset.load(Ordering::Relaxed);

    // Check for existing ETag to enable conditional requests
    // Only for complete file downloads (not partial/range requests)
    if downloaded == 0 && task.chunk_size.load(Ordering::Relaxed) == 0 {
        // First check if the local file exists before considering ETag
        if part_path.exists() {
            if let Some(stored_etag) = load_etag(&part_path) {
                log::debug!("Adding If-None-Match header with ETag '{}' for conditional request", stored_etag);
                request = request.header("If-None-Match", &format!("\"{}\"", stored_etag));
            }
        } else {
            log::debug!("Local file {} doesn't exist, skipping ETag header", part_path.display());
            // Remove stale ETag sidecar so that next attempt performs full GET.
            let etag_path = part_path.with_extension("etag");
            let _ = std::fs::remove_file(&etag_path);
        }
    }

    // If we have a chunk size, use a specific range
    if task.chunk_size.load(Ordering::Relaxed) > 0 {
        let end = downloaded + task.chunk_size.load(Ordering::Relaxed) - 1; // HTTP ranges are inclusive
        log::debug!("download_file setting Range header: bytes={}-{}", downloaded, end);
        request = request.header("Range", &format!("bytes={}-{}", downloaded, end));
    } else if downloaded > 0 {
        // Otherwise request from offset to the end
        log::debug!("download_file setting Range header: bytes={}-", downloaded);
        request = request.header("Range", &format!("bytes={}-", downloaded));
    }

    let request_start = std::time::Instant::now();
    match request.call() {
        Ok(response) => {
            let latency = request_start.elapsed().as_millis() as u64;

            // Handle 304 Not Modified responses for ETag conditional requests
            if response.status().as_u16() == 304 {
                log::debug!("Received 304 Not Modified - file unchanged on server");
                task.set_message(format!("File unchanged (ETag match), skipping download - {}", part_path.display()));

                // If the final file exists, send it to the channel if needed
                if let Some(channel) = &data_channel {
                    send_file_to_channel(&part_path, channel)
                        .map_err(|e| eyre!("Failed to send cached file to channel: {}", e))?;
                }

                // Log this as a successful conditional request
                if let Err(e) = append_http_log(&url, HttpEvent::Latency(latency)) {
                    log::warn!("Failed to log 304 latency: {}", e);
                }

                return Err(eyre!("Download skipped - file unchanged (ETag match)"));
            }

            // Log latency info for successful requests
            if let Err(e) = append_http_log(&url, HttpEvent::Latency(latency)) {
                log::warn!("Failed to log download latency: {}", e);
            }
            Ok(response)
        },
        Err(ureq::Error::StatusCode(code)) => {
            let latency = request_start.elapsed().as_millis() as u64;
            log::debug!("download_file got HTTP error code: {}", code);

            // Log latency and error
            if let Err(e) = append_http_log(&url, HttpEvent::Latency(latency)) {
                log::warn!("Failed to log latency: {}", e);
            }

            if code == 416 && downloaded > 0 {
                // The requested byte range is outside the size of the file
                log::debug!("download_file handling HTTP 416 with downloaded={}", downloaded);
                return handle_416_range_error(task);
            }

            // Log specific HTTP error
            let http_event = if code == 404 {
                HttpEvent::NoContent
            } else {
                HttpEvent::HttpError(code)
            };

            if let Err(e) = append_http_log(&url, http_event) {
                log::warn!("Failed to log HTTP error: {}", e);
            }

            handle_non_416_http_error(code, &url, task)
        }
        Err(ureq::Error::Io(e)) => {
            let error_msg = format!("Network error: {} - {}", e, url);

            // Log network error
            if let Err(log_err) = append_http_log(&url, HttpEvent::NetError(error_msg.clone())) {
                log::warn!("Failed to log network error: {}", log_err);
            }

            task.set_message(error_msg.clone());
            Err(DownloadError::NetworkError(error_msg).into())
        }
        Err(e) => {
            // Check if this is a timeout error
            let error_str = e.to_string();
            let error_msg = format!("Error downloading: {} - {}", error_str, url);

            // Log general error as network error
            if let Err(log_err) = append_http_log(&url, HttpEvent::NetError(error_msg.clone())) {
                log::warn!("Failed to log download error: {}", log_err);
            }

            task.set_message(error_msg.clone());

            if error_str.contains("timeout") {
                Err(DownloadError::Timeout(error_msg).into())
            } else {
                Err(DownloadError::NetworkError(error_msg).into())
            }
        }
    }
}

/// Handle 416 Range Not Satisfiable error with full access to required context
fn handle_416_range_error(
    task: &DownloadTask,
) -> Result<http::Response<ureq::Body>> {
    let url = task.get_resolved_url();
    let part_path = &task.chunk_path;
    let data_channel = match task.data_channel.lock() {
        Ok(dc) => dc.clone(),
        Err(_) => None,
    };

    let downloaded = task.chunk_offset.load(Ordering::Relaxed); // Use task's chunk_offset instead of downloaded parameter
    // Send a request to check remote size and time, then compare with local
    let metadata_request_start = std::time::Instant::now();
            let client = task.get_client()?;
    let remote_metadata = client.get(url.replace("///", "/"))
        .config()
        .max_redirects(3)
        .build()
        .call()
        .with_context(|| format!("Failed to make HTTP request for {}", url))?;
    let metadata_latency = metadata_request_start.elapsed().as_millis() as u64;

    // Log metadata request latency
    if let Err(e) = append_http_log(&url, HttpEvent::Latency(metadata_latency)) {
        log::warn!("Failed to log metadata request latency: {}", e);
    }

    let remote_size_opt = parse_content_length(&remote_metadata);
    let remote_size = remote_size_opt.unwrap_or(0);
    log::debug!("download_file remote_size: {} (Content-Length present: {}), local_size: {}",
               remote_size, remote_size_opt.is_some(), downloaded);

    let remote_timestamp_opt = parse_remote_timestamp(&remote_metadata);

    let local_metadata = fs::metadata(part_path).map_err(|e| eyre!("Failed to get local file metadata: {}", e))?;
    let local_size = local_metadata.len();
    let local_last_modified_sys_time = local_metadata.modified().map_err(|e| eyre!("Failed to get local file modification time: {}", e))?;
    let local_last_modified: OffsetDateTime = local_last_modified_sys_time.into();

    if let Some(remote_ts) = remote_timestamp_opt {
        // A remote timestamp was successfully parsed from headers
        if remote_size == local_size && (remote_ts - local_last_modified).unsigned_abs() <= std::time::Duration::from_secs(2) {
            log::debug!("download_file sizes and timestamps match (remote_ts: {}, local_ts: {}), skipping download.", remote_ts, local_last_modified);
            let message = format!("Remote file unchanged (size and timestamp match), skipping download {}", part_path.display());
            task.set_message(message.clone());
            if let Some(channel) = &data_channel {
                send_file_to_channel(part_path, channel).map_err(|e| eyre!("Failed to send file to channel: {}", e))?;
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
            let error_msg = format!("{}, restarting download from 0: {}", reason, url);
            log::debug!("{}", error_msg.clone());
            task.set_message(error_msg.clone());
            fs::remove_file(part_path).map_err(|e| eyre!("Failed to remove part file '{}': {}", part_path.display(), e))?;
            return Err(DownloadError::TimestampMismatch(error_msg).into());
        }
    } else {
        // No valid remote timestamp header found, or parsing failed.
        if remote_size == local_size {
            log::debug!("No remote timestamp but size matches, assuming file is unchanged – skipping download.");
            task.set_message(format!("Remote file unchanged (size match), skipping download {}", part_path.display()));
            if let Some(channel) = &data_channel {
                send_file_to_channel(part_path, channel).map_err(|e| eyre!("Failed to send file to channel: {}", e))?;
            }
            return Err(eyre!("Download skipped - file unchanged"));
        }

        log::debug!("No valid remote timestamp – size differs (remote {}, local {}), forcing re-download.", remote_size, local_size);
        let error_msg = format!("Size mismatch without timestamp, re-downloading (current local size: {}, path: {})", local_size, part_path.display());
        task.set_message(error_msg.clone());
        fs::remove_file(part_path).map_err(|e| eyre!("Failed to remove part file '{}': {}", part_path.display(), e))?;
        return Err(DownloadError::TimestampMismatch(error_msg).into());
    }
}

/// Handle non-416 HTTP errors
fn handle_non_416_http_error(code: u16, url: &str, task: &DownloadTask) -> Result<http::Response<ureq::Body>> {
    let error_msg = format!("HTTP {}", code);
    task.set_message(format!("{} - {}", error_msg, url));

    if code >= 400 && code < 500 {
        // For client errors (like 403, 404), create a simple DownloadError without verbose backtrace
        log::debug!("Client error {} for {}", code, url);
        Err(DownloadError::Fatal(error_msg).into())
    } else {
        log::debug!("Server error {} for {}", code, url);
        Err(DownloadError::NetworkError(format!("HTTP error: {}", error_msg)).into())
    }
}

/// Validate response content type to detect HTML login pages
fn validate_response_content_type(
    response: &http::Response<ureq::Body>,
    url: &str,
    task: &DownloadTask,
) -> Result<()> {
    if let Some(content_type) = response.headers().get("content-type").and_then(|v| v.to_str().ok()) {
        if content_type.contains("text/html") {
            // Check if content is encoded (compressed) - this often indicates legitimate content
            if let Some(content_encoding) = response.headers().get("content-encoding").and_then(|v| v.to_str().ok()) {
                if content_encoding.contains("gzip") || content_encoding.contains("deflate") || content_encoding.contains("xz") {
                    log::debug!("HTML content detected but with content-encoding '{}', allowing download from {}", content_encoding, url);
                    return Ok(());
                }
            }

            let error_msg = "Received HTML page instead of file. This may indicate an authentication issue with the server.";
            task.set_message(error_msg.to_string());
            return Err(eyre!("Fatal error while downloading from {}: {}", url, error_msg.to_string()));
        }
    }
    Ok(())
}

/// Handle resume logic - check if server supports resuming and adjust downloaded bytes accordingly
fn handle_resume_logic(
    task: &DownloadTask,
    existing_bytes: u64,
    status: u16,
) -> Result<u64> {
    let part_path = &task.chunk_path;
    let url = task.get_resolved_url();

    if existing_bytes > 0 && status != 206 {
        // Log that server doesn't support range requests
        if let Err(e) = append_http_log(&url, HttpEvent::NoRange) {
            log::warn!("Failed to log range support info: {}", e);
        }

        fs::remove_file(part_path).map_err(|e| eyre!("Failed to remove part file '{}': {}", part_path.display(), e))?;
        task.set_message(format!("Server cannot resume, restarting - {}", url));
        Ok(0)
    } else {
        Ok(existing_bytes)
    }
}

/// Unified content download function that handles both master and chunk tasks
///
/// This function replaces both download_content() and download_chunk_content()
/// by using task.is_master_task() to handle master-specific logic
fn download_chunk_content(
    mut response: http::Response<ureq::Body>,
    task: &DownloadTask,
) -> Result<u64> {
    let data_channel = if task.is_master_task() {
        match task.data_channel.lock() {
            Ok(dc) => dc.clone(),
            Err(_) => None,
        }
    } else {
        None // Chunk tasks don't use data channels
    };

    // Validate response for chunk tasks
    if task.is_chunk_task() && response.status().as_u16() != 206 {
        return Err(eyre!("Expected 206 Partial Content for chunk download, got {}", response.status()));
    }

    // Setup file for writing
    let (mut file, existing_bytes) = setup_download_file(task)?;

    let mut reader = response.body_mut().as_reader();
    let mut buffer = vec![0; 8192];
    let mut last_update = std::time::Instant::now();
    let mut last_ondemand_check = std::time::Instant::now();

    // Track bytes separately
    let mut total_downloaded = existing_bytes; // Total bytes in file (reused + network)
    let mut network_bytes = 0u64; // Only bytes received from network

    let is_master = task.is_master_task();

    loop {
        let bytes_read = match reader.read(&mut buffer) {
            Ok(0) => break, // EOF reached
            Ok(n) => n,
            Err(e) => {
                if is_master {
                    let error_msg = format!("Read error at {} bytes: {}", total_downloaded,
                        task.resolved_url.lock().map(|r| r.clone()).unwrap_or_else(|_| task.url.clone()));
                    task.set_message(error_msg.clone());
                }
                return Err(eyre!("Failed to read from response (total_downloaded={}, buffer_size={}): {}", total_downloaded, buffer.len(), e));
            }
        };

        // Calculate bytes to write based on chunk boundaries
        let bytes_to_write = calculate_write_bytes(task, bytes_read, total_downloaded);

        // Write data to file with boundary checks
        let written_bytes = write_chunk_data(
            &mut file,
            &buffer,
            bytes_to_write,
            task,
            total_downloaded
        )?;

        if written_bytes == 0 {
            break; // Chunk task completed or reached boundary
        }

        total_downloaded += written_bytes as u64;
        network_bytes += written_bytes as u64;

        // Send data to channel for master tasks
        if let Some(channel) = &data_channel {
            if let Err(_) = channel.send(buffer[..written_bytes].to_vec()) {
                // Channel was closed, but we continue downloading
            }
        }

        if written_bytes < bytes_read {
            // Reached chunk boundary for master task
            break;
        }

        // Store only network bytes in received_bytes
        task.received_bytes.store(network_bytes, Ordering::Relaxed);

        // Update progress tracking
        update_download_progress(task, total_downloaded, &mut last_update);

        // Check for on-demand chunking opportunities
        check_ondemand_chunking(task, total_downloaded, &mut last_ondemand_check);
    }

    // Final progress update
    if is_master {
        let (total_received, _downloading_chunks) = task.get_total_progress_bytes();
        task.set_position(total_received);
    } else {
        task.set_position(total_downloaded);
    }

    log::debug!("download_content completed: {} total bytes ({} network bytes) written to {}",
               total_downloaded, network_bytes, task.chunk_path.display());

    Ok(total_downloaded)
}

/// Validate that the downloaded size matches the expected Content-Length
fn validate_download_size(downloaded: u64, total_size: u64, part_path: &Path) -> Result<()> {
    if total_size > 0 && downloaded != total_size {
        return Err(eyre!("Download size mismatch: Downloaded size ({}) does not match expected size ({}) for {}", downloaded, total_size, part_path.display()));
    }
    Ok(())
}

/// Set file metadata (timestamp and ETag) from response headers
fn set_file_metadata(last_modified: &str, etag: &str, final_path: &Path, task: &DownloadTask) {
    // Set timestamp if available
    if !last_modified.is_empty() {
        if let Ok(timestamp) = OffsetDateTime::parse(last_modified, &Rfc2822) {
            let system_time = filetime::FileTime::from_system_time(timestamp.into());
            if let Err(e) = set_file_mtime(final_path, system_time) {
                log::warn!("Failed to set mtime for {}: {}", final_path.display(), e);
            }
        } else {
            log::warn!("Failed to parse timestamp header value '{}' for mtime", last_modified);
        }
    }

    // Skip saving etag for immutable files since their content won't change over time
    if !etag.is_empty() && !task.is_immutable_file {
        if let Err(e) = save_etag(final_path, etag) {
            log::warn!("Failed to save ETag for {}: {}", final_path.display(), e);
        }
    }
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
fn parse_content_length(response: &http::Response<ureq::Body>) -> Option<u64> {
    // Check if content is compressed - if so, Content-Length is unreliable
    if let Some(content_encoding) = response.headers().get("content-encoding") {
        if let Ok(encoding) = content_encoding.to_str() {
            let encoding_lower = encoding.to_lowercase();
            if encoding_lower.contains("gzip") ||
               encoding_lower.contains("deflate") ||
               encoding_lower.contains("br") ||
               encoding_lower.contains("compress") ||
               encoding_lower.contains("xz") {
                log::debug!("Content is compressed with '{}', Content-Length ({}) refers to compressed size, not final size",
                           encoding,
                           response.headers().get("content-length")
                               .and_then(|v| v.to_str().ok())
                               .unwrap_or("unknown"));
                return None;
            }
        }
    }

    // 1. Try standard Content-Length header (both uppercase and lowercase versions)
    if let Some(content_length) = response.headers().get("Content-Length").or_else(|| response.headers().get("content-length")) {
        if let Ok(s) = content_length.to_str() {
            if let Ok(size) = s.parse::<u64>() {
                return Some(size);
            } else {
                log::warn!("Failed to parse Content-Length header value '{}': not a valid u64", s);
            }
        }
    }

    // 2. Try Content-Range header (e.g., "bytes 0-1023/4096")
    // Note: Content-Range should be reliable even with compression as it refers to the range
    // of the original (uncompressed) content
    if let Some(content_range) = response.headers().get("content-range") {
        if let Ok(s) = content_range.to_str() {
            // Parse "bytes START-END/TOTAL" format
            if let Some(total_size) = parse_content_range_total(s) {
                return Some(total_size);
            }
        }
    }

    // 3. Try X-Content-Length header (some servers use this)
    if let Some(x_content_length) = response.headers().get("x-content-length") {
        if let Ok(s) = x_content_length.to_str() {
            if let Ok(size) = s.parse::<u64>() {
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

/// Parse remote timestamp from Last-Modified or Date headers
fn parse_remote_timestamp(response: &http::Response<ureq::Body>) -> Option<OffsetDateTime> {
    response.headers().get("last-modified")
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

/// Parse ETag from response headers
///
/// ETags can be in different formats:
/// - etag: "67736fc6-dee" (lowercase with quotes)
/// - ETag: "67736fc6-dee" (mixed case with quotes)
/// - etag: 67736fc6-dee (without quotes)
/// - ETag: W/"67736fc6-dee" (weak ETag)
///
/// This function normalizes the ETag by removing quotes and the W/ prefix for weak ETags
fn parse_etag(response: &http::Response<ureq::Body>) -> Option<String> {
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

/// Save ETag to a sidecar file alongside the downloaded file
///
/// For a file like "package.rpm", this creates "package.rpm.etag" containing the ETag
fn save_etag(file_path: &Path, etag: &str) -> Result<()> {
    let etag_path = file_path.with_extension(
        format!("{}.etag", file_path.extension().and_then(|s| s.to_str()).unwrap_or(""))
    );

    std::fs::write(&etag_path, etag)
        .with_context(|| format!("Failed to save ETag to {}", etag_path.display()))?;

    log::debug!("Saved ETag '{}' to {}", etag, etag_path.display());
    Ok(())
}

/// Load ETag from a sidecar file
///
/// Returns the stored ETag if the sidecar file exists and is readable
fn load_etag(file_path: &Path) -> Option<String> {
    let etag_path = file_path.with_extension(
        format!("{}.etag", file_path.extension().and_then(|s| s.to_str()).unwrap_or(""))
    );

    match std::fs::read_to_string(&etag_path) {
        Ok(etag) => {
            let trimmed_etag = etag.trim().to_string();
            if trimmed_etag.is_empty() {
                None
            } else {
                log::debug!("Loaded ETag '{}' from {}", trimmed_etag, etag_path.display());
                Some(trimmed_etag)
            }
        }
        Err(_) => {
            log::debug!("No ETag file found at {}", etag_path.display());
            None
        }
    }
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
/// This function handles creating chunk tasks based on the task's file size and
/// how much has already been downloaded. It automatically checks if chunking
/// should be performed and logs the decision.
///
/// Cases handled:
/// 1. If task.file_size is None, no chunks are created
/// 2. If the file is smaller than MIN_FILE_SIZE_FOR_CHUNKING, no chunks are created
/// 3. If downloaded > 0, we skip chunks that are already downloaded
/// 4. The master task handles from current offset to next chunk boundary
///
/// Returns a vector of chunk tasks if chunks were created, empty vector otherwise
fn create_chunk_tasks(task: &DownloadTask) -> Result<Vec<Arc<DownloadTask>>> {
    let chunk_count = {
        let chunks_guard = task.chunk_tasks.lock()
            .map_err(|e| eyre!("Failed to lock chunk tasks: {}", e))?;
        chunks_guard.len()
    };
    // Already created?
    if chunk_count > 0 {
        if task.chunk_size.load(Ordering::Relaxed) == 0 {
            log::error!("create_chunk_tasks: chunk_size is 0 for {}", task.chunk_path.display());
            return Err(eyre!("create_chunk_tasks: chunk_size is 0 for {}", task.chunk_path.display()));
        }
        return Ok(Vec::new());
    }

    // Don't chunk if we don't know the file size
    let file_size_val = task.file_size.load(Ordering::Relaxed); if file_size_val == 0 {
        log::debug!("Cannot create chunks: file size unknown (no Content-Length header)");
        return Ok(Vec::new());
    };

    let downloaded = task.chunk_offset.load(Ordering::Relaxed);

    // Don't chunk small files or chunk tasks themselves
    if task.is_chunk_task() || file_size_val < downloaded + MIN_FILE_SIZE_FOR_CHUNKING {
        log::debug!("Skipping chunking: is_chunk_task={}, size={} bytes, min_required={} bytes",
                  task.is_chunk_task(), file_size_val, downloaded + MIN_FILE_SIZE_FOR_CHUNKING);
        return Ok(Vec::new());
    }

    log::debug!("Using known size {} bytes to create chunks (downloaded: {} bytes)", file_size_val, downloaded);

    log::debug!("Creating chunks for {} byte file with {} bytes already downloaded",
              file_size_val, downloaded);

    // Calculate the next chunk boundary after the already downloaded bytes. If we are
    // exactly on a 1 MiB boundary we need to move to the _next_ boundary, otherwise we
    // would produce a zero-length master chunk (next_boundary == downloaded).
    let next_boundary = if downloaded == 0 {
        MIN_CHUNK_SIZE
    } else if (downloaded & MIN_CHUNK_SIZE_MASK) == 0 {
        downloaded + MIN_CHUNK_SIZE
    } else {
        // Round up to the next 1 MiB boundary
        (downloaded + MIN_CHUNK_SIZE_MASK) & !MIN_CHUNK_SIZE_MASK
    };

    // Master task will handle from current offset to next chunk boundary
    let master_chunk_size = std::cmp::min(next_boundary - downloaded, file_size_val - downloaded);

    // Update master task's chunk information
    task.chunk_size.store(master_chunk_size, Ordering::Relaxed);

    log::debug!("Master task will handle {} bytes starting from offset {}",
              master_chunk_size, downloaded);

    // Starting offset for additional chunks is the next boundary
    let mut offset = next_boundary;

    // Create chunk tasks for the remaining parts of the file
    let mut chunk_tasks = Vec::new();
    while offset < file_size_val {
        let chunk_size = std::cmp::min(MIN_CHUNK_SIZE, file_size_val - offset);
        let chunk_task = task.create_chunk_task(offset, chunk_size);
        chunk_tasks.push(chunk_task);
        offset += chunk_size;
    }

    if chunk_tasks.is_empty() {
        log::debug!("No additional chunks needed for {} byte file with {} bytes already downloaded",
                 file_size_val, downloaded);
    } else {
        log::debug!("Created {} chunk tasks for {} byte file", chunk_tasks.len(), file_size_val);
    }

    Ok(chunk_tasks)
}


/// Unified download task function that handles both master and chunk tasks
///
/// This function coordinates the download process by using helper functions for
/// different aspects of the download lifecycle.
fn download_chunk_task(task: &DownloadTask) -> Result<()> {
    let url = &task.url;
    let chunk_path = &task.chunk_path;

    // Determine if we need Range support based on task characteristics
    let need_range = task.chunk_offset.load(Ordering::Relaxed) > 0 ||
                     task.chunk_size.load(Ordering::Relaxed) > 0 ||
                     task.is_chunk_task();

    // Resolve mirror for this attempt with appropriate Range requirements
    let (resolved_url, _final_path) = resolve_mirror_in_url(url, &task.output_dir, need_range)?;

    // Update resolved URL in task
    if let Ok(mut resolved) = task.resolved_url.lock() {
        *resolved = resolved_url.clone();
    }

    // Track mirror usage with RAII guard
    let _mirror_guard = MirrorUsageGuard::new(&resolved_url);

    let chunk_offset = task.chunk_offset.load(Ordering::Relaxed);
    let chunk_size = task.chunk_size.load(Ordering::Relaxed);

    if task.is_master_task() {
        log::debug!("Starting master download for {}", url);
        task.set_message(resolved_url.clone());
    } else {
        log::debug!("Starting chunk download: {} bytes at offset {} for {}",
                   chunk_size, chunk_offset, url);
    }

    // Check if file already exists and handle resumption
    let existing_bytes = get_existing_file_size(chunk_path)?;

    // Check if chunk task is already complete
    if check_chunk_completion(task, existing_bytes)? {
        return Ok(());
    }

    // Setup resumption state
    setup_resumption_state(task, existing_bytes);

    // Create directories if needed
    if let Some(parent) = chunk_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Create and execute HTTP request with comprehensive error handling
    let response = make_download_request_with_416_handling(task)?;

    // Handle task-specific response validation
    if task.is_master_task() {
        if handle_master_task_response(task, &response, &resolved_url, existing_bytes)? {
            return Ok(()); // Task completed early
        }

        // Handle metadata for master tasks
        handle_response_metadata(&response, task);

        // Save ETag for future conditional requests
        if let Some(etag) = parse_etag(&response) {
            if let Err(e) = save_etag(&task.chunk_path.with_extension("etag"), &etag) {
                log::warn!("Failed to save ETag for {}: {}", task.chunk_path.display(), e);
            }
        }
    } else {
        handle_chunk_task_response(&response, &resolved_url)?;
    }

    // Download the content using unified function
    let download_start = std::time::Instant::now();
    let final_downloaded = download_chunk_content(response, task)?;
    let download_duration = download_start.elapsed().as_millis() as u64;

    // Validate download size for master tasks
    if task.is_master_task() {
        let expected_size = task.file_size.load(Ordering::Relaxed);
        if expected_size > 0 {
            validate_download_size(final_downloaded, expected_size, &task.chunk_path)?;
        }
    }

    // Log download completion (use resolved_url since we don't have request_latency)
    log::debug!("Download completed for {}: {} bytes in {}ms",
               resolved_url, final_downloaded, download_duration);

    // Final logging and status update
    if task.is_chunk_task() {
        log::debug!("Completed chunk download: {} bytes at offset {} for {}",
                   chunk_size, chunk_offset, url);
        update_download_status(task, DownloadStatus::Completed)?;
    } else {
        log::debug!("Completed master download: {} bytes for {}", final_downloaded, url);
    }

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
/// Process a completed chunk by streaming data and merging to master file
fn process_completed_chunk(
    chunk_task: &DownloadTask,
    master_task: &DownloadTask,
    data_channel: &Option<Sender<Vec<u8>>>,
    chunk_index: i32
) -> Result<()> {
    let chunk_offset = chunk_task.chunk_offset.load(Ordering::Relaxed);
    log::debug!("Chunk {} at offset {} completed", chunk_index, chunk_offset);

    // Process the completed chunk immediately (STREAMING)
    if chunk_task.chunk_path.exists() {
        // If we have a data channel, stream the chunk data
        if let Some(ref channel) = data_channel {
            log::debug!("Streaming chunk {} data from {}", chunk_index, chunk_task.chunk_path.display());
            send_file_to_channel(&chunk_task.chunk_path, channel)?;
        }

        // Concatenate this chunk to the master file
        if let Err(e) = append_file_to_file(&chunk_task.chunk_path, &master_task.chunk_path) {
            log::warn!("Failed to append chunk {} to master file: {}", chunk_index, e);
        } else {
            log::debug!("Appended chunk {} to master file", chunk_index);
        }

        // Clean up this chunk file after processing
        if let Err(e) = fs::remove_file(&chunk_task.chunk_path) {
            log::warn!("Failed to clean up chunk file {}: {}", chunk_task.chunk_path.display(), e);
        } else {
            log::debug!("Cleaned up chunk file: {}", chunk_task.chunk_path.display());
        }
    } else {
        log::warn!("Chunk file not found: {}", chunk_task.chunk_path.display());
    }

    Ok(())
}

/// Handle a failed chunk by retrying or marking as failed
/// Returns true if the chunk should be retried, false if it failed permanently
fn handle_failed_chunk(
    chunk_task: &DownloadTask,
    master_task: &DownloadTask,
    chunk_index: i32,
    error: &str
) -> bool {
    let current_attempt = chunk_task.attempt_number.load(Ordering::SeqCst);

    if current_attempt < master_task.max_retries {
        // Retry the chunk: increment attempt number and reset status to Pending
        chunk_task.attempt_number.fetch_add(1, Ordering::SeqCst);

        if let Ok(mut status) = chunk_task.status.lock() {
            *status = DownloadStatus::Pending;
            let chunk_offset = chunk_task.chunk_offset.load(Ordering::Relaxed);
            log::info!("Retrying chunk {} at offset {} (attempt {}/{}): {}",
                     chunk_index, chunk_offset, current_attempt + 1, master_task.max_retries, error);
        }

        true // Retry the chunk
    } else {
        // Max retries exceeded - record failure
        let chunk_offset = chunk_task.chunk_offset.load(Ordering::Relaxed);
        log::error!("Chunk {} at offset {} failed after {} attempts: {}",
                  chunk_index, chunk_offset, master_task.max_retries, error);

        false // Don't retry, mark as permanently failed
    }
}

/// Update progress display for chunks that are still pending or downloading
fn update_chunk_progress(
    chunk_task: &DownloadTask,
    master_task: &DownloadTask
) {
    // Update progress with current total
    let (total_received, downloading_chunks) = master_task.get_total_progress_bytes();

    // Update progress bar message with chunk count if there are downloading chunks
    let resolved_url = chunk_task.resolved_url.lock()
        .map(|r| r.clone())
        .unwrap_or_else(|_| chunk_task.url.clone());

    if downloading_chunks == 0 {
        master_task.set_message(resolved_url);
    } else {
        master_task.set_message(format!("#{} {}", downloading_chunks, resolved_url));
    }

    master_task.set_position(total_received);
    log::trace!("Chunk progress update: {} bytes received", total_received);
}

/// Remove the first chunk from the chunk tasks list
fn remove_first_chunk(master_task: &DownloadTask) -> Result<()> {
    let mut chunks_guard = master_task.chunk_tasks.lock()
        .map_err(|e| eyre!("Failed to lock chunk tasks for removal: {}", e))?;
    if !chunks_guard.is_empty() {
        chunks_guard.remove(0);
    }
    Ok(())
}

fn wait_for_chunks_and_merge(master_task: &DownloadTask) -> Result<()> {
    // Check if we have any chunks to process at all
    let chunk_count = {
        let chunks_guard = master_task.chunk_tasks.lock()
            .map_err(|e| eyre!("Failed to lock chunk tasks: {}", e))?;
        chunks_guard.len()
    };

    if chunk_count == 0 {
        return Ok(()); // No chunks to wait for
    }

    log::debug!("Processing {} chunks for {}", chunk_count, master_task.url);

    // Check if we have a data channel to stream to
    let data_channel = match master_task.data_channel.lock() {
        Ok(dc) => dc.clone(),
        Err(_) => None,
    };
    let mut any_fail = false; // Track if any chunks failed after exhausting retries

    // Process chunks one by one in order until all are complete
    // STREAMING BEHAVIOR: We process each chunk as soon as it's ready, not all at once
    loop {
        // Get first chunk and its status - take lock only briefly
        let (first_chunk, remaining_count) = {
            let chunks_guard = master_task.chunk_tasks.lock()
                .map_err(|e| eyre!("Failed to lock chunk tasks: {}", e))?;

            if chunks_guard.is_empty() {
                break; // All chunks processed
            }

            (Arc::clone(&chunks_guard[0]), chunks_guard.len())
        };
        // Lock is released here

        let chunk_index = -(remaining_count as i32); // For logging (chunks.len() - index from end)

        // Check chunk status without holding any locks
        match first_chunk.get_status() {
            DownloadStatus::Completed => {
                // Process the completed chunk immediately (STREAMING)
                process_completed_chunk(&first_chunk, master_task, &data_channel, chunk_index)?;

                // Remove this chunk from the list
                remove_first_chunk(master_task)?;
            },
            DownloadStatus::Failed(ref err) => {
                // Handle chunk failure and retry logic
                if handle_failed_chunk(&first_chunk, master_task, chunk_index, err) {
                    // Chunk will be retried - don't remove it
                } else {
                    // Max retries exceeded - record failure and remove chunk
                    any_fail = true;
                    remove_first_chunk(master_task)?;
                }
            },
            DownloadStatus::Pending | DownloadStatus::Downloading => {
                // Chunk is not ready yet, continue waiting
                update_chunk_progress(&first_chunk, master_task);

                // Sleep WITHOUT holding any locks
                std::thread::sleep(std::time::Duration::from_millis(500));
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
        let total_size = if task.chunk_size.load(Ordering::Relaxed) > 0 { task.chunk_size.load(Ordering::Relaxed) } else { task.file_size.load(Ordering::Relaxed) };
        let (total_downloaded, _) = task.get_total_progress_bytes(); // includes reused bytes

        if network_downloaded > 0 && total_downloaded < total_size {
            let rate = network_downloaded as f64 / elapsed.as_secs_f64();
            let remaining_bytes = total_size - total_downloaded;
            let estimated_seconds = remaining_bytes as f64 / rate;
            return Duration::from_secs_f64(estimated_seconds);
        }
    }

    Duration::from_secs(0)
}

/// Check if on-demand chunking should be performed and create chunks if appropriate
///
/// This function encapsulates the logic for determining whether a download task
/// should be split into multiple chunks for parallel downloading. It checks:
///
/// 1. If the file size is known and large enough for chunking
/// 2. If the estimated remaining download time is significant (>5s)
/// 3. If there's enough remaining data to justify creating at least 2 chunks
///
/// The function is called periodically during download with a rate-limiting delay
/// to prevent excessive chunking decisions based on unstable early measurements.
fn check_for_ondemand_chunking_opportunity(task: &DownloadTask, existing_bytes: u64) {
    // Only proceed if we know the file size
    let file_size_val = task.file_size.load(Ordering::Relaxed);
    if file_size_val > 0 {
        if file_size_val < MIN_FILE_SIZE_FOR_CHUNKING {
            let estimated_time = estimate_remaining_time(task);
            let remaining_size = file_size_val - existing_bytes;

            // Check conditions for on-demand chunking
            if estimated_time > Duration::from_secs(5) && remaining_size >= 2 * ONDEMAND_CHUNK_SIZE {
                log::debug!("On-demand chunking opportunity: estimated {}s remaining, {} bytes left",
                           estimated_time.as_secs(), remaining_size);

                // Create on-demand chunks for remaining data
                if let Ok(chunk_count) = create_ondemand_chunks(task, existing_bytes, remaining_size) {
                    log::info!("Created {} on-demand chunks for {} bytes remaining", chunk_count, remaining_size);
                }
            }
        }
    }
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
    // Calculate the next 256KB boundary after current position.
    // If we are already aligned to a boundary (i.e. existing_bytes is an exact multiple
    // of ONDEMAND_CHUNK_SIZE) we must advance by one full chunk; otherwise `next_boundary`
    // would equal `existing_bytes`, producing a zero-length master chunk and triggering
    // errors later when `create_chunk_tasks` is called again.

    let next_boundary = if (existing_bytes & ONDEMAND_CHUNK_SIZE_MASK) == 0 {
        existing_bytes + ONDEMAND_CHUNK_SIZE
    } else {
        (existing_bytes + ONDEMAND_CHUNK_SIZE_MASK) & !ONDEMAND_CHUNK_SIZE_MASK
    };

    let total_size = existing_bytes + remaining_size;

    // Modify master task to cover from current position to next 256KB boundary
    let master_chunk_size = std::cmp::min(next_boundary - master_task.chunk_offset.load(Ordering::Relaxed), remaining_size);

    // Update master task's chunk information
    master_task.chunk_size.store(master_chunk_size, Ordering::Relaxed);

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

    let pid_content = format!("pid={}\ntime={}\n", pid, timestamp);

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

    // Parse the new format: pid=123\ntime=456\n
    let mut pid_opt = None;

    for line in content.lines() {
        if let Some(value) = line.strip_prefix("pid=") {
            pid_opt = value.parse::<u32>().ok();
            break;
        }
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

    // Sort by offset
    chunk_files.sort_by(|a, b| {
        let extract_offset = |path: &PathBuf| -> u64 {
            if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
                if let Some(offset_part) = filename.split("-O").nth(1) {
                    offset_part.parse::<u64>().unwrap_or(0)
                } else {
                    0
                }
            } else {
                0
            }
        };
        extract_offset(a).cmp(&extract_offset(b))
    });

    Ok(chunk_files)
}

/// Extract and set file metadata (timestamp and ETag) from response headers for master tasks
fn handle_response_metadata(response: &http::Response<ureq::Body>, task: &DownloadTask) {
    if !task.is_master_task() {
        return; // Only master tasks handle metadata
    }

    // Extract metadata from response headers
    let last_modified = response.headers().get("last-modified")
        .and_then(|s| s.to_str().ok())
        .unwrap_or("")
        .to_string();

    let etag = response.headers().get("etag")
        .and_then(|s| s.to_str().ok())
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_default();

    // Set metadata for the final file after download completion
    if !last_modified.is_empty() {
        if let Ok(timestamp) = time::OffsetDateTime::parse(&last_modified, &time::format_description::well_known::Rfc2822) {
            let system_time = filetime::FileTime::from_system_time(timestamp.into());
            if let Err(e) = filetime::set_file_mtime(&task.final_path, system_time) {
                log::warn!("Failed to set mtime for {}: {}", task.final_path.display(), e);
            }
        }
    }

    // Skip saving etag for immutable files since their content won't change over time
    if !etag.is_empty() && !task.is_immutable_file {
        let etag_path = task.final_path.with_extension("etag");
        if let Err(e) = std::fs::write(&etag_path, &etag) {
            log::warn!("Failed to save ETag for {}: {}", task.final_path.display(), e);
        }
    }
}

/// Verify that the downloaded file size matches expectations
fn verify_downloaded_file_size(part_path: &Path, expected_size: Option<u64>) -> Result<()> {
    if let Some(expected) = expected_size {
        let actual_size = fs::metadata(part_path)?.len();
        if actual_size != expected {
            return Err(eyre!("Downloaded file size ({}) doesn't match expected size ({}) for {}",
                            actual_size, expected, part_path.display()));
        }
    }
    Ok(())
}

/// Check if a chunk task is already complete and handle early completion
fn check_chunk_completion(task: &DownloadTask, existing_bytes: u64) -> Result<bool> {
    let chunk_size = task.chunk_size.load(Ordering::Relaxed);

    if task.is_chunk_task() && chunk_size > 0 && existing_bytes >= chunk_size {
        log::debug!("Chunk file already exists and is complete: {}", task.chunk_path.display());
        task.resumed_bytes.store(chunk_size, Ordering::Relaxed);
        task.received_bytes.store(0, Ordering::Relaxed);
        update_download_status(task, DownloadStatus::Completed)?;
        return Ok(true); // Task is complete
    }
    Ok(false) // Task is not complete
}

/// Setup resumption state for a download task
fn setup_resumption_state(task: &DownloadTask, existing_bytes: u64) {
    if existing_bytes > 0 {
        if task.is_master_task() {
            task.chunk_offset.store(existing_bytes, Ordering::Relaxed);
        }
        task.resumed_bytes.store(existing_bytes, Ordering::Relaxed);
        log::debug!("Resuming download from {} bytes for {}", existing_bytes, &task.url);
    }
}

/// Create and execute HTTP request with appropriate Range headers
fn create_and_execute_http_request(
    task: &DownloadTask,
    client: &Agent,
    resolved_url: &str,
    existing_bytes: u64
) -> Result<(http::Response<ureq::Body>, u64)> {
    let mut request = client.get(resolved_url);

    // Add Range header if needed (for resumption or chunking)
    if existing_bytes > 0 || task.is_chunk_task() {
        let chunk_offset = task.chunk_offset.load(Ordering::Relaxed);
        let chunk_size = task.chunk_size.load(Ordering::Relaxed);

        let start_offset = if task.is_chunk_task() {
            chunk_offset + existing_bytes
        } else {
            existing_bytes
        };

        let range_header = if task.is_chunk_task() && chunk_size > 0 {
            format!("bytes={}-{}", start_offset, chunk_offset + chunk_size - 1)
        } else {
            format!("bytes={}-", start_offset)
        };

        request = request.header("Range", &range_header);
        log::debug!("Using Range header: {}", range_header);
    }

    // Execute the request with timing and error handling
    let request_start = std::time::Instant::now();

    let response = request
        .call()
        .map_err(|e| {
            // Log HTTP errors
            let http_event = match e {
                ureq::Error::StatusCode(code) => {
                    if code == 404 {
                        HttpEvent::NoContent
                    } else {
                        HttpEvent::HttpError(code)
                    }
                }
                ureq::Error::Io(_) => HttpEvent::NetError(e.to_string()),
                _ => HttpEvent::NetError(e.to_string()),
            };
            if let Err(log_err) = append_http_log(resolved_url, http_event) {
                log::warn!("Failed to log HTTP error: {}", log_err);
            }

            eyre!("Failed to make request for {}: {}", resolved_url, e)
        })?;

    let request_latency = request_start.elapsed().as_millis() as u64;

    // Log request latency
    if let Err(e) = append_http_log(resolved_url, HttpEvent::Latency(request_latency)) {
        log::warn!("Failed to log request latency: {}", e);
    }

    Ok((response, request_latency))
}

/// Handle master task specific response validation and setup
fn handle_master_task_response(
    task: &DownloadTask,
    response: &http::Response<ureq::Body>,
    resolved_url: &str,
    existing_bytes: u64
) -> Result<bool> {
    // Check for unchanged file case
    if let Some(status_line) = response.headers().get("status") {
        if let Ok(status_str) = status_line.to_str() {
            if status_str.contains("304") || status_str.contains("unchanged") {
                // File hasn't changed, just ensure final file exists
                if !task.final_path.exists() && task.chunk_path.exists() {
                    atomic_file_completion(&task.chunk_path, &task.final_path)?;
                }
                return Ok(true); // Task is complete
            }
        }
    }

    // Validate response and handle resume logic for master tasks
    validate_response_content_type(response, resolved_url, task)?;

    if existing_bytes > 0 && response.status().as_u16() != 206 {
        // Resume failed, restart from beginning
        if task.chunk_path.exists() {
            fs::remove_file(&task.chunk_path)?;
        }
        task.chunk_offset.store(0, Ordering::Relaxed);
        task.resumed_bytes.store(0, Ordering::Relaxed);
        log::debug!("Server doesn't support resume, restarting download");
        return Err(eyre!("Resume failed, need to restart download"));
    }

    // Setup file size and progress tracking for master tasks
    if task.file_size.load(Ordering::Relaxed) == 0 {
        if let Some(content_length) = parse_content_length(response) {
            let total_size = content_length + existing_bytes;
            task.file_size.store(total_size, Ordering::Relaxed);
            log::debug!("Content-Length header found, file size = {}", total_size);
        }
    }

    let file_size_val = task.file_size.load(Ordering::Relaxed);
    if file_size_val > 0 {
        task.set_length(file_size_val);
    }
    task.set_position(existing_bytes);

    // Set start time for estimation
    if let Ok(mut start_time) = task.start_time.lock() {
        if start_time.is_none() {
            *start_time = Some(std::time::Instant::now());
        }
    }

    Ok(false) // Task is not complete yet
}

/// Handle chunk task specific response validation
fn handle_chunk_task_response(response: &http::Response<ureq::Body>, resolved_url: &str) -> Result<()> {
    // For chunk tasks, validate we got partial content
    if response.status().as_u16() == 200 {
        // Server ignoring Range header - would corrupt chunk
        if let Err(e) = append_http_log(resolved_url, HttpEvent::NoRange) {
            log::warn!("Failed to log chunk range error: {}", e);
        }
        return Err(eyre!("Server returned 200 instead of 206 for range request - would corrupt chunk"));
    }
    if response.status().as_u16() != 206 {
        return Err(eyre!("Server returned {} for range request", response.status()));
    }
    Ok(())
}

/// Log download completion statistics
fn log_download_completion(
    task: &DownloadTask,
    resolved_url: &str,
    request_latency: u64,
    download_duration: u64
) {
    let network_bytes = task.received_bytes.load(Ordering::Relaxed);
    if network_bytes > 0 {
        if let Err(e) = append_download_log(
            resolved_url,
            task.chunk_offset.load(Ordering::Relaxed),
            network_bytes,
            download_duration + request_latency,
            true,
        ) {
            log::warn!("Failed to log download completion: {}", e);
        }
    }
}

/// Setup file for download content writing
fn setup_download_file(task: &DownloadTask) -> Result<(File, u64)> {
    let chunk_path = &task.chunk_path;

    // Check if we need to append to an existing file
    let existing_bytes = if chunk_path.exists() {
        fs::metadata(chunk_path)
            .map(|m| m.len() as u64)
            .unwrap_or(0)
    } else {
        0
    };

    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(existing_bytes > 0) // Append if file exists with content
        .truncate(existing_bytes == 0) // Only truncate if file is empty or doesn't exist
        .open(chunk_path)
        .map_err(|e| eyre!("Failed to open file '{}': {}", chunk_path.display(), e))?;

    Ok((file, existing_bytes))
}

/// Calculate bytes to write considering chunk boundaries
fn calculate_write_bytes(
    task: &DownloadTask,
    bytes_read: usize,
    total_downloaded: u64
) -> usize {
    let chunk_size_val = task.chunk_size.load(Ordering::Relaxed);

    if chunk_size_val > 0 {
        let boundary = if task.is_master_task() {
            task.chunk_offset.load(Ordering::Relaxed) + chunk_size_val
        } else {
            chunk_size_val // For chunk tasks, chunk_size is the limit
        };

        if total_downloaded >= boundary {
            if task.is_master_task() {
                log::debug!("Master task reached chunk boundary at {} bytes, stopping", total_downloaded);
            } else {
                log::debug!("Chunk task completed at {} bytes", total_downloaded);
            }
            return 0; // Signal to stop
        }

        // Adjust bytes to write if we're approaching the limit
        if total_downloaded + bytes_read as u64 > boundary {
            (boundary - total_downloaded) as usize
        } else {
            bytes_read
        }
    } else {
        bytes_read
    }
}

/// Handle chunk task specific writing with boundary checks
fn write_chunk_data(
    file: &mut File,
    buffer: &[u8],
    bytes_to_write: usize,
    task: &DownloadTask,
    total_downloaded: u64
) -> Result<usize> {
    if !task.is_master_task() {
        let chunk_size_val = task.chunk_size.load(Ordering::Relaxed);
        if chunk_size_val > 0 {
            let remaining = chunk_size_val.saturating_sub(total_downloaded);
            if remaining == 0 {
                log::warn!("Chunk task received {} surplus bytes, discarding", bytes_to_write);
                return Ok(0); // Signal to stop
            }
            let write_len = std::cmp::min(bytes_to_write, remaining as usize);

            file.write_all(&buffer[..write_len])
                .map_err(|e| eyre!("Failed to write {} bytes to chunk file '{}': {}",
                                  write_len, task.chunk_path.display(), e))?;

            if write_len < bytes_to_write && total_downloaded + write_len as u64 > chunk_size_val {
                log::warn!("Chunk {} exceeded expected size by {} bytes; extra data ignored",
                          task.chunk_path.display(), (total_downloaded + write_len as u64) - chunk_size_val);
            }

            return Ok(write_len);
        }
    }

    // Master task or no size limit
    file.write_all(&buffer[..bytes_to_write])
        .map_err(|e| eyre!("Failed to write {} bytes to file '{}': {}",
                          bytes_to_write, task.chunk_path.display(), e))?;

    Ok(bytes_to_write)
}

/// Update progress tracking for download tasks
fn update_download_progress(
    task: &DownloadTask,
    total_downloaded: u64,
    last_update: &mut std::time::Instant
) {
    let now = std::time::Instant::now();
    if now.duration_since(*last_update) > Duration::from_millis(500) {
        if task.is_master_task() {
            // For master tasks, show total progress across all chunks (reused + network bytes)
            let (total_received, downloading_chunks) = task.get_total_progress_bytes();
            task.set_position(total_received);

            // Update progress bar message with chunk count if there are downloading chunks
            let resolved_url = task.resolved_url.lock()
                .map(|r| r.clone())
                .unwrap_or_else(|_| task.url.clone());
            if downloading_chunks == 0 {
                task.set_message(resolved_url);
            } else {
                task.set_message(format!("+{} {}", downloading_chunks, resolved_url));
            }
        } else {
            // For chunk tasks, show total downloaded (reused + network bytes)
            task.set_position(total_downloaded);
        }
        *last_update = now;
    }
}

/// Check for on-demand chunking opportunities (master tasks only)
fn check_ondemand_chunking(
    task: &DownloadTask,
    total_downloaded: u64,
    last_ondemand_check: &mut std::time::Instant
) {
    let now = std::time::Instant::now();
    if task.is_master_task() &&
       task.chunk_size.load(Ordering::Relaxed) == 0 &&
       now.duration_since(*last_ondemand_check) > Duration::from_secs(1) &&
       DOWNLOAD_MANAGER.current_task_count.load(Ordering::Relaxed) <= DOWNLOAD_MANAGER.nr_parallel / 2 {

        check_for_ondemand_chunking_opportunity(task, total_downloaded);
        *last_ondemand_check = now;
    }
}
