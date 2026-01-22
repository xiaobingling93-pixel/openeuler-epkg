// ============================================================================
// DOWNLOAD TYPES - Core Data Structures and Constants
//
// This module defines the fundamental data structures, types, and constants used
// throughout the download system. It provides the building blocks for download
// tasks, status tracking, file integrity validation, and system configuration.
//
// Key Components:
// - DownloadTask: Core structure representing an individual download operation
// - DownloadStatus: Enumeration of possible download states
// - DownloadError: Error types specific to download operations
// - FileType: Classification of file types for integrity handling
// - ServerMetadata: HTTP response metadata for consistency validation
// - ChunkInfo: Information about download chunks for parallel processing
// - Various constants for chunking, threading, and timing configurations
// ============================================================================

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize},
        Arc, Mutex,
    },
};

use serde::{Deserialize, Serialize};

/// Download task flags for various characteristics
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadFlags(u8);

impl DownloadFlags {
    pub const ADB: DownloadFlags = DownloadFlags(1 << 0);  // Alpine/Arch Database file
    pub const LOCAL: DownloadFlags = DownloadFlags(1 << 1); // Local file (no download needed)

    pub const fn empty() -> Self {
        DownloadFlags(0)
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }
}

impl std::ops::BitOr for DownloadFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        DownloadFlags(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for DownloadFlags {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        DownloadFlags(self.0 & rhs.0)
    }
}

/// Constants for chunking configuration
// Using power of 2 for efficient bit operations
pub const PGET_CHUNK_SIZE: u64 = 1 << 20;                       // 1MB chunks
pub const PGET_CHUNK_MASK: u64 = PGET_CHUNK_SIZE - 1;           // Mask for modulo operations
pub const MIN_FILE_SIZE_FOR_CHUNKING: u64 = 3 * 1024 * 1024;    // 3MB

pub const ONDEMAND_CHUNK_SIZE: u64 = 256 * 1024;                // 256KB chunks
pub const ONDEMAND_CHUNK_SIZE_MASK: u64 = ONDEMAND_CHUNK_SIZE - 1;

// Chunking threshold constants
pub const CHUNK_MERGE_THRESHOLD: u64 = PGET_CHUNK_SIZE / 8;     // Threshold for merging small chunks

// Threading and scheduling constants
pub const MAX_CHUNK_THREADS_MULTIPLIER: usize = 8;              // Maximum chunk threads as multiple of parallel downloads
pub const CHUNK_PARALLEL_MULTIPLIER: usize = 2;                 // Thread spawn multiplier for parallel chunk tasks
pub const WAIT_TASK_DURATION_MS: u64 = 100;                     // Wait for task and thread coordination
pub const CHUNK_SLEEP_DURATION_MS: u64 = 500;                   // Chunk task wait for merge and error recovery

// ETA and timing constants
pub const MIN_ETA_THRESHOLD_SECONDS: u64 = 5;                   // Minimum ETA threshold for ondemand chunking
pub const TIMESTAMP_TOLERANCE_SECONDS: u64 = 600;               // 10 minutes tolerance for timestamp comparison
pub const PROGRESS_UPDATE_INTERVAL_MS: u64 = 500;               // Progress update interval

// Display and logging constants
pub const MAX_DISPLAY_STATS: usize = 30;                        // Maximum items to display in logs
pub const PROGRESS_BAR_WIDTH: usize = 10;                       // Progress bar width in characters

// HTTP status code constants
pub const HTTP_CLIENT_ERROR_START: u16 = 400;                   // Start of 4xx client errors
pub const HTTP_SERVER_ERROR_START: u16 = 500;                   // Start of 5xx server errors

#[derive(Debug)]
pub struct DownloadTask {
    pub url:                  String,
    pub resolved_url:         Mutex<String>,
    #[allow(dead_code)]
    pub output_dir:           PathBuf,
    pub max_retries:          usize,
    pub client:               Arc<Mutex<Option<ureq::Agent>>>,                  // HTTP client created on-demand
    pub data_channels:        Arc<Mutex<Vec<std::sync::mpsc::SyncSender<Vec<u8>>>>>,           // Support multiple data channels for deduplication
                                                                                              // to avoid blocking the consumer side
    pub status:               Arc<Mutex<DownloadStatus>>,
    pub final_path:           PathBuf,                                    // Store the final download path
    pub file_size:            AtomicU64,                                  // Expected file size for prioritization and verification (0 = unknown)
    pub attempt_number:       AtomicUsize,                                // Track which attempt number this is (0 = first attempt)

    // Mirror usage tracking - stores the selected mirror for this task
    pub mirror_inuse:         Arc<Mutex<Option<crate::mirror::Mirror>>>,  // Selected mirror for usage tracking

    // File type classification for integrity and metadata handling
    pub file_type:            FileType,

    // Server metadata from response headers, stored for later application
    pub serving_metadata:     Mutex<Option<ServerMetadata>>,
    pub servers_metadata:     Mutex<Vec<ServerMetadata>>,                 // metadata responses from different mirrors

    // Repository information for mirror selection
    pub repodata_name:        String,                                     // Repository name for mirror selection
    pub flags:                DownloadFlags,                              // Download task flags (ADB, LOCAL, etc.)

    // Chunking semantics rules:
    // 1. chunk_offset - decided on initial allocation, won't change over time; 0 for master task
    // 2. chunk_size - decided on initial allocation, won't change over time but may be lowered on ondemand chunking; equals file_size for master task without chunking
    // 3. append_offset (used in functions) = chunk_offset + resumed_bytes, advances during network downloading
    // 4. On success: resumed_bytes + received_bytes == chunk_size
    // 5. When create_ondemand_chunks() creates new chunks, master task.chunk_size will be reduced to a lower boundary,
    //    and process_chunk_download_stream() will use the latest chunk_size as boundary
    pub chunk_tasks:          Arc<Mutex<Vec<Arc<DownloadTask>>>>,
    pub chunk_path:           PathBuf,                                    // Full path to the chunk file (for master: .part, for chunks: .part-O{offset})
    pub chunk_offset:         AtomicU64,                                  // Starting byte offset for this chunk (0 for master task, fixed on allocation)
    pub chunk_size:           AtomicU64,                                  // Size of this chunk in bytes (fixed on allocation, equals file_size for master without chunking)

    pub start_time:           Mutex<Option<std::time::Instant>>,          // Set after got HTTP response and won't skip download
    pub duration_ms:          AtomicU64,                                  // Total download duration in milliseconds, set on Completed and reset on setting start_time

    pub resumed_bytes:        AtomicU64,                                  // Bytes reused from local partial files
    pub received_bytes:       AtomicU64,                                  // Bytes actually received from network

    // ETA calculation atomic fields
    pub throughput_bps:       AtomicU64,                                  // Current throughput in bytes per second
    pub eta:                  AtomicU64,                                  // Estimated time to completion in seconds

    // Ensure we only stream the pre-existing local file once per overall download attempt
    pub has_sent_existing:    AtomicBool,

    // Progress bar for this download task
    pub progress_bar:         Mutex<Option<indicatif::ProgressBar>>,                 // will never change

    // Chunk status for reliable state management - avoids race conditions in chunking decisions
    pub chunk_status:         Arc<Mutex<ChunkStatus>>,

    // Range request type for this download task
    pub range_request:        Mutex<RangeRequest>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DownloadStatus {
    Pending,
    Downloading,
    Completed,
    Failed(String),
}

/// Chunk status enumeration for reliable chunking state management
///
/// This enum tracks the chunking lifecycle to avoid race conditions:
/// - NoChunk: Task has no chunks and is not being considered for chunking
/// - NeedOndemandChunk: Task has been selected by the global scheduler for ondemand chunking
/// - HasOndemandChunk: Task has ondemand chunks created (chunk_tasks not empty, created during download)
/// - HasBeforehandChunk: Task has beforehand chunks created (chunk_tasks not empty, created before download)
///
/// The latter two values imply that task.chunk_tasks is not empty
#[derive(Debug, Clone, PartialEq)]
pub enum ChunkStatus {
    NoChunk,
    NeedOndemandChunk,
    HasOndemandChunk,
    HasBeforehandChunk,
}

// =======================================
// Data Integrity System - Data Structures
// =======================================

/// File type classification for appropriate integrity handling
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FileType {
    // An Immutable file means the remote file either remains static;
    // An AppendOnly file will only be appended to over time.
    // This implies that:
    // - Any locally downloaded data is always valid as a prefix.
    // - If local_size == remote_size, the file is complete.
    // - If local_size < remote_size, a partial (range) download can complete it.
    // - If local_size > remote_size, the local file is considered corrupt and must be re-downloaded.
    Immutable,    // .deb, .rpm, .apk, by-hash files
    Mutable,      // Release, repomd.xml, APKINDEX.tar.gz
    AppendOnly,   // Future extension
}

/// Result of existing file validation
#[derive(Debug)]
pub enum ValidationResult {
    SkipDownload(String),
    ResumeFromPartial,
    StartFresh,
    CorruptionDetected,
}

/// Server metadata for consistency validation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerMetadata {
    pub url: String,                                                      // The resolved URL this metadata came from
    pub remote_size: Option<u64>,
    pub last_modified: Option<String>,
    pub timestamp: u64,  // parsed from last_modified
    pub etag: Option<String>,
}

impl ServerMetadata {
    pub(crate) fn matches_with(&self, other: &Self) -> bool {
        // If etag matches, then result is matches
        if self.etag.is_some() && other.etag.is_some() && self.etag == other.etag {
            return true;
        }

        // remote_size can only be matches if both are some not none
        let remote_size_matches = if self.remote_size.is_some() && other.remote_size.is_some() {
            self.remote_size == other.remote_size
        } else {
            true // If either is None, consider it a match
        };

        // for timestamp, result is match if time_diff <= Duration::from_secs(600)
        let timestamp_matches = if self.timestamp > 0 && other.timestamp > 0 {
            let time_diff = if self.timestamp > other.timestamp {
                self.timestamp - other.timestamp
            } else {
                other.timestamp - self.timestamp
            };
            time_diff <= TIMESTAMP_TOLERANCE_SECONDS // 600 seconds = 10 minutes
        } else {
            true // If either timestamp is 0, consider it a match
        };

        remote_size_matches && timestamp_matches
    }
}

/// Range request type for download tasks
#[derive(Debug, Clone, PartialEq)]
pub enum RangeRequest {
    /// No range request needed (full file download)
    None,
    /// Resume from partial file (Range: bytes=X-)
    Resume,
    /// Chunk download (Range: bytes=X-Y)
    Chunk,
}

/// Download metadata saved to .etag.json files
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadMetadata {
    pub serving_metadata: Option<ServerMetadata>,  // Metadata from the mirror that served the download
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers_metadata: Vec<ServerMetadata>,     // All metadata responses from different mirrors (for debugging)
}

/// Specific error types for download operations
#[derive(Debug, Clone)]
pub enum DownloadError {
    /// Fatal HTTP errors (4xx) that shouldn't be retried
    Fatal { code: u16, message: String },
    /// The server responded with HTTP 429 Too Many Requests
    TooManyRequests,
    /// Network connectivity or timeout issues
    Network { details: String },
    /// Content validation failed (size mismatch, corrupted data, etc.)
    #[allow(dead_code)]
    ContentValidation { expected: String, actual: String },
    /// Mirror selection or resolution failed
    MirrorResolution { details: String },
    /// Server returned unexpected response
    UnexpectedResponse { code: u16, details: String },
    /// Chunk was already complete and was skipped
    AlreadyComplete,
    /// Disk/IO errors that should not mark the mirror as bad
    DiskError { details: String },
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::Fatal { code, message } => write!(f, "Fatal error (HTTP {}): {}", code, message),
            DownloadError::TooManyRequests => write!(f, "Too many requests (HTTP 429)"),
            DownloadError::Network { details } => write!(f, "Network error: {}", details),

            DownloadError::ContentValidation { expected, actual } => write!(f, "Content validation failed: expected {}, got {}", expected, actual),
            DownloadError::MirrorResolution { details } => write!(f, "Mirror resolution failed: {}", details),
            DownloadError::UnexpectedResponse { code, details } => {
                write!(f, "Unexpected HTTP response {}: {}", code, details)
            },
            DownloadError::AlreadyComplete => {
                write!(f, "Chunk already complete")
            },
            DownloadError::DiskError { details } => {
                write!(f, "Disk/IO error: {}", details)
            },
        }
    }
}

impl std::error::Error for DownloadError {}

/// Result type for processing operations to indicate whether to continue or complete
#[derive(Debug, PartialEq)]
pub enum ProcessingResult {
    Continue,
    AllCompleted,
}

/// ETA calculation results for a single task
#[allow(dead_code)]
pub enum CacheDecision {
    UseCache { reason: String },
    AppendDownload { reason: String },
    RedownloadDueTo { reason: String },
}

/// Download manager statistics for progress tracking and ETA calculation
#[derive(Debug, Clone, Default)]
pub struct DownloadManagerStats {
    pub global_ideal_eta: u64,          // Global ideal ETA in seconds
    pub slowest_task_eta: u64,          // Slowest task ETA in seconds
    pub fastest_task_eta: u64,          // Fastest task ETA in seconds
    pub total_remaining_bytes: u64,     // Total bytes remaining across all downloads
    pub total_rate_bps: u64,            // Total download rate in bytes per second
    pub active_tasks: usize,            // Number of actively downloading tasks
    pub pending_tasks: usize,           // Number of pending tasks
    pub complete_tasks: usize,          // Number of completed tasks
    pub master_tasks: usize,            // Number of master tasks
    pub l2_chunk_tasks: usize,          // Number of L2 chunk tasks
    pub l3_chunk_tasks: usize,          // Number of L3 chunk tasks
}

/// Chunk information for split download areas
#[derive(Debug, Clone)]
pub struct ChunkInfo {
    pub offset: u64,
    pub size: u64,      // Total chunk size (from offset to end of chunk)
    pub filesize: u64,  // Existing file size (bytes already downloaded)
}
