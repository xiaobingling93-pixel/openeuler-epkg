// ============================================================================
// DOWNLOAD UTILS - Shared Utility Functions
//
// This module contains shared utility functions used throughout the download
// system, including data streaming, error mapping, logging helpers, and
// common operations that support multiple components.
//
// Key Features:
// - Safe HTTP event logging with error handling
// - IO error mapping to DownloadError types
// - Data streaming to channels for concurrent processing
// - File system utilities and path operations
// - Common validation and conversion helpers
// ============================================================================

use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::{
        atomic::Ordering,
        mpsc::SyncSender as Sender,
    },
};

use color_eyre::eyre::{eyre, Result};

use crate::mirror;
use super::types::*;


/// Safe wrapper for logging HTTP events that ignores failures
pub fn log_http_event_safe(url: &str, event: mirror::HttpEvent) {
    if let Err(e) = mirror::append_http_log(url, event) {
        log::warn!("Failed to log HTTP event for {}: {}", url, e);
    }
}

/// Map IO errors to DownloadError with context
pub fn map_io_error<T>(result: std::io::Result<T>, context: &str, path: &Path) -> Result<T> {
    result.map_err(|e| DownloadError::DiskError {
        details: format!("Failed to {} '{}': {} (line: {})", context, path.display(), e, line!())
    }.into())
}

/// Enhanced error logging with backtrace support
pub fn log_error_with_backtrace<E: std::fmt::Display + std::fmt::Debug>(url: &str, error: &E) {
    log::error!("Download task failed for {}: {}", url, error);

    // Check if we should dump backtraces
    let should_dump_backtrace = cfg!(debug_assertions) ||
                               std::env::var("RUST_BACKTRACE").is_ok() ||
                               std::env::var("EPKG_BACKTRACE").is_ok();

    if should_dump_backtrace {
        log::error!("Full backtrace:\n{:?}", error);
    }
}

/// Calculate how many bytes to write to avoid overwriting existing data
pub fn calculate_write_bytes(
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
                log::debug!("Master task reached chunk boundary at {} bytes, stopping for {}", chunk_append_offset, task.chunk_path.display());
            } else {
                log::debug!("Chunk task completed at {} bytes for {}", chunk_append_offset, task.chunk_path.display());
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
pub fn write_chunk_data(
    file: &mut File,
    buffer: &[u8],
    bytes_to_write: usize,
    task: &DownloadTask,
    chunk_append_offset: u64
) -> Result<usize> {
    let chunk_size_val = task.chunk_size.load(Ordering::Relaxed);
    let write_len = if chunk_size_val > 0 {
        let remaining = chunk_size_val.saturating_sub(chunk_append_offset);
        if remaining == 0 {
            log::warn!("Chunk task received {} surplus bytes, discarding for {}", bytes_to_write, task.chunk_path.display());
            return Ok(0); // Signal to stop
        }
        std::cmp::min(bytes_to_write, remaining as usize)
    } else {
        bytes_to_write
    };

    file.write_all(&buffer[..write_len])
        .map_err(|e| DownloadError::DiskError {
            details: format!("Failed to write {} bytes to chunk file '{}': {}",
                           write_len, task.chunk_path.display(), e)
        })?;

    // Validate file size after write to detect disk space issues or corruption
    if let Ok(metadata) = file.metadata() {
        let expected_size = chunk_append_offset + write_len as u64;
        if metadata.len() != expected_size {
            log::error!(
                "File size mismatch after write: expected {} bytes but file has {} bytes for {}",
                expected_size, metadata.len(), task.chunk_path.display()
            );
            return Err(DownloadError::DiskError {
                details: format!("File size mismatch after write: expected {} bytes but file has {} bytes for {}",
                               expected_size, metadata.len(), task.chunk_path.display())
            }.into());
        }
    }

    if write_len < bytes_to_write && chunk_append_offset + write_len as u64 > chunk_size_val {
        log::warn!("Chunk {} exceeded expected size by {} bytes; extra data ignored",
                  task.chunk_path.display(), (chunk_append_offset + write_len as u64) - chunk_size_val);
    }

    Ok(write_len)
}

/// Send complete file data to channel (for master tasks)
pub fn send_file_to_channel(
    task: &DownloadTask,
) -> Result<()> {
    // Only master tasks should send data to channel
    if !task.is_master_task() {
        return Err(eyre!("Should not call send_file_to_channel() for chunk task {:?}", task));
    }

    // Get all data channels from task
    let data_channels = task.get_all_data_channels();
    if data_channels.is_empty() {
        return Ok(()); // No channels to send to
    }

    // Initialize progress bar message for processing existing file
    let total_size = std::fs::metadata(&task.final_path)
        .map(|m| m.len())
        .unwrap_or(0);
    task.set_length(total_size);
    task.set_message(format!("Processing {}", task.final_path.display()));

    // The channel receivers process_packages_content()/process_filelist_content() expect full file
    // to decompress and compute hash, so send the existing file content first. This fixes bug
    // "Decompression error: stream/file format not recognized"
    send_chunk_to_all_channels(&task, &task.final_path, &data_channels, true)
}

/// Send a chunk file to all data channels (for broadcasting to duplicate downloads)
pub fn send_chunk_to_all_channels(
    task: &DownloadTask,
    part_path: &Path,
    data_channels: &[Sender<Vec<u8>>],
    update_progress: bool,
) -> Result<()> {
    // Ensure we only stream the pre-existing file once per download_file_with_retries() lifetime
    if task.has_sent_existing.swap(true, Ordering::SeqCst) {
        log::debug!("Existing file already streamed once – skipping second send for {}", part_path.display());
        return Ok(());
    }

    log::debug!("Sending chunk file to {} channels: {}", data_channels.len(), part_path.display());

    let mut file = map_io_error(std::fs::File::open(part_path), "open file for channel", part_path)?;
    let mut buffer = vec![0; 64 * 1024]; // 64KB buffer
    let mut chunks_sent = 0;
    let mut total_bytes_sent = 0u64;
    let mut last_update = std::time::Instant::now();

    loop {
        let bytes_read = map_io_error(file.read(&mut buffer), "read file for channel", part_path)?;
        if bytes_read == 0 {
            break; // EOF
        }

        chunks_sent += 1;
        total_bytes_sent += bytes_read as u64;
        let chunk_data = buffer[..bytes_read].to_vec();

        // Send to all data channels
        for (i, data_channel) in data_channels.iter().enumerate() {
            match data_channel.send(chunk_data.clone()) {
                Ok(_) => {
                    log::trace!("Sent chunk {} ({} bytes) to channel {} from {}", chunks_sent, bytes_read, i, part_path.display());
                }
                Err(e) => {
                    // Treat closed receiver channel as a non-fatal condition for chunks too
                    log::warn!("Channel {} closed while sending chunk {} from {}: {}", i, chunks_sent, part_path.display(), e);
                }
            }
        }

        // Update position with rate limiting when called from send_file_to_channel
        if update_progress {
            let now = std::time::Instant::now();
            if now.duration_since(last_update) > std::time::Duration::from_millis(crate::download::PROGRESS_UPDATE_INTERVAL_MS) {
                task.set_position(total_bytes_sent);
                last_update = now;
            }
        }
    }

    Ok(())
}

/// Send a chunk file to the data channel (for streaming fresh chunk data)
/// This bypasses the master task and has_sent_existing guards
pub fn send_chunk_to_channel(
    task: &DownloadTask,
    part_path: &Path,
    data_channel: &Sender<Vec<u8>>,
) -> Result<()> {
    send_chunk_to_all_channels(task, part_path, &[data_channel.clone()], false)
}
