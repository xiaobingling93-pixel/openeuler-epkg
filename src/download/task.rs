// ============================================================================
// DOWNLOAD TASK - Individual Download Task Management
//
// This module provides functionality for managing individual download tasks,
// including task status updates, client management, and task lifecycle
// operations. It handles the core logic for executing and monitoring single
// download operations within the broader download system.
//
// Key Features:
// - Download task status management and updates
// - HTTP client creation and configuration
// - Task retry logic and error handling
// - Mirror selection and URL resolution
// - File type classification and task configuration
// ============================================================================

use color_eyre::eyre::{eyre, Result, WrapErr};
use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}, mpsc::SyncSender as Sender};
use std::path::PathBuf;
use std::time::Duration;
use ureq::Agent;
use crate::config;
use crate::lfs;
use crate::mirror;
use crate::utils;
use super::types::*;
use super::validation::classify_file_type;

fn should_skip_chunking_at_start(url: &str) -> bool {
    // URLs with $mirror should keep mirror-based retry/chunking behavior.
    if url.contains("$mirror") {
        return false;
    }

    // Some known servers often have limited/unstable Range behavior.
    const KNOWN_NO_RANGE_SITES: [&str; 2] = ["gitee.com", "atomgit.com"];
    let site = mirror::url2site(url).to_lowercase();

    KNOWN_NO_RANGE_SITES.iter().any(|known| site.contains(known))
}

// Macro to add line number to error context
macro_rules! error_context {
    ($msg:expr) => {
        format!("{} (line: {})", $msg, line!())
    };
}

/// Helper function to update a download task's status
///
/// This function handles the common pattern of updating a task's status
/// while properly handling mutex locks and error reporting.
pub(crate) fn update_download_status(task: &DownloadTask, new_status: DownloadStatus) -> Result<()> {
    let mut status = task.status.lock()
        .map_err(|e| eyre!("Failed to lock download status mutex: {}", e))?;

    // Check if status already equals new_status and show a warning
    if *status == new_status {
        log::warn!("Attempting to set download status to same value: {:?} for {}", new_status, task.url);
        return Err(eyre!("Attempting to set download status to same value: {:?} for {}", new_status, task.url));
    }

    *status = new_status;
    Ok(())
}

impl DownloadTask {
    pub fn new(url: String) -> Result<Self> {
        Self::with_size(url, None, "".to_string(), DownloadFlags::empty(), None, None)
    }

    pub fn with_size(
        url: String,
        file_size: Option<u64>,
        repodata_name: String,
        flags: DownloadFlags,
        sha256sum: Option<String>,
        sha1sum: Option<String>,
    ) -> Result<Self> {
        let mut flags = flags;
        // Use detect_url_proto_path to determine if this is a local file
        let (protocol, final_path) = mirror::Mirrors::detect_url_proto_path(&url, &repodata_name)?;

        // Set LOCAL flag if the protocol is Local
        // Note: in with_size() we already set task.final_path pointing to the original local file url
        // (detect_url_proto_path/local_url_to_path's behavior), so can avoid the extra copy
        if protocol == mirror::UrlProtocol::Local {
            flags = flags | DownloadFlags::LOCAL;
        }

        Ok(Self::with_path(url, final_path, file_size, repodata_name, flags, sha256sum, sha1sum))
    }

    fn with_path(
        url: String,
        final_path: PathBuf,
        file_size: Option<u64>,
        repodata_name: String,
        flags: DownloadFlags,
        sha256sum: Option<String>,
        sha1sum: Option<String>,
    ) -> Self {
        let max_retries = config().common.nr_retry;
        let skip_chunking = should_skip_chunking_at_start(&url);
        // Initialize chunk_path to the standard .part file for master tasks
        let chunk_path = utils::append_suffix(&final_path, "part");

        // Classify file type for integrity and metadata handling
        let file_type = classify_file_type(&final_path, file_size);

        Self {
            url:               url.clone(),
            resolved_url:      Mutex::new(url),             // Initialize resolved_url with the original url
            output_dir:        PathBuf::new(), // Not used in with_path
            max_retries,
            client:            Arc::new(Mutex::new(None)),  // Initialize with no client
            data_channels:     Arc::new(Mutex::new(Vec::new())),
            status:            Arc::new(Mutex::new(DownloadStatus::Pending)),
            final_path,
            file_size:         AtomicU64::new(file_size.unwrap_or(0)),
            attempt_number:    AtomicUsize::new(0),         // Initialize to 0 (first attempt)
            file_type,                                      // File type for integrity and metadata handling
            flags:             flags,
            serving_metadata:  Mutex::new(None),            // Will store metadata in HTTP response
            servers_metadata:  Mutex::new(Vec::new()),
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
            throughput_bps:    AtomicU64::new(0),
            eta:               AtomicU64::new(0),
            duration_ms:       AtomicU64::new(0),
            range_request:     Mutex::new(RangeRequest::None),
            mirror_inuse:      Arc::new(Mutex::new(None)),
            repodata_name:     repodata_name,
            sha256sum,
            sha1sum,
            skip_chunking:     AtomicBool::new(skip_chunking),
        }
    }

    pub fn with_data_channel(self, channel: Sender<Vec<u8>>) -> Self {
        if let Ok(mut channels) = self.data_channels.lock() {
            channels.push(channel);
        }
        self
    }

    pub(crate) fn get_status(&self) -> DownloadStatus {
        self.status.lock()
            .unwrap_or_else(|e| panic!("Failed to lock download status mutex: {}", e))
            .clone()
    }

    /// Check if this is a master task (has chunk tasks)
    pub(crate) fn is_master_task(&self) -> bool {
        self.chunk_path.to_string_lossy().ends_with(".part")
    }

    /// Check if this is a chunk task (has non-zero offset or is explicitly a chunk)
    pub(crate) fn is_chunk_task(&self) -> bool {
        self.chunk_path.to_string_lossy().contains(".part-O")
    }

    /// Get the current chunk status
    pub(crate) fn get_chunk_status(&self) -> ChunkStatus {
        self.chunk_status.lock()
            .unwrap_or_else(|e| panic!("Failed to lock chunk status mutex: {}", e))
            .clone()
    }

    /// Set the chunk status
    pub(crate) fn set_chunk_status(&self, status: ChunkStatus) -> Result<()> {
        let mut chunk_status = self.chunk_status.lock()
            .map_err(|e| eyre!("Failed to lock chunk status mutex: {}", e))?;
        *chunk_status = status;
        Ok(())
    }

    /// Get the resolved URL, falling back to the original URL if resolution failed
    pub(crate) fn get_resolved_url(&self) -> String {
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
    pub(crate) fn get_client(&self) -> Result<Agent> {
        let mut client_guard = self.client.lock()
            .map_err(|e| eyre!("Failed to lock client mutex: {}", e))?;

        if client_guard.is_none() {
            // Create client with proxy configuration from config
            let mut config_builder = Agent::config_builder()
                .user_agent("curl/8.13.0")
                // Use more conservative network timeouts to avoid premature failures on slow mirrors
                .timeout_connect(Some(Duration::from_secs(15)))  // was 5s
                .timeout_recv_response(Some(Duration::from_secs(60)));  // was 9s

            let proxy_config = &config().common.proxy;
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
    pub(crate) fn create_chunk_task(&self, offset: u64, size: u64) -> Arc<DownloadTask> {
        // Create a chunk task with a specific offset and size
        // The chunk file will be named <final_path>.part-O{offset} to avoid nested "-O" components
        let chunk_path = format!("{}.part-O{}", self.final_path.to_string_lossy(), offset);

        Arc::new(DownloadTask {
            url:                  self.url.clone(),
            // Chunks should start with empty resolved_url so they can select their own mirror
            // This enables parallel downloads from multiple mirrors for better performance
            resolved_url:         Mutex::new(String::new()),  // Empty = will use self.url in get_resolved_url()
            output_dir:           self.output_dir.clone(),
            max_retries:          self.max_retries,
            client:               Arc::new(Mutex::new(None)),           // Initialize with no client
            data_channels:        Arc::new(Mutex::new(Vec::new())),     // Chunks don't need data channels
            status:               Arc::new(Mutex::new(DownloadStatus::Pending)),
            final_path:           self.final_path.clone(),
            file_size:            AtomicU64::new(self.file_size.load(Ordering::Relaxed)),
            attempt_number:       AtomicUsize::new(0),                  // Initialize to 0 (first attempt)
            mirror_inuse:         Arc::new(Mutex::new(None)),           // No mirror selected yet for chunk tasks
            file_type:            self.file_type.clone(),               // Copy file type classification
            flags:                self.flags,
            // Chunks should start with None serving_metadata so they select their own mirror
            // Metadata validation will ensure ETag/timestamp consistency across mirrors
            serving_metadata:     Mutex::new(None),  // Will be set when chunk downloads and validates against master
            servers_metadata:     Mutex::new(Vec::new()),
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
            throughput_bps:       AtomicU64::new(0),
            eta:                  AtomicU64::new(0),
            duration_ms:          AtomicU64::new(0),
            range_request:        Mutex::new(RangeRequest::None),
            repodata_name:        self.repodata_name.clone(),
            sha256sum:            self.sha256sum.clone(),
            sha1sum:              self.sha1sum.clone(),
            // Chunk tasks inherit skip_chunking from parent, though they won't use it
            skip_chunking:        AtomicBool::new(self.skip_chunking.load(Ordering::Relaxed)),
        })
    }

    pub(crate) fn progress(&self) -> u64 {
        let received = self.received_bytes.load(Ordering::Relaxed);
        let reused = self.resumed_bytes.load(Ordering::Relaxed);
        received + reused
    }

    pub(crate) fn remaining(&self) -> u64 {
        let chunk_size = self.chunk_size.load(Ordering::Relaxed);
        if chunk_size == 0 {
            log::warn!("chunk_size=0 for task {:?}", self);
        }

        chunk_size.saturating_sub(self.progress())
    }

    /// Get total progress bytes across all chunks (reused + network bytes)
    /// This represents the total download progress for display purposes
    pub(crate) fn get_total_progress_bytes(&self) -> (u64, u64, usize) {
        let mut total_received = 0u64;
        let mut total_reused = 0u64;
        let mut downloading_chunks = 0;

        // Use iterate_task_levels for this single task to get all L2 and L3 chunks
        self.iterate_task_levels(|task, _level| {
            total_received += task.received_bytes.load(Ordering::Relaxed);
            total_reused += task.resumed_bytes.load(Ordering::Relaxed);

            // Count chunks with Downloading status
            if let Ok(status) = task.status.lock() {
                if *status == DownloadStatus::Downloading {
                    downloading_chunks += 1;
                }
            }
        });

        (total_received, total_reused, downloading_chunks)
    }

    /// Helper function to iterate through all levels of this task (L1, L2, L3)
    /// Similar to DownloadManager::iterate_3level_tasks but for a single task
    pub(crate) fn iterate_task_levels<F>(&self, mut callback: F)
    where F: FnMut(&DownloadTask, usize) // (task, level)
    {
        // Level 1: This task itself (master task)
        callback(self, 1);

        // Level 2: L2 tasks (direct children of this task)
        if let Ok(chunks) = self.chunk_tasks.lock() {
            for l2_task in chunks.iter() {
                callback(l2_task, 2);

                // Level 3: L3 tasks (children of L2 tasks)
                if let Ok(l3_chunks) = l2_task.chunk_tasks.lock() {
                    for l3_task in l3_chunks.iter() {
                        callback(l3_task, 3);
                    }
                }
            }
        }
    }

    /// Clean helper to get data channel without repeated error handling
    pub(crate) fn get_data_channel(&self) -> Option<Sender<Vec<u8>>> {
        self.data_channels.lock().ok().and_then(|channels| channels.first().cloned())
    }

    /// Add a data channel for duplicate downloads
    pub(crate) fn add_data_channel(&self, channel: Sender<Vec<u8>>) {
        if let Ok(mut channels) = self.data_channels.lock() {
            channels.push(channel);
        }
    }

    /// Get all data channels for broadcasting
    pub(crate) fn get_all_data_channels(&self) -> Vec<Sender<Vec<u8>>> {
        self.data_channels.lock().ok().map(|channels| channels.clone()).unwrap_or_default()
    }

    /// Clean helper to take data channels (for closing)
    pub(crate) fn take_data_channels(&self) -> Vec<Sender<Vec<u8>>> {
        self.data_channels.lock().ok().map(|mut channels| {
            let result = channels.clone();
            channels.clear();
            result
        }).unwrap_or_default()
    }

    /// Set progress bar length
    pub(crate) fn set_length(&self, length: u64) {
        log::debug!("pb.set_length [{}]: {}", self.url, length);
        if let Ok(pb_guard) = self.progress_bar.lock() {
            if let Some(ref pb) = *pb_guard {
                pb.set_length(length);
            }
        }
    }

    /// Set progress bar position
    pub(crate) fn set_position(&self, position: u64) {
        log::debug!("pb.set_position [{}]: {}", self.url, position);
        if let Ok(pb_guard) = self.progress_bar.lock() {
            if let Some(ref pb) = *pb_guard {
                pb.set_position(position);
            }
        }
    }

    /// Set progress bar message
    pub(crate) fn set_message(&self, message: String) {
        log::debug!("pb.set_message [{}]: {}", self.url, message);
        if let Ok(pb_guard) = self.progress_bar.lock() {
            if let Some(ref pb) = *pb_guard {
                pb.set_message(message);
            }
        }
    }

    /// Finish progress bar with message
    pub(crate) fn finish_with_message(&self, message: String) {
        log::debug!("pb.finish_with_message [{}]: {}", self.url, message);
        if let Ok(pb_guard) = self.progress_bar.lock() {
            if let Some(ref pb) = *pb_guard {
                pb.finish_with_message(message);
            }
        }
    }
}

impl DownloadTask {
    /// Returns the path to the .etag.json file, which is based on the final file path.
    pub(crate) fn meta_json_path(&self) -> PathBuf {
        utils::append_suffix(&self.final_path, "etag.json")
    }

    /// Saves download metadata to .etag.json file
    pub(crate) fn save_remote_metadata(&self) -> Result<()> {
        if !self.is_master_task() ||
            self.file_type == FileType::Immutable {
            return Ok(());
        }

        let meta_path = self.meta_json_path();

        // If metadata file is a symlink, delete it so we can write our own metadata
        if lfs::is_symlink(&meta_path) {
            log::debug!("Metadata file {} is a symlink, removing it", meta_path.display());
            std::fs::remove_file(&meta_path)
                .with_context(|| format!("Failed to remove symlink {}", meta_path.display()))?;
        }

        let serving_metadata = if let Ok(guard) = self.serving_metadata.lock() {
            guard.clone()
        } else {
            None
        };

        let servers_metadata = if let Ok(guard) = self.servers_metadata.lock() {
            guard.clone()
        } else {
            Vec::new()
        };

        // Only save if we have some metadata
        if serving_metadata.is_none() && servers_metadata.is_empty() {
            return Ok(());
        }

        let metadata = DownloadMetadata {
            serving_metadata,
            servers_metadata,
        };

        let json_content = serde_json::to_string_pretty(&metadata)
            .with_context(|| "Failed to serialize DownloadMetadata to JSON")?;

        std::fs::write(&meta_path, json_content)
            .with_context(|| error_context!(format!("save_remote_metadata failed for meta_path: {}", meta_path.display())))?;

        log::debug!("Saved metadata to {}", meta_path.display());
        Ok(())
    }

    /// Loads download metadata from .etag.json file
    pub(crate) fn load_remote_metadata(&self) -> Result<Option<DownloadMetadata>> {
        if !self.is_master_task() ||
            self.file_type == FileType::Immutable {
            return Ok(None);
        }

        let meta_path = self.meta_json_path();

        if !meta_path.exists() {
            return Ok(None);
        }

        match crate::io::read_json_file::<DownloadMetadata>(&meta_path) {
            Ok(metadata) => {
                log::debug!("Loaded metadata from {}", meta_path.display());
                Ok(Some(metadata))
            }
            Err(e) => {
                log::warn!("Failed to parse .etag.json file {}: {}", meta_path.display(), e);
                Ok(None)
            }
        }
    }

    /// Setup the range_request member based on task state
    pub(crate) fn setup_download_range(&self) {
        let range_request = if self.is_chunk_task() {
            RangeRequest::Chunk
        } else if self.chunk_size.load(Ordering::Relaxed) != self.file_size.load(Ordering::Relaxed) {
            // Master task with chunking
            RangeRequest::Chunk
        } else if self.resumed_bytes.load(Ordering::Relaxed) > 0 {
            // Master task resuming from partial file
            RangeRequest::Resume
        } else {
            // Master task without chunking or resume
            RangeRequest::None
        };
        if let Ok(mut guard) = self.range_request.lock() {
            *guard = range_request;
        }
    }

    /// Get the current range request type
    pub(crate) fn get_range_request(&self) -> RangeRequest {
        match self.range_request.lock() {
            Ok(guard) => (*guard).clone(),
            Err(_) => RangeRequest::None,
        }
    }
}

