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
use ureq::Agent;
use ureq::http;
use crate::dirs;
use crate::models::*;
use crate::mirror::{append_download_log, append_http_log, HttpEvent, MirrorUsageGuard, MIRRORS, Mirrors};
use time::{OffsetDateTime, format_description::well_known::Rfc2822};
use filetime::set_file_mtime;

#[derive(Debug)]
pub struct DownloadTask {
    pub url:                  String,
    pub resolved_url:         Mutex<String>,
    #[allow(dead_code)]
    pub output_dir:           PathBuf,
    pub max_retries:          usize,
    pub client:               Arc<Mutex<Option<Agent>>>,                  // HTTP client created on-demand
    pub data_channel:         Arc<Mutex<Option<Sender<Vec<u8>>>>>,        // will never change, but need take()
                                                                          // to avoid blocking the consumer side
    pub status:               Arc<Mutex<DownloadStatus>>,
    pub final_path:           PathBuf,                                    // Store the final download path
    pub file_size:            AtomicU64,                                  // Expected file size for prioritization and verification (0 = unknown)
    pub attempt_number:       AtomicUsize,                                // Track which attempt number this is (0 = first attempt)
    pub is_immutable_file:    bool,                                       // True for files whose content won't change over time (filename == some id)

    // Chunking semantics rules:
    // 1. chunk_offset - decided on initial allocation, won't change over time; 0 for master task
    // 2. chunk_size - decided on initial allocation, won't change over time but may be lowered on ondemand chunking; equals file_size for master task without chunking
    // 3. append_offset (used in functions) = chunk_offset + resumed_bytes, advances during network downloading
    // 4. On success: resumed_bytes + received_bytes == chunk_size
    // 5. When create_ondemand_chunks() creates new chunks, master task.chunk_size will be reduced to a lower boundary,
    //    and process_chunk_download_stream() will use the latest chunk_size as boundary

    pub chunk_tasks:          Arc<Mutex<Vec<Arc<DownloadTask>>>>,
    pub chunk_path:           PathBuf,                                    // Full path to the chunk file (for master: .part, for chunks: .part-O{offset})
    pub chunk_offset:         AtomicU64,                                  // Starting byte offset for this chunk (0 for master task, fixed on allocation)
    pub chunk_size:           AtomicU64,                                  // Size of this chunk in bytes (fixed on allocation, equals file_size for master without chunking)
    pub start_time:           Mutex<Option<std::time::Instant>>,
    pub received_bytes:       AtomicU64,                                  // Bytes actually received from network
    pub resumed_bytes:        AtomicU64,                                  // Bytes reused from local partial files

    // Ensure we only stream the pre-existing local file once per overall download attempt
    pub has_sent_existing:    AtomicBool,

    // Progress bar for this download task
    pub progress_bar:         Mutex<Option<ProgressBar>>,                 // will never change

    // Chunk status for reliable state management - avoids race conditions in chunking decisions
    pub chunk_status:         Arc<Mutex<ChunkStatus>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DownloadStatus {
    Pending,
    Downloading,
    Completed,
    Failed(String),
}

/// Chunk status enumeration for reliable chunking state management
///
/// This enum tracks the chunking lifecycle to avoid race conditions:
/// - NoChunk: Task has no chunks and is not being considered for chunking
/// - NeedOndemandChunk: Task has been selected by the global scheduler for ondemand chunking
/// - HasOndemandChunk: Task has ondemand chunks created (chunk_tasks not empty, created during download)
/// - HasBeforehandChunk: Task has beforehand chunks created (chunk_tasks not empty, created before download)
///
/// The latter two values imply that task.chunk_tasks is not empty
#[derive(Debug, Clone, PartialEq)]
pub enum ChunkStatus {
    NoChunk,
    NeedOndemandChunk,
    HasOndemandChunk,
    HasBeforehandChunk,
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
        let is_immutable_file = is_immutable_file(&file_path);

        Self {
            url:               url.clone(),
            resolved_url:      Mutex::new(url),             // Initialize resolved_url with the original url
            output_dir,
            max_retries,
            client:            Arc::new(Mutex::new(None)),  // Initialize with no client
            data_channel:      Arc::new(Mutex::new(None)),
            status:            Arc::new(Mutex::new(DownloadStatus::Pending)),
            final_path,
            file_size:         AtomicU64::new(file_size.unwrap_or(0)),
            attempt_number:    AtomicUsize::new(0),         // Initialize to 0 (first attempt)
            is_immutable_file,
            chunk_tasks:       Arc::new(Mutex::new(Vec::new())),
            chunk_path,
            chunk_offset:      AtomicU64::new(0),
            chunk_size:        AtomicU64::new(file_size.unwrap_or(0)),
            start_time:        Mutex::new(None),
            received_bytes:    AtomicU64::new(0),
            resumed_bytes:     AtomicU64::new(0),
            has_sent_existing: AtomicBool::new(false),
            progress_bar:      Mutex::new(None),
            chunk_status:      Arc::new(Mutex::new(ChunkStatus::NoChunk)),
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

    /// Get the current chunk status
    pub fn get_chunk_status(&self) -> ChunkStatus {
        self.chunk_status.lock()
            .unwrap_or_else(|e| panic!("Failed to lock chunk status mutex: {}", e))
            .clone()
    }

    /// Set the chunk status
    pub fn set_chunk_status(&self, status: ChunkStatus) -> Result<()> {
        let mut chunk_status = self.chunk_status.lock()
            .map_err(|e| eyre!("Failed to lock chunk status mutex: {}", e))?;
        *chunk_status = status;
        Ok(())
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
                // Use more conservative network timeouts to avoid premature failures on slow mirrors
                .timeout_connect(Some(Duration::from_secs(15)))  // was 5s
                .timeout_recv_response(Some(Duration::from_secs(60)));  // was 9s

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
            url:                  self.url.clone(),
            resolved_url:         Mutex::new(self.get_resolved_url()),
            output_dir:           self.output_dir.clone(),
            max_retries:          self.max_retries,
            client:               Arc::new(Mutex::new(None)),           // Initialize with no client
            data_channel:         Arc::new(Mutex::new(None)),           // Chunks don't need data channels
            status:               Arc::new(Mutex::new(DownloadStatus::Pending)),
            final_path:           self.final_path.clone(),
            file_size:            AtomicU64::new(self.file_size.load(Ordering::Relaxed)),
            attempt_number:       AtomicUsize::new(0),                  // Initialize to 0 (first attempt)
            is_immutable_file:    self.is_immutable_file,               // Copy immutable file flag

            chunk_tasks:          Arc::new(Mutex::new(Vec::new())),
            chunk_path:           PathBuf::from(chunk_path),
            chunk_offset:         AtomicU64::new(offset),
            chunk_size:           AtomicU64::new(size),
            start_time:           Mutex::new(None),
            received_bytes:       AtomicU64::new(0),
            resumed_bytes:        AtomicU64::new(0),
            has_sent_existing:    AtomicBool::new(false),
            progress_bar:         Mutex::new(None),
            chunk_status:         Arc::new(Mutex::new(ChunkStatus::NoChunk)),
        })
    }

    #[allow(dead_code)]
    pub fn final_append_offset(&self) -> u64 {
        let offset = self.chunk_offset.load(Ordering::Relaxed);
        offset + self.progress()
    }

    #[allow(dead_code)]
    pub fn download_start_offset(&self) -> u64 {
        let offset = self.chunk_offset.load(Ordering::Relaxed);
        let reused = self.resumed_bytes.load(Ordering::Relaxed);
        offset + reused
    }

    pub fn progress(&self) -> u64 {
        let received = self.received_bytes.load(Ordering::Relaxed);
        let reused = self.resumed_bytes.load(Ordering::Relaxed);
        received + reused
    }

    pub fn remaining(&self) -> u64 {
        let chunk_size = self.chunk_size.load(Ordering::Relaxed);
        if chunk_size == 0 {
            log::warn!("chunk_size=0 for task {:?}", self);
        }

        chunk_size.saturating_sub(self.progress())
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

    /// Clean helper to get data channel without repeated error handling
    pub fn get_data_channel(&self) -> Option<Sender<Vec<u8>>> {
        self.data_channel.lock().ok().and_then(|dc| dc.clone())
    }

    /// Clean helper to take data channel (for closing)
    pub fn take_data_channel(&self) -> Option<Sender<Vec<u8>>> {
        self.data_channel.lock().ok().and_then(|mut dc| dc.take())
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
            tasks:              Arc::new(Mutex::new(HashMap::new())),
            nr_parallel,
            task_handles:       Arc::new(Mutex::new(Vec::new())),
            chunk_handles:      Arc::new(Mutex::new(Vec::new())),
            is_processing:      Arc::new(AtomicBool::new(false)),
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

        thread::spawn(move || {
            Self::run_main_processing_loop(
                tasks,
                multi_progress,
                is_processing,
                task_handles,
                chunk_handles,
                nr_parallel,
                current_task_count_arc,
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
    ) {
        loop {
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
                    thread::sleep(Duration::from_millis(100));
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

        log::debug!("Spawned {} new download threads", spawned_count);
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
                log::error!("Download task failed for {}: {}", task_clone.url, e);
            }

            // CRITICAL: Take data_channel to close it and unblock receivers
            // This prevents recv() from blocking forever after download completion
            let _data_channel = task_clone.take_data_channel();
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
            if pending_chunks.is_empty() {
                Self::run_global_ondemand_scheduler(tasks);
            }
            return;
        }

        log::debug!(
            "pending_chunks={} max_threads={} active_chunks={} to_spawn={}",
            pending_chunks.len(), max_chunk_threads, current_chunk_count, threads_to_spawn
        );

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
            // The callback now handles status checking itself
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

    /// Global ondemand scheduler - selects task with slowest single ETA (if > global ETA)
    fn run_global_ondemand_scheduler(tasks: &Arc<Mutex<HashMap<String, Arc<DownloadTask>>>>) {
        let mut slowest_task_for_ondemand_chunking: Option<Arc<DownloadTask>> = None;
        let mut slowest_eta = Duration::from_secs(0);

        // Calculate global ETA once for comparison
        let global_eta = calculate_global_eta(tasks);

        // Use the 3-level iterator to find candidates
        DownloadManager::iterate_3level_tasks(tasks, |task, _level| {
            // Only consider downloading tasks
            if !matches!(task.get_status(), DownloadStatus::Downloading) {
                return;
            }

            // Check if this task is eligible for ondemand chunking
            if !may_ondemand_chunking(task) {
                return;
            }

            let single_eta = calculate_single_task_eta(task);

            if slowest_eta < single_eta {
                slowest_eta = single_eta;
                slowest_task_for_ondemand_chunking = Some(Arc::clone(task));
            }
        });

        // Set slowest ETA task for ondemand chunking (if ETA > global ETA)
        if let Some(ref task) = slowest_task_for_ondemand_chunking {
            if slowest_eta > global_eta && slowest_eta.as_secs() > 30 {
                if let Err(e) = task.set_chunk_status(ChunkStatus::NeedOndemandChunk) {
                    log::warn!("Failed to set NeedOndemandChunk status for slowest ETA task: {}", e);
                } else {
                    log::info!(
                        "Global scheduler selected slowest ETA task {} (ETA:{:.1}s > global:{:.1}s) for ondemand chunking",
                        task.url, slowest_eta.as_secs_f64(), global_eta.as_secs_f64()
                    );
                    log::debug!(
                        "Global ondemand scheduler: selected slowest_eta={:.1}s, global_eta={:.1}s",
                        slowest_eta.as_secs_f64(), global_eta.as_secs_f64()
                    );
                }
            }
        }
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

                match chunk_result {
                    DownloadResult::Success { bytes_transferred } => {
                        log::debug!(
                            "Chunk for {} at offset {} completed successfully: {} bytes",
                            chunk_clone.url, chunk_clone.chunk_offset.load(Ordering::Relaxed), bytes_transferred
                        );

                        // Mark chunk as completed
                        if let Ok(mut status) = chunk_clone.status.lock() {
                            *status = DownloadStatus::Completed;
                        }
                    },
                    DownloadResult::Skipped { reason } => {
                        log::debug!(
                            "Chunk for {} at offset {} was skipped: {}",
                            chunk_clone.url, chunk_clone.chunk_offset.load(Ordering::Relaxed), reason
                        );

                        // Mark chunk as completed
                        if let Ok(mut status) = chunk_clone.status.lock() {
                            *status = DownloadStatus::Completed;
                        }
                    },
                    DownloadResult::Failed { error } => {
                        log::debug!(
                            "Chunk task failed for {} at offset {}: {}",
                            chunk_clone.url, chunk_clone.chunk_offset.load(Ordering::Relaxed), error
                        );

                        // Mark chunk as failed
                        if let Ok(mut status) = chunk_clone.status.lock() {
                            *status = DownloadStatus::Failed(format!("{}", error));
                        }
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

/// Download operation result with clear semantics instead of mixed error types
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum DownloadResult {
    /// Download completed successfully (either from network or local file)
    Success { bytes_transferred: u64 },
    /// Download was skipped because file already exists or is up-to-date
    Skipped { reason: String },
    /// Download failed with a specific error
    Failed { error: DownloadError },
}

/// Specific error types for download operations
#[derive(Debug, Clone)]
pub enum DownloadError {
    /// Fatal HTTP errors (4xx) that shouldn't be retried
    Fatal { code: u16, message: String },
    /// Network connectivity or timeout issues
    Network { details: String },
    /// File system errors (permissions, disk space, etc.)
    FileSystem { operation: String, path: String, details: String },
    /// Content validation failed (size mismatch, corrupted data, etc.)
    ContentValidation { expected: String, actual: String },
    /// Mirror selection or resolution failed
    MirrorResolution { details: String },
    /// Server returned unexpected response
    UnexpectedResponse { code: u16, details: String },
    /// Chunk was already complete and was skipped
    AlreadyComplete { bytes: u64 },
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::Fatal { code, message } => write!(f, "Fatal error (HTTP {}): {}", code, message),
            DownloadError::Network { details } => write!(f, "Network error: {}", details),
            DownloadError::FileSystem { operation, path, details } => write!(f, "File system error during {} at {}: {}", operation, path, details),
            DownloadError::ContentValidation { expected, actual } => write!(f, "Content validation failed: expected {}, got {}", expected, actual),
            DownloadError::MirrorResolution { details } => write!(f, "Mirror resolution failed: {}", details),
            DownloadError::UnexpectedResponse { code, details } => {
                write!(f, "Unexpected HTTP response {}: {}", code, details)
            },
            DownloadError::AlreadyComplete { bytes } => {
                write!(f, "Chunk already complete with {} bytes", bytes)
            },
        }
    }
}

impl std::error::Error for DownloadError {}

/// Context information for a download operation
#[derive(Debug)]
pub struct DownloadContext {
    pub resolved_url: String,
    pub existing_bytes: u64,
}

/// Result type for processing operations to indicate whether to continue or complete
#[derive(Debug, PartialEq)]
enum ProcessingResult {
    Continue,
    AllCompleted,
}

/// Clear response action instead of mysterious boolean returns
#[derive(Debug, PartialEq)]
enum ResponseAction {
    ContinueDownload,
    CompleteTask,
}

/// Cache decision logic to replace complex nested conditionals
#[derive(Debug)]
enum CacheDecision {
    UseCache { reason: String },
    RedownloadDueTo { reason: String },
}

#[derive(Debug)]
#[allow(dead_code)]
struct FatalError(String);

impl std::fmt::Display for FatalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for FatalError {}

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

/// Resolve mirror placeholder in URL with smart mirror selection
///
/// Uses pre-filtered mirrors and intelligent retry logic for optimal performance
/// Determine if a file is immutable based on its file path
/// Immutable files are those whose content won't change over time
fn is_immutable_file(file_path: &str) -> bool {
    file_path.ends_with(".deb") ||
    file_path.ends_with(".rpm") ||
    file_path.ends_with(".apk") ||
    file_path.ends_with(".conda") ||
    file_path.contains("/by-hash/") ||
    file_path.ends_with(".gz") ||
    file_path.ends_with(".xz") ||
    file_path.ends_with(".zst")
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
            send_file_to_channel(task)
                .with_context(|| format!("Failed to send existing file to channel: {}", final_path.display()))?;

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
#[allow(dead_code)]
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
    let expected_size = task.file_size.load(Ordering::Relaxed);

    log::debug!("download_task starting for {}, has_channel: {}, expected_size: {:?}", url, task.get_data_channel().is_some(), expected_size);

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

    let mut retries = 0;

    loop {
        log::debug!("download_file_with_retries calling download_file for {}, attempt {}", url, retries + 1);

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
                            log::debug!("download_file_with_retries got fatal error {} for {}: {}", code, resolved_url, message);
                        },
                        DownloadError::Network { details } => {
                            log::debug!("download_file_with_retries got network error for {}: {}", resolved_url, details);
                        },
                        DownloadError::FileSystem { operation, path, details } => {
                            log::debug!("download_file_with_retries got filesystem error during {} on {} for {}: {}", operation, path, resolved_url, details);
                        },
                        DownloadError::ContentValidation { expected, actual } => {
                            log::debug!("download_file_with_retries got content validation error for {}: expected {}, got {}", resolved_url, expected, actual);
                        },
                        DownloadError::MirrorResolution { details } => {
                            log::debug!("download_file_with_retries got mirror resolution error for {}: {}", resolved_url, details);
                        },
                        DownloadError::UnexpectedResponse { code, details } => {
                            log::debug!("download_file_with_retries got unexpected response {} for {}: {}", code, resolved_url, details);
                        },
                        DownloadError::AlreadyComplete { bytes } => {
                            log::debug!("download_file_with_retries got already complete response for {}: {} bytes", resolved_url, bytes);
                            return Ok(());
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
    task: &DownloadTask,
) -> Result<()> {
    // Only master tasks should send data to channel
    if !task.is_master_task() {
        return Err(eyre!("Should not call send_file_to_channel() for chunk task {:?}", task));
    }

    // Get data channel from task
    let data_channel = match task.get_data_channel() {
        Some(channel) => channel,
        None => return Ok(()), // No channel to send to
    };

    // Ensure we only stream the pre-existing file once per download_file_with_retries() lifetime
    if task.has_sent_existing.swap(true, Ordering::SeqCst) {
        log::debug!("Existing file already streamed once – skipping second send for {}", task.chunk_path.display());
    }

    // The channel receivers process_packages_content()/process_filelist_content() expect full file
    // to decompress and compute hash, so send the existing file content first. This fixes bug
    // "Decompression error: stream/file format not recognized"
    send_chunk_to_channel(&task.chunk_path, &data_channel)
}

/// Send a chunk file to the data channel (for streaming fresh chunk data)
/// This bypasses the master task and has_sent_existing guards
fn send_chunk_to_channel(
    part_path: &Path,
    data_channel: &Sender<Vec<u8>>,
) -> Result<()> {
    log::debug!("Sending chunk file to channel: {}", part_path.display());

    let mut file = map_io_error(File::open(part_path), "open file for channel", part_path)?;
    let mut buffer = vec![0; 64 * 1024]; // 64KB buffer
    let mut chunks_sent = 0;

    loop {
        let bytes_read = map_io_error(file.read(&mut buffer), "read file for channel", part_path)?;
        if bytes_read == 0 {
            break; // EOF
        }

        chunks_sent += 1;
        match data_channel.send(buffer[..bytes_read].to_vec()) {
            Ok(_) => {
                log::trace!("Sent chunk {} ({} bytes) from {}", chunks_sent, bytes_read, part_path.display());
            }
            Err(e) => {
                // Treat closed receiver channel as a non-fatal condition for chunks too
                log::warn!("Channel closed while sending chunk {} from {}: {}", chunks_sent, part_path.display(), e);
                break;
            }
        }
    }

    log::debug!("Finished sending {} chunks from {}", chunks_sent, part_path.display());
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
            send_file_to_channel(task).map_err(|e|
                eyre!("Failed to send file '{}' to channel: {}", task.chunk_path.display(), e)
            )?;
        }
    }

    // Use the unified download_chunk_task for both master and chunk tasks
    match download_chunk_task(task) {
        DownloadResult::Success { bytes_transferred } => {
            log::debug!("Task downloaded successfully: {} bytes", bytes_transferred);
        },
        DownloadResult::Skipped { reason } => {
            log::debug!("Task was skipped: {}", reason);
        },
        DownloadResult::Failed { error } => {
            return Err(eyre!("Task failed: {}", error));
        },
    }

    // Master task post-processing
    if task.is_master_task() {
        // Wait for all chunks to complete and merge them
        log::debug!("Master task waiting for chunks to complete");
        wait_for_chunks_and_merge(task)?;

        // Validate download size after all chunks are merged
        let file_size_val = task.file_size.load(Ordering::Relaxed);
        if file_size_val > 0 {
            let final_size = get_existing_file_size(&task.chunk_path)?;
            validate_download_size(final_size, file_size_val, &task.chunk_path)?;
        }

        // Finalize download atomically
        atomic_file_completion(&task.chunk_path, &task.final_path)?;

        // Apply metadata (timestamp and ETag) to the final file
        let etag_path = task.chunk_path.with_extension("etag");
        if let Some(etag) = load_etag(&etag_path) {
            // Set metadata on the final file
            set_file_metadata("", &etag, &task.final_path, task);

            // Move ETag file to final location
            let final_etag_path = task.final_path.with_extension("etag");
            if let Err(e) = std::fs::rename(&etag_path, &final_etag_path) {
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

/// Execute HTTP download request with comprehensive error handling
///
/// This function handles:
/// - ETag conditional requests (304 Not Modified)
/// - Range request errors (416 Range Not Satisfiable)
/// - Network and timeout errors
/// - Request logging and metrics
fn execute_download_request(
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
    if chunk_offset > 0 {
        // The end of the requested range is always the base offset plus the chunk size minus
        // one, regardless of how many bytes we have already.
        let end = chunk_offset + chunk_size - 1;
        log::debug!("Setting Range header: bytes={}-{}", chunk_offset + resumed_bytes, end);
        request = request.header("Range", &format!("bytes={}-{}", chunk_offset + resumed_bytes, end));
    } else if chunk_offset == 0 && resumed_bytes > 0 && chunk_size == file_size {
        // Resume request from offset to end
        log::debug!("Setting Range header: bytes={}-", resumed_bytes);
        request = request.header("Range", &format!("bytes={}-", resumed_bytes));
    } else if chunk_offset == 0 && chunk_size == file_size {
        // is master task w/o chunking
        if part_path.exists() {
            if let Some(stored_etag) = load_etag(part_path) {
                log::debug!("Adding If-None-Match header with ETag '{}' for conditional request", stored_etag);
                request = request.header("If-None-Match", &format!("\"{}\"", stored_etag));
            }
        } else {
            log::debug!("Local file {} doesn't exist, skipping ETag header", part_path.display());
            let etag_path = part_path.with_extension("etag");
            let _ = std::fs::remove_file(&etag_path);
        }
    }

    // Execute the request and handle all possible outcomes
    let request_start = std::time::Instant::now();
    match request.call() {
        Ok(response) => handle_successful_response(response, task, resolved_url, request_start),
        Err(ureq::Error::StatusCode(code)) => handle_http_status_error(code, task, resolved_url, existing_bytes, request_start),
        Err(ureq::Error::Io(e)) => handle_network_io_error(e, task, resolved_url),
        Err(e) => handle_general_request_error(e, task, resolved_url),
    }
}

/// Handle successful HTTP responses (2xx status codes)
/// Level 6: Response Processing - handles successful HTTP responses
fn handle_successful_response(
    response: http::Response<ureq::Body>,
    task: &DownloadTask,
    resolved_url: &str,
    request_start: std::time::Instant,
) -> Result<http::Response<ureq::Body>> {
    let latency = request_start.elapsed().as_millis() as u64;

    // Handle 304 Not Modified responses for ETag conditional requests
    if response.status().as_u16() == 304 {
        return handle_304_not_modified_response(task, resolved_url, latency);
    }

    // Log latency info for successful requests
    log_http_event_safe(resolved_url, HttpEvent::Latency(latency));

    Ok(response)
}

/// Handle HTTP status code errors (4xx, 5xx responses)
/// Level 6: Error Handling - processes HTTP status code errors
fn handle_http_status_error(
    code: u16,
    task: &DownloadTask,
    resolved_url: &str,
    existing_bytes: u64,
    request_start: std::time::Instant,
) -> Result<http::Response<ureq::Body>> {
    let latency = request_start.elapsed().as_millis() as u64;
    log::debug!("HTTP error code {}", code);

    // Log latency even for errors
    log_http_request_metrics(resolved_url, latency);

    // Handle specific HTTP status codes
    if code == 416 && existing_bytes > 0 {
        log::debug!("Handling HTTP 416 with existing_bytes={}", existing_bytes);
        return handle_416_range_error(task);
    }

    // Log the specific HTTP error type
    log_http_status_error(resolved_url, code);

    handle_non_416_http_error(code, resolved_url, task)
}

/// Handle network I/O errors
/// Level 6: Error Handling - processes network I/O errors
fn handle_network_io_error(
    e: std::io::Error,
    task: &DownloadTask,
    resolved_url: &str,
) -> Result<http::Response<ureq::Body>> {
    let error_msg = format!("Network error: {} - {}", e, resolved_url);

    // Log network error
    log_http_event_safe(resolved_url, HttpEvent::NetError(error_msg.clone()));

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
    log_http_event_safe(resolved_url, HttpEvent::NetError(error_msg.clone()));

    task.set_message(error_msg.clone());

    // Classify error type based on error message
    if error_str.contains("timeout") {
        Err(DownloadError::Network { details: error_msg }.into())
    } else {
        Err(DownloadError::Network { details: error_msg }.into())
    }
}

/// Log HTTP request metrics for performance tracking
/// Level 7: Logging - handles HTTP metrics logging
fn log_http_request_metrics(resolved_url: &str, latency: u64) {
    log_http_event_safe(resolved_url, HttpEvent::Latency(latency));
}

/// Log specific HTTP status errors for monitoring
/// Level 7: Logging - handles HTTP status error logging
fn log_http_status_error(resolved_url: &str, code: u16) {
    let http_event = if code == 404 {
        HttpEvent::NoContent
    } else {
        HttpEvent::HttpError(code)
    };

    log_http_event_safe(resolved_url, http_event);
}

/// Handle 416 Range Not Satisfiable error with full access to required context
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
fn handle_416_range_error(
    task: &DownloadTask,
) -> Result<http::Response<ureq::Body>> {
    let url = task.get_resolved_url();

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
    log_http_event_safe(&url, HttpEvent::Latency(metadata_latency));

    let remote_size_opt = parse_content_length(&remote_metadata);
    let remote_size = remote_size_opt.unwrap_or(0);
    log::debug!("download_file remote_size: {} (Content-Length present: {}), local_size: {}",
               remote_size, remote_size_opt.is_some(), task.file_size.load(Ordering::Relaxed));

    let remote_timestamp_opt = parse_remote_timestamp(&remote_metadata);

    let local_metadata = map_io_error(fs::metadata(&task.chunk_path), "get local file metadata", &task.chunk_path)?;
    let local_size = local_metadata.len();
    let local_last_modified_sys_time = local_metadata.modified().map_err(|e| eyre!("Failed to get local file modification time: {}", e))?;
    let local_last_modified: OffsetDateTime = local_last_modified_sys_time.into();

    // Use clean cache decision logic instead of complex nested conditionals
    let decision = should_redownload(remote_timestamp_opt, remote_size, local_size, local_last_modified);

    match decision {
        CacheDecision::UseCache { reason } => {
            log::debug!("Using cached file: {}", reason);
            task.set_message(format!("Remote file unchanged ({}), skipping download {}", reason, task.chunk_path.display()));
            send_file_to_channel(task).map_err(|e| eyre!("Failed to send file to channel: {}", e))?;
            return Err(eyre!("Download skipped - file unchanged"));
        }
        CacheDecision::RedownloadDueTo { reason } => {
            let error_msg = format!("{}, restarting download from 0: {}", reason, url);
            log::debug!("{}", error_msg);
            task.set_message(error_msg.clone());
            // Remove stale partial file and reset resume state so that the next
            // attempt doesn't send an out-of-range Range request.
            safe_remove_file(&task.chunk_path, "part")?;
            task.resumed_bytes.store(0, Ordering::Relaxed);
            return Err(DownloadError::ContentValidation {
                expected: "timestamp match".to_string(),
                actual: error_msg
            }.into());
        }
    }
}

/// Handle non-416 HTTP errors
fn handle_non_416_http_error(code: u16, url: &str, task: &DownloadTask) -> Result<http::Response<ureq::Body>> {
    let error_msg = format!("HTTP {}", code);
    task.set_message(format!("{} - {}", error_msg, url));

    if code >= 400 && code < 500 {
        // For client errors (like 403, 404), create a simple DownloadError without verbose backtrace
        log::debug!("Client error {} for {}", code, url);
        Err(DownloadError::Fatal { code, message: error_msg }.into())
    } else {
        log::debug!("Server error {} for {}", code, url);
        Err(DownloadError::UnexpectedResponse { code, details: format!("HTTP error: {}", error_msg) }.into())
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

/// Unified content download function that handles both master and chunk tasks
///
/// This function replaces both download_content() and download_chunk_content()
/// by using task.is_master_task() to handle master-specific logic
fn download_chunk_content(
    mut response: http::Response<ureq::Body>,
    task: &DownloadTask,
) -> Result<u64> {
    // Initialize download context and validate response
    let download_context = initialize_chunk_download_context(task, &response)?;

    // Setup file for writing
    let (mut file, existing_bytes) = setup_download_file(task)?;

    // Execute the main download stream processing
    let chunk_append_offset = process_chunk_download_stream(
        &mut response,
        &mut file,
        task,
        existing_bytes,
        &download_context,
    )?;

    // Finalize download with progress updates and logging
    finalize_chunk_download(task, chunk_append_offset, existing_bytes)
}

/// Initialize download context and validate response for chunk downloads
/// Level 5: Context Initialization - sets up download state and validates response
fn initialize_chunk_download_context(
    task: &DownloadTask,
    response: &http::Response<ureq::Body>,
) -> Result<ChunkDownloadContext> {
    // Get data channel for master tasks only
    let data_channel = if task.is_master_task() {
        task.get_data_channel()
    } else {
        None // Chunk tasks don't use data channels
    };

    // Validate response for chunk tasks
    if task.is_chunk_task() && response.status().as_u16() != 206 {
        return Err(eyre!("Expected 206 Partial Content for chunk download, got {}", response.status()));
    }

    Ok(ChunkDownloadContext {
        data_channel,
        last_update: std::time::Instant::now(),
        last_ondemand_check: std::time::Instant::now(),
    })
}

/// Process the main download stream with chunked reading and progress tracking
/// Level 5: Stream Processing - handles the core download loop with boundaries
fn process_chunk_download_stream(
    response: &mut http::Response<ureq::Body>,
    file: &mut File,
    task: &DownloadTask,
    existing_bytes: u64,
    download_context: &ChunkDownloadContext,
) -> Result<u64> {
    let mut reader = response.body_mut().as_reader();
    let mut buffer = vec![0; 8192];
    let mut chunk_append_offset = existing_bytes;
    let mut network_bytes = 0u64;
    let mut last_update = download_context.last_update;
    let mut last_ondemand_check = download_context.last_ondemand_check;

    loop {
        // Read data from network stream
        let bytes_read = read_chunk_from_stream(&mut reader, &mut buffer, task, chunk_append_offset)?;

        if bytes_read == 0 {
            break; // EOF reached
        }

        // Process and write the chunk data
        let written_bytes = process_chunk_data_write(
            file,
            &buffer,
            bytes_read,
            task,
            chunk_append_offset,
        )?;

        if written_bytes == 0 {
            break; // Chunk boundary reached
        }

        // Update download counters
        chunk_append_offset += written_bytes as u64;
        network_bytes += written_bytes as u64;

        // Handle data channel and boundary checks
        handle_chunk_data_processing(
            &download_context.data_channel,
            &buffer,
            written_bytes,
            bytes_read,
            task,
            network_bytes,
        );

        if written_bytes < bytes_read {
            break; // Reached chunk boundary for master task
        }

        // Perform periodic updates
        perform_chunk_periodic_updates(task, chunk_append_offset, &mut last_update, &mut last_ondemand_check);
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
        Ok(0) => Ok(0), // EOF reached
        Ok(n) => Ok(n),
        Err(e) => {
            if task.is_master_task() {
                let error_msg = format!("Read error at {} bytes: {}", chunk_append_offset,
                    task.resolved_url.lock().map(|r| r.clone()).unwrap_or_else(|_| task.url.clone()));
                task.set_message(error_msg);
            }
            Err(eyre!("Failed to read from response (chunk_append_offset={}, buffer_size={}): {}", chunk_append_offset, buffer.len(), e))
        }
    }
}

/// Process chunk data and write to file with boundary checking
/// Level 6: Data Processing - handles chunk boundary logic and file writing
fn process_chunk_data_write(
    file: &mut File,
    buffer: &[u8],
    bytes_read: usize,
    task: &DownloadTask,
    chunk_append_offset: u64,
) -> Result<usize> {
    // Calculate bytes to write based on chunk boundaries
    let bytes_to_write = calculate_write_bytes(task, bytes_read, chunk_append_offset);

    // Write data to file with boundary checks
    write_chunk_data(file, buffer, bytes_to_write, task, chunk_append_offset)
}

/// Handle data channel communication and progress tracking
/// Level 6: Progress Management - manages data channels and counters
fn handle_chunk_data_processing(
    data_channel: &Option<Sender<Vec<u8>>>,
    buffer: &[u8],
    written_bytes: usize,
    _bytes_read: usize,
    task: &DownloadTask,
    network_bytes: u64,
) {
    // Send data to channel for master tasks
    if let Some(channel) = data_channel {
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

    // Store only network bytes in received_bytes
    task.received_bytes.store(network_bytes, Ordering::Relaxed);
}

/// Perform periodic progress updates and chunking checks
/// Level 6: Update Management - handles timed progress and chunking updates
fn perform_chunk_periodic_updates(
    task: &DownloadTask,
    chunk_append_offset: u64,
    last_update: &mut std::time::Instant,
    last_ondemand_check: &mut std::time::Instant,
) {
    update_download_progress(task, chunk_append_offset, last_update);
    check_ondemand_chunking(task, chunk_append_offset, last_ondemand_check);
}

/// Finalize chunk download with progress updates and completion logging
/// Level 5: Download Finalization - completes download with final updates
fn finalize_chunk_download(
    task: &DownloadTask,
    chunk_append_offset: u64,
    existing_bytes: u64,
) -> Result<u64> {
    let network_bytes = chunk_append_offset - existing_bytes;

    // Final progress update
    if task.is_master_task() {
        let (total_progress, _downloading_chunks) = task.get_total_progress_bytes();
        task.set_position(total_progress);
    } else {
        task.set_position(chunk_append_offset);
    }

    log::debug!("download_content completed: {} total bytes ({} network bytes) written to {}",
               chunk_append_offset, network_bytes, task.chunk_path.display());

    Ok(chunk_append_offset)
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
               encoding_lower.contains("compress") ||
               encoding_lower.contains("br") ||
               encoding_lower.contains("xz") {
                   log::debug!(
                       "Content is compressed with '{}', Content-Length ({}) refers to compressed size, not final size",
                       encoding,
                       response.headers().get("content-length")
                           .and_then(|v| v.to_str().ok())
                           .unwrap_or("unknown")
                   );
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

    let resumed = task.resumed_bytes.load(Ordering::Relaxed);

    // Don't chunk small files or chunk tasks themselves
    if task.is_chunk_task() || file_size_val < resumed + MIN_FILE_SIZE_FOR_CHUNKING {
        log::debug!("Skipping chunking: is_chunk_task={}, size={} bytes, min_required={} bytes",
                  task.is_chunk_task(), file_size_val, resumed + MIN_FILE_SIZE_FOR_CHUNKING);
        return Ok(Vec::new());
    }

    log::debug!("Using known size {} bytes to create chunks (resumed: {} bytes)", file_size_val, resumed);

    log::debug!("Creating chunks for {} byte file with {} bytes resumed",
              file_size_val, resumed);

    // Calculate the next chunk boundary after the chunk start offset. If we are
    // exactly on a 1 MiB boundary we need to move to the _next_ boundary, otherwise we
    // would produce a zero-length master chunk (next_boundary == resumed).
    let next_boundary = if resumed == 0 {
        MIN_CHUNK_SIZE
    } else if (resumed & MIN_CHUNK_SIZE_MASK) == 0 {
        resumed + MIN_CHUNK_SIZE
    } else {
        // Round up to the next 1 MiB boundary
        (resumed + MIN_CHUNK_SIZE_MASK) & !MIN_CHUNK_SIZE_MASK
    };

    // Master task will handle from current offset to next chunk boundary
    let master_chunk_size = std::cmp::min(next_boundary - resumed, file_size_val - resumed);

    // Update master task's chunk information
    task.chunk_size.store(master_chunk_size, Ordering::Relaxed);

    log::debug!("Master task will handle {} bytes starting from offset {}",
              master_chunk_size, resumed);

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
        log::debug!("No additional chunks needed for {} byte file with {} bytes resumed",
                 file_size_val, resumed);
    } else {
        log::debug!("Created {} chunk tasks for {} byte file", chunk_tasks.len(), file_size_val);

        // Set chunk status to HasBeforehandChunk since these chunks were created before download
        if let Err(e) = task.set_chunk_status(ChunkStatus::HasBeforehandChunk) {
            log::warn!("Failed to set chunk status to HasBeforehandChunk: {}", e);
        } else {
            log::debug!("Set chunk status to HasBeforehandChunk for {} chunks", chunk_tasks.len());
        }
    }

    Ok(chunk_tasks)
}

/// Unified download task function that handles both master and chunk tasks
///
/// This function coordinates the download process by delegating to specialized functions.
/// Level 3: Download Strategy - coordinates download execution
fn download_chunk_task(task: &DownloadTask) -> DownloadResult {
    // Phase 1: Setup and validation (split into concrete steps)
    let (existing_bytes, is_complete) = match check_existing_file_and_completion(task) {
        Ok(res) => res,
        Err(e) => {
            // Check if this is an "already complete" case (success)
            if let Some(download_err) = e.downcast_ref::<DownloadError>() {
                if let DownloadError::AlreadyComplete { bytes } = download_err {
                    return DownloadResult::Success { bytes_transferred: *bytes };
                }
            }
            let download_error = if let Some(download_err) = e.downcast_ref::<DownloadError>() {
                download_err.clone()
            } else {
                DownloadError::FileSystem {
                    operation: "check existing file and completion".to_string(),
                    path: task.chunk_path.display().to_string(),
                    details: format!("{}", e),
                }
            };
            return DownloadResult::Failed { error: download_error };
        }
    };
    if is_complete {
        return DownloadResult::Success { bytes_transferred: existing_bytes };
    }

    let need_range = should_download_range(task, existing_bytes);
    let resolved_url = match resolve_mirror_and_update_task(task, need_range) {
        Ok(url) => url,
        Err(e) => {
            let download_error = if let Some(download_err) = e.downcast_ref::<DownloadError>() {
                download_err.clone()
            } else {
                DownloadError::MirrorResolution {
                    details: format!("{}", e)
                }
            };
            return DownloadResult::Failed { error: download_error };
        }
    };
    if let Err(e) = ensure_chunk_directory_exists(task) {
        let download_error = if let Some(download_err) = e.downcast_ref::<DownloadError>() {
            download_err.clone()
        } else {
            DownloadError::FileSystem {
                operation: "ensure chunk directory exists".to_string(),
                path: task.chunk_path.display().to_string(),
                details: format!("{}", e),
            }
        };
        return DownloadResult::Failed { error: download_error };
    }

    // Track mirror usage with RAII guard
    let _mirror_guard = MirrorUsageGuard::new(&resolved_url);

    // Phase 2: Execute HTTP request
    let response = match execute_download_request(task, &resolved_url, existing_bytes) {
        Ok(resp) => resp,
        Err(e) => {
            let download_error = DownloadError::Network { details: format!("{}", e) };
            return DownloadResult::Failed { error: download_error };
        }
    };

    // Phase 3: Process response and download content
    match process_download_response(task, response, &DownloadContext { resolved_url, existing_bytes }) {
        Ok(bytes) => DownloadResult::Success { bytes_transferred: bytes },
        Err(e) => {
            let download_error = DownloadError::ContentValidation {
                expected: "successful download".to_string(),
                actual: format!("{}", e),
            };
            return DownloadResult::Failed { error: download_error };
        }
    }
}

/// Prepare download context and validate readiness
/// Level 4: Setup Operations - handles download preparation
fn prepare_download_context(task: &DownloadTask) -> Result<DownloadContext> {
    let url = &task.url;
    let chunk_path = &task.chunk_path;

    // Check existing file size for resumption
    let existing_bytes = match get_existing_file_size(chunk_path) {
        Ok(bytes) => bytes,
        Err(e) => return Err(DownloadError::FileSystem {
            operation: "check existing file size".to_string(),
            path: chunk_path.display().to_string(),
            details: format!("{}", e),
        }.into()),
    };

    // Check if chunk task is already complete
    match check_chunk_completion(task, existing_bytes) {
        Ok(true) => {
            log::debug!("Chunk already complete with {} bytes, skipping download", existing_bytes);
            return Err(DownloadError::AlreadyComplete { bytes: existing_bytes }.into());
        },
        Ok(false) => {
            // Continue with download preparation
        },
        Err(e) => return Err(DownloadError::FileSystem {
            operation: "check chunk completion".to_string(),
            path: chunk_path.display().to_string(),
            details: format!("{}", e),
        }.into()),
    }

    setup_resumption_state(task, existing_bytes);

    // Determine if we need Range support based on task characteristics
    let need_range = task.resumed_bytes.load(Ordering::Relaxed) > 0 ||
                     task.chunk_size.load(Ordering::Relaxed) != task.file_size.load(Ordering::Relaxed) ||
                     task.is_chunk_task();

    // Resolve mirror for this attempt with appropriate Range requirements
    let (resolved_url, _final_path) = match resolve_mirror_in_url(url, &task.output_dir, need_range) {
        Ok(result) => result,
        Err(e) => return Err(DownloadError::MirrorResolution {
            details: format!("{}", e)
        }.into()),
    };

    // Update resolved URL in task
    if let Ok(mut resolved) = task.resolved_url.lock() {
        *resolved = resolved_url.clone();
    }

    // Ensure parent directory exists
    if let Some(parent) = chunk_path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            return Err(DownloadError::FileSystem {
                operation: "create directory".to_string(),
                path: parent.display().to_string(),
                details: format!("{}", e),
            }.into());
        }
    }

    Ok(DownloadContext {
        resolved_url,
        existing_bytes,
    })
}

/// Process HTTP response and execute content download
/// Level 4: Response Processing - handles HTTP response validation and content download
fn process_download_response(
    task: &DownloadTask,
    response: http::Response<ureq::Body>,
    context: &DownloadContext
) -> Result<u64> {
    // Handle task-specific response validation and metadata
    if task.is_master_task() {
        let response_action = handle_master_task_response(task, &response, &context.resolved_url, context.existing_bytes)?;
        if response_action == ResponseAction::CompleteTask {
            return Ok(0); // Early exit for completed downloads
        }
        // Extract and store metadata for later use
        handle_response_metadata(&response, task);
    } else {
        handle_chunk_task_response(&response, &context.resolved_url)?;
    }

    // Download the content using unified function
    let download_start = std::time::Instant::now();
    let final_downloaded = download_chunk_content(response, task)?;
    let download_duration = download_start.elapsed().as_millis() as u64;

    // Validate individual chunk size if this is a chunk task or master task with chunk_size set
    let expected_chunk_size = task.chunk_size.load(Ordering::Relaxed);
    if expected_chunk_size > 0 {
        // For chunk tasks and master tasks with chunking, validate against the expected chunk size
        validate_download_size(final_downloaded, expected_chunk_size, &task.chunk_path)?;
    }

    // Log download completion
    log_download_completion(task, &context.resolved_url, 0, download_duration);

    Ok(final_downloaded)
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
            // For chunk streaming, we bypass the guards since this is fresh data being streamed
            send_chunk_to_channel(&chunk_task.chunk_path, channel)?;
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

    master_task.set_message(format_progress_message(&resolved_url, downloading_chunks));

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
    let mut source = map_io_error(File::open(source_path), "open source file", source_path)?;

    // Open target file for appending
    let mut target = map_io_error(
        OpenOptions::new()
            .write(true)
            .append(true)
            .open(target_path),
        "open target file",
        target_path
    )?;

    // Copy data from source to target
    map_io_error(std::io::copy(&mut source, &mut target), "append file", source_path)?;

    // Ensure data is written to disk
    map_io_error(target.sync_all(), "sync target file", target_path)?;

    Ok(())
}

/// Calculate ETA for a single task based on its current progress and download rate
fn calculate_single_task_eta(task: &DownloadTask) -> Duration {
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
        let total_size = task.chunk_size.load(Ordering::Relaxed);
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

/// Helper to accumulate ETA statistics for a single task
/// Returns (total_remaining_bytes, total_rate, debug_stat_option)
fn single_task_eta_stats(
    task: &DownloadTask,
    task_prefix: &str,
) -> (u64, f64, Option<String>) {
    let task_eta = calculate_single_task_eta(task);
    let task_eta_secs = task_eta.as_secs_f64();

    if task_eta_secs > 0.0 {
        let (total_progress, _) = task.get_total_progress_bytes();
        let total_size = if task.is_master_task() {
            task.file_size.load(Ordering::Relaxed)
        } else {
            task.chunk_size.load(Ordering::Relaxed)
        };
        let network_bytes = task.get_network_bytes();

        if total_size > 0 && network_bytes > 0 {
            let rate = network_bytes as f64 / task_eta_secs;
            let remaining = total_size.saturating_sub(total_progress);

            let debug_stat = if task.is_master_task() {
                format!(
                    "{}[{}]: {:.1}MB/{:.1}MB @{:.1}KB/s ETA:{:.1}s",
                    task_prefix,
                    task.url.chars().take(20).collect::<String>(),
                    total_progress as f64 / (1024.0 * 1024.0),
                    total_size as f64 / (1024.0 * 1024.0),
                    rate / 1024.0,
                    task_eta_secs
                )
            } else {
                format!(
                    "{}[O{}]: {:.1}KB/{:.1}KB @{:.1}KB/s ETA:{:.1}s",
                    task_prefix,
                    task.chunk_offset.load(Ordering::Relaxed) / 1024,
                    total_progress as f64 / 1024.0,
                    total_size as f64 / 1024.0,
                    rate / 1024.0,
                    task_eta_secs
                )
            };

            return (remaining, rate, Some(debug_stat));
        }
    }

    (0, 0.0, None)
}



/// Calculate global ETA for all downloading tasks across the 3-level architecture
///
/// This provides an estimate for completing all currently active downloads
/// by aggregating progress across master tasks and their chunk tasks.
/// Reuses calculate_single_task_eta() for consistency and adds debug information.
fn calculate_global_eta(tasks: &Arc<Mutex<HashMap<String, Arc<DownloadTask>>>>) -> Duration {
    let mut total_remaining_bytes = 0u64;
    let mut total_rate = 0.0f64;
    let mut active_downloads = 0usize;

    // Debug stats collection
    let mut debug_stats = Vec::new();
    let mut master_count = 0usize;
    let mut l2_chunk_count = 0usize;
    let mut l3_chunk_count = 0usize;
    let mut slowest_task_eta = Duration::from_secs(0);
    let mut fastest_task_eta = Duration::from_secs(u64::MAX);

    // Single pass: iterate and collect ETA data
    DownloadManager::iterate_3level_tasks(tasks, |task, level| {
        // Only consider downloading tasks
        if !matches!(task.get_status(), DownloadStatus::Downloading) {
            return;
        }

        let task_prefix = match level {
            1 => {
                master_count += 1;
                "M"
            },
            2 => {
                l2_chunk_count += 1;
                "L2"
            },
            3 => {
                l3_chunk_count += 1;
                "L3"
            },
            _ => "U"
        };

        let (remaining, rate, debug_stat) = single_task_eta_stats(task, task_prefix);

        if remaining > 0 && rate > 0.0 {
            total_remaining_bytes += remaining;
            total_rate += rate;
            active_downloads += 1;

            // Update debug stats
            let task_eta = calculate_single_task_eta(task);
            if task_eta > slowest_task_eta {
                slowest_task_eta = task_eta;
            }
            if task_eta < fastest_task_eta {
                fastest_task_eta = task_eta;
            }

            if let Some(stat) = debug_stat {
                debug_stats.push(stat);
            }
        }
    });

    let global_eta = if total_rate > 0.0 && active_downloads > 0 {
        let estimated_seconds = total_remaining_bytes as f64 / total_rate;
        Duration::from_secs_f64(estimated_seconds)
    } else {
        Duration::from_secs(0)
    };

    // Log comprehensive debug information for performance analysis
    if active_downloads > 0 {
        log::debug!(
            "Global ETA calculation: {:.1}s for {:.1}MB remaining across {} active downloads",
            global_eta.as_secs_f64(),
            total_remaining_bytes as f64 / (1024.0 * 1024.0),
            active_downloads
        );
        log::debug!(
            "Download types: {} masters, {} L2 chunks, {} L3 chunks",
            master_count, l2_chunk_count, l3_chunk_count
        );
        log::debug!(
            "ETA range: fastest={:.1}s, slowest={:.1}s, global={:.1}s, aggregate_rate={:.1}KB/s",
            if fastest_task_eta.as_secs() == u64::MAX { 0.0 } else { fastest_task_eta.as_secs_f64() },
            slowest_task_eta.as_secs_f64(),
            global_eta.as_secs_f64(),
            total_rate / 1024.0
        );

        // Log individual task stats (limited to prevent spam)
        for (i, stat) in debug_stats.iter().take(10).enumerate() {
            log::debug!("Task {}: {}", i + 1, stat);
        }
        if debug_stats.len() > 10 {
            log::debug!("... and {} more tasks", debug_stats.len() - 10);
        }
    }

    global_eta
}

/// Check if a task may be suitable for ondemand chunking
///
/// This function encapsulates the logic for determining whether a download task
/// could be a candidate for ondemand chunking. It checks:
///
/// 1. If the chunk status is NoChunk or NeedOndemandChunk
/// 2. If the remaining size is at least 2 * ONDEMAND_CHUNK_SIZE (512KB)
///
/// The actual chunking decision and creation is handled by the global scheduler in
/// collect_pending_chunks() for global optimized decision and executed in individual task threads
/// to avoid race conditions.
fn may_ondemand_chunking(task: &DownloadTask) -> bool {

    let file_size = task.file_size.load(Ordering::Relaxed);
    if file_size == 0 {
        return false; // Unknown file size = no clue at all
    }

    let chunk_status = task.get_chunk_status();
    if !matches!(chunk_status, ChunkStatus::NoChunk | ChunkStatus::NeedOndemandChunk) {
        return false;
    }

    // Check if there's enough remaining data to make chunking worthwhile
    task.remaining() >= 2 * ONDEMAND_CHUNK_SIZE  // 512KB
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
fn create_ondemand_chunks(master_task: &DownloadTask, chunk_append_offset: u64, remaining_size: u64) -> Result<usize> {
    // Calculate the next 256KB boundary after current position.
    // If we are already aligned to a boundary (i.e. chunk_append_offset is an exact multiple
    // of ONDEMAND_CHUNK_SIZE) we must advance by one full chunk; otherwise `next_boundary`
    // would equal `chunk_append_offset`, producing a zero-length master chunk and triggering
    // errors later when `create_chunk_tasks` is called again.

    let chunk_offset = master_task.chunk_offset.load(Ordering::Relaxed);
    let final_append_offset = chunk_append_offset + chunk_offset;
    let next_boundary = if (final_append_offset & ONDEMAND_CHUNK_SIZE_MASK) == 0 {
        final_append_offset + ONDEMAND_CHUNK_SIZE
    } else {
        (final_append_offset + ONDEMAND_CHUNK_SIZE_MASK) & !ONDEMAND_CHUNK_SIZE_MASK
    };

    let total_size = final_append_offset + remaining_size;

    // Modify master task to cover from current position to next 256KB boundary
    let master_chunk_size = std::cmp::min(next_boundary - chunk_offset, remaining_size);

    // Update master task's chunk information
    master_task.chunk_size.store(master_chunk_size, Ordering::Relaxed);

    log::debug!(
        "Modified master task at offset {} to boundary {}",
        chunk_offset, next_boundary
    );

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

        log::info!(
            "Created {} on-demand chunks (256KB each) for {} bytes remaining, master covers {}→{} bytes",
            chunk_tasks.len(), remaining_size, chunk_offset, next_boundary
        );
    } else {
        return Err(eyre!("Failed to lock master task's chunk list"));
    }

    Ok(chunk_tasks.len())
}

// ============================================================================
// PROCESS COORDINATION
// ============================================================================

/// Create a PID file for download coordination and clean up stale PID files
fn create_pid_file(final_path: &Path) -> Result<PathBuf> {
    let pid_file = final_path.with_extension("download.pid");

    // Check for existing downloads and clean up stale PID files
    if pid_file.exists() {
        if is_pid_file_active(&pid_file) {
            return Err(eyre!("Another download process is already active for: {}", final_path.display()));
        } else {
            // Clean up stale PID file
            log::info!("Cleaning up stale PID file: {}", pid_file.display());
            cleanup_pid_file(&pid_file)?;
        }
    }

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

    let etag = parse_etag(response).unwrap_or_default();

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
        if let Err(e) = save_etag(&task.final_path, &etag) {
            log::warn!("Failed to save ETag for {}: {}", task.final_path.display(), e);
        }
    }
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
        task.resumed_bytes.store(existing_bytes, Ordering::Relaxed);
        log::debug!("Resuming download from {} bytes for {}", existing_bytes, &task.url);
    }
}

/// Handle master task specific response validation and setup
fn handle_master_task_response(
    task: &DownloadTask,
    response: &http::Response<ureq::Body>,
    resolved_url: &str,
    existing_bytes: u64
) -> Result<ResponseAction> {
    // Check for unchanged file case
    if let Some(status_line) = response.headers().get("status") {
        if let Ok(status_str) = status_line.to_str() {
            if status_str.contains("304") || status_str.contains("unchanged") {
                // File hasn't changed, just ensure final file exists
                if !task.final_path.exists() && task.chunk_path.exists() {
                    atomic_file_completion(&task.chunk_path, &task.final_path)?;
                }
                return Ok(ResponseAction::CompleteTask);
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
        task.resumed_bytes.store(0, Ordering::Relaxed);
        log::debug!("Server doesn't support resume, restarting download");
        return Err(eyre!("Resume failed, need to restart download"));
    }

    // Setup file size and progress tracking for master tasks
    if task.file_size.load(Ordering::Relaxed) == 0 {
        if let Some(content_length) = parse_content_length(response) {
            let total_size = content_length + existing_bytes;
            task.file_size.store(total_size, Ordering::Relaxed);
            task.chunk_size.store(total_size, Ordering::Relaxed);
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

    Ok(ResponseAction::ContinueDownload)
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

/// Clean cache decision logic replacing complex nested conditionals
fn should_redownload(
    remote_ts: Option<OffsetDateTime>,
    remote_size: u64,
    local_size: u64,
    local_ts: OffsetDateTime
) -> CacheDecision {
    use std::time::Duration;

    match remote_ts {
        Some(ts) if remote_size == local_size && (ts - local_ts).unsigned_abs() <= Duration::from_secs(2) => {
            CacheDecision::UseCache {
                reason: format!("Size and timestamp match (remote: {}, local: {})", ts, local_ts)
            }
        }
        Some(ts) => {
            let mut reasons = Vec::new();
            if remote_size != local_size {
                reasons.push(format!("size mismatch: remote {}, local {}", remote_size, local_size));
            }
            if ts != local_ts {
                reasons.push(format!("timestamp mismatch: remote {}, local {}", ts, local_ts));
            }
            CacheDecision::RedownloadDueTo { reason: reasons.join(" and ") }
        }
        None if remote_size == local_size => {
            CacheDecision::UseCache { reason: "Size matches, no timestamp available".to_string() }
        }
        None => {
            CacheDecision::RedownloadDueTo {
                reason: format!("Size differs (remote {}, local {}) and no timestamp", remote_size, local_size)
            }
        }
    }
}

/// Helper to safely remove files with consistent error handling
fn safe_remove_file(path: &Path, context: &str) -> Result<()> {
    fs::remove_file(path).map_err(|e| eyre!("Failed to remove {} file '{}': {}", context, path.display(), e))
}

/// Helper to safely log HTTP events with error handling
fn log_http_event_safe(url: &str, event: HttpEvent) {
    if let Err(e) = append_http_log(url, event) {
        log::warn!("Failed to log HTTP event for {}: {}", url, e);
    }
}

/// Helper for consistent error mapping patterns
fn map_io_error<T>(result: std::io::Result<T>, context: &str, path: &Path) -> Result<T> {
    result.map_err(|e| eyre!("Failed to {} '{}': {}", context, path.display(), e))
}

/// Helper to format progress messages with chunk counts
fn format_progress_message(resolved_url: &str, downloading_chunks: usize) -> String {
    if downloading_chunks == 0 {
        resolved_url.to_string()
    } else {
        format!("+{} {}", downloading_chunks, resolved_url)
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

    let file = map_io_error(
        OpenOptions::new()
            .create(true)
            .write(true)
            .append(existing_bytes > 0) // Append if file exists with content
            .truncate(existing_bytes == 0) // Only truncate if file is empty or doesn't exist
            .open(chunk_path),
        "open file",
        chunk_path
    )?;

    Ok((file, existing_bytes))
}

/// Calculate bytes to write considering chunk boundaries
fn calculate_write_bytes(
    task: &DownloadTask,
    bytes_read: usize,
    chunk_append_offset: u64
) -> usize {
    let chunk_size_val = task.chunk_size.load(Ordering::Relaxed);

    if chunk_size_val > 0 {
        let boundary = if task.is_master_task() {
            task.chunk_offset.load(Ordering::Relaxed) + chunk_size_val
        } else {
            chunk_size_val // For chunk tasks, chunk_size is the limit
        };

        if chunk_append_offset >= boundary {
            if task.is_master_task() {
                log::debug!("Master task reached chunk boundary at {} bytes, stopping", chunk_append_offset);
            } else {
                log::debug!("Chunk task completed at {} bytes", chunk_append_offset);
            }
            return 0; // Signal to stop
        }

        // Adjust bytes to write if we're approaching the limit
        if chunk_append_offset + bytes_read as u64 > boundary {
            (boundary - chunk_append_offset) as usize
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
    chunk_append_offset: u64
) -> Result<usize> {
    if !task.is_master_task() {
        let chunk_size_val = task.chunk_size.load(Ordering::Relaxed);
        if chunk_size_val > 0 {
            let remaining = chunk_size_val.saturating_sub(chunk_append_offset);
            if remaining == 0 {
                log::warn!("Chunk task received {} surplus bytes, discarding", bytes_to_write);
                return Ok(0); // Signal to stop
            }
            let write_len = std::cmp::min(bytes_to_write, remaining as usize);

            file.write_all(&buffer[..write_len])
                .map_err(|e| eyre!("Failed to write {} bytes to chunk file '{}': {}",
                                  write_len, task.chunk_path.display(), e))?;

            if write_len < bytes_to_write && chunk_append_offset + write_len as u64 > chunk_size_val {
                log::warn!("Chunk {} exceeded expected size by {} bytes; extra data ignored",
                          task.chunk_path.display(), (chunk_append_offset + write_len as u64) - chunk_size_val);
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
    chunk_append_offset: u64,
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
            let message = format_progress_message(&resolved_url, downloading_chunks);
            task.set_message(message);
        } else {
            // For chunk tasks, show chunk append offset (reused + network bytes)
            task.set_position(chunk_append_offset);
        }
        *last_update = now;
    }
}

/// Check for on-demand chunking execution (master tasks and L2 chunks)
///
/// This function checks if the task has been marked for ondemand chunking by the global
/// scheduler and executes the chunking if needed. It now supports both master tasks creating
/// L2 chunks and L2 chunks creating L3 chunks.
fn check_ondemand_chunking(
    task: &DownloadTask,
    chunk_append_offset: u64,
    _last_ondemand_check: &mut std::time::Instant
) {
    // Early return if not flagged for ondemand chunking
    if task.get_chunk_status() != ChunkStatus::NeedOndemandChunk {
        return;
    }

    let file_size_val = task.file_size.load(Ordering::Relaxed);
    // Early return if file size is invalid
    if file_size_val <= 0 {
        return;
    }

    let remaining_size = task.remaining();

    // Create on-demand chunks for remaining data
    match create_ondemand_chunks(task, chunk_append_offset, remaining_size) {
        Ok(chunk_count) => {
            let level = if task.is_master_task() { "L2" } else { "L3" };
            log::info!("Created {} on-demand {} chunks for {} bytes remaining", chunk_count, level, remaining_size);
            // Mark as having ondemand chunks
            if let Err(e) = task.set_chunk_status(ChunkStatus::HasOndemandChunk) {
                log::warn!("Failed to set chunk status to HasOndemandChunk: {}", e);
            }
        }
        Err(_) => {
            log::warn!("Failed to create ondemand chunks, resetting status to NoChunk");
            if let Err(e) = task.set_chunk_status(ChunkStatus::NoChunk) {
                log::warn!("Failed to reset chunk status: {}", e);
            }
        }
    }
}

/// Handle 304 Not Modified response
fn handle_304_not_modified_response(
    task: &DownloadTask,
    resolved_url: &str,
    latency: u64,
) -> Result<http::Response<ureq::Body>> {
    log::debug!("Received 304 Not Modified - file unchanged on server");
    task.set_message(format!("File unchanged (ETag match), skipping download - {}", task.chunk_path.display()));

    // If the final file exists, send it to the channel if needed
    send_file_to_channel(task)
        .map_err(|e| eyre!("Failed to send cached file to channel: {}", e))?;

    // Log this as a successful conditional request
    if let Err(e) = append_http_log(resolved_url, HttpEvent::Latency(latency)) {
        log::warn!("Failed to log 304 latency: {}", e);
    }

    Err(eyre!("Download skipped - file unchanged (ETag match)"))
}

/// Context for chunk download operations
struct ChunkDownloadContext {
    data_channel: Option<Sender<Vec<u8>>>,
    last_update: std::time::Instant,
    last_ondemand_check: std::time::Instant,
}

/// Check existing file size and validate chunk completion
/// Returns existing bytes and whether the chunk is already complete
fn check_existing_file_and_completion(task: &DownloadTask) -> Result<(u64, bool)> {
    let chunk_path = &task.chunk_path;

    // Check existing file size for resumption
    let existing_bytes = match get_existing_file_size(chunk_path) {
        Ok(bytes) => bytes,
        Err(e) => return Err(DownloadError::FileSystem {
            operation: "check existing file size".to_string(),
            path: chunk_path.display().to_string(),
            details: format!("{}", e),
        }.into()),
    };
    setup_resumption_state(task, existing_bytes);

    // Check if chunk task is already complete
    match check_chunk_completion(task, existing_bytes) {
        Ok(true) => {
            log::debug!("Chunk already complete with {} bytes, skipping download", existing_bytes);
            Ok((existing_bytes, true))
        },
        Ok(false) => Ok((existing_bytes, false)),
        Err(e) => Err(DownloadError::FileSystem {
            operation: "check chunk completion".to_string(),
            path: chunk_path.display().to_string(),
            details: format!("{}", e),
        }.into()),
    }
}

// Determine if we need Range support based on task characteristics
fn should_download_range(task: &DownloadTask, existing_bytes: u64) -> bool {
    task.resumed_bytes.load(Ordering::Relaxed) > 0 ||
    task.chunk_size.load(Ordering::Relaxed) != task.file_size.load(Ordering::Relaxed) ||
    task.is_chunk_task()
}

/// Resolve mirror URL and update task with resolved URL
fn resolve_mirror_and_update_task(task: &DownloadTask, need_range: bool) -> Result<String> {
    let url = &task.url;

    // Resolve mirror for this attempt with appropriate Range requirements
    let (resolved_url, _final_path) = match resolve_mirror_in_url(url, &task.output_dir, need_range) {
        Ok(result) => result,
        Err(e) => return Err(DownloadError::MirrorResolution {
            details: format!("{}", e)
        }.into()),
    };

    // Update resolved URL in task
    if let Ok(mut resolved) = task.resolved_url.lock() {
        *resolved = resolved_url.clone();
    }

    Ok(resolved_url)
}

/// Ensure parent directory exists for the chunk file
fn ensure_chunk_directory_exists(task: &DownloadTask) -> Result<()> {
    let chunk_path = &task.chunk_path;

    if let Some(parent) = chunk_path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            return Err(DownloadError::FileSystem {
                operation: "create directory".to_string(),
                path: parent.display().to_string(),
                details: format!("{}", e),
            }.into());
        }
    }
    Ok(())
}
