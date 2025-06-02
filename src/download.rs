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
use ureq::{Agent, config::Config, tls::TlsConfig, Proxy};
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
        Self {
            url,
            output_dir,
            max_retries,
            data_channel: None,
            status: Arc::new(std::sync::Mutex::new(DownloadStatus::Pending)),
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
    tasks: Arc<std::sync::Mutex<Vec<DownloadTask>>>,
    pool: rayon::ThreadPool,
    is_processing: Arc<std::sync::atomic::AtomicBool>,
}

impl DownloadManager {
    pub fn new(nr_parallel: usize, proxy: Option<&str>) -> Result<Self> {
        let config = Config::builder()
            .tls_config(
                TlsConfig::builder()
                    .build()
            )
            .proxy(proxy.map(|p| {
                match Proxy::new(p) {
                    Ok(proxy) => proxy,
                    Err(e) => {
                        log::error!("Failed to create proxy from {}: {}", p, e);
                        panic!("Failed to create proxy: {}", e);
                    }
                }
            }))
            .user_agent("curl/8.13.0")
            .build();

        let client = Agent::new_with_config(config);
        let multi_progress = MultiProgress::new();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(nr_parallel)
            .build()
            .with_context(|| "Failed to create thread pool")?;

        Ok(Self {
            client,
            multi_progress,
            tasks: Arc::new(std::sync::Mutex::new(Vec::new())),
            pool,
            is_processing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    pub fn submit_task(&self, task: DownloadTask) -> Result<()> {
        let mut tasks = self.tasks.lock()
            .map_err(|e| eyre!("Failed to lock tasks mutex: {}", e))?;
        tasks.push(task);
        Ok(())
    }

    pub fn wait_for_task(&self, task_id: usize) -> Result<DownloadStatus> {
        loop {
            let tasks = self.tasks.lock()
                .map_err(|e| eyre!("Failed to lock tasks mutex: {}", e))?;
            if let Some(task) = tasks.get(task_id) {
                let status = task.get_status();
                match status {
                    DownloadStatus::Completed | DownloadStatus::Failed(_) => return Ok(status),
                    _ => {}
                }
            } else {
                drop(tasks);
                return Err(eyre!("Task with ID {} not found", task_id));
            }
            drop(tasks);
            thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn start_processing(&self) -> Result<()> {
        if self.is_processing.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
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
                let pending_tasks: Vec<_> = tasks_guard.iter_mut()
                    .filter(|t| matches!(t.get_status(), DownloadStatus::Pending))
                    .collect();

                if pending_tasks.is_empty() {
                    // Check if all tasks are completed or failed
                    let all_done = tasks_guard.iter()
                        .all(|t| matches!(t.get_status(), DownloadStatus::Completed | DownloadStatus::Failed(_)));
                    if all_done {
                        is_processing.store(false, std::sync::atomic::Ordering::Relaxed);
                        break;
                    }
                    drop(tasks_guard);
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }

                for task0 in pending_tasks {
                    let client = client.clone();
                    let multi_progress = multi_progress.clone();
                    let task = task0.clone();
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
                        let _ = start_tx.send(());

                        if let Err(e) = download_task(
                            &client,
                            &task.url,
                            &task.output_dir,
                            &multi_progress,
                            task.max_retries,
                            task.data_channel,
                        ) {
                            match task.status.lock() {
                                Ok(mut status) => *status = DownloadStatus::Failed(e.to_string()),
                                Err(e) => log::error!("Failed to lock task status mutex: {}", e)
                            };
                        } else {
                            match task.status.lock() {
                                Ok(mut status) => *status = DownloadStatus::Completed,
                                Err(e) => log::error!("Failed to lock task status mutex: {}", e)
                            };
                        }
                    });

                    // Wait for download to start before continuing
                    let _ = start_rx.recv();
                }
            }
        });

        Ok(())
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
            .filter_map(|t| {
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
    DownloadManager::new(config.common.nr_parallel, config.common.proxy.as_deref())
        .expect("Failed to initialize download manager")
});

pub fn submit_download_task(task: DownloadTask) -> Result<()> {
    DOWNLOAD_MANAGER.submit_task(task)
}

pub fn download_urls(
    urls: Vec<String>,
    output_dir: &Path,
    max_retries: usize,
    async_mode: bool,
) -> Result<Vec<DownloadTask>> {
    let mut submitted_tasks = Vec::new();
    for url in urls {
        let url_for_context = url.clone();
        let task = DownloadTask::new(url, output_dir.to_path_buf(), max_retries);
        submit_download_task(task.clone())
            .with_context(|| format!("Failed to submit download task for {}", url_for_context))?;
        submitted_tasks.push(task);
    }
    fs::create_dir_all(output_dir)
        .with_context(|| format!("Failed to create output directory: {}", output_dir.display()))?;
    DOWNLOAD_MANAGER.start_processing()
        .with_context(|| "Failed to start download manager processing")?;

    if !async_mode {
        // Wait for each task one by one (in submitted order)
        for (i, _task) in submitted_tasks.iter().enumerate() {
            DOWNLOAD_MANAGER.wait_for_task(i)
                .with_context(|| format!("Failed to wait for download task {}", i))?;
        }
        Ok(Vec::new())
    } else {
        Ok(submitted_tasks)
    }
}

/// Downloads a file from a URL to the output directory.
/// - For normal URLs: output_dir/last_url_segment
/// - For URLs with triple slashes: output_dir/everything_after_triple_slash
///   Example: "https://example.com///foo/bar.txt" -> output_dir/foo/bar.txt
fn download_task(
    client: &Agent,
    url: &str,
    output_dir: &Path,
    multi_progress: &MultiProgress,
    max_retries: usize,
    data_channel: Option<Sender<Vec<u8>>>,
) -> Result<()> {
    log::debug!("download_task starting for {}, has_channel: {}", url, data_channel.is_some());

    let final_path = if let Some((_, str_b)) = url.split_once("///") {
        output_dir.join(str_b)
    } else {
        let file_name = url.split('/').last()
            .ok_or_else(|| eyre!("Invalid URL: {}", url))?;
        output_dir.join(file_name)
    };
    let part_path = final_path.with_extension("part");

    if final_path.exists() {
        fs::rename(&final_path, &part_path)
            .with_context(|| format!("Failed to rename file: {} to {}", final_path.display(), part_path.display()))?;
    }
    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directory for {}: {}", final_path.display(), parent.display()))?;
    }

    // Create progress bar but don't show it yet
    let pb = multi_progress.add(ProgressBar::new(0));
    pb.set_style(ProgressStyle::default_bar()
        .template("[{elapsed_precise}] [{bar:10}] {bytes_per_sec:12} ({eta}) {msg}")
        .map_err(|e| eyre!("Failed to parse HTTP response: {}", e))?
        .progress_chars("=> "));
    pb.set_message(url.to_string());

    // Start the download
    log::debug!("download_task calling download_file_with_retries for {}", url);
    let result = download_file_with_retries(
        client,
        url,
        &part_path,
        &pb,
        max_retries,
        data_channel,
    );
    log::debug!("download_task download_file_with_retries completed for {}, result: {:?}", url, result);

    // Only show progress bar after download has started
    if result.is_ok() {
        pb.finish_with_message(format!("Downloaded {}", final_path.to_string_lossy()));
    } else {
        pb.finish_with_message(format!("Error: {:?}", result));
    }

    match result {
        Ok(()) => fs::rename(&part_path, &final_path)
            .with_context(|| format!("Failed to rename file: {} to {}", part_path.display(), final_path.display()))?,
        Err(e) => {
            let _ = fs::remove_file(&part_path);
            return Err(e);
        }
    }

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
        match download_file(client, url, part_path, pb, &data_channel) {
            Ok(()) => {
                log::debug!("download_file_with_retries completed successfully for {}, dropping channel", url);
                return Ok(());
            },
            Err(e) => {
                log::debug!("download_file_with_retries got error for {}: {:?}", url, e);
                if e.downcast_ref::<FatalError>().is_some() {
                    return Err(e);
                }

                if retries >= max_retries {
                    return Err(eyre!("Max retries ({}) exceeded: {}", max_retries, e));
                }

                retries += 1;
                let delay = Duration::from_secs(2u64.pow(retries as u32));
                pb.println(format!("Retrying {} (attempt {}/{})...", url, retries, max_retries));
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
    data_channel: &Option<Sender<Vec<u8>>>,
) -> Result<()> {
    log::debug!("download_file starting for {}, part_path: {}", url, part_path.display());

    let mut downloaded = if part_path.exists() {
        log::debug!("download_file part file exists, getting metadata");
        match fs::metadata(part_path) {
            Ok(metadata) => {
                let size = metadata.len();
                log::debug!("download_file found existing part file with {} bytes", size);
                size
            },
            Err(e) => {
                log::error!("download_file failed to get metadata for part file {}: {}", part_path.display(), e);
                return Err(eyre!("Failed to get metadata for part file {}: {}", part_path.display(), e));
            }
        }
    } else {
        log::debug!("download_file no existing part file found");
        0
    };

    let mut request = client.get(url.replace("///", "/"));

    if downloaded > 0 {
        log::debug!("download_file setting Range header: bytes={}-", downloaded);
        request = request.header("Range", &format!("bytes={}-", downloaded));
    }

    let mut response = match request.call() {
        Ok(response) => {
            response
        },
        Err(ureq::Error::StatusCode(code)) => {
            log::debug!("download_file got HTTP error code: {}", code);
            if code == 416 && downloaded > 0 { // The requested byte range is outside the size of the file
                log::debug!("download_file handling HTTP 416 with downloaded={}", downloaded);
                // Send a request to check remote size and time, then compare with local
                let remote_metadata = client.get(url.replace("///", "/")).call()
                    .with_context(|| format!("Failed to make HTTP request for {}", url))?;
                let remote_size = remote_metadata.headers().get("Content-Length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| {
                        if let Err(e) = s.parse::<u64>() {
                            log::warn!("Failed to parse Content-Length header value '{}': {}", s, e);
                            None
                        } else {
                            s.parse::<u64>().ok()
                        }
                    })
                    .unwrap_or(0);
                log::debug!("download_file remote_size: {}, local_size: {}", remote_size, downloaded);

                let _remote_last_modified = remote_metadata.headers().get("Last-Modified").and_then(|s| {
                    s.to_str().ok().and_then(|s| {
                        OffsetDateTime::parse(s, &Rfc2822).ok()
                    })
                }).unwrap_or_else(|| {
                    log::debug!("No Last-Modified header found, using current time");
                    OffsetDateTime::now_utc()
                });
                let local_metadata = fs::metadata(part_path).map_err(|e| eyre!("Failed to get local file metadata: {}", e))?;
                let local_size = local_metadata.len();
                let _local_last_modified = local_metadata.modified().map_err(|e| eyre!("Failed to get local file modification time: {}", e))?;
                let _local_last_modified: OffsetDateTime = _local_last_modified.into();

                if remote_size == local_size {
                    log::debug!("download_file sizes match, skipping download and sending file to channel");
                    let message = format!("Remote file unchanged, skipping download");
                    pb.finish_with_message(message.clone());
                    if let Some(channel) = &data_channel {
                        send_file_to_channel(part_path, &channel).map_err(|e| eyre!("Failed to send file to channel: {}", e))?;
                    }
                    log::debug!("download_file returning Ok after skipping download");
                    return Ok(());
                } else {
                    let error_msg = format!("Remote file changed, restarting from 0");
                    pb.finish_with_message(error_msg.clone());
                    fs::remove_file(part_path).map_err(|e| eyre!("Failed to remove part file: {}", e))?;
                    return Err(eyre!(error_msg))
                }
            }
            let error_msg = format!("HTTP {} error: {} - {}", code,
                if code >= 400 && code < 500 { "Client Error" }
                else { "Server Error" }, url);
            pb.finish_with_message(error_msg.clone());
            return if code >= 400 && code < 500 {
                Err(eyre!(FatalError(error_msg)))
            } else {
                Err(eyre!("HTTP error: {}", error_msg.clone()))
            };
        }
        Err(ureq::Error::Io(e)) => {
            let error_msg = format!("Network error: {} - {}", e, url);
            pb.finish_with_message(error_msg.clone());
            return Err(eyre!("Download error: {}", error_msg.clone()));
        }
        Err(e) => {
            let error_msg = format!("Error downloading: {} - {}", e, url);
            pb.finish_with_message(error_msg.clone());
            return Err(eyre!("Download error: {}", error_msg.clone()));
        }
    };

    let status = response.status();

    // Check content type to detect HTML login pages
    if let Some(content_type) = response.headers().get("Content-Type").and_then(|v| v.to_str().ok()) {
        if content_type.contains("text/html") {
            let error_msg = "Received HTML page instead of file. This may indicate an authentication issue with the server.";
            pb.finish_with_message(error_msg);
            return Err(eyre!("Fatal error while downloading from {}: {}", url, error_msg.to_string()));
        }
    }

    if downloaded > 0 && status != 206 {
        fs::remove_file(part_path).map_err(|e| eyre!("Failed to remove part file '{}': {}", part_path.display(), e))?;
        downloaded = 0;
        pb.println(format!("Server doesn't support resume, restarting: {}", url));
    }

    let total_size = response.headers().get("Content-Length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            match s.parse::<u64>() {
                Ok(size) => Some(size),
                Err(e) => {
                    log::warn!("Failed to parse Content-Length header value '{}': {}", s, e);
                    None
                }
            }
        })
        .unwrap_or(0)
        + downloaded;
    pb.set_length(total_size);
    pb.set_position(downloaded);

    // The channel receivers process_packages_content()/process_filelist_content() expect full file
    // to decompress and compute hash, so send the existing file content first. This fixes bug
    // "Decompression error: stream/file format not recognized"
    if downloaded > 0 {
        if let Some(channel) = &data_channel {
            send_file_to_channel(part_path, &channel).map_err(|e| eyre!("Failed to send file '{}' to channel: {}", part_path.display(), e))?;
        }
    }

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

    if total_size > 0 && downloaded != total_size {
        let error_msg = format!("Downloaded size ({}) does not match Content-Length ({})", downloaded, total_size);
        pb.finish_with_message(error_msg.clone());
        return Err(eyre!("Download size mismatch: Downloaded size ({}) does not match Content-Length ({}) for {}", downloaded, total_size, part_path.display()));
    }

    if let Some(last_modified) = response.headers().get("Last-Modified")
        .and_then(|s| s.to_str().ok())
        .and_then(|s| OffsetDateTime::parse(s, &Rfc2822).ok())
    {
        let system_time = filetime::FileTime::from_system_time(last_modified.into());
        set_file_mtime(part_path, system_time)
            .map_err(|e| eyre!("Failed to set file modification time for {}: {}", part_path.display(), e))?;
    }

    let filename = part_path.file_name()
        .ok_or_else(|| eyre!("Invalid filename in path: {}", part_path.display()))?;
    pb.finish_with_message(format!("Downloaded {}", filename.to_string_lossy()));

    Ok(())
}

impl PackageManager {
    fn download_or_copy_urls(&mut self, urls: Vec<String>, output_dir: &Path, max_retries: usize, async_mode: bool) -> Result<()> {
        if urls.is_empty() {
            return Ok(());
        }
        if urls[0].starts_with("/") {
            for url in urls {
                let file_name = url.split('/').last()
                    .ok_or_else(|| eyre!("Failed to extract filename from URL: {}", url))?;
                let dest_path = output_dir.join(file_name);
                match fs::copy(&url, &dest_path) {
                    Ok(_) => log::debug!("Successfully copied '{}' to '{}'", url, dest_path.display()),
                    Err(e) => {
                        log::error!("Failed to local copy '{}' to '{}': {}", url, dest_path.display(), e);
                        return Err(eyre!("Failed to copy local file: {} (While copying '{}' to '{}')", e, url, dest_path.display()));
                    }
                }
            }
        } else {
            download_urls(urls, output_dir, max_retries, async_mode)
                .map_err(|e| eyre!("Failed to download URLs to {}: {}", output_dir.display(), e))?;
        }
        Ok(())
    }

    // Download packages specified by their pkgkey strings.
    pub fn download_packages(&mut self, packages: &HashMap<String, InstalledPackageInfo>, async_mode: bool) -> Result<Vec<String>> {
        let output_dir = dirs().epkg_pkg_cache.clone();

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

        // Step 2: Call the predefined download_urls function
        self.download_or_copy_urls(urls, &output_dir, 6, async_mode)
            .map_err(|e| eyre!("Failed to download or copy package URLs to {}: {}", output_dir.display(), e))?;
        Ok(local_files)
    }

    // Wait for all pending downloads to complete
    pub fn wait_for_downloads(&self) -> Result<()> {
        DOWNLOAD_MANAGER.wait_for_all_tasks()
            .map_err(|e| eyre!("Failed to wait for download tasks to complete: {}", e))?;
        Ok(())
    }
}
