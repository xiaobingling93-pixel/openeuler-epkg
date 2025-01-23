use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use dirs::home_dir;

use anyhow::{anyhow, Context, Result};
use crossbeam_channel::bounded;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use ureq::{Agent, AgentBuilder};
use crate::models::*;

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
struct FatalError;

impl std::fmt::Display for FatalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Fatal error")
    }
}

impl std::error::Error for FatalError {}

pub fn download_urls(
    urls: Vec<String>,
    output_dir: &str,
    nr_parallel: usize,
    max_retries: usize,
    proxy: Option<&str>,
) -> Result<()> {
    // Create HTTP client with optional proxy
    let client = if let Some(proxy_url) = proxy {
        AgentBuilder::new()
            .proxy(ureq::Proxy::new(proxy_url)?)
            .build()
    } else {
        AgentBuilder::new().build()
    };

    fs::create_dir_all(output_dir)?;
    let multi_progress = MultiProgress::new();

    let (sender, receiver) = bounded(urls.len());
    for url in urls {
        sender.send(url)?;
    }
    drop(sender);

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(nr_parallel)
        .build()?;

    let errors = Arc::new(std::sync::Mutex::new(Vec::new()));

    pool.scope(|s| {
        for url in receiver {
            let client = client.clone();
            let output_dir = output_dir.to_string();
            let multi_progress = multi_progress.clone();
            let errors = Arc::clone(&errors);

            s.spawn(move |_| {
                if let Err(e) = download_task(&client, &url, &output_dir, &multi_progress, max_retries) {
                    errors.lock().unwrap().push(e);
                }
            });
        }
    });

    let errors = Arc::try_unwrap(errors).unwrap().into_inner().unwrap();
    if !errors.is_empty() {
        return Err(anyhow!("{} downloads failed", errors.len()));
    }

    Ok(())
}

fn download_task(
    client: &Agent,
    url: &str,
    output_dir: &str,
    multi_progress: &MultiProgress,
    max_retries: usize,
) -> Result<()> {
    let file_name = url.split('/').last()
        .ok_or_else(|| anyhow!("Invalid URL: {}", url))?;
    let final_path = Path::new(output_dir).join(file_name);
    let part_path = final_path.with_extension("part");

    if final_path.exists() {
        return Ok(());
    }

    let pb = multi_progress.add(ProgressBar::new(0));
    pb.set_style(ProgressStyle::default_bar()
        .template("[{elapsed_precise}] [{bar:10}] {bytes_per_sec:12} ({eta}) {msg}")
        .unwrap()
        .progress_chars("=> "));
    pb.set_message(file_name.to_string());

    let result = download_file_with_retries(
        client,
        url,
        &part_path,
        &pb,
        max_retries
    );

    pb.finish_and_clear();

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
) -> Result<()> {
    let mut retries = 0;

    loop {
        match download_file(client, url, part_path, pb) {
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
) -> Result<()> {
    let mut downloaded = if part_path.exists() {
        fs::metadata(part_path)?.len()
    } else {
        0
    };

    let mut request = client.get(url);
    if downloaded > 0 {
        request = request.set("Range", &format!("bytes={}-", downloaded));
    }

    let response = request.call()?;
    let status = response.status();

    if !(status >= 200 && status < 300) && status != 206 {
        pb.finish_with_message(format!("HTTP error {}: {}", status, url));
        return if status >= 400 && status < 500 {
            Err(anyhow!("Fatal HTTP error: {}", status).context(FatalError))
        } else {
            Err(anyhow!("HTTP error: {}", status))
        };
    }

    if downloaded > 0 && status != 206 {
        fs::remove_file(part_path)?;
        downloaded = 0;
        pb.println(format!("Server doesn't support resume, restarting: {}", url));
    }

    let total_size = response.header("Content-Length")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
        + downloaded;
    pb.set_length(total_size);
    pb.set_position(downloaded);

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(part_path)?;

    let mut reader = response.into_reader();
    let mut buffer = [0; 8192];
    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        file.write_all(&buffer[..bytes_read])?;
        downloaded += bytes_read as u64;
        pb.set_position(downloaded);
    }

    if total_size > 0 && downloaded != total_size {
        pb.finish_with_message(format!("Incomplete download: {}/{} bytes", downloaded, total_size));
        return Err(anyhow!("Download incomplete"));
    }

    pb.finish_with_message(format!("Downloaded {}", part_path.file_name().unwrap().to_string_lossy()));
    Ok(())
}

impl PackageManager {

    /// Download packages specified by their pkgline strings.
    pub fn download_packages(&self, packages: &HashMap<String, InstalledPackageInfo>) -> Result<Vec<String>> {

        let home = home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
        let output_dir = format!(
            "{}/.cache/epkg/packages",
            home.display()
        );

        // Step 1: Compose URLs for each pkgline
        let mut urls = Vec::new();
        let mut local_files = Vec::new();
        for pkgline in packages.keys() {
            let pkghash = &pkgline[..32]; // Extract the first 32 characters as the hash
            if let Some(spec) = self.pkghash2spec.get(pkghash) {
                let repo = &spec.repo;
                let url = format!(
                    "{}/{}/{}/store/{}/{}.epkg",
                    self.env_config.channel.baseurl,
                    repo,
                    self.options.arch,
                    &pkgline[..2], // First 2 characters of the hash
                    pkgline
                );
                urls.push(url);
                local_files.push(format!("{}/{}.epkg", output_dir, pkgline));
            } else {
                return Err(anyhow::anyhow!("Package spec not found for {}", pkgline));
            }
        }

        // Step 2: Call the predefined download_urls function
        download_urls(urls, &output_dir, 6, 6, None)?;
        Ok(local_files)
    }
}
