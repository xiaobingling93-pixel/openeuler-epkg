// ============================================================================
// DOWNLOAD MANAGER - Core Download Coordination and Execution
//
// This module implements the central download manager that coordinates all
// download operations in epkg. It provides thread-safe task management,
// concurrency control, and comprehensive error handling for parallel downloads.
// ============================================================================

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

use color_eyre::eyre::{eyre, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicUsize, Ordering}, LazyLock};
use std::thread;
use std::time::Duration;
use indicatif::MultiProgress;
use crate::config;
use crate::mirror;
use super::types::*;
use super::orchestration::download_task;
use super::utils::log_error_with_backtrace;
use super::progress::{collect_task_eta_stats, update_global_stats, dump_global_stats_ratelimit};
use super::chunk::{may_ondemand_chunking, download_chunk_task};
use super::task::update_download_status;

pub struct DownloadManager {
    multi_progress: MultiProgress,
    pub(crate) tasks: Arc<Mutex<HashMap<String, Arc<DownloadTask>>>>,
    nr_parallel: usize,
    task_handles: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
    chunk_handles: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
    is_processing: Arc<AtomicBool>,
    current_task_count: Arc<AtomicUsize>,

    // ETA and download statistics - replaced atomically
    pub(crate) stats: Arc<Mutex<DownloadManagerStats>>,

    // Cancellation flag for graceful shutdown
    cancelled: Arc<AtomicBool>,
}

/// Download manager statistics - replaced atomically as a whole
impl DownloadManager {
    pub(crate) fn new(nr_parallel: usize) -> Result<Self> {
        // Note: Proxy configuration is now handled per-task via on-demand client creation from config().common.proxy

        let multi_progress = MultiProgress::new();

        Ok(Self {
            multi_progress,
            tasks:                Arc::new(Mutex::new(HashMap::new())),
            nr_parallel,
            task_handles:         Arc::new(Mutex::new(Vec::new())),
            chunk_handles:        Arc::new(Mutex::new(Vec::new())),
            is_processing:        Arc::new(AtomicBool::new(false)),
            current_task_count:   Arc::new(AtomicUsize::new(0)),
            stats:                Arc::new(Mutex::new(DownloadManagerStats::default())),
            cancelled:            Arc::new(AtomicBool::new(false)),
        })
    }

    fn submit_task(&self, task: DownloadTask) -> Result<()> {
        let mut tasks = self.tasks.lock()
            .map_err(|e| eyre!("Failed to lock tasks mutex: {}", e))?;
        if !tasks.contains_key(&task.url) {
            tasks.insert(task.url.clone(), Arc::new(task));
        } else {
            // For duplicate URLs, share the data channel with the existing task
            if let Some(existing_task) = tasks.get(&task.url) {
                if let Some(new_data_channel) = task.get_data_channel() {
                    // Add the new data channel to the existing task for data sharing
                    existing_task.add_data_channel(new_data_channel);
                    log::debug!("Added data channel for duplicate download URL: {} final_path: {}",
                               task.url, task.final_path.display());
                } else {
                    log::warn!("Skipping duplicate download URL (no data channel): {} final_path: {}",
                               task.url, task.final_path.display());
                }
            }
        }
        Ok(())
    }

    pub(crate) fn get_task(&self, url: &str) -> Option<Arc<DownloadTask>> {
        let tasks = self.tasks.lock().ok()?;
        tasks.get(url).map(|task| Arc::clone(task))
    }

    /// Check if a task exists for the given URL
    fn has_task(&self, url: &str) -> bool {
        if let Ok(tasks) = self.tasks.lock() {
            tasks.contains_key(url)
        } else {
            false
        }
    }

    pub fn wait_for_task(&self, task_url: String) -> Result<(DownloadStatus, Option<PathBuf>)> {
        loop {
            // Check for cancellation first
            if self.cancelled.load(Ordering::Relaxed) {
                return Err(eyre!("Download cancelled by user"));
            }

            // Take a clone of the task under the lock, then release it before get_status().
            // Holding manager.tasks while calling task.get_status() can deadlock with the
            // processing thread (see wait_for_any_task).
            let task_ref = {
                let tasks = self.tasks.lock()
                    .map_err(|e| eyre!("Failed to lock tasks mutex: {}", e))?;
                tasks.get(&task_url).map(Arc::clone)
            };
            let Some(task) = task_ref else {
                return Err(eyre!("Task with URL {} not found", task_url));
            };
            let status = task.get_status();
            match status {
                DownloadStatus::Completed => {
                    let final_path = Some(task.final_path.clone());
                    return Ok((status, final_path));
                }
                DownloadStatus::Failed(_) => {
                    return Ok((status, None));
                }
                _ => {}
            }
            thread::sleep(Duration::from_millis(WAIT_TASK_DURATION_MS));
        }
    }

    /// Wait for any download task to complete and return the completed task's URL
    fn wait_for_any_task(&self, task_urls: &[String]) -> Result<Option<String>> {
        if task_urls.is_empty() {
            return Ok(None);
        }

        loop {
            // Check for cancellation first
            if self.cancelled.load(Ordering::Relaxed) {
                return Err(eyre!("Download cancelled by user"));
            }

            // Collect task refs under the lock, then release it before calling get_status().
            // Holding manager.tasks while calling task.get_status() (which locks task.status)
            // can deadlock with the processing thread that needs manager.tasks in
            // iterate_3level_tasks / process_pending_master_tasks.
            let to_check: Vec<(String, Arc<DownloadTask>)> = {
                let tasks = self.tasks.lock()
                    .map_err(|e| eyre!("Failed to lock tasks mutex: {}", e))?;
                task_urls.iter()
                    .filter_map(|url| tasks.get(url).map(|t| (url.clone(), Arc::clone(t))))
                    .collect()
            };

            for (task_url, task) in &to_check {
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

            thread::sleep(Duration::from_millis(WAIT_TASK_DURATION_MS));
        }
    }

    pub fn start_processing(&self) {
        if self.is_processing.load(Ordering::Relaxed) {
            return;
        }

        self.is_processing.store(true, Ordering::Relaxed);

        // Initialize processing thread with all required context
        self.spawn_main_processing_thread();
    }

    /// Spawn the main processing thread that coordinates all download activities
    /// Level 2: Thread Management - handles the main processing lifecycle
    fn spawn_main_processing_thread(&self) {
        let tasks                  = Arc::clone(&self.tasks);
        let multi_progress         = self.multi_progress.clone();
        let is_processing          = Arc::clone(&self.is_processing);
        let task_handles           = Arc::clone(&self.task_handles);
        let chunk_handles          = Arc::clone(&self.chunk_handles);
        let nr_parallel            = self.nr_parallel;
        let current_task_count_arc = Arc::clone(&self.current_task_count);
        let cancelled              = Arc::clone(&self.cancelled);

        thread::spawn(move || {
            Self::run_main_processing_loop(
                tasks,
                multi_progress,
                is_processing,
                task_handles,
                chunk_handles,
                nr_parallel,
                current_task_count_arc,
                cancelled,
            );
        });
    }

    /// Main processing loop that handles task scheduling and coordination
    /// Level 3: Task Coordination - orchestrates task and chunk processing
    fn run_main_processing_loop(
        tasks: Arc<Mutex<HashMap<String, Arc<DownloadTask>>>>,
        multi_progress: MultiProgress,
        is_processing: Arc<AtomicBool>,
        task_handles: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
        chunk_handles: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
        nr_parallel: usize,
        current_task_count_arc: Arc<AtomicUsize>,
        cancelled: Arc<AtomicBool>,
    ) {
        loop {
            // Check for cancellation
            if cancelled.load(Ordering::Relaxed) {
                log::info!("Main processing loop cancelled, stopping");
                is_processing.store(false, Ordering::Relaxed);
                break;
            }

            // Clean up finished threads
            Self::cleanup_finished_handles(&task_handles);
            Self::cleanup_finished_handles(&chunk_handles);

            // Check for completion or process pending tasks
            match Self::process_pending_master_tasks(
                &tasks,
                &multi_progress,
                &task_handles,
                nr_parallel,
                &current_task_count_arc,
            ) {
                ProcessingResult::AllCompleted => {
                    is_processing.store(false, Ordering::Relaxed);
                    break;
                }
                ProcessingResult::Continue => {
                    // Start chunk processing for existing tasks
                    Self::start_chunks_processing(&tasks, &chunk_handles, nr_parallel);
                    thread::sleep(Duration::from_millis(WAIT_TASK_DURATION_MS));
                }
            }
        }
    }

    /// Process pending master tasks and spawn new download threads
    /// Level 4: Task Scheduling - handles master task prioritization and spawning
    fn process_pending_master_tasks(
        tasks: &Arc<Mutex<HashMap<String, Arc<DownloadTask>>>>,
        multi_progress: &MultiProgress,
        task_handles: &Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
        nr_parallel: usize,
        current_task_count_arc: &Arc<AtomicUsize>,
    ) -> ProcessingResult {
        let tasks_guard = match tasks.lock() {
            Ok(guard) => guard,
            Err(e) => {
                log::error!("Failed to lock tasks mutex: {}", e);
                return ProcessingResult::AllCompleted;
            }
        };

        // Collect and prioritize pending tasks
        let pending_tasks = Self::collect_and_prioritize_pending_tasks(&tasks_guard);

        if pending_tasks.is_empty() {
            // Check if all tasks are completed
            let all_done = tasks_guard.iter()
                .all(|(_, t)| matches!(t.get_status(), DownloadStatus::Completed | DownloadStatus::Failed(_)));

            drop(tasks_guard);

            if all_done {
                return ProcessingResult::AllCompleted;
            } else {
                return ProcessingResult::Continue;
            }
        }

        // Spawn new task threads within capacity limits
        let spawned_count = Self::spawn_task_threads(
            pending_tasks,
            multi_progress,
            task_handles,
            nr_parallel,
            current_task_count_arc,
        );

        drop(tasks_guard);

        log::trace!("Spawned {} new download threads", spawned_count);
        ProcessingResult::Continue
    }

    /// Collect pending tasks and sort them by priority (largest first)
    /// Level 5: Task Collection - handles task filtering and prioritization
    fn collect_and_prioritize_pending_tasks(
        tasks_guard: &HashMap<String, Arc<DownloadTask>>
    ) -> Vec<(String, Arc<DownloadTask>)> {
        let mut pending_tasks: Vec<_> = tasks_guard.iter()
            .filter(|(_, t)| matches!(t.get_status(), DownloadStatus::Pending))
            .map(|(url, task)| (url.clone(), Arc::clone(task)))
            .collect();

        // Sort by file size (largest first) for optimal resource utilization
        pending_tasks.sort_by(|(_, a), (_, b)| {
            let size_a = a.file_size.load(Ordering::Relaxed);
            let size_b = b.file_size.load(Ordering::Relaxed);
            size_b.cmp(&size_a) // Descending order
        });

        pending_tasks
    }

    /// Spawn task threads for pending downloads within capacity limits
    /// Level 5: Thread Spawning - handles individual task thread creation
    fn spawn_task_threads(
        pending_tasks: Vec<(String, Arc<DownloadTask>)>,
        multi_progress: &MultiProgress,
        task_handles: &Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
        nr_parallel: usize,
        current_task_count_arc: &Arc<AtomicUsize>,
    ) -> usize {
        let mut current_task_count = {
            let handles_guard = task_handles.lock().unwrap();
            handles_guard.len()
        };

        let mut spawned_count = 0;

        for (_task_url, task) in pending_tasks {
            current_task_count_arc.store(current_task_count, Ordering::Relaxed);

            if current_task_count >= nr_parallel {
                break; // Reached task thread limit
            }

            if Self::spawn_single_task_thread(task, multi_progress, task_handles) {
                current_task_count += 1;
                spawned_count += 1;
            }
        }

        spawned_count
    }

    /// Spawn a single task thread with proper error handling and cleanup
    /// Level 6: Individual Thread Management - handles single task execution
    fn spawn_single_task_thread(
        task: Arc<DownloadTask>,
        multi_progress: &MultiProgress,
        task_handles: &Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
    ) -> bool {
        // Mark task as downloading
        match task.status.lock() {
            Ok(mut status) => *status = DownloadStatus::Downloading,
            Err(e) => {
                log::error!("Failed to lock task status mutex: {}", e);
                return false;
            }
        };

        let multi_progress = multi_progress.clone();
        let task_clone = Arc::clone(&task);

        // Spawn task thread
        let handle = thread::spawn(move || {
            if let Err(e) = download_task(&task_clone, &multi_progress) {
                log_error_with_backtrace(&task_clone.url, &e);
                if let Ok(mut status) = task_clone.status.lock() {
                    *status = DownloadStatus::Failed(format!("{}", e));
                }
            } else if let Ok(mut status) = task_clone.status.lock() {
                *status = DownloadStatus::Completed;
            }

            // CRITICAL: Take data_channel to close it and unblock receivers
            // This prevents recv() from blocking forever after download completion
            let _data_channels = task_clone.take_data_channels();
        });

        // Store the handle
        if let Ok(mut handles_guard) = task_handles.lock() {
            handles_guard.push(handle);
            true
        } else {
            false
        }
    }

    /// Clean up finished thread handles
    fn cleanup_finished_handles(handles: &Arc<Mutex<Vec<thread::JoinHandle<()>>>>) {
        if let Ok(mut handles_guard) = handles.lock() {
            let mut unfinished_handles = Vec::new();

            // Drain all handles and separate finished from unfinished
            for handle in handles_guard.drain(..) {
                if handle.is_finished() {
                    // Handle is finished, join it to clean up
                    if let Err(e) = handle.join() {
                        log::warn!("Thread join failed: {:?}", e);
                    }
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
        nr_parallel: usize,
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
            if pending_chunks.is_empty() {
                Self::run_global_ondemand_scheduler();
            }
            return;
        }

        log::debug!(
            "pending_chunks={} max_threads={} active_chunks={} to_spawn={}",
            pending_chunks.len(), max_chunk_threads, current_chunk_count, threads_to_spawn
        );

        let active_chunk_counts_by_file = Self::collect_active_chunk_counts_by_file(tasks);
        Self::spawn_chunk_threads(
            &pending_chunks,
            threads_to_spawn,
            chunk_handles,
            active_chunk_counts_by_file,
        );
    }

    /// Calculate the maximum number of chunk threads based on parallel limit and available mirrors
    fn calculate_max_chunk_threads(nr_parallel: usize) -> usize {
        if nr_parallel == 1 {
            return 1;
        }

        // Get available mirrors count
        let available_mirrors_count = {
            if let Ok(mirrors) = mirror::MIRRORS.lock() {
                mirrors.available_mirrors.len()
            } else {
                1
            }
        };

        std::cmp::max(
            nr_parallel,
            std::cmp::min(nr_parallel * MAX_CHUNK_THREADS_MULTIPLIER, available_mirrors_count)
        ) * CHUNK_PARALLEL_MULTIPLIER
    }

    /// Helper function to iterate through all task levels using 3-level architecture
    ///
    /// Calls the provided closure for each task found across all 3 levels:
    /// - Level 1: Master tasks
    /// - Level 2: L2 tasks (beforehand or ondemand tasks from master_task.chunk_tasks)
    /// - Level 3: L3 tasks (ondemand tasks from l2_task.chunk_tasks)
    fn iterate_3level_tasks<F>(
        tasks: &Arc<Mutex<HashMap<String, Arc<DownloadTask>>>>,
        mut callback: F
    )
    where F: FnMut(&Arc<DownloadTask>, usize) // (task, level)
    {
        let tasks_guard = match tasks.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };

        // Level 1: Master tasks
        for (_url, master_task) in tasks_guard.iter() {
            if !master_task.is_master_task() {
                log::warn!("Level-1 task is not master_task: {:?}", master_task);
            }

            callback(master_task, 1);

            // Level 2: L2 tasks (chunk tasks from master)
            let chunks = match master_task.chunk_tasks.lock() {
                Ok(chunks) => chunks,
                Err(_) => continue,
            };

            for l2_task in chunks.iter() {
                // l2_task is beforehand task or ondemand task
                callback(l2_task, 2);

                // Level 3: L3 tasks (chunk tasks from l2_task)
                let l3_chunks = match l2_task.chunk_tasks.lock() {
                    Ok(chunks) => chunks,
                    Err(_) => continue,
                };

                for l3_task in l3_chunks.iter() {
                    // l3_task is ondemand task
                    callback(l3_task, 3);
                }
            }
        }
    }

    /// Collect all pending chunk tasks from all download tasks using the 3-level architecture
    fn collect_pending_chunks(
        tasks: &Arc<Mutex<HashMap<String, Arc<DownloadTask>>>>
    ) -> Vec<(f64, Arc<DownloadTask>)> {
        let mut collected = Vec::new();

        Self::iterate_3level_tasks(tasks, |task, _level| {
            // Skip master tasks (.part) – we only want real chunk tasks (".part-O{offset}")
            if !task.is_chunk_task() {
                return;
            }

            // Collect tasks that are still pending
            if matches!(task.get_status(), DownloadStatus::Pending) {
                let chunk_offset = task.chunk_offset.load(Ordering::Relaxed);
                let file_size = task.file_size.load(Ordering::Relaxed);
                let priority = if file_size > 0 {
                    chunk_offset as f64 / file_size as f64
                } else {
                    0.0
                };
                collected.push((priority, Arc::clone(task)));
            }
        });

        // Sort chunks by priority (chunk_offset / file_size - lower offset = higher priority)
        collected.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        if !collected.is_empty() {
            log::debug!(
                "3-level chunking: pending_chunks={}",
                collected.len()
            );
        }

        collected
    }

    /// Global ondemand scheduler - collects stats and selects task with slowest single ETA for chunking
    fn run_global_ondemand_scheduler() {
        let mut slowest_task_for_ondemand_chunking: Option<Arc<DownloadTask>> = None;
        let mut slowest_eta_for_ondemand: u64 = 0;

        // Create new stats instance locally
        let mut new_stats = DownloadManagerStats::default();
        new_stats.fastest_task_eta = u64::MAX; // Initialize to max value
        let mut debug_stats = Vec::new();

        // Single pass: collect stats and find ondemand candidates
        DownloadManager::iterate_3level_tasks(&DOWNLOAD_MANAGER.tasks, |task, level| {
            // Collect stats for this task
            collect_task_eta_stats(task, level, &mut new_stats, &mut debug_stats);

            // Check for ondemand chunking candidates (only downloading tasks)
            if !matches!(task.get_status(), DownloadStatus::Downloading) {
                return;
            }

            if !may_ondemand_chunking(task) {
                return;
            }

            let single_eta = task.eta.load(Ordering::Relaxed);
            if slowest_eta_for_ondemand < single_eta {
                slowest_eta_for_ondemand = single_eta;
                slowest_task_for_ondemand_chunking = Some(Arc::clone(task));
            }
        });

        // Update and log stats
        let global_ideal_eta = update_global_stats(new_stats.clone(), &debug_stats);
        dump_global_stats_ratelimit(&new_stats, global_ideal_eta, &debug_stats);

        // Set slowest ETA task for ondemand chunking (if ETA > global ETA)
        if let Some(ref task) = slowest_task_for_ondemand_chunking {
            if slowest_eta_for_ondemand >= global_ideal_eta   // >= handles single-large-file case
               && slowest_eta_for_ondemand > MIN_ETA_THRESHOLD_SECONDS
            {
                if let Err(e) = task.set_chunk_status(ChunkStatus::NeedOndemandChunk) {
                    log::warn!("Failed to set NeedOndemandChunk status for slowest ETA task: {}", e);
                } else {
                    log::info!(
                        "Global scheduler selected slowest ETA task {} (ETA:{:.1}s > global:{:.1}s) for ondemand chunking",
                        task.url, slowest_eta_for_ondemand as f64, global_ideal_eta as f64
                    );
                    log::debug!(
                        "Global ondemand scheduler: selected slowest_eta={:.1}s, global_ideal_eta={:.1}s",
                        slowest_eta_for_ondemand as f64, global_ideal_eta as f64
                    );
                }
            }
        }
    }

    /// Count active chunk downloads by final output file path.
    fn collect_active_chunk_counts_by_file(
        tasks: &Arc<Mutex<HashMap<String, Arc<DownloadTask>>>>
    ) -> HashMap<PathBuf, usize> {
        let mut active_counts = HashMap::new();

        Self::iterate_3level_tasks(tasks, |task, _level| {
            if !task.is_chunk_task() {
                return;
            }
            if !matches!(task.get_status(), DownloadStatus::Downloading) {
                return;
            }
            *active_counts.entry(task.final_path.clone()).or_insert(0) += 1;
        });

        active_counts
    }

    /// Spawn chunk download threads for the given pending chunks
    fn spawn_chunk_threads(
        pending_chunks: &[(f64, Arc<DownloadTask>)],
        threads_to_spawn: usize,
        chunk_handles: &Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
        mut active_chunk_counts_by_file: HashMap<PathBuf, usize>,
    ) {
        let mut spawned_count = 0;

        for (_, chunk_task) in pending_chunks.iter() {
            if spawned_count >= threads_to_spawn {
                break;
            }

            let chunk_clone = Arc::clone(chunk_task);
            let _chunk_handles_clone = Arc::clone(chunk_handles);

            let active_for_file = *active_chunk_counts_by_file
                .get(&chunk_clone.final_path)
                .unwrap_or(&0);
            if active_for_file >= MAX_ACTIVE_CHUNKS_PER_FILE {
                continue;
            }

            // Double-check that the task is still pending before spawning
            if !matches!(chunk_clone.get_status(), DownloadStatus::Pending) {
                log::debug!("Skipping chunk task {} that is no longer pending (status: {:?})",
                           chunk_clone.chunk_path.display(), chunk_clone.get_status());
                continue;
            }

            // Mark chunk as downloading NOW to avoid duplicate scheduling in the next scheduler tick
            if let Err(e) = update_download_status(&chunk_clone, DownloadStatus::Downloading) {
                log::error!("Failed to set chunk status to Downloading for {}: {}", chunk_clone.chunk_path.display(), e);
                continue; // Skip spawning thread if we cannot update status
            }

            log::debug!("Spawning chunk thread for {} (offset: {}, size: {})",
                       chunk_clone.chunk_path.display(),
                       chunk_clone.chunk_offset.load(Ordering::Relaxed),
                       chunk_clone.chunk_size.load(Ordering::Relaxed));

            let final_path_for_count = chunk_clone.final_path.clone();
            let handle = thread::spawn(move || {

                match download_chunk_task(&chunk_clone) {
                    Ok(()) => {
                        log::debug!(
                            "Chunk for {} at offset {} completed successfully (path: {})",
                            chunk_clone.get_resolved_url(), chunk_clone.chunk_offset.load(Ordering::Relaxed), chunk_clone.chunk_path.display()
                        );

                        // Mark chunk as completed
                        if let Ok(mut status) = chunk_clone.status.lock() {
                            *status = DownloadStatus::Completed;
                        }
                    },
                    Err(e) => {
                        log::debug!(
                            "Chunk task failed for {} at offset {} (path: {}): {:#}",
                            chunk_clone.get_resolved_url(), chunk_clone.chunk_offset.load(Ordering::Relaxed), chunk_clone.chunk_path.display(), e
                        );

                        // Mark chunk as failed
                        if let Ok(mut status) = chunk_clone.status.lock() {
                            *status = DownloadStatus::Failed(format!("{}", e));
                        }
                    }
                }
            });

            *active_chunk_counts_by_file
                .entry(final_path_for_count)
                .or_insert(0) += 1;
            spawned_count += 1;

            // Store the chunk handle
            if let Ok(mut handles_guard) = chunk_handles.lock() {
                handles_guard.push(handle);
            }
        }
    }

    /// Dump comprehensive information about all download tasks in a tree-like structure.
    ///
    /// Each master (L1) task is treated as the root of a tree (one file per tree).
    /// Chunk tasks (L2 and L3) are printed as the children of their parent task.
    /// One task per line with useful fields for quick debugging.
    pub fn dump_all_tasks(&self) {
        self.dump_download_manager_stats();
        self.dump_task_tree();
    }

    /// Cancel all pending downloads and stop processing
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.is_processing.store(false, Ordering::Relaxed);
    }

    /// Dump DownloadManagerStats information
    fn dump_download_manager_stats(&self) {
        println!("=== DownloadManagerStats ===");
        if let Ok(stats) = self.stats.lock() {
            println!("Global ideal ETA: {}s", stats.global_ideal_eta);
            println!("Slowest task ETA: {}s", stats.slowest_task_eta);
            println!("Fastest task ETA: {}s", stats.fastest_task_eta);
            println!("Total remaining bytes: {} ({:.1}MB)",
                stats.total_remaining_bytes,
                stats.total_remaining_bytes as f64 / (1024.0 * 1024.0));
            println!("Total rate: {} B/s ({:.1}KB/s)",
                stats.total_rate_bps,
                stats.total_rate_bps as f64 / 1024.0);
            println!("Active tasks: {}", stats.active_tasks);
            println!("Pending tasks: {}", stats.pending_tasks);
            println!("Complete tasks: {}", stats.complete_tasks);
            println!("Master tasks: {}", stats.master_tasks);
            println!("L2 chunk tasks: {}", stats.l2_chunk_tasks);
            println!("L3 chunk tasks: {}", stats.l3_chunk_tasks);
        } else {
            println!("Could not access DownloadManagerStats (lock contention)");
        }
        println!("");
    }

    /// Dump task tree structure with formatting
    fn dump_task_tree(&self) {

        // Track whether we have printed a master already (for spacing)
        let mut first_master = true;

        Self::iterate_3level_tasks(&self.tasks, |task, level| {
            if level == 1 {
                if !first_master {
                    println!(""); // Blank line between different trees
                }
                first_master = false;
            }

            // Build indentation prefix based on level (simple tree)
            let indent = match level {
                1 => "".to_string(),
                2 => "  ├─ ".to_string(),
                3 => "      └─ ".to_string(),
                _ => format!("{}- ", " ".repeat(level * 2)),
            };

            println!("{}{}", indent, format_task_line(task, level));
        });
    }
}

// Helper to format a single task line for output
fn format_task_line(task: &Arc<DownloadTask>, level: usize) -> String {
    use std::sync::atomic::Ordering;

    let status = task.get_status();
    let offset = task.chunk_offset.load(Ordering::Relaxed);
    let size   = task.chunk_size.load(Ordering::Relaxed);
    let recv   = task.received_bytes.load(Ordering::Relaxed);
    let eta    = task.eta.load(Ordering::Relaxed);
    let chunk_status = task.get_chunk_status();
    let resumed_bytes = task.resumed_bytes.load(Ordering::Relaxed);
    let start_time = task.start_time.lock().ok().and_then(|st| st.map(|t| t.elapsed().as_secs())).map(|secs| format!("{}s ago", secs)).unwrap_or_else(|| "None".to_string());
    let resolved_url = task.get_resolved_url();
    let url = if !resolved_url.is_empty() { &resolved_url } else { &task.url };

    let name = if level == 1 {
        url.clone()
    } else {
        task.chunk_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| task.chunk_path.to_string_lossy().to_string())
    };

    // Only show chunk status if not NoChunk
    let chunk_str = match chunk_status {
        ChunkStatus::NoChunk => None,
        _ => Some(format!("{:?}", chunk_status)),
    };

    let mut fields = vec![
        format!(
            "{}  {:?}  {:?}  offset={}  size={}  resumed={} recv={}  rate={}KB/s  eta={}s",
            name, task.mutability, status,
            offset, size, resumed_bytes, recv,
            task.throughput_bps.load(Ordering::Relaxed) >> 10, eta
        ),
        format!("start_time={}", start_time),
        format!("progress={}", task.progress()),
    ];
    if size > 0 {
        fields.push(format!("remaining={}", task.remaining()));
    }
    if let Some(chunk) = chunk_str {
        fields.push(chunk);
    }
    if level == 1 {
        fields.push(format!("file_size={}", task.file_size.load(Ordering::Relaxed)));
        fields.push(format!("file_path={}", task.final_path.display()));
    }
    if level >= 2 {
        if !resolved_url.is_empty() {
            let site = mirror::url2site(&resolved_url);
            fields.push(format!("site={}", site));
        }
    }
    fields.join("  ")
}

pub static DOWNLOAD_MANAGER: LazyLock<DownloadManager> = LazyLock::new(|| {
    DownloadManager::new(config().common.nr_parallel_download)
        .expect("Failed to initialize download manager")
});

pub fn submit_download_task(task: DownloadTask) -> Result<()> {
    DOWNLOAD_MANAGER.submit_task(task)
}

/// Check if a download task exists for the given URL
pub fn has_download_task(url: &str) -> bool {
    DOWNLOAD_MANAGER.has_task(url)
}

/// Wait for any of the specified download tasks to complete
pub fn wait_for_any_download_task(task_urls: &[String]) -> Result<Option<String>> {
    DOWNLOAD_MANAGER.wait_for_any_task(task_urls)
}

/// Cancel all pending downloads
pub fn cancel_downloads() {
    DOWNLOAD_MANAGER.cancel();
}

