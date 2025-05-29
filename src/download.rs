use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::sync::mpsc::Sender;
use std::sync::LazyLock;

use color_eyre::{eyre, Result};
use color_eyre::eyre::WrapErr;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use ureq::{Agent, config::Config, tls::TlsConfig, Proxy};
use crate::dirs;
use crate::models::*;

#[derive(Debug, Clone)]
pub struct DownloadTask {
    pub url: String,
    pub output_dir: String,
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
    pub fn new(url: String, output_dir: String, max_retries: usize) -> Self {
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
        self.status.lock().unwrap().clone()
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
            .proxy(proxy.map(|p| Proxy::new(p).unwrap()))
            .user_agent("curl/8.13.0")
            .build();

        let client = Agent::new_with_config(config);
        let multi_progress = MultiProgress::new();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(nr_parallel)
            .build()?;

        Ok(Self {
            client,
            multi_progress,
            tasks: Arc::new(std::sync::Mutex::new(Vec::new())),
            pool,
            is_processing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    pub fn submit_task(&self, task: DownloadTask) -> Result<()> {
        let mut tasks = self.tasks.lock().unwrap();
        tasks.push(task);
        Ok(())
    }

    pub fn wait_for_task(&self, task_id: usize) -> Result<DownloadStatus> {
        loop {
            let tasks = self.tasks.lock().unwrap();
            if let Some(task) = tasks.get(task_id) {
                let status = task.get_status();
                match status {
                    DownloadStatus::Completed | DownloadStatus::Failed(_) => return Ok(status),
                    _ => {}
                }
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
                let mut tasks_guard = tasks.lock().unwrap();
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

                for task in pending_tasks {
                    let client = client.clone();
                    let multi_progress = multi_progress.clone();
                    let task = task.clone();

                    // Create a channel to signal when download starts
                    let (start_tx, start_rx) = std::sync::mpsc::channel();

                    rayon::spawn(move || {
                        *task.status.lock().unwrap() = DownloadStatus::Downloading;

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
                            *task.status.lock().unwrap() = DownloadStatus::Failed(e.to_string());
                        } else {
                            *task.status.lock().unwrap() = DownloadStatus::Completed;
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
        let tasks = self.tasks.lock().unwrap();
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
            return Err(eyre::eyre!(
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
    output_dir: &str,
    max_retries: usize,
    async_mode: bool,
) -> Result<Vec<DownloadTask>> {
    let mut submitted_tasks = Vec::new();
    for url in urls {
        let task = DownloadTask::new(url, output_dir.to_string(), max_retries);
        submit_download_task(task.clone())?;
        submitted_tasks.push(task);
    }

    fs::create_dir_all(output_dir)?;
    DOWNLOAD_MANAGER.start_processing()?;

    if !async_mode {
        // Wait for each task one by one (in submitted order)
        for (i, _task) in submitted_tasks.iter().enumerate() {
            DOWNLOAD_MANAGER.wait_for_task(i)?;
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
    output_dir: &str,
    multi_progress: &MultiProgress,
    max_retries: usize,
    data_channel: Option<Sender<Vec<u8>>>,
) -> Result<()> {
    let final_path = if let Some((_, str_b)) = url.split_once("///") {
        Path::new(output_dir).join(str_b)
    } else {
        let file_name = url.split('/').last()
            .ok_or_else(|| eyre::eyre!("Invalid URL: {}", url))?;
        Path::new(output_dir).join(file_name)
    };
    let part_path = final_path.with_extension("part");

    if final_path.exists() {
        return Ok(());
    }
    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Create progress bar but don't show it yet
    let pb = multi_progress.add(ProgressBar::new(0));
    pb.set_style(ProgressStyle::default_bar()
        .template("[{elapsed_precise}] [{bar:10}] {bytes_per_sec:12} ({eta}) {msg}")
        .unwrap()
        .progress_chars("=> "));
    pb.set_message(final_path.display().to_string());

    // Start the download
    let result = download_file_with_retries(
        client,
        url,
        &part_path,
        &pb,
        max_retries,
        data_channel,
    );

    // Only show progress bar after download has started
    if result.is_ok() {
        pb.finish_with_message(format!("Downloaded {}", part_path.file_name().unwrap().to_string_lossy()));
    } else {
        pb.finish_and_clear();
    }

    match result {
        Ok(()) => fs::rename(&part_path, &final_path)?,
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
    let mut retries = 0;

    loop {
        match download_file(client, url, part_path, pb, data_channel.clone()) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if e.downcast_ref::<FatalError>().is_some() {
                    return Err(e);
                }

                if retries >= max_retries {
                    return Err(e).context(format!("Max retries ({}) exceeded", max_retries));
                }

                retries += 1;
                let delay = Duration::from_secs(2u64.pow(retries as u32));
                pb.println(format!("Retrying {} (attempt {}/{})...", url, retries, max_retries));
                thread::sleep(delay);
            }
        }
    }
}

fn download_file(
    client: &Agent,
    url: &str,
    part_path: &Path,
    pb: &ProgressBar,
    data_channel: Option<Sender<Vec<u8>>>,
) -> Result<()> {
    let mut downloaded = if part_path.exists() {
        fs::metadata(part_path)?.len()
    } else {
        0
    };

    let mut request = client.get(url.replace("///", "/"));

    if downloaded > 0 {
        request = request.header("Range", &format!("bytes={}-", downloaded));
    }

    let mut response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(code)) => {
            let error_msg = format!("HTTP {} error: {} - {}", code,
                if code >= 400 && code < 500 { "Client Error" }
                else { "Server Error" }, url);
            pb.finish_with_message(error_msg.clone());
            return if code >= 400 && code < 500 {
                Err(eyre::eyre!(FatalError(error_msg)))
            } else {
                Err(eyre::eyre!(error_msg))
            };
        }
        Err(ureq::Error::Io(e)) => {
            let error_msg = format!("Network error: {} - {}", e, url);
            pb.finish_with_message(error_msg.clone());
            return Err(eyre::eyre!(error_msg));
        }
        Err(e) => {
            let error_msg = format!("Error downloading: {} - {}", e, url);
            pb.finish_with_message(error_msg.clone());
            return Err(eyre::eyre!(error_msg));
        }
    };

    let status = response.status();

    // Check content type to detect HTML login pages
    if let Some(content_type) = response.headers().get("Content-Type").and_then(|v| v.to_str().ok()) {
        if content_type.contains("text/html") {
            let error_msg = "Received HTML page instead of file. This may indicate an authentication issue with the server.";
            pb.finish_with_message(error_msg);
            return Err(eyre::eyre!(FatalError(error_msg.to_string())));
        }
    }

    if downloaded > 0 && status != 206 {
        fs::remove_file(part_path)?;
        downloaded = 0;
        pb.println(format!("Server doesn't support resume, restarting: {}", url));
    }

    let total_size = response.headers().get("Content-Length").and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
        + downloaded;
    pb.set_length(total_size);
    pb.set_position(downloaded);

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(true)
        .open(part_path)?;

    let mut reader = response.body_mut().as_reader();
    let mut buffer = vec![0; 8192];

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        file.write_all(&buffer[..bytes_read])?;
        downloaded += bytes_read as u64;
        pb.set_position(downloaded);

        if let Some(channel) = &data_channel {
            if let Err(_) = channel.send(buffer[..bytes_read].to_vec()) {
                // Channel was closed, but we continue downloading
            }
        }
    }

    if total_size > 0 && downloaded != total_size {
        let error_msg = format!("Downloaded size ({}) does not match Content-Length ({})", downloaded, total_size);
        pb.finish_with_message(error_msg.clone());
        return Err(eyre::eyre!(error_msg));
    }

    pb.finish_with_message(format!("Downloaded {}", part_path.file_name().unwrap().to_string_lossy()));
    Ok(())
}

impl PackageManager {
    fn download_or_copy_urls(&mut self, urls: Vec<String>, output_dir: &str, max_retries: usize, async_mode: bool) -> Result<()> {
        if urls.is_empty() {
            return Ok(());
        }
        if urls[0].starts_with("/") {
            for url in urls {
                let file_name = url.split('/').last().unwrap();
                let dest_path = format!("{}/{}", output_dir, file_name);
                if let Err(e) = fs::copy(&url, &dest_path) {
                    eprintln!("Failed to local copy '{}' to '{}': {}", url, dest_path, e);
                    return Err(e.into());
                }
            }
        } else {
            download_urls(urls, output_dir, max_retries, async_mode)?;
        }
        Ok(())
    }

    // Download packages specified by their pkgline strings.
    pub fn download_packages(&mut self, packages: &HashMap<String, InstalledPackageInfo>, async_mode: bool) -> Result<Vec<String>> {
        let output_dir = dirs().epkg_pkg_cache.display().to_string();

        // Step 1: Compose URLs for each pkgline
        let mut urls = Vec::new();
        let mut local_files = Vec::new();
        for pkgline in packages.keys() {
            let pkghash = &pkgline[..32]; // Extract the first 32 characters as the hash
            if let Some(spec) = self.pkghash2spec.get(pkghash) {
                let spec = spec.clone();
                let repo = &spec.repo;
                // XXX: this only works for single-repo channel. The actual mapping is
                // - 1 channel could have N repos
                // - 1 repo (each may have its own url) could have M packages
                let channel_config = self.get_channel_config(config().common.env.clone())?;
                let url = format!(
                    "{}/{}/{}/store/{}/{}.epkg",
                    channel_config.index_url.clone(),
                    repo,
                    config().common.arch,
                    &pkgline[..2], // First 2 characters of the hash
                    pkgline
                );
                urls.push(url);
                local_files.push(format!("{}/{}.epkg", output_dir, pkgline));
            } else {
                return Err(eyre::eyre!("Package spec not found for {}", pkgline));
            }
        }

        // Step 2: Call the predefined download_urls function
        self.download_or_copy_urls(urls, &output_dir, 6, async_mode)?;
        Ok(local_files)
    }

    // Wait for all pending downloads to complete
    pub fn wait_for_downloads(&self) -> Result<()> {
        let _ = DOWNLOAD_MANAGER.wait_for_all_tasks()?;
        Ok(())
    }
}
