use std::sync::Arc;
use std::path::Path;
use url::Url;
use reqwest::Client;
use tokio::fs::OpenOptions;
use tokio::task::JoinSet;
use tokio::io::AsyncWriteExt; // Import AsyncWriteExt for write_all
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

    // Create a JoinSet to manage parallel tasks
    let mut set: JoinSet<Result<()>> = JoinSet::new();

    // Split the URLs into chunks based on the parallelism level
    let chunks = urls.chunks(nr_parallel);

    // Process each chunk in parallel
    for chunk in chunks {
        for url in chunk {
            let client = Arc::clone(&client);
            let output_dir = output_dir.to_string();
            let url = url.clone();
            let multi_progress = multi_progress.clone();

            // Spawn a task for each URL in the chunk
            set.spawn(async move {
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
                        .template("[{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} {msg}")
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
        }

        // Wait for all tasks in the chunk to complete
        while let Some(result) = set.join_next().await {
            if let Err(e) = result? {
                eprintln!("Error: {}", e);
            }
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
    ];

    let output_dir = "downloads";
    let nr_parallel = 2; // Number of parallel downloads

    // Download the URLs
    download_urls(urls, output_dir, nr_parallel).await?;

    Ok(())
}
*/
