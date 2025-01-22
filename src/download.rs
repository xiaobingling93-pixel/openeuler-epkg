use std::collections::HashMap;
use std::time::Duration;
use std::sync::Arc;
use std::path::Path;
use dirs::home_dir;
use url::Url;
use reqwest::{Client, StatusCode};
use tokio::io::AsyncWriteExt; // Import AsyncWriteExt for write_all
use tokio::sync::Semaphore;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use futures::StreamExt; // Import StreamExt for bytes_stream
use anyhow::{anyhow, Context, Result};
use crate::models::*;

// Main Features:
//
// 1. Concurrent Downloads
//   - Supports parallel downloads with configurable concurrency limit
//   - Uses tokio::sync::Semaphore for resource management

// 2. Resumable Downloads
//   - Creates .part files during download
//   - Uses HTTP Range headers to resume interrupted downloads
//   - Automatically handles servers that don't support resuming

// 3. Error Handling
//   - Distinguishes between fatal (4xx) and transient errors
//   - Implements exponential backoff for retries
//   - Configurable maximum retry count

// 4. Progress Tracking
//   - Shows download progress with indicatif progress bars
//   - Tracks downloaded bytes across retries
//   - Displays ETA and transfer speed

// 5. File Management
//   - Downloads to .part files first
//   - Renames to final filename only after successful completion
//   - Cleans up partial files on failure

// 6. Robustness Features
//   - Verifies downloaded file size matches Content-Length
//   - Handles network interruptions gracefully
//   - Implements proper timeouts

// 7. User Feedback
//   - Provides clear status messages
//   - Shows retry attempts and delays
//   - Indicates when downloads are complete

// 8. Safety Features
//   - Skips already downloaded files
//   - Ensures atomic completion with file renaming
//   - Properly cleans up resources on errors

#[derive(Debug)]
struct FatalError;

impl std::fmt::Display for FatalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Fatal error")
    }
}

impl std::error::Error for FatalError {}

pub async fn download_urls(
    urls: Vec<String>,
    output_dir: &str,
    nr_parallel: usize,
    max_retries: usize,
) -> Result<()> {
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    tokio::fs::create_dir_all(output_dir).await?;

    let multi_progress = MultiProgress::new();
    let semaphore = Arc::new(Semaphore::new(nr_parallel));

    let tasks = urls.into_iter().map(|url| {
        let client = client.clone();
        let output_dir = output_dir.to_string();
        let multi_progress = multi_progress.clone();
        let semaphore = semaphore.clone();
        let max_retries = max_retries;

        tokio::spawn(async move {
            let _permit = semaphore.acquire().await?;

            let file_name = url.split('/').last()
                .map(|s| s.to_string())  // Convert to owned String
                .ok_or_else(|| anyhow!("Invalid URL: {}", url))?;
            let final_path = Path::new(&output_dir).join(&file_name);
            let part_path = final_path.with_extension("part");

            if final_path.exists() {
                return Ok(());
            }

            let pb = multi_progress.add(ProgressBar::new(0));
            pb.set_style(ProgressStyle::default_bar()
                .template("[{elapsed_precise}] [{bar:10}] {bytes_per_sec:12} ({eta}) {msg}")
                .unwrap()
                .progress_chars("=> "));
            pb.set_message(file_name.clone());

            let result = download_file_with_retries(
                &client,
                &url,
                part_path.to_str().unwrap(),
                &pb,
                max_retries
            ).await;

            pb.finish_and_clear();

            match result {
                Ok(()) => {
                    tokio::fs::rename(&part_path, &final_path).await?;
                    Ok(())
                },
                Err(e) => {
                    let _ = tokio::fs::remove_file(&part_path).await;
                    Err(e)
                }
            }
        })
    });

    let results = futures::future::join_all(tasks).await;
    for result in results {
        match result {
            Ok(Ok(_)) => {},
            Ok(Err(e)) => return Err(e),
            Err(e) => return Err(anyhow!("Task failed: {}", e)),
        }
    }

    Ok(())
}

async fn download_file_with_retries(
    client: &Client,
    url: &str,
    part_file_path: &str,
    pb: &ProgressBar,
    max_retries: usize,
) -> Result<()> {
    let mut retries = 0;

    loop {
        match download_file(client, url, part_file_path, pb).await {
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
                tokio::time::sleep(delay).await;
            }
        }
    }
}

async fn download_file(
    client: &Client,
    url: &str,
    part_file_path: &str,
    pb: &ProgressBar,
) -> Result<()> {
    let path = Path::new(part_file_path);
    let mut downloaded = 0u64;

    if path.exists() {
        let meta = tokio::fs::metadata(path).await?;
        downloaded = meta.len();
    }

    let mut request = client.get(url);
    if downloaded > 0 {
        request = request.header("Range", format!("bytes={}-", downloaded));
    }

    let response = request.send().await?;
    let status = response.status();

    // Handle error status codes
    if !status.is_success() && status != StatusCode::PARTIAL_CONTENT {
        pb.finish_with_message(format!("HTTP error {}: {}", status, url));
        return if status.is_client_error() {
            Err(anyhow!("Fatal HTTP error: {}", status).context(FatalError))
        } else {
            Err(anyhow!("HTTP error: {}", status))
        };
    }

    // Handle non-resumable responses
    if downloaded > 0 && status != StatusCode::PARTIAL_CONTENT {
        tokio::fs::remove_file(path).await?;
        downloaded = 0;
        pb.println(format!("Server doesn't support resume, restarting: {}", url));
    }

    // Calculate total size
    let total_size = response.content_length().unwrap_or(0) + downloaded;
    pb.set_length(total_size);
    pb.set_position(downloaded);

    // Open file in append mode
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;

    // Download chunks
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        pb.set_position(downloaded);
    }

    // Verify download size
    if downloaded == total_size {
        pb.finish_with_message(format!("Downloaded {}", path.file_name().unwrap().to_string_lossy()));
    } else if total_size > 0 {
        pb.finish_with_message(format!("Incomplete download: {}/{} bytes", downloaded, total_size));
        return Err(anyhow!("Download incomplete"));
    }

    Ok(())
}

impl PackageManager {

    /// Download packages specified by their pkgline strings.
    #[tokio::main(flavor = "multi_thread", worker_threads = 2)]
    pub async fn download_packages(&self, packages: &HashMap<String, InstalledPackageInfo>) -> Result<Vec<String>> {

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
        download_urls(urls, &output_dir, 6, 6).await?;
        Ok(local_files)
    }
}
