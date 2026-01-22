// ============================================================================
// DOWNLOAD PROGRESS - Progress Bar Setup and Status Tracking
//
// This module manages progress tracking and user feedback during download
// operations. It provides visual progress bars, ETA calculations, transfer
// speed display, and comprehensive status reporting for individual downloads
// and overall download manager statistics.
//
// Key Features:
// - Progress bar setup with indicatif library
// - Real-time download progress updates
// - ETA calculation and display
// - Transfer speed monitoring
// - Chunk progress tracking and aggregation
// - Multi-progress coordination for concurrent downloads
// ============================================================================

use std::{
    sync::atomic::Ordering,
    time::Duration,
};

use indicatif::{ProgressBar, MultiProgress, ProgressStyle};
use color_eyre::eyre::Result;
use ureq::http;

use crate::config;
use super::types::*;
use super::http::get_remote_size;

/// Setup progress bar for download task
pub fn setup_progress_bar(task: &DownloadTask, multi_progress: &MultiProgress, url: &str) -> Result<()> {
    // Skip creating progress bar if quiet mode is enabled
    if config().common.quiet {
        if let Ok(mut pb_guard) = task.progress_bar.lock() {
            *pb_guard = None;
        }
        return Ok(());
    }

    let pb = multi_progress.add(ProgressBar::new(0));
    pb.set_style(ProgressStyle::default_bar()
        .template(&format!("[{{elapsed:>3}}] [{{bar:{}}}] {{bytes_per_sec:>12}} ({{bytes:>11}}) {{msg}}", PROGRESS_BAR_WIDTH))
        .map_err(|e| color_eyre::eyre::eyre!("Failed to parse HTTP response: {}", e))?
        .progress_chars("=> "));
    pb.set_message(url.to_string());

    if let Ok(mut pb_guard) = task.progress_bar.lock() {
        *pb_guard = Some(pb);
    }
    Ok(())
}

/// Update ETA calculation for a single task
pub fn update_single_task_eta(task: &DownloadTask) -> (u64, u64, u64, u64, u64) {
    let start_time = {
        if let Ok(start_guard) = task.start_time.lock() {
            start_guard.clone()
        } else {
            // Return zero values
            return (0, 0, 0, 0, 0);
        }
    };

    if task.duration_ms.load(Ordering::Relaxed) > 0 {
        // Avoid wrong calculation on old start_time in race window: task just re-opened for
        // retry download, but has not refreshed its start_time
        return (0, 0, 0, 0, 0);
    }

    let chunk_size = task.chunk_size.load(Ordering::Relaxed);
    let received_bytes = task.received_bytes.load(Ordering::Relaxed);
    let resumed_bytes = task.resumed_bytes.load(Ordering::Relaxed);
    let total_progress = received_bytes + resumed_bytes;

    if let Some(start) = start_time {
        let elapsed = start.elapsed();
        // Use only this task's network received bytes (not child chunks)
        let network_downloaded = received_bytes;

        if network_downloaded > 0 && elapsed.as_secs() > 0 {
            let rate = network_downloaded as f64 / elapsed.as_secs_f64();
            let remaining_bytes = chunk_size.saturating_sub(total_progress);
            let estimated_seconds = remaining_bytes as f64 / rate;

            // Update atomic fields
            task.throughput_bps.store(rate as u64, Ordering::Relaxed);
            task.eta.store(estimated_seconds as u64, Ordering::Relaxed);

            return (estimated_seconds as u64, rate as u64, remaining_bytes, total_progress, chunk_size);
        }
    }

    // Store zero values in atomic fields
    task.throughput_bps.store(0, Ordering::Relaxed);
    task.eta.store(0, Ordering::Relaxed);

    (0, 0, chunk_size.saturating_sub(total_progress), total_progress, chunk_size)
}

/// Collect ETA statistics for a single task and update accumulators
pub fn collect_task_eta_stats(
    task: &DownloadTask,
    level: usize,
    stats: &mut DownloadManagerStats,
    debug_stats: &mut Vec<String>,
) {
    // Count task types by status
    match task.get_status() {
        DownloadStatus::Pending => stats.pending_tasks += 1,
        DownloadStatus::Completed => stats.complete_tasks += 1,
        DownloadStatus::Downloading => {
            // Will be processed below for ETA calculation
        },
        DownloadStatus::Failed(_) => return,
    }

    // Only consider downloading tasks for ETA stats
    if !matches!(task.get_status(), DownloadStatus::Downloading) {
        return;
    }

    // Count by task level
    match level {
        1 => stats.master_tasks += 1,
        2 => stats.l2_chunk_tasks += 1,
        3 => stats.l3_chunk_tasks += 1,
        _ => {},
    }

    let task_prefix = match level {
        1 => "M",
        2 => "L2",
        3 => "L3",
        _ => "U"
    };

    // Calculate ETA and get values directly
    let (eta_secs, throughput_bps, remaining_bytes, total_progress, chunk_size) = update_single_task_eta(task);
    let received_bytes = task.received_bytes.load(Ordering::Relaxed);

    if eta_secs > 0 && remaining_bytes > 0 && throughput_bps > 0 {
        stats.total_remaining_bytes += remaining_bytes;
        stats.total_rate_bps += throughput_bps;
        stats.active_tasks += 1;

        // Update ETA extremes
        if eta_secs > stats.slowest_task_eta {
            stats.slowest_task_eta = eta_secs;
        }
        if eta_secs < stats.fastest_task_eta {
            stats.fastest_task_eta = eta_secs;
        }

        // Generate debug stat if we have meaningful data
        if chunk_size > 0 && received_bytes > 0 {
            let debug_stat = format!(
                "{}[{}]: {:.1}KB/{:.1}KB @{:.1}KB/s ETA:{:.1}s",
                task_prefix,
                task.chunk_offset.load(Ordering::Relaxed) / 1024,
                total_progress / 1024,
                chunk_size / 1024,
                throughput_bps / 1024,
                eta_secs
            );
            debug_stats.push(debug_stat);
        }
    }
}

/// Update download manager stats and log global ETA
pub fn update_global_stats(
    mut new_stats: DownloadManagerStats,
    _debug_stats: &[String],
) -> u64 {
    // Calculate global ETA and finalize stats
    let global_ideal_eta = if new_stats.total_rate_bps > 0 && new_stats.active_tasks > 0 {
        (new_stats.total_remaining_bytes as f64 / new_stats.total_rate_bps as f64) as u64
    } else {
        0
    };
    new_stats.global_ideal_eta = global_ideal_eta;
    new_stats.fastest_task_eta = if new_stats.fastest_task_eta == u64::MAX {
        0
    } else {
        new_stats.fastest_task_eta
    };

    // Update stats atomically
    if let Ok(mut stats_guard) = super::manager::DOWNLOAD_MANAGER.stats.lock() {
        *stats_guard = new_stats.clone();
    }

    global_ideal_eta
}

// Rate-limit to once per second
pub fn dump_global_stats_ratelimit(
    stats: &DownloadManagerStats,
    global_ideal_eta: u64,
    debug_stats: &[String],
) {
    if !log::log_enabled!(log::Level::Debug) {
        return
    }

    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static LAST_LOG_TIME: AtomicU64 = AtomicU64::new(0);
    let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let last_log_time = LAST_LOG_TIME.load(Ordering::Relaxed);

    if current_time > last_log_time {
        LAST_LOG_TIME.store(current_time, Ordering::Relaxed);
        dump_global_stats(stats, global_ideal_eta, debug_stats);
    }
}

/// Log global ETA debug information
pub fn dump_global_stats(
    stats: &DownloadManagerStats,
    global_ideal_eta: u64,
    debug_stats: &[String],
) {
    if stats.active_tasks == 0 {
        return
    }

    println!(
        "Global ETA calculation: {:.1}s for {:.1}MB remaining across {} active downloads",
        global_ideal_eta as f64,
        stats.total_remaining_bytes as f64 / (1024.0 * 1024.0),
        stats.active_tasks
    );
    println!(
        "Download types: {} masters, {} L2 chunks, {} L3 chunks",
        stats.master_tasks, stats.l2_chunk_tasks, stats.l3_chunk_tasks
    );
    println!(
        "ETA range: fastest={:.1}s, slowest={:.1}s, global={:.1}s, aggregate_rate={:.1}KB/s",
        stats.fastest_task_eta as f64,
        stats.slowest_task_eta as f64,
        global_ideal_eta as f64,
        stats.total_rate_bps as f64 / 1024.0
    );

    let max_show_items = super::types::MAX_DISPLAY_STATS;
    // Log individual task stats (limited to prevent spam)
    for (i, stat) in debug_stats.iter().take(max_show_items).enumerate() {
        println!("Task {}: {}", i + 1, stat);
    }
    if debug_stats.len() > max_show_items {
        println!("... and {} more tasks", debug_stats.len() - max_show_items);
    }
}

/// Format progress message with chunk count information
pub fn format_progress_message(resolved_url: &str, downloading_chunks: usize) -> String {
    if downloading_chunks == 0 {
        resolved_url.to_string()
    } else {
        format!("+{} {}", downloading_chunks, resolved_url)
    }
}

/// Update progress for master task with chunk information
pub fn update_master_task_progress(master_task: &DownloadTask, resolved_url: String) {
    let (total_received, total_reused, downloading_chunks) = master_task.get_total_progress_bytes();

    master_task.set_position(total_received + total_reused);
    master_task.set_message(format_progress_message(&resolved_url, downloading_chunks));
}

/// Update progress when master chunk is downloading
pub fn update_download_progress(
    task: &DownloadTask,
    last_update: &mut std::time::Instant
) {
    if !task.is_master_task() {
        return;
    }

    let now = std::time::Instant::now();
    if now.duration_since(*last_update) > Duration::from_millis(super::types::PROGRESS_UPDATE_INTERVAL_MS) {
        update_master_task_progress(task, task.get_resolved_url());
        *last_update = now;
    }
}

/// Update progress when master task is waiting for other chunks downloading
pub fn update_chunk_progress(
    chunk_task: &DownloadTask,
    master_task: &DownloadTask
) {
    // Avoid redrawing completed/failed bars; repeated set_position/set_message on
    // finished bars causes MultiProgress to reprint the line each refresh.
    match master_task.get_status() {
        DownloadStatus::Completed | DownloadStatus::Failed(_) => return,
        _ => {}
    }

    update_master_task_progress(master_task, chunk_task.get_resolved_url());
}

/// Log download completion with final statistics
pub fn log_download_completion(
    task: &DownloadTask,
    resolved_url: &str,
) {
    update_single_task_eta(task);
    task.eta.store(0, Ordering::Relaxed);

    // Calculate and set download duration
    if let Ok(start_guard) = task.start_time.lock() {
        if let Some(start_time) = *start_guard {
            let duration = start_time.elapsed();
            let duration_ms = duration.as_millis() as u64;
            task.duration_ms.store(duration_ms, Ordering::Relaxed);
        }
    }

    let network_bytes = task.received_bytes.load(Ordering::Relaxed);
    if network_bytes > 0 {
        let duration_ms = task.duration_ms.load(Ordering::Relaxed);
        if let Err(e) = crate::mirror::append_download_log(
            resolved_url,
            task.chunk_offset.load(Ordering::Relaxed),
            network_bytes,
            duration_ms,
            true,
        ) {
            log::warn!("Failed to log download completion: {}", e);
        }
    }
}

/// Setup file size and progress tracking for the task
pub(crate) fn setup_task_progress_tracking(
    task: &DownloadTask,
    response: &http::Response<ureq::Body>,
    existing_bytes: u64,
) -> Result<()> {
    if task.is_master_task() {
        task.save_remote_metadata()?;

        // Setup file size and progress tracking for master tasks
        if task.file_size.load(Ordering::Relaxed) == 0 {
            if let Some(remote_size) = get_remote_size(task, response) {
                task.file_size.store(remote_size, Ordering::Relaxed);
                task.chunk_size.store(remote_size, Ordering::Relaxed);
                log::debug!("Remote size determined: {} for {}", remote_size, task.chunk_path.display());
            }
        }

        if task.attempt_number.load(Ordering::SeqCst) == 0 {
            let file_size_val = task.file_size.load(Ordering::Relaxed);
            if file_size_val > 0 {
                task.set_length(file_size_val);
            }
            task.set_position(existing_bytes);
        }
    }

    // Set start time for estimation
    if let Ok(mut start_time) = task.start_time.lock() {
        if start_time.is_none() {
            *start_time = Some(std::time::Instant::now());
        } else {
            // This could happen in retries
            log::debug!("Clearing start_time for chunk {} at offset {}: {:?} (chunk_path={})",
                task.chunk_path.display(), task.chunk_offset.load(Ordering::Relaxed), start_time, task.chunk_path.display());
            task.received_bytes.store(0, Ordering::Relaxed);
            task.duration_ms.store(0, Ordering::Relaxed);
        }
    }

    Ok(())
}
