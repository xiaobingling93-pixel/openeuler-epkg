use std::collections::HashMap;
use std::fs::{self, OpenOptions};
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
use time::{OffsetDateTime, format_description::well_known::Rfc2822};
use filetime::set_file_mtime;

#[derive(Debug, Clone)]
pub struct DownloadTask {
    pub url: String,
    pub output_dir: PathBuf,
    pub max_retries: usize,
    pub data_channel: Option<Sender<Vec<u8>>>,
    pub status: Arc<std::sync::Mutex<DownloadStatus>>,
    pub final_path: PathBuf, // Store the final download path
    pub size: Option<u32>, // Expected file size for prioritization and verification
}

#[derive(Debug, Clone)]
pub enum DownloadStatus {
    Pending,
    Downloading,
    Completed,
    Failed(String),
}

impl DownloadTask {
    pub fn new(url: String, output_dir: PathBuf, max_retries: usize) -> Self {
        Self::with_size(url, output_dir, max_retries, None)
    }

    pub fn with_size(url: String, output_dir: PathBuf, max_retries: usize, size: Option<u32>) -> Self {
        // Calculate final_path during task creation
        // - For normal URLs: output_dir/last_url_segment
        // - For URLs with triple slashes: output_dir/everything_after_triple_slash
        //   Example: "https://example.com///foo/bar.txt" -> output_dir/foo/bar.txt
        let final_path = if let Some((_, str_b)) = url.split_once("///") {
            output_dir.join(str_b)
        } else {
            let file_name = url.split('/').last()
                .unwrap_or("unknown_file");
            output_dir.join(file_name)
        };

        Self {
            url,
            output_dir,
            max_retries,
            data_channel: None,
            status: Arc::new(std::sync::Mutex::new(DownloadStatus::Pending)),
            final_path,
            size,
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
}

pub struct DownloadManager {
    client: Agent,
    multi_progress: MultiProgress,
    tasks: Arc<std::sync::Mutex<HashMap<String, DownloadTask>>>,
    pool: rayon::ThreadPool,
    is_processing: Arc<std::sync::atomic::AtomicBool>,
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
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(nr_parallel)
            .build()
            .with_context(|| "Failed to create thread pool")?;

        Ok(Self {
            client,
            multi_progress,
            tasks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            pool,
            is_processing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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

        self.pool.spawn(move || {
            loop {
                let mut tasks_guard = match tasks.lock() {
                    Ok(guard) => guard,
                    Err(e) => {
                        log::error!("Failed to lock tasks mutex: {}", e);
                        is_processing.store(false, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                };
                let mut pending_tasks: Vec<_> = tasks_guard.iter_mut()
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

                // Sort pending tasks by size (largest first) to prioritize large downloads
                // Tasks without size information are treated as having size 0 and go last
                pending_tasks.sort_by(|(_, a), (_, b)| {
                    let size_a = a.size.unwrap_or(0);
                    let size_b = b.size.unwrap_or(0);
                    size_b.cmp(&size_a) // Descending order (largest first)
                });

                for (_task_url, task0) in pending_tasks {
                    let client = client.clone();
                    let multi_progress = multi_progress.clone();
                    let task = task0.clone();

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
                    task0.data_channel.take();  // unblock recv()

                    // Create a channel to signal when download starts
                    let (start_tx, start_rx) = std::sync::mpsc::channel();

                    rayon::spawn(move || {
                        match task.status.lock() {
                            Ok(mut status) => *status = DownloadStatus::Downloading,
                            Err(e) => {
                                log::error!("Failed to lock task status mutex: {}", e);
                                return;
                            }
                        };

                        // Signal that download is starting
                        if let Err(e) = start_tx.send(()) {
                            log::error!("Failed to send download start signal: {}", e);
                            // The download will proceed, but synchronization might be affected.
                            // Consider if more robust error handling is needed here.
                        }

                        if let Err(e) = download_task(
                            &client,
                            &task,
                            &multi_progress,
                        ) {
                            // Status is already updated in the download_task function
                            log::error!("Download task failed for {}: {}", task.url, e);
                        }
                        // Status is already updated in the download_task function for success case too
                    });

                    // Wait for download to start before continuing
                    if let Err(e) = start_rx.recv() {
                        log::error!("Failed to receive download start signal: {}. The download task might have failed to start properly.", e);
                        // Consider if the loop should continue or if this is a critical failure.
                    }
                }
            }
        });
    }

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
//   - Uses a thread pool (Rayon) for resource management
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
//   - Relies on a thread pool for parallelism
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

pub static DOWNLOAD_MANAGER: LazyLock<DownloadManager> = LazyLock::new(|| {
    let config = config();
    DownloadManager::new(config.common.nr_parallel, &config.common.proxy)
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
                let actual_size = metadata.len() as u32;
                if actual_size == task.size.unwrap() {
                    log::info!("File {} already exists with correct size {}, treating as already downloaded",
                              final_path.display(), actual_size);

                    // Mark task as completed
                    let mut status = task.status.lock()
                        .map_err(|e| eyre!("Failed to lock download status mutex: {}", e))?;
                    *status = DownloadStatus::Completed;
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
fn verify_file_size(part_path: &Path, expected_size: Option<u32>, url: &str) -> Result<()> {
    if let Some(expected) = expected_size {
        if let Ok(metadata) = fs::metadata(part_path) {
            let actual_size = metadata.len() as u32;
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

/// Finalize download by renaming part file and marking task as completed
fn finalize_download(part_path: &Path, final_path: &Path, task: &DownloadTask) -> Result<()> {
    fs::rename(part_path, final_path)
        .with_context(|| format!("Failed to rename file: {} to {}", part_path.display(), final_path.display()))?;

    // Mark task as completed
    let mut status = task.status.lock()
        .map_err(|e| eyre!("Failed to lock download status mutex: {}", e))?;
    *status = DownloadStatus::Completed;

    Ok(())
}

/// Handle download failure by cleaning up and marking task as failed
fn handle_download_failure(part_path: &Path, task: &DownloadTask, error: color_eyre::eyre::Error) -> Result<()> {
    if part_path.exists() {
        fs::remove_file(part_path)?;
    }

    // Mark task as failed
    let mut status = task.status.lock()
        .map_err(|e| eyre!("Failed to lock download status mutex: {}", e))?;
    *status = DownloadStatus::Failed(format!("{}", error));

    Err(error)
}

/// Downloads a file from a URL to the output directory.
/// Uses the final_path that was calculated when the task was created.
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

    let part_path = final_path.with_extension("part");

    // Check if we can skip download for existing package files
    if let Some(()) = check_existing_package_file(task)? {
        return Ok(());
    }

    // Prepare download environment
    prepare_download_environment(final_path, &part_path)?;

    // Setup progress bar
    let pb = setup_progress_bar(multi_progress, url)?;

    // Start the download
    log::debug!("download_task calling download_file_with_retries for {}", url);
    let result = download_file_with_retries(
        client,
        url,
        &part_path,
        &pb,
        max_retries,
        data_channel.clone(),
    );
    log::debug!("download_task download_file_with_retries completed for {}, result: {:?}", url, result);

    // Update progress bar based on result
    if result.is_ok() {
        pb.finish_with_message(format!("Downloaded {}", final_path.to_string_lossy()));
    } else {
        pb.finish_with_message(format!("Error: {:?}", result));
    }

    // Handle download result
    match result {
        Ok(()) => {
            // Verify file size
            verify_file_size(&part_path, expected_size, url)?;

            // Finalize download
            finalize_download(&part_path, final_path, task)?;
            Ok(())
        },
        Err(e) => handle_download_failure(&part_path, task, e),
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
            let actual_size = metadata.len() as u32;
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
    let mut status = task.status.lock()
        .map_err(|e| eyre!("Failed to lock download status mutex: {}", e))?;
    *status = DownloadStatus::Completed;

    Ok(())
}

fn download_file_with_retries(
    client: &Agent,
    url: &str,
    part_path: &Path,
    pb: &ProgressBar,
    max_retries: usize,
    data_channel: Option<Sender<Vec<u8>>>,
) -> Result<()> {
    log::debug!("download_file_with_retries starting for {}, has_channel: {}", url, data_channel.is_some());
    let mut retries = 0;

    loop {
        log::debug!("download_file_with_retries calling download_file for {}, attempt {}", url, retries + 1);
        log::debug!("About to call download_file with data_channel.is_some() = {}", data_channel.is_some());
        match download_file(client, url, part_path, pb, retries, &data_channel) {
            Ok(()) => {
                log::debug!("download_file_with_retries completed successfully for {}, dropping channel", url);
                return Ok(());
            },
            Err(e) => {
                log::debug!("download_file_with_retries got error for {}: {:?}", url, e);

                // Check if this is a fatal error (like 404) that shouldn't be retried
                if e.downcast_ref::<FatalError>().is_some() {
                    log::info!("Skipping retries for fatal error (client error 4xx) for {}", url);
                    return Err(e);
                }

                if retries >= max_retries {
                    return Err(eyre!("Max retries ({}) exceeded for {}: {}", max_retries, url, e));
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

fn download_file(
    client: &Agent,
    url: &str,
    part_path: &Path,
    pb: &ProgressBar,
    retries: usize,
    data_channel: &Option<Sender<Vec<u8>>>,
) -> Result<()> {
    log::debug!("download_file starting for {}, part_path: {}", url, part_path.display());

    let downloaded = get_existing_file_size(part_path)?;
    let mut response = match make_download_request_with_416_handling(client, url, downloaded, pb, part_path, data_channel) {
        Ok(response) => response,
        Err(e) => {
            // Check if this is the special case where download was skipped due to file being unchanged
            if e.to_string().contains("Download skipped - file unchanged") {
                return Ok(());
            }
            return Err(e);
        }
    };
    let status = response.status();

    validate_response_content_type(&response, url, pb)?;
    let downloaded = handle_resume_logic(part_path, pb, url, downloaded, status.as_u16())?;

    let total_size = setup_progress_tracking(&response, pb, downloaded);

    // Send existing file content to channel if resuming
    if downloaded > 0 && retries == 0 {
        if let Some(channel) = &data_channel {
            send_file_to_channel(part_path, &channel).map_err(|e| eyre!("Failed to send file '{}' to channel: {}", part_path.display(), e))?;
        }
    }

    let final_downloaded = download_content(&mut response, part_path, pb, downloaded, data_channel)?;

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
    downloaded: u64,
    pb: &ProgressBar,
    part_path: &Path,
    data_channel: &Option<Sender<Vec<u8>>>,
) -> Result<http::Response<ureq::Body>> {
    let mut request = client.get(url.replace("///", "/"));

    if downloaded > 0 {
        log::debug!("download_file setting Range header: bytes={}-", downloaded);
        request = request.header("Range", &format!("bytes={}-", downloaded));
    }

    match request.call() {
        Ok(response) => Ok(response),
        Err(ureq::Error::StatusCode(code)) => {
            log::debug!("download_file got HTTP error code: {}", code);
            if code == 416 && downloaded > 0 {
                // The requested byte range is outside the size of the file
                log::debug!("download_file handling HTTP 416 with downloaded={}", downloaded);
                return handle_416_range_error(client, url, downloaded, pb, part_path, data_channel);
            }
            handle_non_416_http_error(code, url, pb)
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
    downloaded: u64,
    pb: &ProgressBar,
    part_path: &Path,
    data_channel: &Option<Sender<Vec<u8>>>,
) -> Result<http::Response<ureq::Body>> {
    // Send a request to check remote size and time, then compare with local
    let remote_metadata = client.get(url.replace("///", "/")).call()
        .with_context(|| format!("Failed to make HTTP request for {}", url))?;

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
    downloaded: u64,
    status: u16,
) -> Result<u64> {
    if downloaded > 0 && status != 206 {
        fs::remove_file(part_path).map_err(|e| eyre!("Failed to remove part file '{}': {}", part_path.display(), e))?;
        pb.println(format!("Server doesn't support resume, restarting: {}", url));
        Ok(0)
    } else {
        Ok(downloaded)
    }
}

/// Set up progress tracking with total size and current position
fn setup_progress_tracking(response: &http::Response<ureq::Body>, pb: &ProgressBar, downloaded: u64) -> u64 {
    let total_size = parse_content_length(response) + downloaded;
    pb.set_length(total_size);
    pb.set_position(downloaded);
    total_size
}

/// Download the actual content from the response to the file
fn download_content(
    response: &mut http::Response<ureq::Body>,
    part_path: &Path,
    pb: &ProgressBar,
    mut downloaded: u64,
    data_channel: &Option<Sender<Vec<u8>>>,
) -> Result<u64> {
    // Open the file in append mode to resume partial downloads
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(true)
        .open(part_path).map_err(|e| eyre!("Failed to open file '{}' for writing (downloaded={}): {}", part_path.display(), downloaded, e))?;

    let mut reader = response.body_mut().as_reader();
    let mut buffer = vec![0; 8192];
    let mut last_update = std::time::Instant::now();

    loop {
        let bytes_read = reader.read(&mut buffer).map_err(|e| eyre!("Failed to read from response (downloaded={}, buffer_size={}): {}", downloaded, buffer.len(), e))?;
        if bytes_read == 0 {
            break;
        }
        file.write_all(&buffer[..bytes_read]).map_err(|e| eyre!("Failed to write {} bytes to file '{}' (downloaded={}): {}", bytes_read, part_path.display(), downloaded, e))?;
        downloaded += bytes_read as u64;

        // Update progress bar more frequently
        let now = std::time::Instant::now();
        if now.duration_since(last_update) > Duration::from_millis(300) {
            pb.set_position(downloaded);
            last_update = now;
        }

        if let Some(channel) = &data_channel {
            if let Err(_) = channel.send(buffer[..bytes_read].to_vec()) {
                // Channel was closed, but we continue downloading
            }
        }
    }

    // Final progress update
    pb.set_position(downloaded);
    Ok(downloaded)
}

/// Validate that the downloaded size matches the expected Content-Length
fn validate_download_size(downloaded: u64, total_size: u64, part_path: &Path) -> Result<()> {
    if total_size > 0 && downloaded != total_size {
        let error_msg = format!("Downloaded size ({}) does not match Content-Length ({})", downloaded, total_size);
        return Err(eyre!("Download size mismatch: Downloaded size ({}) does not match Content-Length ({}) for {}", downloaded, total_size, part_path.display()));
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
                Some(package.size)
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
            let cache_path = crate::repo::url_to_cache_path(&url)
                .map_err(|e| eyre!("Failed to convert URL to cache path: {}: {}", url, e))?;
            Ok(cache_path.to_string_lossy().to_string())
        }
    }

    // Download packages specified by their pkgkey strings.
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
            let cache_path = crate::repo::url_to_cache_path(&url)
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
    pub fn wait_for_downloads(&self) -> Result<()> {
        DOWNLOAD_MANAGER.wait_for_all_tasks()
            .map_err(|e| eyre!("Failed to wait for download tasks to complete: {}", e))?;
        Ok(())
    }
}
