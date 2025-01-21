use std::sync::Arc;
use std::path::Path;
use url::Url;
use reqwest::Client;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt; // Import AsyncWriteExt for write_all
use tokio::sync::Semaphore;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use futures::StreamExt; // Import StreamExt for bytes_stream
use anyhow::Result;

/// Downloads a list of URLs to a specified directory with a given level of parallelism.
///
/// # Arguments
/// * `urls` - A vector of URLs to download.
/// * `output_dir` - The directory where the downloaded files will be saved.
/// * `nr_parallel` - The number of parallel downloads.
///
/// # Returns
/// A `Result` indicating success or failure.
pub async fn download_urls(urls: Vec<String>, output_dir: &str, nr_parallel: usize) -> Result<()> {
    // Create the output directory if it doesn't exist
    if !Path::new(output_dir).exists() {
        tokio::fs::create_dir_all(output_dir).await?;
    }

    // Create a reqwest client
    let client = Arc::new(Client::new());

    // Create a MultiProgress instance to manage multiple progress bars
    let multi_progress = MultiProgress::new();

    // Create a semaphore to control the number of concurrent downloads
    let semaphore = Arc::new(Semaphore::new(nr_parallel));

    // Create a vector to hold the join handles for the download tasks
    let mut handles: Vec<tokio::task::JoinHandle<Result<()>>> = Vec::new();

    // Iterate over the URLs and spawn download tasks
    for url in urls {
        let client = Arc::clone(&client);
        let output_dir = output_dir.to_string();
        let multi_progress = multi_progress.clone();
        let semaphore = Arc::clone(&semaphore);

        // Acquire a permit from the semaphore (blocks if no permits are available)
        let permit = Arc::clone(&semaphore).acquire_owned().await?;

        // Spawn a task for each URL
        let handle = tokio::spawn(async move {
            // Release the permit when the task is done
            let _permit = permit;

            // Parse the URL
            let url = Url::parse(&url)?;

            // Derive the output file path from the URL
            let file_name = url
                .path_segments()
                .and_then(|segments| segments.last())
                .unwrap_or("file");
            let output_path = format!("{}/{}", output_dir, file_name);

            // Skip if the file already exists
            if Path::new(&output_path).exists() {
                return Ok(());
            }

            // Create the output file
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&output_path)
                .await?;

            // Create a progress bar for this download
            let pb = multi_progress.add(ProgressBar::new(0));
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("[{elapsed_precise}] [{bar:10}] {bytes}/{total_bytes} {msg}")
                    .unwrap()
                    .progress_chars("=> "),
            );
            pb.set_message(output_path.clone());

            // Send a GET request to the URL
            let response = client.get(url.as_str()).send().await?;

            // Ensure the request was successful
            if !response.status().is_success() {
                pb.finish_with_message(format!("Failed to download {}: {}", url, response.status()));
                return Ok(());
            }

            // Get the total content length for the progress bar
            if let Some(total_size) = response.content_length() {
                pb.set_length(total_size);
            }

            // Stream the response body to the file
            let mut content = response.bytes_stream();
            let mut file = tokio::io::BufWriter::new(file);
            while let Some(chunk) = content.next().await {
                let chunk = chunk?;
                file.write_all(&chunk).await?;
                pb.inc(chunk.len() as u64);
            }

            pb.finish_with_message(format!("Downloaded {} to {}", url, output_path));
            Ok(())
        });

        // Store the join handle for the task
        handles.push(handle);
    }

    // Wait for all tasks to complete
    for handle in handles {
        if let Err(e) = handle.await? {
            eprintln!("Error: {}", e);
        }
    }

    println!("Complete!");
    Ok(())
}

/*
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let urls = vec![
        "https://cdn.kernel.org/pub/linux/kernel/v6.x/patch-6.8.7.xz".to_string(),
        "https://cdn.kernel.org/pub/linux/kernel/v6.x/patch-6.7.12.xz".to_string(),
        "https://cdn.kernel.org/pub/linux/kernel/v5.x/patch-5.15.156.xz".to_string(),
        "https://cdn.kernel.org/pub/linux/kernel/v5.x/patch-5.4.274.xz".to_string(),
        "https://cdn.kernel.org/pub/linux/kernel/v4.x/patch-4.19.312.xz ".to_string(),
    ];

    let output_dir = "downloads";
    let nr_parallel = 2; // Number of parallel downloads

    // Download the URLs
    download_urls(urls, output_dir, nr_parallel).await?;

    Ok(())
}
*/
