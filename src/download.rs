use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write, Seek},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, AtomicU64, Ordering},
        Arc, Mutex,
        LazyLock,
        mpsc::Sender,
    },
    thread,
    time::{SystemTime, UNIX_EPOCH, Duration},
    collections::HashMap,
};

use serde::{Deserialize, Serialize};
use color_eyre::eyre::{eyre, Result, WrapErr};

// Macro to add line number to error context
macro_rules! error_context {
    ($msg:expr) => {
        format!("{} (line: {})", $msg, line!())
    };
}
use time::{OffsetDateTime, format_description::well_known::Rfc2822};
use indicatif::{ProgressBar, MultiProgress, ProgressStyle};

// =====================================================================================
// NOTICE: DO NOT change `http::Response<ureq::Body>` to `ureq::Response` or
//           `response.headers().get(...)` to `response.header(...)` in this file!
//
// ureq's public API returns `http::Response<ureq::Body>` and uses `.headers().get()`.
// Changing these will break the build. This is the correct, working usage for ureq.
//
// Also, we use `std::sync::mpsc` for channels throughout the codebase instead of
// `crossbeam_channel` for consistency and to avoid extra dependencies.
// =====================================================================================


use ureq::Agent;
use ureq::http;

use crate::dirs;
use crate::models::*;
use crate::mirror;
use crate::utils;

/// Constants for chunking configuration
// Using power of 2 for efficient bit operations
const PGET_CHUNK_SIZE: u64 = 1 << 20;                       // 1MB chunks
const PGET_CHUNK_MASK: u64 = PGET_CHUNK_SIZE - 1;           // Mask for modulo operations
const MIN_FILE_SIZE_FOR_CHUNKING: u64 = 3 * 1024 * 1024;    // 3MB

const ONDEMAND_CHUNK_SIZE: u64 = 256 * 1024;                // 256KB chunks
const ONDEMAND_CHUNK_SIZE_MASK: u64 = ONDEMAND_CHUNK_SIZE - 1;

// Chunking threshold constants
const CHUNK_MERGE_THRESHOLD: u64 = PGET_CHUNK_SIZE / 8;     // Threshold for merging small chunks

// Threading and scheduling constants
const MAX_CHUNK_THREADS_MULTIPLIER: usize = 8;              // Maximum chunk threads as multiple of parallel downloads
const CHUNK_PARALLEL_MULTIPLIER: usize = 2;                 // Thread spawn multiplier for parallel chunk tasks
const WAIT_TASK_DURATION_MS: u64 = 100;                     // Wait for task and thread coordination
const CHUNK_SLEEP_DURATION_MS: u64 = 500;                   // Chunk task wait for merge and error recovery

// ETA and timing constants
const MIN_ETA_THRESHOLD_SECONDS: u64 = 5;                   // Minimum ETA threshold for ondemand chunking
const TIMESTAMP_TOLERANCE_SECONDS: u64 = 600;               // 10 minutes tolerance for timestamp comparison
const PROGRESS_UPDATE_INTERVAL_MS: u64 = 500;               // Progress update interval

// Display and logging constants
const MAX_DISPLAY_STATS: usize = 30;                        // Maximum items to display in logs
const PROGRESS_BAR_WIDTH: usize = 10;                       // Progress bar width in characters

// HTTP status code constants
const HTTP_CLIENT_ERROR_START: u16 = 400;                   // Start of 4xx client errors
const HTTP_SERVER_ERROR_START: u16 = 500;                   // Start of 5xx server errors

impl DownloadTask {
    /// Returns the path to the .pget-status file, which is based on the final file path.
    pub fn pget_status_path(&self) -> PathBuf {
        utils::append_suffix(&self.final_path, "pget-status")
    }

    /// Returns the path to the ETag file, which is based on the final file path.
    pub fn etag_path(&self) -> PathBuf {
        utils::append_suffix(&self.final_path, "etag")
    }

    /// Saves the ETag to a file named after the download's final path with a .etag extension.
    pub fn save_etag(&self, etag: &str) -> Result<()> {
        let etag_path = self.etag_path();
        std::fs::write(&etag_path, etag)
            .with_context(|| error_context!(format!("save_etag failed for etag_path: {}", etag_path.display())))?;
        log::debug!("Saved ETag '{}' to {}", etag, etag_path.display());
        Ok(())
    }

    /// Loads an ETag from a sidecar file.
    ///
    /// Checks for an ETag file next to the final path.
    /// Returns the stored ETag if a sidecar file exists and is readable.
    pub fn load_etag(&self) -> Option<String> {
        let etag_path = self.etag_path();
        if let Ok(etag) = std::fs::read_to_string(&etag_path) {
            let trimmed_etag = etag.trim().to_string();
            if !trimmed_etag.is_empty() {
                log::debug!("Loaded ETag '{}' from {}", trimmed_etag, etag_path.display());
                return Some(trimmed_etag);
            }
        }
        None
    }

    /// Setup the range_request member based on task state
    pub fn setup_download_range(&self) {
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
    pub fn get_range_request(&self) -> RangeRequest {
        match self.range_request.lock() {
            Ok(guard) => (*guard).clone(),
            Err(_) => RangeRequest::None,
        }
    }
}

// ============================================================================
// DOWNLOAD TASK CHUNKING ARCHITECTURE DOCUMENTATION
// ============================================================================
/*
 * ╔══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════╗
 * ║                                                                                                                                          ║
 * ║                                                CHUNKED DOWNLOAD SYSTEM ARCHITECTURE                                                      ║
 * ║                                                                                                                                          ║
 * ║  This system implements LFTP-like parallel chunked downloading with master-child task coordination,                                      ║
 * ║  intelligent resumption, and real-time streaming capabilities.                                                                           ║
 * ║                                                                                                                                          ║
 * ╚══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════╝
 *
 *
 * ┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
 * │                                                                                                                                          │
 * │                                                  3-LEVEL TASK HIERARCHY AND RELATIONSHIPS                                                │
 * │                                                                                                                                          │
 * └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 *
 *  ROOT SOURCE: DOWNLOAD_MANAGER.tasks (HashMap<String, Arc<DownloadTask>>)
 *  ═══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════
 *
 *  ┌───────────────────────────────────────────────────────────────────────────────┐
 *  │                         LEVEL 1: MASTER TASKS                                 │
 *  │                    (Stored in DOWNLOAD_MANAGER.tasks)                         │
 *  │                                                                               │
 *  │  ┌─────────────────────┐    ┌─────────────────────┐    ┌─────────────────────┐│
 *  │  │   Master Task A     │    │   Master Task B     │    │   Master Task C     ││
 *  │  │  • chunk_offset: 0  │    │  • chunk_offset: 0  │    │  • chunk_offset: 0  ││
 *  │  │  • chunk_size: 1MB  │    │  • chunk_size: 1MB  │    │  • chunk_size: 1MB  ││
 *  │  │  • file_size: 5MB   │    │  • file_size: 8MB   │    │  • file_size:  3MB  ││
 *  │  │  • file.part        │    │  • file2.part       │    │  • file3.part       ││
 *  │  └─────────────────────┘    └─────────────────────┘    └─────────────────────┘│
 *  └───────────────────────────────────────────────────────────────────────────────┘
 *                        │                            ▼                           ▼
 *                        │ A.chunk_tasks<A1, A2, A3, A4>
 *                        ▼
 *                     ┌───────────────────────────────────────────────────────────────────────────────┐
 *                     │                         LEVEL 2: CHUNK TASKS                                  │
 *                     │                   (Stored in parent_task.chunk_tasks)                         │
 *                     │                                                                               │
 *                     │  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐  ┌───────────┐ │
 *                     │  │  Chunk A1       │  │  Chunk A2       │  │  Chunk A3       │  │  Chunk A4 │ │
 *                     │  │ offset: 1MB     │  │ offset: 2MB     │  │ offset: 3MB     │  │offset:4MB │ │
 *                     │  │ size: 1MB       │  │ size: 1MB       │  │ size: 1MB       │  │size: 1MB  │ │
 *                     │  │ .part-O1048576  │  │ .part-O2097152  │  │ .part-O3145728  │  │.part-O... │ │
 *                     │  │ (beforehand/    │  │ (beforehand/    │  │ (beforehand/    │  │(ondemand) │ │
 *                     │  │  recovery/      │  │  recovery/      │  │  recovery/      │  │           │ │
 *                     │  │  ondemand)      │  │  ondemand)      │  │  ondemand)      │  │           │ │
 *                     │  └─────────────────┘  └─────────────────┘  └─────────────────┘  └───────────┘ │
 *                     └───────────────────────────────────────────────────────────────────────────────┘
 *                                             │ Shrink + Split on OnDemand Chunking
 *                                             ▼
 *                                             ┌─────────────────┐┌─────────────────────────────────────────────────────────┐
 *                                             │ Shrinked A2     ││              LEVEL 3: SUB-CHUNK TASKS                   │
 *                                             │ offset: 2.0MB   ││         (Stored in level2_chunk.chunk_tasks)            │
 *                                             │ size: 256KB     ││               *** ONDEMAND CHUNKS ONLY ***              │
 *                                             │ .part-O2097152  ││                                                         │
 *                                             │ (ondemand only) ││ ┌─────────────────┐  ┌─────────────────┐  ┌───────────┐ │
 *                                             └─────────────────┘│ │ Sub-chunk A2.1  │  │ Sub-chunk A2.2  │  │Sub-chunk  │ │
 *                                                                │ │ offset: 2.25MB  │  │ offset: 2.5MB   │  │A2.3       │ │
 *                                                                │ │ size: 256KB     │  │ size: 256KB     │  │offset:... │ │
 *                                                                │ │ .part-O2359296  │  │ .part-O2621440  │  │size:256KB │ │
 *                                                                │ │ (ondemand only) │  │ (ondemand only) │  │.part-O... │ │
 *                                                                │ └─────────────────┘  └─────────────────┘  └───────────┘ │
 *                                                                └─────────────────────────────────────────────────────────┘
 *
 *
 *  CRITICAL HIERARCHY INVARIANTS:
 *  ═══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════
 *
 *  1. UP-DOWN LEVEL CONTINUITY:
 *     parent_task.chunk_offset + parent_task.chunk_size == parent_task.chunk_tasks[0].chunk_offset
 *
 *     Example: Master Task A (0 → 1MB) connects to Chunk A1 (1MB → 2MB) (w/o ondemand chunking)
 *              Chunk A3 (3MB → 3.25MB) connects to Sub-chunk A3.1 (3.25MB → 3.5MB) (w/ ondemand chunking)
 *
 *  2. SAME LEVEL SIBLING CONTINUITY:
 *     chunk_tasks[i].chunk_offset + chunk_tasks[i].chunk_size == chunk_tasks[i+1].chunk_offset
 *
 *     Example: Chunk A1 (1MB → 2MB) → Chunk A2 (2MB → 3MB) → Chunk A3 (3MB → 3.25MB)
 *              Sub-chunk A3.1 (3.25MB → 3.5MB) → Sub-chunk A3.2 (3.5MB → 3.75MB)
 *
 *  3. NEXT SIBLING BOUNDARY:
 *     parent_task's next sibling chunk_offset == parent_task.chunk_tasks.last().chunk_offset + chunk_size
 *
 *     Example: Chunk A4 starts where Sub-chunk A3.3 ends (4MB)
 *
 *  4. LEVEL-SPECIFIC CHUNK TYPES:
 *     • 2-Level: Can be beforehand, recovery, or ondemand chunks
 *     • 3-Level: ONLY ondemand chunks (created during slow downloads)
 *
 *
 * ┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
 * │                                                                                                                                          │
 * │                                                          FILE LAYOUT AND BYTE RANGES                                                     │
 * │                                                                                                                                          │
 * └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 *
 *  File: example.deb (5MB total)
 *  ═══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════
 *
 *   Byte 0           1MB              2MB              3MB              4MB              5MB
 *    │                │                │                │                │                │
 *    ▼                ▼                ▼                ▼                ▼                ▼
 *    ┌────────────────┬────────────────┬────────────────┬────────────────┬────────────────┐
 *    │  MASTER TASK   │  CHUNK TASK 1  │  CHUNK TASK 2  │  CHUNK TASK 3  │  CHUNK TASK 4  │
 *    │   Range:       │   Range:       │   Range:       │   Range:       │   Range:       │
 *    │   0 → 1MB      │   1MB → 2MB    │   2MB → 3MB    │   3MB → 4MB    │   4MB → 5MB    │
 *    │                │                │                │                │                │
 *    │  File:         │  File:         │  File:         │  File:         │  File:         │
 *    │  example.part  │  example.part- │  example.part- │  example.part- │  example.part- │
 *    │                │  O1048576      │  O2097152      │  O3145728      │  O4194304      │
 *    └────────────────┴────────────────┴────────────────┴────────────────┴────────────────┘
 *
 *
 * ┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
 * │                                                                                                                                          │
 * │                                                         CHUNK CREATION STRATEGIES                                                        │
 * │                                                                                                                                          │
 * └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 *
 *  1. BEFOREHAND CHUNKING (before HTTP request):
 *     ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
 *     • Triggered when task.size is known (>3MB)
 *     • Creates 1MB chunks immediately
 *     • Master handles first chunk (0 → 1MB)
 *     • Additional chunks created for remaining data
 *     • ChunkStatus: HasBeforehandChunk
 *
 *  2. ONDEMAND CHUNKING (during download):
 *     ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
 *     • Triggered when download is slow (>5s remaining)
 *     • Creates 256KB chunks for remaining data
 *     • Master task range is reduced to next boundary
 *     • ChunkStatus: HasOndemandChunk
 *
 *  3. RECOVERY CHUNKING (from partial files):
 *     ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
 *     • Detects existing .part-O{offset} files
 *     • Recreates chunk tasks based on file offsets
 *     • Validates chunk boundaries and integrity
 *     • ChunkStatus: HasBeforehandChunk (recovered)
 *
 *
 * ┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
 * │                                                                                                                                          │
 * │                                                           BYTE OFFSET SEMANTICS                                                          │
 * │                                                                                                                                          │
 * └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 *
 *  CRITICAL INVARIANTS:
 *  ═══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════
 *
 *  1. chunk_offset:        Fixed at allocation, never changes, 0 for master task
 *  2. chunk_size:          Fixed at allocation, may be reduced by ondemand chunking
 *  3. append_offset:       chunk_offset + resumed_bytes, advances during download
 *  4. final_append_offset: chunk_offset + resumed_bytes + received_bytes (end position)
 *  5. Progress equation:   resumed_bytes + received_bytes == chunk_size (on completion)
 *
 *  BYTE TRACKING:
 *  ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
 *  • resumed_bytes:  Bytes reused from existing partial files (not downloaded from network)
 *  • received_bytes: Bytes actually received from network during this session
 *  • total_bytes:    resumed_bytes + received_bytes (total progress for this chunk)
 *
 *  HTTP RANGE REQUESTS:
 *  ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
 *  • Master task resuming:  "Range: bytes=400000-"         (from append_offset to end)
 *  • Chunk task complete:   "Range: bytes=1048576-2097151" (exact chunk boundaries)
 *  • Chunk task resuming:   "Range: bytes=1500000-2097151" (from append_offset to chunk end)
 *
 *
 * ┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
 * │                                                                                                                                          │
 * │                                                     STREAMING AND MERGE COORDINATION                                                     │
 * │                                                                                                                                          │
 * └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 *
 *  STREAMING REQUIREMENTS:
 *  ═══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════
 *  • Data must be streamed to data_channel in sequential order (by offset)
 *  • Chunks complete out-of-order but must be processed in-order
 *  • Master task streams data while chunks are still downloading
 *  • Non-blocking progress updates during merge operations
 *
 *  MERGE PROCESS:
 *  ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
 *    ┌───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
 *    │  1. Master task completes first chunk (0 → 1MB) and streams to channel immediately                                                    │
 *    │  2. Wait for chunk tasks to complete in offset order (1MB → 2MB, then 2MB → 3MB, etc.)                                                │
 *    │  3. As each chunk completes, append its data to master .part file and stream to channel                                               │
 *    │  4. Perform boundary validation (ensure chunk N ends exactly where chunk N+1 begins)                                                  │
 *    │  5. Clean up individual chunk files after successful merge                                                                            │
 *    │  6. Atomically rename .part file to final destination                                                                                 │
 *    └───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 *
 *
 * ┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
 * │                                                                                                                                          │
 * │                                                        FAILURE HANDLING AND RETRY LOGIC                                                  │
 * │                                                                                                                                          │
 * └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 *
 *  CHUNK FAILURE SCENARIOS:
 *  ═══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════
 *  • Individual chunk failures don't immediately fail the entire download
 *  • Failed chunks are retried independently with exponential backoff
 *  • Master task continues downloading while chunks retry in background
 *  • Only when all retries are exhausted does the overall download fail
 *
 *  CORRUPTION DETECTION:
 *  ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
 *  • Chunk boundary validation ensures no gaps or overlaps
 *  • File size validation against Content-Length headers
 *  • Existing partial file validation against expected sizes
 *  • Automatic cleanup of corrupted chunk files
 *
 *  RECOVERY MECHANISMS:
 *  ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
 *  • Resume from existing .part files (master and chunks)
 *  • Reconstruct chunk tasks from existing .part-O{offset} files
 *  • Graceful degradation to single-threaded download if chunking fails
 *  • Process coordination via PID files to prevent conflicts
 *
 *
 * ┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
 * │                                                                                                                                          │
 * │                                                         THREAD POOL ARCHITECTURE                                                         │
 * │                                                                                                                                          │
 * └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 *
 *  THREAD MANAGEMENT:
 *  ═══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════
 *  • Main Task Pool:  Limited to nr_parallel (respects user-configured parallelism)
 *  • Chunk Task Pool: Limited to 2 * nr_parallel (allows higher chunk parallelism)
 *  • Automatic cleanup of finished threads
 *  • Graceful shutdown with cancellation support
 *
 *    ┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
 *    │                                                                                                                                                          │
 *    │   ┌─────────────────────────────────────────┐    ┌───────────────────────────────────────────────────────────────────────────────────────────────────┐   │
 *    │   │           MAIN TASK POOL                │    │                                      CHUNK TASK POOL                                              │   │
 *    │   │          (nr_parallel threads)          │    │                                   (2 * nr_parallel threads)                                       │   │
 *    │   │                                         │    │                                                                                                   │   │
 *    │   │  ┌─────────────┐  ┌─────────────┐       │    │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │   │
 *    │   │  │  Master 1   │  │  Master 2   │ ...   │    │  │   Chunk 1   │  │   Chunk 2   │  │   Chunk 3   │  │   Chunk 4   │  │   Chunk 5   │    ...       │   │
 *    │   │  │   Task      │  │   Task      │       │    │  │    Task     │  │    Task     │  │    Task     │  │    Task     │  │    Task     │              │   │
 *    │   │  └─────────────┘  └─────────────┘       │    │  └─────────────┘  └─────────────┘  └─────────────┘  └─────────────┘  └─────────────┘              │   │
 *    │   └─────────────────────────────────────────┘    └───────────────────────────────────────────────────────────────────────────────────────────────────┘   │
 *    │                                                                                                                                                          │
 *    └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 *
 *
 * ┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
 * │                                                                                                                                          │
 * │                                                      PERFORMANCE AND MONITORING                                                          │
 * │                                                                                                                                          │
 * └──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 *
 *  ETA CALCULATION:
 *  ══════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════
 *  • Individual task ETAs based on throughput and remaining bytes
 *  • Global ETA considers slowest task (bottleneck analysis)
 *  • Real-time updates with rate limiting to prevent UI spam
 *  • Automatic throughput calculation from network bytes only
 *
 *  PROGRESS REPORTING:
 *  ──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
 *  • Master task shows aggregate progress across all chunks
 *  • Individual chunk progress tracked separately
 *  • Non-blocking progress updates during merge operations
 *  • Visual progress bars with detailed chunk status
 *
 *  STATISTICS COLLECTION:
 *  ──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
 *  • Network bytes vs resumed bytes tracking
 *  • Download duration and throughput metrics
 *  • Retry count and failure rate monitoring
 *  • Chunk efficiency and parallelism effectiveness
 *
 */

#[derive(Debug)]
pub struct DownloadTask {
    pub url:                  String,
    pub resolved_url:         Mutex<String>,
    #[allow(dead_code)]
    pub output_dir:           PathBuf,
    pub max_retries:          usize,
    pub client:               Arc<Mutex<Option<Agent>>>,                  // HTTP client created on-demand
    pub data_channels:        Arc<Mutex<Vec<Sender<Vec<u8>>>>>,           // Support multiple data channels for deduplication
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
    pub master_metadata:      Mutex<Option<ServerMetadata>>,

    // Repository information for mirror selection
    pub repodata_name:        String,                                     // Repository name for mirror selection

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
    pub progress_bar:         Mutex<Option<ProgressBar>>,                 // will never change

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
    pub remote_size: Option<u64>,
    pub last_modified: Option<String>,
    pub timestamp: u64,  // parsed from last_modified
    pub etag: Option<String>,
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

impl ServerMetadata {
    pub fn matches_with(&self, other: &Self) -> bool {
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

/// .pget-status file format for download state persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PgetStatus {
    pub url: String,
    pub file_type: FileType,
    pub metadata: ServerMetadata,
}


// ============================================================================

/// Helper function to update a download task's status
///
/// This function handles the common pattern of updating a task's status
/// while properly handling mutex locks and error reporting.
pub fn update_download_status(task: &DownloadTask, new_status: DownloadStatus) -> Result<()> {
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
    pub fn new(url: String, output_dir: PathBuf, max_retries: usize) -> Self {
        Self::with_size(url, output_dir, max_retries, None, "".to_string())
    }

    pub fn with_size(url: String, output_dir: PathBuf, max_retries: usize, file_size: Option<u64>, repodata_name: String) -> Self {
        let final_path = mirror::Mirrors::resolve_mirror_path(&url, &output_dir, &repodata_name);
        Self::with_path(url, final_path, max_retries, file_size, repodata_name)
    }

    pub fn with_path(url: String, final_path: PathBuf, max_retries: usize, file_size: Option<u64>, repodata_name: String) -> Self {
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
            master_metadata:   Mutex::new(None),            // Will store metadata in HTTP response
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
        }
    }

    pub fn with_data_channel(self, channel: Sender<Vec<u8>>) -> Self {
        if let Ok(mut channels) = self.data_channels.lock() {
            channels.push(channel);
        }
        self
    }

    pub fn get_status(&self) -> DownloadStatus {
        self.status.lock()
            .unwrap_or_else(|e| panic!("Failed to lock download status mutex: {}", e))
            .clone()
    }

    /// Check if this is a master task (has chunk tasks)
    pub fn is_master_task(&self) -> bool {
        self.chunk_path.to_string_lossy().ends_with(".part")
    }

    /// Check if this is a chunk task (has non-zero offset or is explicitly a chunk)
    pub fn is_chunk_task(&self) -> bool {
        self.chunk_path.to_string_lossy().contains(".part-O")
    }

    /// Check if this is the first download attempt for this task
    /// This is more reliable than checking the retries parameter in download_file
    #[allow(dead_code)]
    pub fn is_first_attempt(&self) -> bool {
        self.attempt_number.load(Ordering::SeqCst) == 0
    }

    /// Increment the attempt number when a retry is needed
    #[allow(dead_code)]
    pub fn increment_attempt(&self) {
        self.attempt_number.fetch_add(1, Ordering::SeqCst);
    }

    /// Reset the attempt number to zero
    #[allow(dead_code)]
    pub fn reset_attempt(&self) {
        self.attempt_number.store(0, Ordering::SeqCst);
    }

    /// Get the current chunk status
    pub fn get_chunk_status(&self) -> ChunkStatus {
        self.chunk_status.lock()
            .unwrap_or_else(|e| panic!("Failed to lock chunk status mutex: {}", e))
            .clone()
    }

    /// Set the chunk status
    pub fn set_chunk_status(&self, status: ChunkStatus) -> Result<()> {
        let mut chunk_status = self.chunk_status.lock()
            .map_err(|e| eyre!("Failed to lock chunk status mutex: {}", e))?;
        *chunk_status = status;
        Ok(())
    }

    /// Get the resolved URL, falling back to the original URL if resolution failed
    pub fn get_resolved_url(&self) -> String {
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
    pub fn get_client(&self) -> Result<Agent> {
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
    pub fn create_chunk_task(&self, offset: u64, size: u64) -> Arc<DownloadTask> {
        // Create a chunk task with a specific offset and size
        // The chunk file will be named <final_path>.part-O{offset} to avoid nested "-O" components
        let chunk_path = format!("{}.part-O{}", self.final_path.to_string_lossy(), offset);

        Arc::new(DownloadTask {
            url:                  self.url.clone(),
            resolved_url:         Mutex::new(self.get_resolved_url()),
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
            master_metadata:      Mutex::new(self.master_metadata.lock().unwrap().clone()),  // Each chunk's HTTP response must align with master_metadata
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
        })
    }

    #[allow(dead_code)]
    pub fn final_append_offset(&self) -> u64 {
        let offset = self.chunk_offset.load(Ordering::Relaxed);
        offset + self.progress()
    }

    #[allow(dead_code)]
    pub fn download_start_offset(&self) -> u64 {
        let offset = self.chunk_offset.load(Ordering::Relaxed);
        let reused = self.resumed_bytes.load(Ordering::Relaxed);
        offset + reused
    }

    pub fn progress(&self) -> u64 {
        let received = self.received_bytes.load(Ordering::Relaxed);
        let reused = self.resumed_bytes.load(Ordering::Relaxed);
        received + reused
    }

    pub fn remaining(&self) -> u64 {
        let chunk_size = self.chunk_size.load(Ordering::Relaxed);
        if chunk_size == 0 {
            log::warn!("chunk_size=0 for task {:?}", self);
        }

        chunk_size.saturating_sub(self.progress())
    }

    /// Get total progress bytes across all chunks (reused + network bytes)
    /// This represents the total download progress for display purposes
    pub fn get_total_progress_bytes(&self) -> (u64, u64, usize) {
        let mut total_received = self.received_bytes.load(Ordering::Relaxed);
        let mut total_reused = self.resumed_bytes.load(Ordering::Relaxed);
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
    fn iterate_task_levels<F>(&self, mut callback: F)
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

    /// Set progress bar message
    pub fn set_message(&self, message: String) {
        if let Ok(pb_guard) = self.progress_bar.lock() {
            if let Some(ref pb) = *pb_guard {
                pb.set_message(message);
            }
        }
    }

    /// Clean helper to get data channel without repeated error handling
    pub fn get_data_channel(&self) -> Option<Sender<Vec<u8>>> {
        self.data_channels.lock().ok().and_then(|channels| channels.first().cloned())
    }

    /// Add a data channel for duplicate downloads
    pub fn add_data_channel(&self, channel: Sender<Vec<u8>>) {
        if let Ok(mut channels) = self.data_channels.lock() {
            channels.push(channel);
        }
    }

    /// Get all data channels for broadcasting
    pub fn get_all_data_channels(&self) -> Vec<Sender<Vec<u8>>> {
        self.data_channels.lock().ok().map(|channels| channels.clone()).unwrap_or_default()
    }

    /// Clean helper to take data channels (for closing)
    pub fn take_data_channels(&self) -> Vec<Sender<Vec<u8>>> {
        self.data_channels.lock().ok().map(|mut channels| {
            let result = channels.clone();
            channels.clear();
            result
        }).unwrap_or_default()
    }

    /// Set progress bar length
    pub fn set_length(&self, length: u64) {
        if let Ok(pb_guard) = self.progress_bar.lock() {
            if let Some(ref pb) = *pb_guard {
                pb.set_length(length);
            }
        }
    }

    /// Set progress bar position
    pub fn set_position(&self, position: u64) {
        if let Ok(pb_guard) = self.progress_bar.lock() {
            if let Some(ref pb) = *pb_guard {
                pb.set_position(position);
            }
        }
    }

    /// Finish progress bar with message
    pub fn finish_with_message(&self, message: String) {
        if let Ok(pb_guard) = self.progress_bar.lock() {
            if let Some(ref pb) = *pb_guard {
                pb.finish_with_message(message);
            }
        }
    }
}

pub struct DownloadManager {
    multi_progress: MultiProgress,
    tasks: Arc<Mutex<HashMap<String, Arc<DownloadTask>>>>,
    nr_parallel: usize,
    task_handles: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
    chunk_handles: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
    is_processing: Arc<AtomicBool>,
    current_task_count: Arc<AtomicUsize>,

    // ETA and download statistics - replaced atomically
    stats: Arc<Mutex<DownloadManagerStats>>,

    // Cancellation flag for graceful shutdown
    cancelled: Arc<AtomicBool>,
}

/// Download manager statistics - replaced atomically as a whole
#[derive(Debug, Clone, Default)]
struct DownloadManagerStats {
    global_ideal_eta: u64,          // Global ideal ETA in seconds
    slowest_task_eta: u64,          // Slowest task ETA in seconds
    fastest_task_eta: u64,          // Fastest task ETA in seconds
    total_remaining_bytes: u64,     // Total bytes remaining across all downloads
    total_rate_bps: u64,            // Total download rate in bytes per second
    active_tasks: usize,            // Number of actively downloading tasks
    pending_tasks: usize,           // Number of pending tasks
    complete_tasks: usize,          // Number of completed tasks
    master_tasks: usize,            // Number of master tasks
    l2_chunk_tasks: usize,          // Number of L2 chunk tasks
    l3_chunk_tasks: usize,          // Number of L3 chunk tasks
}

impl DownloadManager {
    pub fn new(nr_parallel: usize) -> Result<Self> {
        // Note: Proxy configuration is now handled per-task via on-demand client creation from config().common.proxy

        let multi_progress = MultiProgress::new();

        Ok(Self {
            multi_progress,
            tasks:                Arc::new(Mutex::new(HashMap::new())),
            nr_parallel,
            task_handles:         Arc::new(Mutex::new(Vec::new())),
            chunk_handles:        Arc::new(Mutex::new(Vec::new())),
            is_processing:        Arc::new(AtomicBool::new(false)),
            current_task_count:   Arc::new(AtomicUsize::new(0)),
            stats:                Arc::new(Mutex::new(DownloadManagerStats::default())),
            cancelled:            Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn submit_task(&self, task: DownloadTask) -> Result<()> {
        let mut tasks = self.tasks.lock()
            .map_err(|e| eyre!("Failed to lock tasks mutex: {}", e))?;
        if !tasks.contains_key(&task.url) {
            tasks.insert(task.url.clone(), Arc::new(task));
        } else {
            // For duplicate URLs, share the data channel with the existing task
            if let Some(existing_task) = tasks.get(&task.url) {
                if let Some(new_data_channel) = task.get_data_channel() {
                    // Add the new data channel to the existing task for data sharing
                    existing_task.add_data_channel(new_data_channel);
                    log::debug!("Added data channel for duplicate download URL: {} final_path: {}",
                               task.url, task.final_path.display());
                } else {
                    log::warn!("Skipping duplicate download URL (no data channel): {} final_path: {}",
                               task.url, task.final_path.display());
                }
            }
        }
        Ok(())
    }

    /// Check if a task exists for the given URL and return its status
    #[allow(dead_code)]
    pub fn get_task_status(&self, url: &str) -> Option<DownloadStatus> {
        let tasks = self.tasks.lock().ok()?;
        let task = tasks.get(url)?;
        task.status.lock().ok().map(|status| status.clone())
    }

    /// Check if a task exists for the given URL
    pub fn has_task(&self, url: &str) -> bool {
        if let Ok(tasks) = self.tasks.lock() {
            tasks.contains_key(url)
        } else {
            false
        }
    }

    pub fn wait_for_task(&self, task_url: String) -> Result<DownloadStatus> {
        loop {
            // Check for cancellation first
            if self.cancelled.load(Ordering::Relaxed) {
                return Err(eyre!("Download cancelled by user"));
            }

            let tasks = self.tasks.lock()
                .map_err(|e| eyre!("Failed to lock tasks mutex: {}", e))?;
            if let Some(task) = tasks.get(&task_url) {
                let status = task.get_status();
                match status {
                    DownloadStatus::Completed | DownloadStatus::Failed(_) => return Ok(status),
                    _ => {}
                }
            } else {
                drop(tasks);
                return Err(eyre!("Task with URL {} not found", task_url));
            }
            drop(tasks);
            thread::sleep(Duration::from_millis(WAIT_TASK_DURATION_MS));
        }
    }

    /// Wait for any download task to complete and return the completed task's URL
    pub fn wait_for_any_task(&self, task_urls: &[String]) -> Result<Option<String>> {
        if task_urls.is_empty() {
            return Ok(None);
        }

        loop {
            // Check for cancellation first
            if self.cancelled.load(Ordering::Relaxed) {
                return Err(eyre!("Download cancelled by user"));
            }

            let tasks = self.tasks.lock()
                .map_err(|e| eyre!("Failed to lock tasks mutex: {}", e))?;

            // Check if any of the specified tasks have completed
            for task_url in task_urls {
                if let Some(task) = tasks.get(task_url) {
                    match task.get_status() {
                        DownloadStatus::Completed => {
                            return Ok(Some(task_url.clone()));
                        }
                        DownloadStatus::Failed(err) => {
                            return Err(eyre!("Download failed for {}: {}", task_url, err));
                        }
                        _ => {} // Still downloading or pending
                    }
                }
            }

            drop(tasks);
            thread::sleep(Duration::from_millis(WAIT_TASK_DURATION_MS));
        }
    }

    pub fn start_processing(&self) {
        if self.is_processing.load(Ordering::Relaxed) {
            return;
        }

        self.is_processing.store(true, Ordering::Relaxed);

        // Initialize processing thread with all required context
        self.spawn_main_processing_thread();
    }

    /// Spawn the main processing thread that coordinates all download activities
    /// Level 2: Thread Management - handles the main processing lifecycle
    fn spawn_main_processing_thread(&self) {
        let tasks                  = Arc::clone(&self.tasks);
        let multi_progress         = self.multi_progress.clone();
        let is_processing          = Arc::clone(&self.is_processing);
        let task_handles           = Arc::clone(&self.task_handles);
        let chunk_handles          = Arc::clone(&self.chunk_handles);
        let nr_parallel            = self.nr_parallel;
        let current_task_count_arc = Arc::clone(&self.current_task_count);
        let cancelled              = Arc::clone(&self.cancelled);

        thread::spawn(move || {
            Self::run_main_processing_loop(
                tasks,
                multi_progress,
                is_processing,
                task_handles,
                chunk_handles,
                nr_parallel,
                current_task_count_arc,
                cancelled,
            );
        });
    }

    /// Main processing loop that handles task scheduling and coordination
    /// Level 3: Task Coordination - orchestrates task and chunk processing
    fn run_main_processing_loop(
        tasks: Arc<Mutex<HashMap<String, Arc<DownloadTask>>>>,
        multi_progress: MultiProgress,
        is_processing: Arc<AtomicBool>,
        task_handles: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
        chunk_handles: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
        nr_parallel: usize,
        current_task_count_arc: Arc<AtomicUsize>,
        cancelled: Arc<AtomicBool>,
    ) {
        loop {
            // Check for cancellation
            if cancelled.load(Ordering::Relaxed) {
                log::info!("Main processing loop cancelled, stopping");
                is_processing.store(false, Ordering::Relaxed);
                break;
            }

            // Clean up finished threads
            Self::cleanup_finished_handles(&task_handles);
            Self::cleanup_finished_handles(&chunk_handles);

            // Check for completion or process pending tasks
            match Self::process_pending_master_tasks(
                &tasks,
                &multi_progress,
                &task_handles,
                nr_parallel,
                &current_task_count_arc,
            ) {
                ProcessingResult::AllCompleted => {
                    is_processing.store(false, Ordering::Relaxed);
                    break;
                }
                ProcessingResult::Continue => {
                    // Start chunk processing for existing tasks
                    Self::start_chunks_processing(&tasks, &chunk_handles, nr_parallel);
                    thread::sleep(Duration::from_millis(WAIT_TASK_DURATION_MS));
                }
            }
        }
    }

    /// Process pending master tasks and spawn new download threads
    /// Level 4: Task Scheduling - handles master task prioritization and spawning
    fn process_pending_master_tasks(
        tasks: &Arc<Mutex<HashMap<String, Arc<DownloadTask>>>>,
        multi_progress: &MultiProgress,
        task_handles: &Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
        nr_parallel: usize,
        current_task_count_arc: &Arc<AtomicUsize>,
    ) -> ProcessingResult {
        let tasks_guard = match tasks.lock() {
            Ok(guard) => guard,
            Err(e) => {
                log::error!("Failed to lock tasks mutex: {}", e);
                return ProcessingResult::AllCompleted;
            }
        };

        // Collect and prioritize pending tasks
        let pending_tasks = Self::collect_and_prioritize_pending_tasks(&tasks_guard);

        if pending_tasks.is_empty() {
            // Check if all tasks are completed
            let all_done = tasks_guard.iter()
                .all(|(_, t)| matches!(t.get_status(), DownloadStatus::Completed | DownloadStatus::Failed(_)));

            drop(tasks_guard);

            if all_done {
                return ProcessingResult::AllCompleted;
            } else {
                return ProcessingResult::Continue;
            }
        }

        // Spawn new task threads within capacity limits
        let spawned_count = Self::spawn_task_threads(
            pending_tasks,
            multi_progress,
            task_handles,
            nr_parallel,
            current_task_count_arc,
        );

        drop(tasks_guard);

        log::trace!("Spawned {} new download threads", spawned_count);
        ProcessingResult::Continue
    }

    /// Collect pending tasks and sort them by priority (largest first)
    /// Level 5: Task Collection - handles task filtering and prioritization
    fn collect_and_prioritize_pending_tasks(
        tasks_guard: &HashMap<String, Arc<DownloadTask>>
    ) -> Vec<(String, Arc<DownloadTask>)> {
        let mut pending_tasks: Vec<_> = tasks_guard.iter()
            .filter(|(_, t)| matches!(t.get_status(), DownloadStatus::Pending))
            .map(|(url, task)| (url.clone(), Arc::clone(task)))
            .collect();

        // Sort by file size (largest first) for optimal resource utilization
        pending_tasks.sort_by(|(_, a), (_, b)| {
            let size_a = a.file_size.load(Ordering::Relaxed);
            let size_b = b.file_size.load(Ordering::Relaxed);
            size_b.cmp(&size_a) // Descending order
        });

        pending_tasks
    }

    /// Spawn task threads for pending downloads within capacity limits
    /// Level 5: Thread Spawning - handles individual task thread creation
    fn spawn_task_threads(
        pending_tasks: Vec<(String, Arc<DownloadTask>)>,
        multi_progress: &MultiProgress,
        task_handles: &Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
        nr_parallel: usize,
        current_task_count_arc: &Arc<AtomicUsize>,
    ) -> usize {
        let mut current_task_count = {
            let handles_guard = task_handles.lock().unwrap();
            handles_guard.len()
        };

        let mut spawned_count = 0;

        for (_task_url, task) in pending_tasks {
            current_task_count_arc.store(current_task_count, Ordering::Relaxed);

            if current_task_count >= nr_parallel {
                break; // Reached task thread limit
            }

            if Self::spawn_single_task_thread(task, multi_progress, task_handles) {
                current_task_count += 1;
                spawned_count += 1;
            }
        }

        spawned_count
    }

    /// Spawn a single task thread with proper error handling and cleanup
    /// Level 6: Individual Thread Management - handles single task execution
    fn spawn_single_task_thread(
        task: Arc<DownloadTask>,
        multi_progress: &MultiProgress,
        task_handles: &Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
    ) -> bool {
        // Mark task as downloading
        match task.status.lock() {
            Ok(mut status) => *status = DownloadStatus::Downloading,
            Err(e) => {
                log::error!("Failed to lock task status mutex: {}", e);
                return false;
            }
        };

        let multi_progress = multi_progress.clone();
        let task_clone = Arc::clone(&task);

        // Spawn task thread
        let handle = thread::spawn(move || {
            if let Err(e) = download_task(&task_clone, &multi_progress) {
                log_error_with_backtrace(&task_clone.url, &e);
                if let Ok(mut status) = task_clone.status.lock() {
                    *status = DownloadStatus::Failed(format!("{}", e));
                }
            } else if let Ok(mut status) = task_clone.status.lock() {
                *status = DownloadStatus::Completed;
            }

            // CRITICAL: Take data_channel to close it and unblock receivers
            // This prevents recv() from blocking forever after download completion
            let _data_channels = task_clone.take_data_channels();
        });

        // Store the handle
        if let Ok(mut handles_guard) = task_handles.lock() {
            handles_guard.push(handle);
            true
        } else {
            false
        }
    }

    /// Clean up finished thread handles
    fn cleanup_finished_handles(handles: &Arc<Mutex<Vec<thread::JoinHandle<()>>>>) {
        if let Ok(mut handles_guard) = handles.lock() {
            let mut unfinished_handles = Vec::new();

            // Drain all handles and separate finished from unfinished
            for handle in handles_guard.drain(..) {
                if handle.is_finished() {
                    // Handle is finished, join it to clean up
                    if let Err(e) = handle.join() {
                        log::warn!("Thread join failed: {:?}", e);
                    }
                } else {
                    // Keep unfinished handles
                    unfinished_handles.push(handle);
                }
            }

            // Put back the unfinished handles
            *handles_guard = unfinished_handles;
        }
    }

    /// Internal chunk processing that doesn't require external chunk list
    fn start_chunks_processing(
        tasks: &Arc<Mutex<HashMap<String, Arc<DownloadTask>>>>,
        chunk_handles: &Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
        nr_parallel: usize,
    ) {
        let max_chunk_threads = Self::calculate_max_chunk_threads(nr_parallel);

        // Get current chunk thread count
        let current_chunk_count = {
            let handles_guard = match chunk_handles.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };
            handles_guard.len()
        };

        if current_chunk_count >= max_chunk_threads {
            return; // Already at capacity
        }

        let pending_chunks = Self::collect_pending_chunks(tasks);

        // Spawn chunk threads up to the limit
        let threads_to_spawn = std::cmp::min(
            max_chunk_threads - current_chunk_count,
            pending_chunks.len()
        );

        if threads_to_spawn <= 0 {
            if pending_chunks.is_empty() {
                Self::run_global_ondemand_scheduler();
            }
            return;
        }

        log::debug!(
            "pending_chunks={} max_threads={} active_chunks={} to_spawn={}",
            pending_chunks.len(), max_chunk_threads, current_chunk_count, threads_to_spawn
        );

        Self::spawn_chunk_threads(&pending_chunks, threads_to_spawn, chunk_handles);
    }

    /// Calculate the maximum number of chunk threads based on parallel limit and available mirrors
    fn calculate_max_chunk_threads(nr_parallel: usize) -> usize {
        if nr_parallel == 1 {
            return 1;
        }

        // Get available mirrors count
        let available_mirrors_count = {
            if let Ok(mirrors) = mirror::MIRRORS.lock() {
                mirrors.available_mirrors.len()
            } else {
                1
            }
        };

        std::cmp::max(
            nr_parallel,
            std::cmp::min(nr_parallel * MAX_CHUNK_THREADS_MULTIPLIER, available_mirrors_count)
        ) * CHUNK_PARALLEL_MULTIPLIER
    }

    /// Helper function to iterate through all task levels using 3-level architecture
    ///
    /// Calls the provided closure for each task found across all 3 levels:
    /// - Level 1: Master tasks
    /// - Level 2: L2 tasks (beforehand or ondemand tasks from master_task.chunk_tasks)
    /// - Level 3: L3 tasks (ondemand tasks from l2_task.chunk_tasks)
    fn iterate_3level_tasks<F>(
        tasks: &Arc<Mutex<HashMap<String, Arc<DownloadTask>>>>,
        mut callback: F
    )
    where F: FnMut(&Arc<DownloadTask>, usize) // (task, level)
    {
        let tasks_guard = match tasks.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };

        // Level 1: Master tasks
        for (_url, master_task) in tasks_guard.iter() {
            if !master_task.is_master_task() {
                log::warn!("Level-1 task is not master_task: {:?}", master_task);
            }

            callback(master_task, 1);

            // Level 2: L2 tasks (chunk tasks from master)
            let chunks = match master_task.chunk_tasks.lock() {
                Ok(chunks) => chunks,
                Err(_) => continue,
            };

            for l2_task in chunks.iter() {
                // l2_task is beforehand task or ondemand task
                callback(l2_task, 2);

                // Level 3: L3 tasks (chunk tasks from l2_task)
                let l3_chunks = match l2_task.chunk_tasks.lock() {
                    Ok(chunks) => chunks,
                    Err(_) => continue,
                };

                for l3_task in l3_chunks.iter() {
                    // l3_task is ondemand task
                    callback(l3_task, 3);
                }
            }
        }
    }

    /// Collect all pending chunk tasks from all download tasks using the 3-level architecture
    fn collect_pending_chunks(
        tasks: &Arc<Mutex<HashMap<String, Arc<DownloadTask>>>>
    ) -> Vec<(f64, Arc<DownloadTask>)> {
        let mut collected = Vec::new();

        Self::iterate_3level_tasks(tasks, |task, _level| {
            // Skip master tasks (.part) – we only want real chunk tasks (".part-O{offset}")
            if !task.is_chunk_task() {
                return;
            }

            // Collect tasks that are still pending
            if matches!(task.get_status(), DownloadStatus::Pending) {
                let chunk_offset = task.chunk_offset.load(Ordering::Relaxed);
                let file_size = task.file_size.load(Ordering::Relaxed);
                let priority = if file_size > 0 {
                    chunk_offset as f64 / file_size as f64
                } else {
                    0.0
                };
                collected.push((priority, Arc::clone(task)));
            }
        });

        // Sort chunks by priority (chunk_offset / file_size - lower offset = higher priority)
        collected.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        if !collected.is_empty() {
            log::debug!(
                "3-level chunking: pending_chunks={}",
                collected.len()
            );
        }

        collected
    }

    /// Global ondemand scheduler - collects stats and selects task with slowest single ETA for chunking
    fn run_global_ondemand_scheduler() {
        let mut slowest_task_for_ondemand_chunking: Option<Arc<DownloadTask>> = None;
        let mut slowest_eta_for_ondemand: u64 = 0;

        // Create new stats instance locally
        let mut new_stats = DownloadManagerStats::default();
        new_stats.fastest_task_eta = u64::MAX; // Initialize to max value
        let mut debug_stats = Vec::new();

        // Single pass: collect stats and find ondemand candidates
        DownloadManager::iterate_3level_tasks(&DOWNLOAD_MANAGER.tasks, |task, level| {
            // Collect stats for this task
            collect_task_eta_stats(task, level, &mut new_stats, &mut debug_stats);

            // Check for ondemand chunking candidates (only downloading tasks)
            if !matches!(task.get_status(), DownloadStatus::Downloading) {
                return;
            }

            if !may_ondemand_chunking(task) {
                return;
            }

            let single_eta = task.eta.load(Ordering::Relaxed);
            if slowest_eta_for_ondemand < single_eta {
                slowest_eta_for_ondemand = single_eta;
                slowest_task_for_ondemand_chunking = Some(Arc::clone(task));
            }
        });

        // Update and log stats
        let global_ideal_eta = update_global_stats(new_stats.clone(), &debug_stats);
        dump_global_stats_ratelimit(&new_stats, global_ideal_eta, &debug_stats);

        // Set slowest ETA task for ondemand chunking (if ETA > global ETA)
        if let Some(ref task) = slowest_task_for_ondemand_chunking {
            if slowest_eta_for_ondemand >= global_ideal_eta   // >= handles single-large-file case
               && slowest_eta_for_ondemand > MIN_ETA_THRESHOLD_SECONDS
            {
                if let Err(e) = task.set_chunk_status(ChunkStatus::NeedOndemandChunk) {
                    log::warn!("Failed to set NeedOndemandChunk status for slowest ETA task: {}", e);
                } else {
                    log::info!(
                        "Global scheduler selected slowest ETA task {} (ETA:{:.1}s > global:{:.1}s) for ondemand chunking",
                        task.url, slowest_eta_for_ondemand as f64, global_ideal_eta as f64
                    );
                    log::debug!(
                        "Global ondemand scheduler: selected slowest_eta={:.1}s, global_ideal_eta={:.1}s",
                        slowest_eta_for_ondemand as f64, global_ideal_eta as f64
                    );
                }
            }
        }
    }

    /// Spawn chunk download threads for the given pending chunks
    fn spawn_chunk_threads(
        pending_chunks: &[(f64, Arc<DownloadTask>)],
        threads_to_spawn: usize,
        chunk_handles: &Arc<Mutex<Vec<thread::JoinHandle<()>>>>
    ) {
        for (_, chunk_task) in pending_chunks.iter().take(threads_to_spawn) {
            let chunk_clone = Arc::clone(chunk_task);
            let _chunk_handles_clone = Arc::clone(chunk_handles);

            // Double-check that the task is still pending before spawning
            if !matches!(chunk_clone.get_status(), DownloadStatus::Pending) {
                log::debug!("Skipping chunk task {} that is no longer pending (status: {:?})",
                           chunk_clone.chunk_path.display(), chunk_clone.get_status());
                continue;
            }

            // Mark chunk as downloading NOW to avoid duplicate scheduling in the next scheduler tick
            if let Err(e) = update_download_status(&chunk_clone, DownloadStatus::Downloading) {
                log::error!("Failed to set chunk status to Downloading for {}: {}", chunk_clone.chunk_path.display(), e);
                continue; // Skip spawning thread if we cannot update status
            }

            log::debug!("Spawning chunk thread for {} (offset: {}, size: {})",
                       chunk_clone.chunk_path.display(),
                       chunk_clone.chunk_offset.load(Ordering::Relaxed),
                       chunk_clone.chunk_size.load(Ordering::Relaxed));

            let handle = thread::spawn(move || {

                match download_chunk_task(&chunk_clone) {
                    Ok(()) => {
                        log::debug!(
                            "Chunk for {} at offset {} completed successfully (path: {})",
                            chunk_clone.get_resolved_url(), chunk_clone.chunk_offset.load(Ordering::Relaxed), chunk_clone.chunk_path.display()
                        );

                        // Mark chunk as completed
                        if let Ok(mut status) = chunk_clone.status.lock() {
                            *status = DownloadStatus::Completed;
                        }
                    },
                    Err(e) => {
                        log::debug!(
                            "Chunk task failed for {} at offset {} (path: {}): {:#}",
                            chunk_clone.get_resolved_url(), chunk_clone.chunk_offset.load(Ordering::Relaxed), chunk_clone.chunk_path.display(), e
                        );

                        // Mark chunk as failed
                        if let Ok(mut status) = chunk_clone.status.lock() {
                            *status = DownloadStatus::Failed(format!("{}", e));
                        }
                    }
                }
            });

            // Store the chunk handle
            if let Ok(mut handles_guard) = chunk_handles.lock() {
                handles_guard.push(handle);
            }
        }
    }

    #[allow(dead_code)]
    pub fn wait_for_all_tasks(&self) -> Result<()> {
        while self.is_processing.load(Ordering::Relaxed) {
            // Check for cancellation
            if self.cancelled.load(Ordering::Relaxed) {
                return Err(eyre!("Download cancelled by user"));
            }
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
            .filter_map(|(_, t)| {
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

    /// Dump comprehensive information about all download tasks in a tree-like structure.
    ///
    /// Each master (L1) task is treated as the root of a tree (one file per tree).
    /// Chunk tasks (L2 and L3) are printed as the children of their parent task.
    /// One task per line with useful fields for quick debugging.
    pub fn dump_all_tasks(&self) {
        self.dump_download_manager_stats();
        self.dump_task_tree();
    }

    /// Cancel all pending downloads and stop processing
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.is_processing.store(false, Ordering::Relaxed);
    }

    /// Check if downloads have been cancelled
    #[allow(dead_code)]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Dump DownloadManagerStats information
    fn dump_download_manager_stats(&self) {
        println!("=== DownloadManagerStats ===");
        if let Ok(stats) = self.stats.lock() {
            println!("Global ideal ETA: {}s", stats.global_ideal_eta);
            println!("Slowest task ETA: {}s", stats.slowest_task_eta);
            println!("Fastest task ETA: {}s", stats.fastest_task_eta);
            println!("Total remaining bytes: {} ({:.1}MB)",
                stats.total_remaining_bytes,
                stats.total_remaining_bytes as f64 / (1024.0 * 1024.0));
            println!("Total rate: {} B/s ({:.1}KB/s)",
                stats.total_rate_bps,
                stats.total_rate_bps as f64 / 1024.0);
            println!("Active tasks: {}", stats.active_tasks);
            println!("Pending tasks: {}", stats.pending_tasks);
            println!("Complete tasks: {}", stats.complete_tasks);
            println!("Master tasks: {}", stats.master_tasks);
            println!("L2 chunk tasks: {}", stats.l2_chunk_tasks);
            println!("L3 chunk tasks: {}", stats.l3_chunk_tasks);
        } else {
            println!("Could not access DownloadManagerStats (lock contention)");
        }
        println!("");
    }

    /// Dump task tree structure with formatting
    fn dump_task_tree(&self) {

        // Track whether we have printed a master already (for spacing)
        let mut first_master = true;

        Self::iterate_3level_tasks(&self.tasks, |task, level| {
            if level == 1 {
                if !first_master {
                    println!(""); // Blank line between different trees
                }
                first_master = false;
            }

            // Build indentation prefix based on level (simple tree)
            let indent = match level {
                1 => "".to_string(),
                2 => "  ├─ ".to_string(),
                3 => "      └─ ".to_string(),
                _ => format!("{}- ", " ".repeat(level * 2)),
            };

            println!("{}{}", indent, format_task_line(task, level));
        });
    }
}

// Helper to format a single task line for output
fn format_task_line(task: &Arc<DownloadTask>, level: usize) -> String {
    use std::sync::atomic::Ordering;

    let status = task.get_status();
    let offset = task.chunk_offset.load(Ordering::Relaxed);
    let size   = task.chunk_size.load(Ordering::Relaxed);
    let recv   = task.received_bytes.load(Ordering::Relaxed);
    let eta    = task.eta.load(Ordering::Relaxed);
    let chunk_status = task.get_chunk_status();
    let resumed_bytes = task.resumed_bytes.load(Ordering::Relaxed);
    let start_time = task.start_time.lock().ok().and_then(|st| st.map(|t| t.elapsed().as_secs())).map(|secs| format!("{}s ago", secs)).unwrap_or_else(|| "None".to_string());
    let resolved_url = task.get_resolved_url();
    let url = if !resolved_url.is_empty() { &resolved_url } else { &task.url };

    let name = if level == 1 {
        url.clone()
    } else {
        task.chunk_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| task.chunk_path.to_string_lossy().to_string())
    };

    // Only show chunk status if not NoChunk
    let chunk_str = match chunk_status {
        ChunkStatus::NoChunk => None,
        _ => Some(format!("{:?}", chunk_status)),
    };

    let mut fields = vec![
        format!(
            "{}  {:?}  {:?}  offset={}  size={}  resumed={} recv={}  rate={}KB/s  eta={}s",
            name, task.file_type, status,
            offset, size, resumed_bytes, recv,
            task.throughput_bps.load(Ordering::Relaxed) >> 10, eta
        ),
        format!("start_time={}", start_time),
        format!("progress={}", task.progress()),
    ];
    if size > 0 {
        fields.push(format!("remaining={}", task.remaining()));
    }
    if let Some(chunk) = chunk_str {
        fields.push(chunk);
    }
    if level == 1 {
        fields.push(format!("file_size={}", task.file_size.load(Ordering::Relaxed)));
        fields.push(format!("file_path={}", task.final_path.display()));
    }
    if level >= 2 {
        if !resolved_url.is_empty() {
            let site = mirror::url2site(&resolved_url);
            fields.push(format!("site={}", site));
        }
    }
    fields.join("  ")
}


// Main Features:
//
// 1. Concurrent Downloads
//   - Supports parallel downloads with configurable concurrency limit
//   - Uses custom thread management with JoinHandle tracking
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
//   - Relies on custom thread management for parallelism
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
enum ProcessingResult {
    Continue,
    AllCompleted,
}

/// ETA calculation results for a single task
#[allow(dead_code)]
#[derive(Debug)]
enum CacheDecision {
    UseCache { reason: String },
    AppendDownload { reason: String },
    RedownloadDueTo { reason: String },
}

/*
 * ============================================================================
 * DOWNLOAD-TIME MIRROR RESOLUTION SYSTEM
 * ============================================================================
 *
 * INTELLIGENT RESOLUTION STRATEGY:
 *
 * This system implements mirror resolution at download time rather than at
 * repo metadata preparation time. This provides several key advantages:
 *
 * 1. **Failure Recovery**: Can switch mirrors on download failures
 * 2. **Load Balancing**: Distributes downloads across multiple mirrors
 * 3. **Performance Optimization**: Uses real-time performance data
 * 4. **Retry Intelligence**: Selects different mirrors for retry attempts
 *
 * RETRY LOGIC:
 *
 * - First attempt: Use optimal mirror with normal concurrent limits
 * - Retry attempts: Reduce concurrent limits to encourage different selection
 * - This naturally provides fallback to less-loaded mirrors on failures
 *
 * DISTRO-AGNOSTIC DESIGN:
 *
 * Since mirrors are pre-filtered by distro at initialization, this function
 * no longer needs to extract distro information from URLs or pass it through
 * the selection chain. This eliminates heuristic parsing and simplifies
 * the resolution logic significantly.
 */

/*
 * ============================================================================
 * SIMPLIFIED MIRROR SYSTEM
 * ============================================================================
 *
 * STREAMLINED INITIALIZATION:
 *
 * The mirror system is now initialized directly with distro filtering at startup,
 * eliminating the need for complex runtime initialization logic:
 *
 * 1. **Immediate Filtering**: Uses channel_config().distro at LazyLock time
 * 2. **Performance Data Loading**: Historical logs loaded during initialization
 * 3. **No Runtime Setup**: Mirrors are ready for use immediately
 * 4. **Simplified Code Path**: Removes initialization complexity from download path
 *
 * This provides better performance and reliability while simplifying the codebase.
 */

pub static DOWNLOAD_MANAGER: LazyLock<DownloadManager> = LazyLock::new(|| {
    DownloadManager::new(config().common.nr_parallel)
        .expect("Failed to initialize download manager")
});

pub fn submit_download_task(task: DownloadTask) -> Result<()> {
    DOWNLOAD_MANAGER.submit_task(task)
}

/// Check if a download task exists for the given URL
pub fn has_download_task(url: &str) -> bool {
    DOWNLOAD_MANAGER.has_task(url)
}

/// Get the status of a download task for the given URL
#[allow(dead_code)]
pub fn get_download_task_status(url: &str) -> Option<DownloadStatus> {
    DOWNLOAD_MANAGER.get_task_status(url)
}

/// Wait for any of the specified download tasks to complete
pub fn wait_for_any_download_task(task_urls: &[String]) -> Result<Option<String>> {
    DOWNLOAD_MANAGER.wait_for_any_task(task_urls)
}

/// Cancel all pending downloads
pub fn cancel_downloads() {
    DOWNLOAD_MANAGER.cancel();
}

pub fn download_urls(
    urls: Vec<String>,
    output_dir: &Path,
    max_retries: usize,
    async_mode: bool,
) -> Result<Vec<DownloadTask>> {
    let mut task_urls = Vec::new();
    for url in urls {
        let url_for_context = url.clone();
        let task = DownloadTask::new(url.clone(), output_dir.to_path_buf(), max_retries);

        // Submit the task - if URL already exists, it will just replace/reuse
        submit_download_task(task)
            .with_context(|| format!("Failed to submit download task for {}", url_for_context))?;
        task_urls.push(url);
    }
    fs::create_dir_all(output_dir)
        .with_context(|| format!("Failed to create output directory: {}", output_dir.display()))?;
    DOWNLOAD_MANAGER.start_processing();

    if !async_mode {
        // Wait for each task using the URLs
        for (i, task_url) in task_urls.iter().enumerate() {
            let status = DOWNLOAD_MANAGER.wait_for_task(task_url.clone())
                .with_context(|| format!("Failed to wait for download task {} (URL: {})", i, task_url))?;
            if let DownloadStatus::Failed(err_msg) = status {
                return Err(eyre!("Download failed for {}: {}", task_url, err_msg));
            }
        }
        Ok(Vec::new())
    } else {
        Ok(Vec::new()) // Return empty vec in async mode since tasks are managed by DownloadManager
    }
}

/// Setup progress bar for download
fn setup_progress_bar(task: &DownloadTask, multi_progress: &MultiProgress, url: &str) -> Result<()> {
    let pb = multi_progress.add(ProgressBar::new(0));
    pb.set_style(ProgressStyle::default_bar()
        .template(&format!("[{{elapsed_precise}}] [{{bar:{}}}] {{bytes_per_sec:12}} ({{eta}}) {{msg}}", PROGRESS_BAR_WIDTH))
        .map_err(|e| eyre!("Failed to parse HTTP response: {}", e))?
        .progress_chars("=> "));
    pb.set_message(url.to_string());

    if let Ok(mut pb_guard) = task.progress_bar.lock() {
        *pb_guard = Some(pb);
    }
    Ok(())
}

/// Verify downloaded file size against expected size
#[allow(dead_code)]
fn verify_file_size(part_path: &Path, expected_size: Option<u64>, url: &str) -> Result<()> {
    if let Some(expected) = expected_size {
        if let Ok(metadata) = fs::metadata(part_path) {
            let actual_size = metadata.len();
            if actual_size != expected {
                let error_msg = format!(
                    "Downloaded file size mismatch: expected {} bytes, got {} bytes",
                    expected, actual_size
                );
                log::warn!("{} for {}", error_msg, url);
                // Note: We could make this a hard error, but for now just warn
                // since size information might not always be accurate
            } else {
                log::debug!("Downloaded file size verified: {} bytes for {}", actual_size, url);
            }
        }
    }

    Ok(())
}

/*
 * ============================================================================
 * DOWNLOAD ORCHESTRATION WITH MIRROR OPTIMIZATION
 * ============================================================================
 *
 * PERFORMANCE LOGGING INTEGRATION:
 *
 * Every download operation contributes to the mirror performance database:
 *
 * 1. **Latency Tracking**: HTTP request timing for all call() operations
 * 2. **Throughput Measurement**: Actual transfer speeds during content download
 * 3. **Error Classification**: Distinguishes server errors from network issues
 * 4. **Capability Detection**: Tracks range request support and content availability
 *
 * This comprehensive logging enables the mirror selection system to make
 * increasingly intelligent decisions over time.
 */

// Current Code Call Graph
// download_task()
// ├── handle_local_file()
// │   └── (file copy operations)
// ├── create_pid_file()
// ├── setup_progress_bar()
// └── download_file_with_retries()
//     └── validate_existing_file()
//       ├── send_file_to_channel() for Immutable file
//       └── handle_corruption_detection()
//     ├── recover_parto_files()
//     └── download_file()
//         ├── create_chunk_tasks()
//         ├── download_chunk_task()
//         │   ├── check_existing_partfile()
//         │   │   └── send_chunk_to_channel() for master task
//         │   ├── resolve_mirror_and_update_task()
//         │   ├── execute_download_request()
//         │   │   ├── handle_http_status_error()
//         │   │   ├── handle_network_io_error()
//         │   │   └── handle_general_request_error()
//         │   ├── process_download_response()
//         │   │   ├── handle_304_not_modified_response()
//         │   │   │   └── send_file_to_channel() for Mutable file
//         │   │   ├── extract_server_metadata()
//         │   │   ├── should_redownload() for Mutable file
//         │   │   └── validate_response_content_type()
//         │   ├── process_chunk_download_stream()
//         │   │   ├── read_chunk_from_stream()
//         │   │   ├── calculate_write_bytes()
//         │   │   ├── write_chunk_data()
//         │   │   ├── channel.send()
//         │   │   ├── update_download_progress()
//         │   │   └── check_ondemand_chunking()
//         │   ├── finalize_chunk_download()
//         │   │   └── validate_chunk_file_boundaries()
//         │   ├── validate_download_size()
//         │   └── log_download_completion()
//         ├── wait_for_chunks_and_merge()
//         │   ├── process_chunks_at_level()
//         │   │   ├── merge_completed_chunk()
//         │   │   │   ├── validate_chunk_file_boundaries()
//         │   │   │   ├── send_chunk_to_channel() for chunk tasks
//         │   │   │   └── append_file_to_file()
//         │   │   ├── handle_failed_chunk()
//         │   │   └── update_chunk_progress()
//         │   └── validate_chunk_merge_integrity()
//         └── finalize_file()
//             ├── verify_file_size()
//             └── (atomic rename .part → final file)

/// Downloads a file from a URL to the output directory.
/// Uses the final_path that was calculated when the task was created.
///
/// Ensures optimal mirror configuration and comprehensive performance logging
fn download_task(
    task: &DownloadTask,
    multi_progress: &MultiProgress,
) -> Result<()> {
    let url = &task.url.clone();
    let final_path = &task.final_path.clone();
    let expected_size = task.file_size.load(Ordering::Relaxed);

    log::debug!("download_task starting for {}, has_channel: {}, expected_size: {:?}", url, task.get_data_channel().is_some(), expected_size);

    // Handle local file URLs (file:// or starting with /)
    if url.starts_with("file://") || url.starts_with("/") {
        return handle_local_file(url, final_path, task);
    }

    // Create PID file for process coordination
    let pid_file = create_pid_file(final_path)?;

    // Setup progress bar and store it in the task
    setup_progress_bar(task, multi_progress, url)?;

    // Start the download - use the old system for now until we can safely integrate the new one
    let result = download_file_with_retries(task);
    log::debug!("download_task download_file_with_retries completed for {}, result: {:?}", url, result);

    // Clean up PID file regardless of result
    let _pid_cleanup_result = cleanup_pid_file(&pid_file);

    // Update progress bar based on result
    if result.is_ok() {
        task.finish_with_message(format!("Downloaded {}", final_path.display()));
    } else {
        task.finish_with_message(format!("Error: {:?}", result));
    }

    result
}

/// Handles local file URLs by copying them to the destination
///
/// - Supports file:// URLs and absolute paths starting with /
/// - Creates parent directories as needed
/// - Marks the download task as completed
/// - Verifies file size if expected size is provided
fn handle_local_file(url: &str, final_path: &Path, task: &DownloadTask) -> Result<()> {
    let source_path = if url.starts_with("file://") {
        Path::new(&url[7..])
    } else {
        Path::new(url)
    };

    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directory for {}: {}", final_path.display(), parent.display()))?;
    }

    fs::copy(source_path, final_path)
        .with_context(|| format!("Failed to copy file from {} to {}", source_path.display(), final_path.display()))?;

    // Verify file size if expected size is provided
    let file_size_val = task.file_size.load(Ordering::Relaxed);
    if file_size_val > 0 {
        if let Ok(metadata) = fs::metadata(final_path) {
            let actual_size = metadata.len();
            if actual_size != file_size_val {
                let error_msg = format!(
                    "Local file size mismatch: expected {} bytes, got {} bytes",
                    file_size_val, actual_size
                );
                log::warn!("{} for {}", error_msg, url);
                // Note: We could make this a hard error, but for now just warn
                // since size information might not always be accurate
            } else {
                log::debug!("Local file size verified: {} bytes for {}", actual_size, url);
            }
        }
    }

    Ok(())
}

// download_file():
//   1. Download content (including chunk merging)
//   2. Extract & store metadata from HTTP response
//   3. Verify file size on .part file
//   4. Atomic completion (.part → final file)
//   5. Set metadata (timestamp + ETag) on final file
//
// download_file_with_retries():
//   - Pure retry wrapper around download_file()
//   - Only handles retry logic and error handling
fn download_file_with_retries(
    task: &DownloadTask,
) -> Result<()> {
    let url = &task.url;
    let max_retries = task.max_retries;

    let mut retries = 0;

    // Validate existing files and determine appropriate download action
    match validate_existing_file(task)? {
        ValidationResult::SkipDownload(reason) => {
            send_file_to_channel(task)
                .with_context(|| format!("Failed to send existing file to channel: {}", task.final_path.display()))?;

            log::info!("Skipping download: {}", reason);
            return Ok(());
        }
        ValidationResult::ResumeFromPartial => {
            log::info!("Resuming from partial file at {}", task.chunk_path.display());
            // Continue with existing partial file
        }
        ValidationResult::CorruptionDetected => {
            log::warn!("Corruption detected in {}, handling...", task.chunk_path.display());
            handle_corruption_detection(task)?;
            // Continue with fresh download
        }
        ValidationResult::StartFresh => {
            log::info!("Starting fresh download for {}", task.get_resolved_url());
            // Continue with fresh download
        }
    }

    // Recover any existing part files for resumption
    match recover_parto_files(task)? {
        ValidationResult::ResumeFromPartial => {
            log::info!("Recovered partial files for resumption at {}", task.chunk_path.display());
        }
        _ => {
            // No recovery needed or recovery failed
        }
    }

    loop {
        log::debug!("download_file_with_retries calling download_file for {} (saving to {}), attempt {}", url, task.chunk_path.display(), retries + 1);

        let download_result = download_file(task);
        let resolved_url = task.get_resolved_url();

        match download_result {
            Ok(()) => {
                log::debug!("download_file_with_retries completed successfully for {}, dropping channel", &resolved_url);

                return Ok(());
            },
            Err(e) => {
                // Check if this is one of our custom download errors to avoid logging stack traces
                if let Some(download_err) = e.downcast_ref::<DownloadError>() {
                    match download_err {
                        DownloadError::Fatal { code, message } => {
                            log::debug!("download_file_with_retries got fatal error {} for {} (saving to {}): {}", code, resolved_url, task.chunk_path.display(), message);
                        },
                        DownloadError::Network { details } => {
                            log::debug!("download_file_with_retries got network error for {} (saving to {}): {}", resolved_url, task.chunk_path.display(), details);
                        },
                        // File system errors are now handled as io::Error
                        DownloadError::ContentValidation { expected, actual } => {
                            log::debug!("download_file_with_retries got content validation error for {} (saving to {}): expected {}, got {}", resolved_url, task.chunk_path.display(), expected, actual);
                        },
                        DownloadError::MirrorResolution { details } => {
                            log::debug!("download_file_with_retries got mirror resolution error for {}: {}", resolved_url, details);
                        },
                        DownloadError::UnexpectedResponse { code, details } => {
                            log::debug!("download_file_with_retries got unexpected response {} for {}: {}", code, resolved_url, details);
                        },
                        DownloadError::AlreadyComplete => {
                            log::debug!("download_file_with_retries got already complete response for {}", resolved_url);
                            return Ok(());
                        },
                        DownloadError::TooManyRequests => {
                            log::debug!("download_file_with_retries got too many requests error for {}", resolved_url);
                        },
                        DownloadError::DiskError { details } => {
                            log::debug!("download_file_with_retries got disk error for {} (saving to {}): {}", resolved_url, task.chunk_path.display(), details);
                            // Don't mark mirror as bad for disk errors - they're local issues
                            return Err(e);
                        },
                    }
                } else {
                    log::debug!("download_file_with_retries got error for {}: {}", resolved_url, e);
                }

                if retries >= max_retries {
                    return Err(eyre!("Max retries ({}) exceeded for {}: {}", max_retries, resolved_url, e));
                }

                // Reset stale chunk tasks before the next retry to avoid inconsistent state
                {
                    if let Ok(mut guard) = task.chunk_tasks.lock() {
                        if !guard.is_empty() {
                            log::debug!("Clearing {} stale chunk tasks before retry", guard.len());
                            guard.clear();
                        }
                    }
                    if let Err(e2) = task.set_chunk_status(ChunkStatus::NoChunk) {
                        log::warn!("Failed to reset chunk_status to NoChunk: {}", e2);
                    }
                    cleanup_chunk_files(task)?;
                }

                retries += 1;
                if retries < max_retries / 2 {
                    // no delay, to quickly try another mirror
                    continue;
                }

                let delay = Duration::from_secs(2u64.pow(retries as u32));
                // Keep showing the original error message in pb for some time
                thread::sleep(delay);
                task.set_message(format!("Retrying (attempt {}/{} after {}s delay): {}", retries + 1, max_retries + 1, delay.as_secs(), resolved_url));
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

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

    // The channel receivers process_packages_content()/process_filelist_content() expect full file
    // to decompress and compute hash, so send the existing file content first. This fixes bug
    // "Decompression error: stream/file format not recognized"
    send_chunk_to_all_channels(&task, &task.final_path, &data_channels)
}

/// Send a chunk file to all data channels (for broadcasting to duplicate downloads)
fn send_chunk_to_all_channels(
    task: &DownloadTask,
    part_path: &Path,
    data_channels: &[Sender<Vec<u8>>],
) -> Result<()> {
    // Ensure we only stream the pre-existing file once per download_file_with_retries() lifetime
    if task.has_sent_existing.swap(true, Ordering::SeqCst) {
        log::debug!("Existing file already streamed once – skipping second send for {}", part_path.display());
        return Ok(());
    }

    log::debug!("Sending chunk file to {} channels: {}", data_channels.len(), part_path.display());

    let mut file = map_io_error(File::open(part_path), "open file for channel", part_path)?;
    let mut buffer = vec![0; 64 * 1024]; // 64KB buffer
    let mut chunks_sent = 0;

    loop {
        let bytes_read = map_io_error(file.read(&mut buffer), "read file for channel", part_path)?;
        if bytes_read == 0 {
            break; // EOF
        }

        chunks_sent += 1;
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
    }

    Ok(())
}

/// Send a chunk file to the data channel (for streaming fresh chunk data)
/// This bypasses the master task and has_sent_existing guards
fn send_chunk_to_channel(
    task: &DownloadTask,
    part_path: &Path,
    data_channel: &Sender<Vec<u8>>,
) -> Result<()> {
    send_chunk_to_all_channels(task, part_path, &[data_channel.clone()])
}

fn download_file(
    task: &DownloadTask,
) -> Result<()> {
    let url = &task.url;
    log::debug!("download_file starting for {} (chunk_path={})", url, task.chunk_path.display());

    // Try to create beforehand chunks
    create_chunk_tasks(task)?;

    download_chunk_task(task)?;

    // Wait for all chunks to complete and merge them
    wait_for_chunks_and_merge(task)?;

    log::debug!("download_file calling finalize_file for {} (chunk_path={})", url, task.chunk_path.display());
    finalize_file(task)?;
    log::info!("download_file completed: {} (chunk_path={})", task.get_resolved_url(), task.chunk_path.display());
    Ok(())
}

/// Get the size of an existing partial file, or 0 if it doesn't exist
fn get_existing_file_size(part_path: &Path) -> Result<u64> {
    if part_path.exists() {
        log::debug!("download_file part file exists, getting metadata for {}", part_path.display());
        match fs::metadata(part_path) {
            Ok(metadata) => {
                let size = metadata.len();
                log::debug!("download_file found existing part file with {} bytes: {}", size, part_path.display());
                Ok(size)
            },
            Err(e) => {
                log::error!("download_file failed to get metadata for part file {}: {}", part_path.display(), e);
                Err(DownloadError::DiskError {
                    details: format!("Failed to get metadata for part file {}: {}", part_path.display(), e)
                }.into())
            }
        }
    } else {
        log::debug!("download_file no existing part file found: {}", part_path.display());
        Ok(0)
    }
}

/// Execute HTTP download request with comprehensive error handling
///
/// This function handles:
/// - ETag conditional requests (304 Not Modified)
/// - Range request errors (416 Range Not Satisfiable)
/// - Network and timeout errors
/// - Request logging and metrics
fn execute_download_request(
    task: &DownloadTask,
    resolved_url: &str,
    existing_bytes: u64,
) -> Result<http::Response<ureq::Body>> {
    // Build HTTP request with appropriate headers
    let client = task.get_client()?;
    let mut request = client.get(resolved_url.replace("///", "/"));

    // Add ETag conditional headers for cache validation
    let part_path = &task.chunk_path;
    let file_size = task.file_size.load(Ordering::Relaxed);
    let chunk_size = task.chunk_size.load(Ordering::Relaxed);
    let chunk_offset = task.chunk_offset.load(Ordering::Relaxed);

    // Add Range headers for partial content requests
    let resumed_bytes = task.resumed_bytes.load(Ordering::Relaxed);

    match task.get_range_request() {
        RangeRequest::Chunk => {
            let end = chunk_offset + chunk_size - 1;
            let start = chunk_offset + resumed_bytes;
            if start >= end {
                log::warn!("Invalid range detected: start={} >= end={} for {} (chunk_offset={}, chunk_size={}, resumed_bytes={}, file_size={}, chunk_path={})",
                          start, end, resolved_url, chunk_offset, chunk_size, resumed_bytes, file_size, part_path.display());
                return Err(eyre!("Invalid range calculation: start > end"));
            }
            log::debug!("Setting Range header: bytes={}-{} (chunk_offset={}, chunk_size={}, resumed_bytes={}, chunk_path={})",
                       start, end, chunk_offset, chunk_size, resumed_bytes, part_path.display());
            request = request.header("Range", &format!("bytes={}-{}", start, end));
        }
        RangeRequest::Resume => {
            if resumed_bytes >= file_size && file_size > 0 {
                log::warn!("Invalid range detected: resumed_bytes={} >= file_size={} for {} (chunk_offset={}, chunk_size={}, chunk_path={})",
                          resumed_bytes, file_size, resolved_url, chunk_offset, chunk_size, part_path.display());
                return Err(eyre!("Invalid range calculation: resumed_bytes >= file_size"));
            }
            log::debug!("Setting Range header: bytes={}- (resume from existing bytes, chunk_path={})", resumed_bytes, part_path.display());
            request = request.header("Range", &format!("bytes={}-", resumed_bytes));
        }
        RangeRequest::None => {
            // is master task w/o chunking
            // For mutable files, check final_path; for others, check part_path
            let file_to_check: Option<&Path> = if matches!(task.file_type, FileType::Mutable) && task.final_path.exists() {
                Some(&task.final_path)
            } else if part_path.exists() {
                Some(part_path)
            } else {
                log::debug!("Local file {} doesn't exist, skipping ETag header", part_path.display());
                None
            };

            if let Some(file_to_check) = file_to_check {
                // Check file size - don't use ETag for 0-byte files
                let file_size = fs::metadata(file_to_check)
                    .map(|m| m.len())
                    .unwrap_or(0);

                if file_size == 0 {
                    log::debug!("File {} is 0 bytes, skipping ETag header to force fresh download", file_to_check.display());
                } else if let Some(stored_etag) = task.load_etag() {
                    log::debug!("Adding If-None-Match header with ETag '{}' for conditional request (file={})", stored_etag, file_to_check.display());
                    request = request.header("If-None-Match", &format!("\"{}\"", stored_etag));
                }
            }
        }
    }

    // Execute the request and handle all possible outcomes
    let request_start = std::time::Instant::now();
    let call_result = request.call();
    let latency = request_start.elapsed().as_millis() as u64;
    log_http_event_safe(resolved_url, mirror::HttpEvent::Latency(latency));

    match call_result {
        Ok(response) => Ok(response),
        Err(ureq::Error::StatusCode(code)) => handle_http_status_error(code, task, resolved_url, existing_bytes),
        Err(ureq::Error::Io(e)) => handle_network_io_error(e, task, resolved_url),
        Err(e) => handle_general_request_error(e, task, resolved_url),
    }
}

/// Handle HTTP status code errors (4xx, 5xx responses)
/// Level 6: Error Handling - processes HTTP status code errors
// ### HTTP Standards & Headers
// #### `Range` (Request Header)
// - Requests a specific part of a resource:
//   ```http
//   Range: bytes=0-499
//   ```
// - Server responds with:
//   - `206 Partial Content` (success) + `Content-Range`.
//   - `416 Range Not Satisfiable` (invalid range).
//
// #### `Accept-Ranges` (Response Header)
// - Indicates if the server supports range requests:
//   ```http
//   Accept-Ranges: bytes
//   ```
//   (or `none` if unsupported).
//
// ### Example Flow
// 1. Client requests a range:
//    ```http
//    GET /largefile.zip HTTP/1.1
//    Range: bytes=0-999
//    ```
// 2. Server responds:
//    ```http
//    HTTP/1.1 206 Partial Content
//    Content-Range: bytes 0-999/5000
//    Content-Length: 1000
//    [...data...]
fn handle_http_status_error(
    code: u16,
    task: &DownloadTask,
    resolved_url: &str,
    _existing_bytes: u64,
) -> Result<http::Response<ureq::Body>> {
    log::debug!("HTTP error code {} for chunk_path={}", code, task.chunk_path.display());

    // Log latency even for errors
    log_http_event_safe(resolved_url, mirror::HttpEvent::HttpStatus(code));

    let error_msg = format!("HTTP {}", code);
    task.set_message(format!("{} - {}", error_msg, resolved_url));

    if code == 429 {
        // Get the active connection count for this mirror **before** logging for better diagnostics
        let active_conns = {
            let site = mirror::url2site(&resolved_url);
            if let Ok(mirrors_guard) = mirror::MIRRORS.lock() {
                mirrors_guard.mirrors.get(&site)
                    .map(|mirror| mirror.shared_usage.active_downloads.load(std::sync::atomic::Ordering::Relaxed))
                    .unwrap_or(0)
            } else {
                0
            }
        };

        log::debug!("Received HTTP 429 Too Many Requests ({} active connections) for {} (chunk_path={})", active_conns, resolved_url, task.chunk_path.display());

        // Log the TooManyRequests event with the connection count
        log_http_event_safe(&resolved_url, mirror::HttpEvent::TooManyRequests(active_conns as u32));

        return Err(DownloadError::TooManyRequests.into());
    }

    if code == 416 {
        // Special handling for 416 Range Not Satisfiable errors
        let chunk_offset = task.chunk_offset.load(Ordering::Relaxed);
        let chunk_size = task.chunk_size.load(Ordering::Relaxed);
        let resumed_bytes = task.resumed_bytes.load(Ordering::Relaxed);
        let file_size = task.file_size.load(Ordering::Relaxed);

        log::warn!("HTTP 416 Range Not Satisfiable for {} (chunk_path={}) - Range calculation details:", resolved_url, task.chunk_path.display());
        log::warn!("  chunk_offset={}, chunk_size={}, resumed_bytes={}, file_size={}",
                  chunk_offset, chunk_size, resumed_bytes, file_size);

        if chunk_offset > 0 || chunk_size != file_size {
            let start = chunk_offset + resumed_bytes;
            let end = chunk_offset + chunk_size - 1;
            log::warn!("  Attempted range: bytes={}-{} (start={}, end={})", start, end, start, end);

            if start > end {
                log::error!("  INVALID RANGE: start > end - this is the root cause of the 416 error");
            } else if end >= file_size && file_size > 0 {
                log::warn!("  Range extends beyond file size: end={} >= file_size={}", end, file_size);
            }
        }

        // For 416 errors, we should try a different mirror or restart the download
        log::warn!("HTTP 416 error indicates invalid range request - will retry with different mirror or restart");
        Err(DownloadError::UnexpectedResponse { code, details: format!("HTTP 416 Range Not Satisfiable: {}", error_msg) }.into())
    } else if code == 502 {
        // 502 Bad Gateway - server is temporarily unavailable (common with unreliable servers like AUR)
        log::warn!("HTTP 502 Bad Gateway for {} (chunk_path={}) - server may be unreliable, will retry", resolved_url, task.chunk_path.display());
        Err(DownloadError::UnexpectedResponse { code, details: format!("HTTP 502 Bad Gateway - server temporarily unavailable: {}", error_msg) }.into())
    } else if code >= HTTP_CLIENT_ERROR_START && code < HTTP_SERVER_ERROR_START {
        // For client errors (like 403, 404), create a simple DownloadError without verbose backtrace
        log::debug!("Client error {} for {} (chunk_path={})", code, resolved_url, task.chunk_path.display());
        Err(DownloadError::Fatal { code, message: error_msg }.into())
    } else {
        log::debug!("Server error {} for {} (chunk_path={})", code, resolved_url, task.chunk_path.display());
        Err(DownloadError::UnexpectedResponse { code, details: format!("HTTP error: {}", error_msg) }.into())
    }
}

/// Handle network I/O errors
/// Level 6: Error Handling - processes network I/O errors
fn handle_network_io_error(
    e: std::io::Error,
    task: &DownloadTask,
    resolved_url: &str,
) -> Result<http::Response<ureq::Body>> {
    log_http_event_safe(resolved_url, mirror::HttpEvent::NetError(e.to_string()));

    log::debug!("Network I/O error for {} (chunk_path={}): {}", resolved_url, task.chunk_path.display(), e);

    let error_msg = format!("Network error: {} - {}", e, resolved_url);
    task.set_message(error_msg.clone());
    Err(DownloadError::Network { details: error_msg }.into())
}

/// Handle general request errors (timeouts, DNS failures, etc.)
/// Level 6: Error Handling - processes general request errors
fn handle_general_request_error(
    e: ureq::Error,
    task: &DownloadTask,
    resolved_url: &str,
) -> Result<http::Response<ureq::Body>> {
    let error_str = e.to_string();
    let error_msg = format!("Error downloading: {} - {}", error_str, resolved_url);

    // Log general error as network error
    log_http_event_safe(resolved_url, mirror::HttpEvent::NetError(error_str.clone()));

    log::debug!("General request error for {} (chunk_path={}): {}", resolved_url, task.chunk_path.display(), error_str);

    task.set_message(error_msg.clone());

    Err(DownloadError::Network { details: error_msg }.into())
}

/// Validate response content type to detect HTML login pages
fn validate_response_content_type(
    response: &http::Response<ureq::Body>,
    url: &str,
    task: &DownloadTask,
) -> Result<()> {
    log::debug!("Validating response content type for {} (chunk_path={})", url, task.chunk_path.display());
    if let Some(content_type) = response.headers().get("content-type").and_then(|v| v.to_str().ok()) {
        if content_type.contains("text/html") {
            // Check if content is encoded (compressed) - this often indicates legitimate content
            if let Some(content_encoding) = response.headers().get("content-encoding").and_then(|v| v.to_str().ok()) {
                if content_encoding.contains("gzip") || content_encoding.contains("deflate") || content_encoding.contains("xz") {
                    log::debug!("HTML content detected but with content-encoding '{}', allowing download from {}", content_encoding, url);
                    return Ok(());
                }
            }

            let error_msg = "Received HTML page instead of file. This may indicate an authentication issue with the server.";
            task.set_message(error_msg.to_string());
            return Err(eyre!("Fatal error while downloading from {}: {}", url, error_msg.to_string()));
        }
    }
    Ok(())
}

/// Process the main download stream with chunked reading and progress tracking
/// Level 5: Stream Processing - handles the core download loop with boundaries
fn process_chunk_download_stream(
    response: &mut http::Response<ureq::Body>,
    task: &DownloadTask,
    existing_bytes: u64,
) -> Result<u64> {
    // Check if content is compressed - if so, we can't trust content-length for validation
    let has_compression = is_content_compressed(response);

    // Get expected response size from Content-Length header for validation
    // Only use content-length for validation if there's no compression
    let expected_response_size = if !has_compression {
        parse_content_length(response)
    } else {
        log::debug!("Content-encoding detected, skipping content-length validation for {}", task.url);
        None
    };

    let mut reader = response.body_mut().as_reader();
    let mut buffer = vec![0; 8192];
    let mut chunk_append_offset = existing_bytes;
    let mut network_bytes = 0u64;
    let mut last_update = std::time::Instant::now();
    let mut last_ondemand_check = std::time::Instant::now();
    let data_channel = task.get_data_channel();

    // Setup file for writing
    let mut file = setup_download_file(task, existing_bytes)?;

    log::debug!("process_chunk_download_stream: Starting to read response body for {} (existing_bytes={})",
               task.chunk_path.display(), existing_bytes);

    loop {
        // Read data from network stream
        let bytes_read = read_chunk_from_stream(&mut reader, &mut buffer, task, chunk_append_offset)?;

        if bytes_read == 0 {
            // EOF reached - validate against expected size if available
            if let Some(expected_size) = expected_response_size {
                if network_bytes < expected_size {
                    log::error!(
                        "Premature EOF: received {} bytes but expected {} bytes for {}",
                        network_bytes, expected_size, task.chunk_path.display()
                    );
                    return Err(DownloadError::Network {
                        details: format!("Premature EOF: received {} bytes but expected {} bytes for {}",
                                       network_bytes, expected_size, task.chunk_path.display())
                    }.into());
                }
            }
            break; // EOF reached
        }

        // Calculate bytes to write based on chunk boundaries
        let bytes_to_write = calculate_write_bytes(task, bytes_read, chunk_append_offset);

        // Write data to file with boundary checks
        let written_bytes = write_chunk_data(&mut file, &buffer, bytes_to_write, task, chunk_append_offset)?;

        if written_bytes == 0 {
            break; // Chunk boundary reached
        }

        // Update download counters
        chunk_append_offset += written_bytes as u64;
        network_bytes += written_bytes as u64;
        task.received_bytes.store(network_bytes, Ordering::Relaxed);

        // Send data to channel for master tasks
        if let Some(ref channel) = data_channel {
            match channel.send(buffer[..written_bytes].to_vec()) {
                Ok(_) => {
                    // Successfully sent data
                }
                Err(e) => {
                    // Channel is disconnected (receiver dropped)
                    log::warn!("Data channel disconnected for {}: {}", task.url, e);
                    // Don't retry since channel is broken - receiver is gone
                }
            }
        }

        if written_bytes < bytes_read {
            break; // Reached chunk boundary for master task
        }

        update_download_progress(task, &mut last_update);
        check_ondemand_chunking(task, chunk_append_offset, &mut last_ondemand_check);
    }

    Ok(chunk_append_offset)
}

/// Read chunk data from network stream with error handling
/// Level 6: Network I/O - handles reading from response stream
fn read_chunk_from_stream(
    reader: &mut dyn std::io::Read,
    buffer: &mut [u8],
    task: &DownloadTask,
    chunk_append_offset: u64,
) -> Result<usize> {
    match reader.read(buffer) {
        Ok(0) => {
            log::debug!("read_chunk_from_stream: EOF reached at offset {} for {}", chunk_append_offset, task.chunk_path.display());
            Ok(0) // EOF reached
        },
        Ok(n) => {
            log::trace!("read_chunk_from_stream: Read {} bytes at offset {} for {}", n, chunk_append_offset, task.chunk_path.display());
            Ok(n)
        },
        Err(e) => {
            log::error!("read_chunk_from_stream: Read error at offset {} for {}: {}", chunk_append_offset, task.chunk_path.display(), e);
            if task.is_master_task() {
                let error_msg = format!("Read error at {} bytes: {}", chunk_append_offset,
                    task.resolved_url.lock().map(|r| r.clone()).unwrap_or_else(|_| task.url.clone()));
                task.set_message(error_msg);
            }
            Err(eyre!("Failed to read from response (chunk_append_offset={}, buffer_size={}): {}", chunk_append_offset, buffer.len(), e))
        }
    }
}

/// Finalize chunk download with progress updates and completion logging
/// Level 5: Download Finalization - completes download with final updates
fn finalize_chunk_download(
    task: &DownloadTask,
    chunk_append_offset: u64,
    existing_bytes: u64,
) -> Result<u64> {
    let network_bytes = chunk_append_offset - existing_bytes;

    // Final progress update
    if task.is_master_task() {
        let (total_received, total_reused, _downloading_chunks) = task.get_total_progress_bytes();
        let total_progress = total_received + total_reused;
        task.set_position(total_progress);
    }

    log::debug!("download_content completed: {} total bytes ({} network bytes) written to {}",
               chunk_append_offset, network_bytes, task.chunk_path.display());

    // Detect 0-byte downloads for mutable files (like AUR packages) - this indicates server issues
    // Check BEFORE finalization so we can retry
    if task.is_master_task() && chunk_append_offset == 0 && matches!(task.file_type, FileType::Mutable) {
        log::info!("Download resulted in 0 bytes for {} - likely server issue (unreliable server like AUR), cleaning up and will retry", task.url);
        // Clean up the 0-byte file before returning error to trigger retry
        if task.chunk_path.exists() {
            if let Err(e) = fs::remove_file(&task.chunk_path) {
                log::warn!("Failed to remove 0-byte file {}: {}", task.chunk_path.display(), e);
            } else {
                log::debug!("Cleaned up 0-byte file: {}", task.chunk_path.display());
            }
        }
        return Err(DownloadError::Network {
            details: format!("Download resulted in 0 bytes for {} - server may be unreliable", task.url)
        }.into());
    }

    // Validate that the chunk file respects its designated boundaries
    validate_chunk_file_boundaries(task, chunk_append_offset)?;

    Ok(chunk_append_offset)
}

/// Validate that the downloaded size matches the expected Content-Length
fn validate_download_size(downloaded: u64, total_size: u64, part_path: &Path) -> Result<()> {
    if total_size > 0 && downloaded != total_size {
        // Escalate to ERROR so that mismatches are clearly visible in logs
        log::error!(
            "Download size mismatch: Downloaded size ({}) does not match expected size ({}) for {}",
            downloaded,
            total_size,
            part_path.display()
        );
        return Err(DownloadError::ContentValidation {
            expected: format!("{} bytes", total_size),
            actual: format!("{} bytes", downloaded)
        }.into());
    }
    Ok(())
}

/// Parse Content-Length header from response
///
/// This function tries multiple approaches to extract the content size:
/// 1. Standard Content-Length header (but only if content is not compressed)
/// 2. Content-Range header (e.g., "bytes 0-1023/4096")
/// 3. X-Content-Length header (some servers use this)
///
/// Note: When content-encoding is present (e.g., gzip), Content-Length refers to
/// the compressed size, not the final uncompressed size. In such cases, we return
/// None since we can't reliably predict the final size.
/// Get remote file size from HTTP response headers, taking into account range requests
///
/// This function computes the total remote file size by considering:
/// - task.resumed_bytes: bytes already downloaded locally
/// - task.get_range_request(): type of range request made
/// - response Content-Length: size of current response
/// - response Content-Range: total file size from range response
///
/// For range requests, it properly calculates the total file size by adding
/// the resumed bytes to the response size or using Content-Range total.
fn get_remote_size(task: &DownloadTask, response: &http::Response<ureq::Body>) -> Option<u64> {

    // 1. Try Content-Range header first (most reliable for range requests)
    // Format: "bytes START-END/TOTAL" or "bytes */TOTAL"
    if let Some(content_range) = response.headers().get("content-range") {
        if let Ok(s) = content_range.to_str() {
            if let Some(total_size) = parse_content_range_total(s) {
                log::debug!("Got total size {} from Content-Range header: {}", total_size, s);
                return Some(total_size);
            }
        }
    }

    // Check if content is compressed - if so, Content-Length is unreliable for full file size
    let is_compressed = is_content_compressed(response);

    if is_compressed {
        log::debug!(
            "Content is compressed with '{}', Content-Length refers to compressed size, not final size",
            response.headers().get("content-encoding")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown")
        );
    }

    // 2. Calculate total size from Content-Length + resumed bytes
    if task.get_range_request() != RangeRequest::Chunk && !is_compressed {
        if let Some(response_size) = parse_content_length(response) {
            let resumed_bytes = task.resumed_bytes.load(Ordering::Relaxed);
            let total_size = resumed_bytes + response_size;
            log::debug!(
                "Range request: Content-Length {} + resumed_bytes {} = total size {}",
                response_size, resumed_bytes, total_size
            );
            return Some(total_size);
        }
    }

    // 3. Try X-Content-Length header (some servers use this)
    if let Some(x_content_length) = response.headers().get("x-content-length") {
        if let Ok(s) = x_content_length.to_str() {
            if let Ok(size) = s.parse::<u64>() {
                log::debug!("Got size {} from X-Content-Length header", size);
                return Some(size);
            }
        }
    }

    None
}

/// Parse the total size from a Content-Range header
///
/// Examples:
/// - "bytes 0-1023/4096" -> Some(4096)
/// - "bytes 1024-2047/4096" -> Some(4096)
/// - "bytes */4096" -> Some(4096)
fn parse_content_range_total(range_str: &str) -> Option<u64> {
    // Match patterns like "bytes 0-1023/4096" or "bytes */4096"
    if let Some(slash_pos) = range_str.rfind('/') {
        let total_part = &range_str[slash_pos + 1..];
        if let Ok(size) = total_part.parse::<u64>() {
            return Some(size);
        }
    }
    None
}

/// Parse remote timestamp from Last-Modified or Date headers
#[allow(dead_code)]
fn parse_remote_timestamp(response: &http::Response<ureq::Body>) -> Option<OffsetDateTime> {
    response.headers().get("last-modified")
        .and_then(|s| {
            s.to_str().ok().and_then(|s_val| {
                match OffsetDateTime::parse(s_val, &Rfc2822) {
                    Ok(dt) => Some(dt),
                    Err(e) => {
                        log::warn!("Failed to parse timestamp header value '{}': {}", s_val, e);
                        None
                    }
                }
            })
        })
}

/// Check if HTTP response content is compressed
///
/// Detects common compression types that make Content-Length unreliable:
/// - gzip, deflate, compress (standard HTTP compression)
/// - br (Brotli), zstd, xz (modern compression)
/// - identity (explicitly uncompressed)
///
/// Returns true if content is compressed, false if uncompressed or unknown
fn is_content_compressed(response: &http::Response<ureq::Body>) -> bool {
    response.headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .map(|encoding| {
            let encoding_lower = encoding.to_lowercase();
            // Check for common compression types
            encoding_lower.contains("gzip") ||
            encoding_lower.contains("deflate") ||
            encoding_lower.contains("compress") ||
            encoding_lower.contains("br") ||
            encoding_lower.contains("zstd") ||
            encoding_lower.contains("xz") ||
            // Some servers use non-standard compression names
            encoding_lower.contains("bzip2") ||
            encoding_lower.contains("lzma") ||
            encoding_lower.contains("lz4")
        })
        .unwrap_or(false)
}

/// Get Content-Length from HTTP response headers
///
/// This function safely extracts and parses the Content-Length header.
/// It handles various edge cases:
/// - Missing Content-Length header
/// - Invalid UTF-8 in header value
/// - Non-numeric header value
/// - Multiple Content-Length headers (uses first one)
///
/// Returns Some(size) if valid Content-Length found, None otherwise
fn parse_content_length(response: &http::Response<ureq::Body>) -> Option<u64> {
    response.headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
}

/// Parse ETag from response headers
///
/// ETags can be in different formats:
/// - etag: "67736fc6-dee" (lowercase with quotes)
/// - ETag: "67736fc6-dee" (mixed case with quotes)
/// - etag: 67736fc6-dee (without quotes)
/// - ETag: W/"67736fc6-dee" (weak ETag)
///
/// This function normalizes the ETag by removing quotes and the W/ prefix for weak ETags
fn parse_etag(response: &http::Response<ureq::Body>) -> Option<String> {
    // Try both lowercase and mixed case headers
    let etag_value = response.headers().get("etag")
        .or_else(|| response.headers().get("ETag"))
        .and_then(|v| v.to_str().ok())?;

    // Remove W/ prefix for weak ETags and quotes
    let cleaned_etag = etag_value.trim()
        .strip_prefix("W/").unwrap_or(etag_value.trim())  // Remove weak ETag prefix
        .trim_matches('"');  // Remove surrounding quotes

    if cleaned_etag.is_empty() {
        None
    } else {
        Some(cleaned_etag.to_string())
    }
}

// ============================================================================
// PACKAGE MANAGER DOWNLOAD INTEGRATION
// ============================================================================

impl PackageManager {
    /// Submit download tasks for packages without waiting for completion
    /// Returns a mapping from download URLs to their package keys for tracking
    pub fn submit_download_tasks(
        &mut self,
        packages: &HashMap<String, InstalledPackageInfo>,
    ) -> Result<HashMap<String, Vec<String>>> {
        let output_dir = dirs().epkg_downloads_cache.clone();
        let mut url_to_pkgkeys: HashMap<String, Vec<String>> = HashMap::new();

        // Create output directory
        fs::create_dir_all(&output_dir)
            .with_context(|| format!("Failed to create output directory: {}", output_dir.display()))?;

        // Submit download tasks for each package (handles both local and remote)
        for pkgkey in packages.keys() {
            let package = self.load_package_info(pkgkey)
                .map_err(|e| eyre!("Failed to load package info for key: {}: {}", pkgkey, e))?;
            let url = format!(
                "{}/{}",
                package.package_baseurl,
                package.location
            );

            // Use the larger of compressed size or installed size for download prioritization
            // This helps the download manager prioritize packages that are likely to take longer
            let size = if package.size > 0 {
                Some(package.size as u64)
            } else {
                None
            };

            // Submit download task with size information (handles both local and remote files)
            let task = DownloadTask::with_size(url.clone(), output_dir.clone(), 6, size, package.repodata_name.clone());
            submit_download_task(task)
                .with_context(|| format!("Failed to submit download task for {}", url))?;
            url_to_pkgkeys.entry(url).or_default().push(pkgkey.clone());
        }

        // Start processing download tasks
        DOWNLOAD_MANAGER.start_processing();

        Ok(url_to_pkgkeys)
    }

    /// Get the local file path for a downloaded package
    pub fn get_package_file_path(&mut self, pkgkey: &str) -> Result<String> {
        let package = self.load_package_info(pkgkey)
            .map_err(|e| eyre!("Failed to load package info for key: {}: {}", pkgkey, e))?;
        let url = format!(
            "{}/{}",
            package.package_baseurl,
            package.location
        );

        // Check if we have a download task for this URL
        let tasks = DOWNLOAD_MANAGER.tasks.lock()
            .map_err(|e| eyre!("Failed to lock tasks mutex: {}", e))?;

        if let Some(task) = tasks.get(&url) {
            // Return the final_path from the task
            return Ok(task.final_path.to_string_lossy().to_string());
        }

        // If no task exists, calculate the path as before (fallback)
        if url.starts_with("/") {
            // Local file - return the destination path in downloads cache
            let file_name = url.split('/').last()
                .ok_or_else(|| eyre!("Failed to extract filename from URL: {}", url))?;
            let dest_path = dirs().epkg_downloads_cache.join(file_name);
            Ok(dest_path.to_string_lossy().to_string())
        } else {
            // Remote file - use the URL to cache path conversion
            let cache_path = mirror::Mirrors::url_to_cache_path(&url, &package.repodata_name)
                .map_err(|e| eyre!("Failed to convert URL to cache path: {}: {}", url, e))?;
            Ok(cache_path.to_string_lossy().to_string())
        }
    }

    // Wait for all pending downloads to complete
    #[allow(dead_code)]
    pub fn wait_for_downloads(&self) -> Result<()> {
        DOWNLOAD_MANAGER.wait_for_all_tasks()
            .map_err(|e| eyre!("Failed to wait for download tasks to complete: {}", e))?;
        Ok(())
    }
}

// ============================================================================
// CHUNKED DOWNLOAD IMPLEMENTATION
// ============================================================================

/// Create and setup chunk tasks for large files
///
/// This function handles creating chunk tasks based on the task's file size and
/// how much has already been downloaded. It automatically checks if chunking
/// should be performed and logs the decision.
///
/// Cases handled:
/// 1. If task.file_size is None, no chunks are created
/// 2. If the file is smaller than MIN_FILE_SIZE_FOR_CHUNKING, no chunks are created
/// 3. If downloaded > 0, we skip chunks that are already downloaded
/// 4. The master task handles from current offset to next chunk boundary
///
/// Returns a vector of chunk tasks if chunks were created, empty vector otherwise
fn create_chunk_tasks(task: &DownloadTask) -> Result<()> {
    let chunk_count = {
        let chunks_guard = task.chunk_tasks.lock()
            .map_err(|e| eyre!("Failed to lock chunk tasks: {}", e))?;
        chunks_guard.len()
    };
    // Already created?
    if chunk_count > 0 {
        if task.chunk_size.load(Ordering::Relaxed) == 0 {
            log::error!("create_chunk_tasks: chunk_size is 0 for {}", task.chunk_path.display());
            return Err(eyre!("create_chunk_tasks: chunk_size is 0 for {}", task.chunk_path.display()));
        }
        return Ok(());
    }

    // Don't chunk if we don't know the file size
    let file_size_val = task.file_size.load(Ordering::Relaxed);
    if file_size_val == 0 {
        log::debug!("Skip create chunks for {}: file size unknown (no Content-Length header)", task.chunk_path.display());
        return Ok(());
    };

    // Get the current on-disk size: task.resumed_bytes is not available now
    let resumed = get_existing_file_size(&task.chunk_path)?;

    // Don't chunk small files or chunk tasks themselves
    if task.is_chunk_task() || file_size_val < resumed + MIN_FILE_SIZE_FOR_CHUNKING {
        log::debug!("Skipping chunking for {}: is_chunk_task={}, size={} bytes, min_required={} bytes", task.chunk_path.display(),
                  task.is_chunk_task(), file_size_val, resumed + MIN_FILE_SIZE_FOR_CHUNKING);
        return Ok(());
    }

    log::debug!("Creating chunks for {} byte file with {} bytes resumed",
              file_size_val, resumed);

    // Calculate the next chunk boundary after the chunk start offset. If we are
    // exactly on a 1 MiB boundary we need to move to the _next_ boundary, otherwise we
    // would produce a zero-length master chunk (next_boundary == resumed).
    let next_boundary = if resumed == 0 {
        PGET_CHUNK_SIZE
    } else if (resumed & PGET_CHUNK_MASK) == 0 {
        resumed + PGET_CHUNK_SIZE
    } else {
        // Round up to the next 1 MiB boundary
        (resumed + PGET_CHUNK_MASK) & !PGET_CHUNK_MASK
    };

    // Validate next_boundary calculation to prevent 416 errors
    if next_boundary <= resumed {
        log::error!("Next boundary calculation resulted in invalid value: next_boundary={} <= resumed={} for {}",
                   next_boundary, resumed, task.url);
        return Err(eyre!("Next boundary calculation resulted in invalid value - this indicates a bug in chunk calculation"));
    }

    // Decide whether we actually need chunking. If the whole file fits into the range
    // up to `next_boundary`, chunking is unnecessary – let the master task handle it.
    if file_size_val <= next_boundary {
        log::debug!(
            "File size ({}) ≤ next boundary ({}); skipping chunk creation for {}",
            file_size_val,
            next_boundary,
            task.url
        );

        // Master task covers the whole file (chunk_size equals total file size)
        task.chunk_size.store(file_size_val, Ordering::Relaxed);
        return Ok(());
    }

    // Master task will handle from current offset (0) to `next_boundary`
    let master_chunk_size = next_boundary;

    // Sanity-check the calculation
    debug_assert!(master_chunk_size > resumed);

    // Update master task's chunk information
    task.chunk_size.store(master_chunk_size, Ordering::Relaxed);

    log::debug!("Master task will handle {} bytes starting from offset {}",
              master_chunk_size, resumed);

    // Starting offset for additional chunks is the next boundary
    let offset = next_boundary;

    // Create chunk tasks for the remaining parts of the file
    let mut chunk_tasks = Vec::new();

    // Calculate the number of full chunks we'll have
    let remaining_bytes = file_size_val - offset;
    let full_chunks = remaining_bytes / PGET_CHUNK_SIZE;
    let last_chunk_size = remaining_bytes % PGET_CHUNK_SIZE;

    // If the last chunk would be too small, merge it with the previous chunk
    let (full_chunks, last_chunk_size) = if last_chunk_size > 0 && last_chunk_size < CHUNK_MERGE_THRESHOLD {
        if full_chunks > 0 {
            // Merge the small last chunk with the last full chunk
            (full_chunks - 1, PGET_CHUNK_SIZE + last_chunk_size)
        } else {
            // If this is the only chunk, just use it as is
            (0, last_chunk_size)
        }
    } else {
        (full_chunks, last_chunk_size)
    };

    // Create all full chunks
    for i in 0..full_chunks {
        let chunk_offset = offset + (i as u64 * PGET_CHUNK_SIZE);

        let chunk_task = task.create_chunk_task(chunk_offset, PGET_CHUNK_SIZE);
        chunk_tasks.push(chunk_task);
    }

    // Handle the last chunk if there are remaining bytes
    if last_chunk_size > 0 {
        let chunk_offset = offset + (full_chunks as u64 * PGET_CHUNK_SIZE);

        let chunk_task = task.create_chunk_task(chunk_offset, last_chunk_size);
        chunk_tasks.push(chunk_task);
    }

    add_chunk_tasks(task, chunk_tasks, ChunkStatus::HasBeforehandChunk)
}

fn add_chunk_tasks(parent_task: &DownloadTask, chunk_tasks: Vec<Arc<DownloadTask>>, chunk_status: ChunkStatus) -> Result<()> {
    if cfg!(debug_assertions) && log::log_enabled!(log::Level::Debug) {
        validate_chunk_tasks(&parent_task, &chunk_tasks, chunk_status.clone())?;
    }

    // Add all chunk tasks to the parent task
    if let Ok(mut tasks_guard) = parent_task.chunk_tasks.lock() {
        let existing_count = tasks_guard.len();

        // Set chunk status to the requested status
        if let Err(e) = parent_task.set_chunk_status(chunk_status.clone()) {
            let error_msg = format!("add_chunk_tasks: failed to set chunk status to {:?}: {}", chunk_status, e);
            log::error!("{}", error_msg);
            return Err(eyre!(error_msg));
        }

        // Add the new chunk tasks
        if !tasks_guard.is_empty() {
            log::warn!("Already has chunks before adding chunk tasks for {}", parent_task.url);
        }
        tasks_guard.extend(chunk_tasks);
        let new_count = tasks_guard.len();

        log::info!("Successfully added {} chunk tasks to parent task ({} -> {} total)",
                  new_count - existing_count, existing_count, new_count);
        log::debug!("Parent task chunk status updated to: {:?}", chunk_status);
    } else {
        let error_msg = "add_chunk_tasks: failed to lock parent task's chunk list";
        log::error!("{}", error_msg);
        return Err(eyre!(error_msg));
    }

    Ok(())
}

fn validate_chunk_tasks(parent_task: &DownloadTask, chunk_tasks: &Vec<Arc<DownloadTask>>, chunk_status: ChunkStatus) -> Result<()> {
    let mut error_messages = Vec::new();

    // Debug dump of input parameters
    println!("add_chunk_tasks called with:");
    println!("  parent_task: {} (is_master: {}, is_chunk: {})",
                parent_task.url, parent_task.is_master_task(), parent_task.is_chunk_task());
    println!("  chunk_tasks count: {}", chunk_tasks.len());
    println!("  requested chunk_status: {:?}", chunk_status);
    println!("  parent_task current chunk_status: {:?}", parent_task.get_chunk_status());
    println!("  parent_task chunk_path: {}", parent_task.chunk_path.display());
    println!("  parent_task final_path: {}", parent_task.final_path.display());

    // Show on-disk file size for parent task if chunk_path exists
    if parent_task.chunk_path.exists() {
        match std::fs::metadata(&parent_task.chunk_path) {
            Ok(metadata) => {
                let size = metadata.len();
                let chunk_size = parent_task.chunk_size.load(Ordering::Relaxed);
                println!("  parent_task on-disk size: {} bytes (chunk_size: {} bytes, {}% of chunk_size)",
                         size, chunk_size, (size as f64 / chunk_size as f64 * 100.0) as u64);

                // Validate size doesn't exceed chunk size
                if size > chunk_size && chunk_size > 0 {
                    let error_msg = format!(
                        "parent_task on-disk size {} bytes exceeds chunk_size {} bytes",
                        size, chunk_size
                    );
                    log::error!("{}", error_msg);
                    error_messages.push(error_msg);
                }
            }
            Err(e) => {
                println!("  Could not get metadata for parent_task chunk_path: {}", e);
            }
        }
    }

    // Validation 1: Check if chunk_tasks is empty
    if chunk_tasks.is_empty() {
        println!("add_chunk_tasks: chunk_tasks is empty, returning early");
        return Ok(());
    }

    // Validation 2: Validate each chunk task
    for (i, chunk_task) in chunk_tasks.iter().enumerate() {
        let chunk_offset = chunk_task.chunk_offset.load(Ordering::Relaxed);
        let chunk_size = chunk_task.chunk_size.load(Ordering::Relaxed);
        let current_size = chunk_task.chunk_path.metadata()
            .map(|m| m.len())
            .unwrap_or(0);

        // Get the chunk name for display
        let display_name = chunk_task.chunk_path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<invalid-path>");

        println!("  Validating chunk {}: {} (offset: {}, size: {}, current: {} bytes) for parent {}",
                   i, display_name,
                   chunk_offset,
                   chunk_size,
                   current_size,
                   parent_task.chunk_path.display());

        // Ensure chunk tasks are actually chunk tasks
        if !chunk_task.is_chunk_task() {
            let error_msg = format!("add_chunk_tasks: chunk {} is not a chunk task (is_master: {}, is_chunk: {})",
                                   i, chunk_task.is_master_task(), chunk_task.is_chunk_task());
            log::error!("{}", error_msg);
            error_messages.push(error_msg);
        }

        // Validate size doesn't exceed chunk size
        if current_size > chunk_size && chunk_size > 0 {
            let error_msg = format!(
                "chunk {} on-disk size {} bytes exceeds chunk_size {} bytes",
                i, current_size, chunk_size
            );
            log::error!("{}", error_msg);
            error_messages.push(error_msg);
        }

        // Validate chunk offset and size are reasonable
        if chunk_size == 0 {
            let error_msg = format!("add_chunk_tasks: chunk {} has zero size", i);
            log::error!("{}", error_msg);
            error_messages.push(error_msg);
        }

        // Validate chunk path format
        let chunk_path_str = chunk_task.chunk_path.to_string_lossy();
        if !chunk_path_str.contains(".part-O") {
            let error_msg = format!("add_chunk_tasks: chunk {} has invalid path format: {}", i, chunk_path_str);
            log::error!("{}", error_msg);
            error_messages.push(error_msg);
        }
    }

    // Validation 3: Check for duplicate offsets
    let mut offsets: Vec<u64> = chunk_tasks.iter()
        .map(|ct| ct.chunk_offset.load(Ordering::Relaxed))
        .collect();
    offsets.sort();
    for i in 1..offsets.len() {
        if offsets[i] == offsets[i-1] {
            let error_msg = format!("add_chunk_tasks: duplicate chunk offset detected: {}", offsets[i]);
            log::error!("{}", error_msg);
            error_messages.push(error_msg);
        }
    }

    // Validation 4: Check for overlapping chunks and gaps (including parent task range)
    let mut sorted_chunks: Vec<_> = chunk_tasks.iter().collect();
    sorted_chunks.sort_by_key(|ct| ct.chunk_offset.load(Ordering::Relaxed));

    // Get parent task's chunk range
    let parent_offset = parent_task.chunk_offset.load(Ordering::Relaxed);
    let parent_size = parent_task.chunk_size.load(Ordering::Relaxed);
    let parent_end = parent_offset + parent_size;

    println!("Parent task range: [{}, {}) bytes ({} bytes) for {}",
             parent_offset, parent_end, parent_size, parent_task.chunk_path.display());

    // Check for overlapping chunks among new chunks
    for i in 1..sorted_chunks.len() {
        let prev_chunk = sorted_chunks[i-1];
        let curr_chunk = sorted_chunks[i];

        let prev_offset = prev_chunk.chunk_offset.load(Ordering::Relaxed);
        let prev_size = prev_chunk.chunk_size.load(Ordering::Relaxed);
        let curr_offset = curr_chunk.chunk_offset.load(Ordering::Relaxed);

        let prev_end = prev_offset + prev_size;

        // Check for overlapping chunks
        if curr_offset < prev_end {
            let error_msg = format!("add_chunk_tasks: overlapping chunks detected: chunk at offset {} ends at {}, but next chunk starts at {}",
                                   prev_offset, prev_end, curr_offset);
            log::error!("{}", error_msg);
            error_messages.push(error_msg);
        }

        // Check for gaps (optional - depends on requirements)
        if curr_offset > prev_end {
            log::warn!("add_chunk_tasks: gap detected between chunks: chunk at offset {} ends at {}, but next chunk starts at {} (gap: {} bytes)",
                      prev_offset, prev_end, curr_offset, curr_offset - prev_end);
        }
    }

    // Check for overlap with parent task range
    if !sorted_chunks.is_empty() {
        let first_chunk_offset = sorted_chunks[0].chunk_offset.load(Ordering::Relaxed);
        let last_chunk = sorted_chunks.last().unwrap();
        let last_chunk_offset = last_chunk.chunk_offset.load(Ordering::Relaxed);
        let last_chunk_size = last_chunk.chunk_size.load(Ordering::Relaxed);
        let last_chunk_end = last_chunk_offset + last_chunk_size;

        // Check if new chunks overlap with parent task range
        if first_chunk_offset < parent_end && last_chunk_end > parent_offset {
            let error_msg = format!("add_chunk_tasks: new chunks overlap with parent task range: parent [{}, {}), chunks [{}, {})",
                                   parent_offset, parent_end, first_chunk_offset, last_chunk_end);
            log::error!("{}", error_msg);
            error_messages.push(error_msg);
        }

        // Check for gaps between parent task and new chunks
        if first_chunk_offset > parent_end {
            log::warn!("add_chunk_tasks: gap detected between parent task and new chunks: parent ends at {}, but first chunk starts at {} (gap: {} bytes) for {}",
                      parent_end, first_chunk_offset, first_chunk_offset - parent_end, parent_task.chunk_path.display());
        } else if last_chunk_end < parent_offset {
            log::warn!("add_chunk_tasks: gap detected between new chunks and parent task: last chunk ends at {}, but parent starts at {} (gap: {} bytes) for {}",
                      last_chunk_end, parent_offset, parent_offset - last_chunk_end, parent_task.chunk_path.display());
        }
    }

    // Validation 5: Validate parent task and chunk size constraints
    let parent_size = parent_task.chunk_size.load(Ordering::Relaxed);
    let resumed_size = parent_task.resumed_bytes.load(Ordering::Relaxed);
    let file_size = parent_task.file_size.load(Ordering::Relaxed);

    // Check that parent size >= resumed size
    if parent_size < resumed_size {
        let error_msg = format!("add_chunk_tasks: parent size {} < resumed size {} - parent task cannot be smaller than what's already downloaded for {}",
                               parent_size, resumed_size, parent_task.chunk_path.display());
        log::error!("{}", error_msg);
        error_messages.push(error_msg);
    }

    // Check that chunks don't extend beyond file size
    if !sorted_chunks.is_empty() && file_size > 0 {
        let last_chunk = sorted_chunks.last().unwrap();
        let last_chunk_offset = last_chunk.chunk_offset.load(Ordering::Relaxed);
        let last_chunk_size = last_chunk.chunk_size.load(Ordering::Relaxed);
        let last_chunk_end = last_chunk_offset + last_chunk_size;

        if last_chunk_end > file_size {
            let error_msg = format!("add_chunk_tasks: last chunk extends beyond file size: chunk [{}, {}) > file size {}",
                                   last_chunk_offset, last_chunk_end, file_size);
            log::error!("{}", error_msg);
            error_messages.push(error_msg);
        }
    }

    // Validation 6: Check chunk status transitions and prevent adding when already has chunks
    let current_status = parent_task.get_chunk_status();

    // Prevent adding chunks if already has chunks (unless it's the same type and we want to add more)
    if matches!(current_status, ChunkStatus::HasBeforehandChunk | ChunkStatus::HasOndemandChunk) {
        let error_msg = format!("add_chunk_tasks: parent task already has chunks (status: {:?}), cannot add more", current_status);
        log::error!("{}", error_msg);
        error_messages.push(error_msg);
    }

    let valid_transitions = match current_status {
        ChunkStatus::NoChunk => vec![ChunkStatus::HasBeforehandChunk],
        ChunkStatus::NeedOndemandChunk => vec![ChunkStatus::HasOndemandChunk],
        ChunkStatus::HasOndemandChunk => vec![],
        ChunkStatus::HasBeforehandChunk => vec![],
    };

    if !valid_transitions.contains(&chunk_status) {
        let error_msg = format!("add_chunk_tasks: invalid chunk status transition from {:?} to {:?}",
                               current_status, chunk_status);
        log::error!("{}", error_msg);
        error_messages.push(error_msg);
    }

    // Return all collected errors if any exist
    if !error_messages.is_empty() {
        let combined_error = error_messages.join("; ");
        return Err(eyre!(combined_error));
    }

    Ok(())
}

/// Unified download task function that handles both master and chunk tasks
///
/// This function coordinates the download process by delegating to specialized functions.
/// Level 3: Download Strategy - coordinates download execution
fn download_chunk_task(task: &DownloadTask) -> Result<()> {
    let chunk_offset = task.chunk_offset.load(Ordering::Relaxed);
    let chunk_size = task.chunk_size.load(Ordering::Relaxed);
    log::debug!("download_chunk_task starting for {} (offset: {}, size: {})",
               task.chunk_path.display(), chunk_offset, chunk_size);

    // Phase 1: Setup and validation (split into concrete steps)
    task.setup_download_range();
    log::debug!("download_chunk_task: range_request set to {:?} for {}",
               task.get_range_request(), task.chunk_path.display());

    let (existing_bytes, is_complete) = check_existing_partfile(task)?;
    if is_complete {
        return Ok(());
    }

    let resolved_url = resolve_mirror_and_update_task(task)?;

    // Extract and hold the Mirror as RAII guard for automatic usage tracking
    // This will automatically call stop_usage_tracking() when the function ends
    let _mirror_guard = {
        let mut mirror_guard = task.mirror_inuse.lock()
            .map_err(|e| eyre!("Failed to lock mirror mutex: {}", e))?;
        mirror_guard.take() // Take ownership of the Mirror if present, will be dropped when function ends
    };

    // Phase 2: Execute HTTP request
    let mut response = execute_download_request(task, &resolved_url, existing_bytes)?;

    // Process the response headers and metadata
    process_download_response(task, &response, &resolved_url, existing_bytes)?;

    // Execute the main download stream processing
    let chunk_append_offset = process_chunk_download_stream(
        &mut response,
        task,
        existing_bytes,
    )?;

    // Finalize download with progress updates and logging
    finalize_chunk_download(task, chunk_append_offset, existing_bytes)?;

    // Validate individual chunk size if this is a chunk task or master task with chunk_size set
    let expected_chunk_size = task.chunk_size.load(Ordering::Relaxed);
    if expected_chunk_size > 0 {
        // For chunk tasks and master tasks with chunking, validate against the expected chunk size
        log::debug!("download_chunk_task: Validating chunk size for {} - downloaded: {}, expected: {}",
                   task.chunk_path.display(), chunk_append_offset, expected_chunk_size);
        validate_download_size(chunk_append_offset, expected_chunk_size, &task.chunk_path)?;
        log::debug!("download_chunk_task: Size validation passed for {}", task.chunk_path.display());
    }

    // Log download completion
    log_download_completion(task, &resolved_url);

    Ok(())
}

/// Process HTTP response and execute content download
/// Level 4: Response Processing - handles HTTP response validation and content download
fn process_download_response(
    task: &DownloadTask,
    response: &http::Response<ureq::Body>,
    resolved_url: &str,
    existing_bytes: u64
) -> Result<()> {
    // Check for unchanged file case
    // Handle 304 Not Modified responses for ETag conditional requests
    if response.status() == 304 {
        return handle_304_not_modified_response(task);
    }

    let metadata = extract_server_metadata(task, response);

    log::debug!("process_download_response for {} chunk: {}, metadata: remote_size={:?}, etag={:?}, last_modified={:?}, response: {:?}",
               resolved_url, task.chunk_path.display(), metadata.remote_size, metadata.etag, metadata.last_modified, response);

    // Store/validate metadata for consistency
    if task.is_master_task() {
        // For mutable files, check if we should redownload
        if matches!(task.file_type, FileType::Mutable) {
            let decision = should_redownload(task, &metadata)?;
            if matches!(decision, CacheDecision::UseCache { .. }) {
                return handle_304_not_modified_response(task);
            }
        }

        if let Ok(mut master_metadata) = task.master_metadata.lock() {
            *master_metadata = Some(metadata.clone());
        }
        save_pget_status(task, &metadata)?;
    } else {
        // For chunk tasks, validate against master metadata
        if let Ok(master_metadata_guard) = task.master_metadata.lock() {
            if let Some(ref master_metadata) = *master_metadata_guard {
                if !metadata.matches_with(master_metadata) {
                    add_url_to_mirror_skip_list(task);
                    return Err(eyre!(
                            "Chunk metadata conflicts with master metadata. Chunk: {:?}, Master: {:?}",
                            metadata,
                            master_metadata
                    ));
                }
            }
        }
    }

    let range_request_type = task.get_range_request();
    log::debug!("process_download_response: range_request={:?}, response_status={}, chunk_path={}",
               range_request_type, response.status(), task.chunk_path.display());

    if range_request_type != RangeRequest::None {
        // For chunk tasks, validate we got partial content
        if response.status() == 200 {
            // Server ignoring Range header - would corrupt chunk
            log::warn!("CORRUPTION PREVENTED: Server returned HTTP 200 instead of 206 for range request to {} (chunk: {})",
                       resolved_url, task.chunk_path.display());
            if let Err(e) = mirror::append_http_log(resolved_url, mirror::HttpEvent::NoRange) {
                log::warn!("Failed to log chunk range error: {}", e);
            }
            return Err(eyre!("Server returned 200 instead of 206 for range request - would corrupt chunk"));
        }

        if response.status() != 206 {
            // Resume failed, restart from beginning
            if task.chunk_path.exists() {
                fs::remove_file(&task.chunk_path)?;
            }
            task.resumed_bytes.store(0, Ordering::Relaxed);
            log::debug!("Server doesn't support resume, restarting download for {}", task.chunk_path.display());
            return Err(eyre!("Server returned {} for range request", response.status()));
        }
    } else {
        log::debug!("process_download_response: No range request validation needed for {}", task.chunk_path.display());
    }

    // Validate response and handle resume logic for master tasks
    validate_response_content_type(response, resolved_url, task)?;

    if task.is_master_task() {
        // Setup file size and progress tracking for master tasks
        if task.file_size.load(Ordering::Relaxed) == 0 {
            if let Some(remote_size) = get_remote_size(task, response) {
                task.file_size.store(remote_size, Ordering::Relaxed);
                task.chunk_size.store(remote_size, Ordering::Relaxed);
                log::debug!("Remote size determined: {} for {}", remote_size, task.chunk_path.display());
            }
        }

        let file_size_val = task.file_size.load(Ordering::Relaxed);
        if file_size_val > 0 {
            task.set_length(file_size_val);
        }
        task.set_position(existing_bytes);
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

/// Wait for chunks to complete and stream their data to the data channel one by one
///
/// CRITICAL STREAMING REQUIREMENT:
/// This function MUST process chunks one by one as they complete, NOT wait for all chunks
/// to finish before processing. This enables real-time streaming of chunk data to the
/// data channel as each chunk becomes available. The streaming behavior is essential for
/// memory efficiency and responsiveness.
///
/// FAILURE/RETRY COORDINATION LOGIC:
/// When a chunk fails, we don't immediately return an error. Instead:
/// 1. If attempt_number < max_retries: increment attempt_number and reset status to Pending
///    This allows start_chunks_processing() to retry the chunk automatically
/// 2. If max_retries exceeded: record the failure but continue processing other chunks
///    This prevents the next master task retry from conflicting with in-progress chunk downloads
/// 3. After all chunks are processed, if any failures occurred, return an error
///    This triggers download_file_with_retries() to retry the entire master task,
///    which will restart all failed chunks from scratch
///
/// This coordination ensures that:
/// - Individual chunk failures don't immediately abort the entire download
/// - Failed chunks get proper retry attempts within the chunking system
/// - Only after all retry attempts are exhausted does the master task retry
/// - No conflicts occur between chunk retries and master task retries
/// Process a completed chunk by streaming data and merging to master file
fn merge_completed_chunk(
    master_task: &DownloadTask,
    chunk_task: &DownloadTask,
    data_channels: &[Sender<Vec<u8>>],
    chunk_index: i32
) -> Result<()> {
    let chunk_offset = chunk_task.chunk_offset.load(Ordering::Relaxed);
    let chunk_size = chunk_task.chunk_size.load(Ordering::Relaxed);
    log::debug!("merge_completed_chunk: Chunk {} at offset {} completed (size: {} bytes, path: {}, url: {})",
               chunk_index, chunk_offset, chunk_size, chunk_task.chunk_path.display(), chunk_task.get_resolved_url());

    // Process the completed chunk immediately (STREAMING)
    if chunk_task.chunk_path.exists() {
        // Re-read the actual file length to ensure perfect consistency.
        let actual_size = fs::metadata(&chunk_task.chunk_path)
            .map(|m| m.len())
            .unwrap_or(0);
        validate_chunk_file_boundaries(&chunk_task, actual_size)?;

        // If we have data channels, stream the chunk data to all of them
        if !data_channels.is_empty() {
            log::debug!("Streaming chunk {} data to {} channels from {}", chunk_index, data_channels.len(), chunk_task.chunk_path.display());
            // For chunk streaming, we bypass the guards since this is fresh data being streamed
            send_chunk_to_all_channels(&chunk_task, &chunk_task.chunk_path, data_channels)?;
        }

        // Decide whether we really need to append this chunk.
        let target_offset = chunk_task.chunk_offset.load(Ordering::Relaxed);
        let master_current_size = fs::metadata(&master_task.chunk_path)
            .map(|m| m.len())
            .unwrap_or(0);

        if target_offset < master_current_size {
            // This chunk's data is already present – likely from an earlier attempt.
            log::debug!(
                "Skipping merge of chunk {} ({}): offset {} already within master size {} (master: {})",
                chunk_index, chunk_task.chunk_path.display(), target_offset, master_current_size, master_task.chunk_path.display()
            );
        } else if target_offset == master_current_size {
            // Safe to append – the chunk starts exactly where the current file ends.
            if let Err(e) = append_file_to_file(&chunk_task.chunk_path, &master_task.chunk_path) {
                log::warn!(
                    "Failed to append chunk {} ({}) to target file ({}) at offset {}: {}",
                    chunk_index, chunk_task.chunk_path.display(), master_task.chunk_path.display(), target_offset, e
                );
            } else {
                log::debug!("Appended chunk {} ({}) to target file ({})",
                           chunk_index, chunk_task.chunk_path.display(), master_task.chunk_path.display());

                // Extend master task boundary so merge validation stays consistent.
                let appended_size = chunk_task.chunk_size.load(Ordering::Relaxed);
                master_task.chunk_size.fetch_add(appended_size, Ordering::Relaxed);
            }
        } else {
            // target_offset > master_current_size → gap should never happen; log an error.
            log::error!(
                "Gap detected when merging chunk {} ({}): master size {} < chunk offset {} (master: {})",
                chunk_index, chunk_task.chunk_path.display(), master_current_size, target_offset, master_task.chunk_path.display()
            );
        }

        // Remove the temporary part file now that we've processed it
        if let Err(e) = fs::remove_file(&chunk_task.chunk_path) {
            log::warn!(
                "Failed to clean up chunk file {}: {}",
                chunk_task.chunk_path.display(),
                e
            );
        } else {
            log::debug!("Cleaned up chunk file: {}", chunk_task.chunk_path.display());
        }

    } else {
        log::warn!("Chunk file not found: {}", chunk_task.chunk_path.display());
    }

    Ok(())
}

/// Handle a failed chunk by retrying or marking as failed
/// Returns true if the chunk should be retried, false if it failed permanently
fn handle_failed_chunk(
    chunk_task: &DownloadTask,
    parent_task: &DownloadTask,
    chunk_index: i32,
    error: &str
) -> bool {
    let current_attempt = chunk_task.attempt_number.load(Ordering::SeqCst);

    if current_attempt < parent_task.max_retries {
        // Retry the chunk: increment attempt number and reset status to Pending
        chunk_task.attempt_number.fetch_add(1, Ordering::SeqCst);

        if let Ok(mut status) = chunk_task.status.lock() {
            *status = DownloadStatus::Pending;
            let chunk_offset = chunk_task.chunk_offset.load(Ordering::Relaxed);
            log::info!("Retrying chunk {} at offset {} (attempt {}/{}): {}",
                     chunk_index, chunk_offset, current_attempt + 1, parent_task.max_retries, error);
        }

        true // Retry the chunk
    } else {
        // Max retries exceeded - record failure
        let chunk_offset = chunk_task.chunk_offset.load(Ordering::Relaxed);
        log::error!("Chunk {} at offset {} failed after {} attempts: {}",
                  chunk_index, chunk_offset, parent_task.max_retries, error);

        false // Don't retry, mark as permanently failed
    }
}

/// Update progress display for chunks that are still pending or downloading
fn update_chunk_progress(
    chunk_task: &DownloadTask,
    master_task: &DownloadTask
) {
    // Update progress with current total
    let (total_received, total_reused, downloading_chunks) = master_task.get_total_progress_bytes();
    let total_progress = total_received + total_reused;

    // Update progress bar message with chunk count if there are downloading chunks
    let resolved_url = chunk_task.resolved_url.lock()
        .map(|r| r.clone())
        .unwrap_or_else(|_| chunk_task.url.clone());

    master_task.set_message(format_progress_message(&resolved_url, downloading_chunks));
    master_task.set_position(total_progress);
    log::trace!("Chunk progress update: {} bytes received", total_received);
}

fn wait_for_chunks_and_merge(master_task: &DownloadTask) -> Result<()> {
    // Check if we have any chunks to process at all
    let chunk_count = {
        let chunks_guard = master_task.chunk_tasks.lock()
            .map_err(|e| eyre!("Failed to lock chunk tasks: {}", e))?;
        chunks_guard.len()
    };

    if chunk_count == 0 {
        return Ok(()); // No chunks to wait for
    }

    log::debug!("Master task waiting for {} chunks for {}", chunk_count, master_task.url);

    // Check if we have data channels to stream to
    let data_channels = master_task.get_all_data_channels();
    let mut any_fail = false; // Track if any chunks failed after exhausting retries

    // Initialize last_merged_offset to master_task's chunk_offset + chunk_size
    // This accounts for the existing file content that the master task represents
    let master_chunk_offset = master_task.chunk_offset.load(Ordering::Relaxed);
    let master_chunk_size = master_task.chunk_size.load(Ordering::Relaxed);
    let mut last_merged_offset: u64 = master_chunk_offset + master_chunk_size;

    // Process all chunks in the 3-level architecture
    // Level 1: Master task (already handled by initialization)
    // Level 2: L2 chunks (direct children of master)
    // Level 3: L3 chunks (children of L2 chunks)
    process_chunks_at_level(master_task, master_task, &data_channels, &mut last_merged_offset, &mut any_fail, 2)?;

    // Handle any failures that occurred after exhausting retries
    // This triggers download_file_with_retries() to retry the entire master task
    if any_fail {
        return Err(eyre!("One or more chunks failed after exhausting retries - master task will retry"));
    }

    log::debug!("All chunks processed for {}", master_task.url);
    Ok(())
}

/// Process chunks at a specific level in the 3-level architecture
///
/// This function recursively processes chunks at each level:
/// - Level 2: Direct children of master task (L2 chunks)
/// - Level 3: Children of L2 chunks (L3 chunks)
///
/// It handles the streaming behavior and maintains proper ordering for merge validation.
fn process_chunks_at_level(
    master_task: &DownloadTask,
    parent_task: &DownloadTask,
    data_channels: &[Sender<Vec<u8>>],
    last_merged_offset: &mut u64,
    any_fail: &mut bool,
    level: usize,
) -> Result<()> {
    // Get chunks for this level
    let mut chunks = {
        let chunks_guard = parent_task.chunk_tasks.lock()
            .map_err(|e| eyre!("Failed to lock chunk tasks for level {}: {}", level, e))?;
        chunks_guard.clone()
    };

    if chunks.is_empty() {
        return Ok(()); // No chunks at this level
    }

    log::debug!("Processing {} chunks at level {} for {}", chunks.len(), level, parent_task.url);

    // Process chunks one by one in order until all are complete
    // STREAMING BEHAVIOR: We process each chunk as soon as it's ready, not all at once
    while !chunks.is_empty() {
        // Always process the first chunk in the list (they're already in order)
        let chunk = &chunks[0];
        let chunk_index = chunks.len() as i32; // For logging (chunks.len() - index from end)

        // Check chunk status without holding any locks
        match chunk.get_status() {
            DownloadStatus::Completed => {
                if !*any_fail {
                    // Perform the actual merge/stream processing
                    merge_completed_chunk(master_task, chunk, data_channels, chunk_index)?;

                    // Validate chunk merge integrity and update tracking
                    let expected_after = validate_chunk_merge_integrity(master_task, chunk, *last_merged_offset)?;
                    *last_merged_offset = expected_after;
                }

                // Process any L3 chunks if this is an L2 chunk
                if level == 2 {
                    process_chunks_at_level(master_task, chunk, data_channels, last_merged_offset, any_fail, 3)?;
                }

                // Remove this chunk from the list
                chunks.remove(0);

                // Update the parent task's chunk list
                {
                    let mut chunks_guard = parent_task.chunk_tasks.lock()
                        .map_err(|e| eyre!("Failed to lock chunk tasks for removal: {}", e))?;
                    if !chunks_guard.is_empty() {
                        chunks_guard.remove(0);
                    }
                }
            },
            DownloadStatus::Failed(ref err) => {
                // Handle chunk failure and retry logic
                if handle_failed_chunk(chunk, parent_task, chunk_index, err) {
                    // Chunk will be retried - don't remove it
                    // Sleep to allow retry to happen
                    std::thread::sleep(std::time::Duration::from_millis(CHUNK_SLEEP_DURATION_MS));
                } else {
                    // Max retries exceeded - record failure and remove chunk
                    *any_fail = true;
                    chunks.remove(0);

                    // Update the parent task's chunk list
                    {
                        let mut chunks_guard = parent_task.chunk_tasks.lock()
                            .map_err(|e| eyre!("Failed to lock chunk tasks for removal: {}", e))?;
                        if !chunks_guard.is_empty() {
                            chunks_guard.remove(0);
                        }
                    }
                }
            },
            DownloadStatus::Pending | DownloadStatus::Downloading => {
                // Chunk is not ready yet, continue waiting
                update_chunk_progress(chunk, master_task);

                // Sleep WITHOUT holding any locks
                std::thread::sleep(std::time::Duration::from_millis(CHUNK_SLEEP_DURATION_MS));
            }
        }
    }

    Ok(())
}

/// Append the contents of one file to another
fn append_file_to_file(source_path: &Path, target_path: &Path) -> Result<()> {
    // Create target file if it doesn't exist
    if !target_path.exists() {
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }
        File::create(target_path)?
            .sync_all()?;
    }

    // Open source file for reading
    let mut source = map_io_error(File::open(source_path), "open source file", source_path)?;

    // Open target file for appending
    let mut target = map_io_error(
        OpenOptions::new()
            .write(true)
            .append(true)
            .open(target_path),
        "open target file",
        target_path
    )?;

    // Copy data from source to target
    map_io_error(std::io::copy(&mut source, &mut target), "append file", source_path)?;

    // Ensure data is written to disk
    map_io_error(target.sync_all(), "sync target file", target_path)?;

    Ok(())
}

/// Calculate ETA for a single task based on its current progress and download rate
/// Does not cover child chunks - calculates only for this task alone
/// Calculate ETA for a single task and update atomic fields
/// Returns (eta_seconds, throughput_bps, remaining_bytes, total_progress, chunk_size)
fn update_single_task_eta(task: &DownloadTask) -> (u64, u64, u64, u64, u64) {
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
fn collect_task_eta_stats(
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
fn update_global_stats(
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
    if let Ok(mut stats_guard) = DOWNLOAD_MANAGER.stats.lock() {
        *stats_guard = new_stats.clone();
    }

    global_ideal_eta
}

// Rate-limit to once per second
fn dump_global_stats_ratelimit(
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
fn dump_global_stats(
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

    let max_show_items = MAX_DISPLAY_STATS;
    // Log individual task stats (limited to prevent spam)
    for (i, stat) in debug_stats.iter().take(max_show_items).enumerate() {
        println!("Task {}: {}", i + 1, stat);
    }
    if debug_stats.len() > max_show_items {
        println!("... and {} more tasks", debug_stats.len() - max_show_items);
    }
}

/// Check if a task may be suitable for ondemand chunking
///
/// This function encapsulates the logic for determining whether a download task
/// could be a candidate for ondemand chunking. It checks:
///
/// 1. If the chunk status is NoChunk or NeedOndemandChunk
/// 2. If the remaining size is at least 2 * ONDEMAND_CHUNK_SIZE (512KB)
///
/// The actual chunking decision and creation is handled by the global scheduler in
/// collect_pending_chunks() for global optimized decision and executed in individual task threads
/// to avoid race conditions.
fn may_ondemand_chunking(task: &DownloadTask) -> bool {

    let file_size = task.file_size.load(Ordering::Relaxed);
    if file_size == 0 {
        return false; // Unknown file size = no clue at all
    }

    let chunk_status = task.get_chunk_status();
    if !matches!(chunk_status, ChunkStatus::NoChunk | ChunkStatus::NeedOndemandChunk) {
        return false;
    }

    // Check if there's enough remaining data to make chunking worthwhile
    task.remaining() >= 2 * ONDEMAND_CHUNK_SIZE  // 512KB
}

/// Create on-demand chunk tasks during download
///
/// This function modifies the parent task's chunk range and creates additional 256KB chunk tasks
/// when a download is slow and we want to parallelize it further. The parent task is modified to
/// cover from current position to the next 256KB boundary, then additional chunks are created
/// for the remaining data.
///
/// Returns the number of chunk tasks created.
// Input: existing_bytes=400KB, remaining_size=900KB
// Output: Master task modified + 4 chunks created
//
// Master Task (Modified):  400KB → 512KB  (112KB)
// Chunk 1:                 512KB → 768KB  (256KB)
// Chunk 2:                 768KB → 1024KB (256KB)
// Chunk 3:                 1024KB → 1280KB (256KB)
// Chunk 4:                 1280KB → 1300KB (20KB final)
//
// Result: 5 parallel downloads (1 parent + 4 chunks)
//
// Step 1: Modify parent task to cover existing_bytes → next_boundary
// Step 2: Create 256KB chunks from next_boundary → end
// Step 3: Add all chunks to parent task atomically
fn create_ondemand_chunks(task: &DownloadTask, chunk_append_offset: u64, remaining_size: u64) -> Result<usize> {
    // Add debug information about the current state
    log::debug!("create_ondemand_chunks: starting for {} with append_offset={}, remaining_size={}, current_chunk_size={}",
               task.chunk_path.display(), chunk_append_offset, remaining_size, task.chunk_size.load(Ordering::Relaxed));

    if remaining_size < ONDEMAND_CHUNK_SIZE * 2 {
        return Err(eyre!("Skip ondemand chunking for {} because remaining size {} is less than 2 * ONDEMAND_CHUNK_SIZE ({})",
                         remaining_size, ONDEMAND_CHUNK_SIZE * 2, task.chunk_path.display()));
    }

    // Calculate the next 256KB boundary after current position.
    // If we are already aligned to a boundary (i.e. chunk_append_offset is an exact multiple
    // of ONDEMAND_CHUNK_SIZE) we must advance by one full chunk; otherwise `next_boundary`
    // would equal `chunk_append_offset`, producing a zero-length parent chunk and triggering
    // errors later when `create_chunk_tasks` is called again.

    let chunk_offset = task.chunk_offset.load(Ordering::Relaxed);
    let final_append_offset = chunk_append_offset + chunk_offset;
    let next_boundary = if (final_append_offset & ONDEMAND_CHUNK_SIZE_MASK) == 0 {
        final_append_offset + ONDEMAND_CHUNK_SIZE
    } else {
        (final_append_offset + ONDEMAND_CHUNK_SIZE_MASK) & !ONDEMAND_CHUNK_SIZE_MASK
    };

    let total_size = final_append_offset + remaining_size;

    // Modify parent task to cover from current position to next 256KB boundary
    let parent_chunk_size = std::cmp::min(next_boundary - chunk_offset, remaining_size);

    // Update parent task's chunk information
    task.chunk_size.store(parent_chunk_size, Ordering::Relaxed);

    log::debug!(
        "Modified parent task range: [{}, {}) ({} bytes) for {}",
        final_append_offset,
        final_append_offset + parent_chunk_size,
        parent_chunk_size,
        task.chunk_path.display()
    );

    // Create additional 256KB chunks from next boundary to end of file
    let mut chunk_tasks = Vec::new();
    let mut offset = next_boundary;

    while offset < total_size {
        let chunk_size = std::cmp::min(ONDEMAND_CHUNK_SIZE, total_size - offset);
        let chunk_task = task.create_chunk_task(offset, chunk_size);
        chunk_tasks.push(chunk_task);
        offset += chunk_size;
    }

    // Add all chunk tasks to the parent task using the unified add_chunk_tasks function
    add_chunk_tasks(task, chunk_tasks.clone(), ChunkStatus::HasOndemandChunk)?;

    log::info!(
        "Created {} on-demand chunks (256KB each) for {} bytes remaining, parent covers {}→{} bytes",
        chunk_tasks.len(), remaining_size, chunk_offset, next_boundary
    );

    Ok(chunk_tasks.len())
}

// ============================================================================
// PROCESS COORDINATION
// ============================================================================

/// Helper function to generate PID file path for a given final path
fn get_pid_file_path(final_path: &Path) -> PathBuf {
    utils::append_suffix(final_path, "download.pid")
}

/// Helper function to generate temporary PID file path for a given final path
fn get_temp_pid_file_path(final_path: &Path) -> PathBuf {
    utils::append_suffix(final_path, "download.pid.tmp")
}

/// Create a PID file for download coordination and clean up stale PID files
fn create_pid_file(final_path: &Path) -> Result<PathBuf> {
    let pid_file = get_pid_file_path(final_path);

    // Check for existing downloads and clean up stale PID files
    check_and_cleanup_existing_downloads(final_path)?;

    // Ensure the parent directory exists
    if let Some(parent) = pid_file.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directory for {}: {}", pid_file.display(), parent.display()))?;
    }

    let pid = std::process::id();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let pid_content = format!("pid={}\ntime={}\n", pid, timestamp);

    // Try to create the PID file atomically
    let temp_pid_file = get_temp_pid_file_path(final_path);
    fs::write(&temp_pid_file, pid_content)?;

    // Atomic rename
    fs::rename(&temp_pid_file, &pid_file)?;

    log::debug!("Created PID file: {}", pid_file.display());
    Ok(pid_file)
}

/// Check if a PID file represents an active download
fn is_pid_file_active(pid_file: &Path) -> bool {
    if !pid_file.exists() {
        return false;
    }

    let content = match fs::read_to_string(pid_file) {
        Ok(content) => content,
        Err(_) => return false,
    };

    // Parse the new format: pid=123\ntime=456\n
    let mut pid_opt = None;

    for line in content.lines() {
        if let Some(value) = line.strip_prefix("pid=") {
            pid_opt = value.parse::<u32>().ok();
            break;
        }
    }

    let pid = match pid_opt {
        Some(pid) => pid,
        None => return false,
    };

    // Get current process ID
    let current_pid = std::process::id();

    // Check if PID in file matches current process ID
    if pid == current_pid {
        return false;
    }

    // If not our PID, check if the process is still running (Unix-like systems)
    #[cfg(unix)]
    {
        use std::process::Command;
        match Command::new("kill").args(&["-0", &pid.to_string()]).output() {
            Ok(output) => output.status.success(),
            Err(_) => false,
        }
    }

    // For Windows or if we can't check, assume it's active for safety
    #[cfg(not(unix))]
    {
        true
    }
}

/// Clean up PID file after download completion
fn cleanup_pid_file(pid_file: &Path) -> Result<()> {
    if pid_file.exists() {
        fs::remove_file(pid_file)?;
        log::debug!("Cleaned up PID file: {}", pid_file.display());
    }
    Ok(())
}

/// Check for existing downloads and clean up stale PID files
fn check_and_cleanup_existing_downloads(final_path: &Path) -> Result<()> {
    let pid_file = get_pid_file_path(final_path);

    if pid_file.exists() {
        if is_pid_file_active(&pid_file) {
            return Err(eyre!("Another download process is already active for: {}", final_path.display()));
        } else {
            // Clean up stale PID file
            log::info!("Cleaning up stale PID file: {}", pid_file.display());
            cleanup_pid_file(&pid_file)?;
        }
    }

    Ok(())
}

/// Recover from crashed chunked downloads
fn find_parto_files(task: &DownloadTask) -> Result<Vec<PathBuf>> {
    let mut chunk_files = Vec::new();

    // Look for chunk files in the same directory as the main file
    if let Some(parent_dir) = task.chunk_path.parent() {
        if let Ok(entries) = std::fs::read_dir(parent_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(filename) = path.file_name() {
                    if let Some(filename_str) = filename.to_str() {
                        // Check if this is a chunk file for this download
                        if filename_str.starts_with(&format!("{}-O", task.chunk_path.file_name().unwrap().to_string_lossy())) {
                            chunk_files.push(path);
                        }
                    }
                }
            }
        }
    }

    Ok(chunk_files)
}

// Extract offset from filename like "file.part-O1048576"
fn extract_offset(path: &Path) -> u64 {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|s| s.split("-O").nth(1))
        .and_then(|offset_str| offset_str.parse().ok())
        .unwrap_or(0)
}

fn recover_chunks_for_parto_files(
    master_task: &DownloadTask,
    chunk_files: Vec<PathBuf>,
    expected_size: u64,
) -> Result<()> {
    if expected_size == 0 {
        return Ok(());
    }

    // 1. Collect and sort chunks
    let chunks = collect_and_sort_chunks(chunk_files)?;

    // 2. Validate chunks and master file
    let valid_chunks = validate_chunks(master_task, &chunks, expected_size)?;

    if valid_chunks.is_empty() {
        log::info!("No valid chunks found, starting fresh download");
        return Ok(());
    }

    // 3. Adjust chunks and create tasks
    adjust_and_create_chunks(master_task, valid_chunks, expected_size)
}

/// Apply stored metadata (timestamp and ETag) to the final downloaded file
fn finalize_file(task: &DownloadTask) -> Result<()> {
    log::debug!("finalize_file starting for {} -> {}", task.chunk_path.display(), task.final_path.display());

    // Check if the chunk file exists before attempting to rename
    if !task.chunk_path.exists() {
        return Err(eyre!("Chunk file does not exist: {}", task.chunk_path.display()));
    }

    // Validate that the completed download size matches the expected file size.
    // This prevents prematurely finalising a partially downloaded or oversized file.
    let expected_size = task.chunk_size.load(Ordering::Relaxed);
    if expected_size > 0 {
        let actual_size = fs::metadata(&task.chunk_path)?.len();
        if actual_size != expected_size {
            log::error!(
                "finalize_file: size mismatch for {} – actual {} bytes, expected {} bytes",
                task.chunk_path.display(), actual_size, expected_size
            );
            return Err(eyre!(
                "Downloaded file size {} does not match expected {} for {}",
                actual_size, expected_size, task.chunk_path.display()
            ));
        }
    }

    // Check if the final path already exists and remove it if it does
    if task.final_path.exists() {
        log::debug!("Final path already exists, removing: {}", task.final_path.display());
        fs::remove_file(&task.final_path)
            .with_context(|| format!("Failed to remove existing final file: {}", task.final_path.display()))?;
    }

    if let Ok(metadata_guard) = task.master_metadata.lock() {
        if let Some(metadata) = &*metadata_guard {
            // Apply Last-Modified timestamp from master_metadata
            if let Some(last_modified) = &metadata.last_modified {
                if let Ok(timestamp) = time::OffsetDateTime::parse(last_modified, &time::format_description::well_known::Rfc2822) {
                    let system_time = filetime::FileTime::from_system_time(timestamp.into());
                    if let Err(e) = filetime::set_file_mtime(&task.chunk_path, system_time) {
                        log::warn!("Failed to set mtime for {}: {}", task.chunk_path.display(), e);
                    }
                }
            }

            // Apply ETag
            if task.file_type == FileType::Mutable {
                if let Some(etag) = &metadata.etag {
                    if let Err(e) = task.save_etag(etag) {
                        log::warn!("Failed to save ETag for {}: {}", task.chunk_path.display(), e);
                    }
                }
            }
        }
    }

    // Perform the atomic rename operation
    log::debug!("Renaming {} to {}", task.chunk_path.display(), task.final_path.display());
    fs::rename(&task.chunk_path, &task.final_path)
        .with_context(|| format!("Failed to rename chunk file {} to final file {}",
                                task.chunk_path.display(), task.final_path.display()))?;

    log::debug!("Successfully finalized file: {}", task.final_path.display());
    Ok(())
}

/// Check if a chunk task is already complete and handle early completion
fn check_chunk_completion(task: &DownloadTask, existing_bytes: u64) -> Result<bool> {
    let chunk_size = task.chunk_size.load(Ordering::Relaxed);

    // A chunk is considered complete only when the on-disk size exactly matches the
    // expected chunk size. "Bigger than expected" indicates corruption and must not
    // be silently accepted.
    if chunk_size > 0 && existing_bytes == chunk_size {
        if task.is_chunk_task() {
            log::debug!("Chunk file already exists and is complete: {}", task.chunk_path.display());
        } else {
            log::debug!("Master chunk already complete (local {} == expected {}) for {}", existing_bytes, chunk_size, task.url);
        }

        // Mark bytes as reused and status as completed
        task.resumed_bytes.store(chunk_size, Ordering::Relaxed);
        task.received_bytes.store(0, Ordering::Relaxed);
        return Ok(true);
    }

    // Detect oversized files eagerly so they can be redownloaded instead of propagated
    if chunk_size > 0 && existing_bytes > chunk_size {
        log::error!(
            "Existing chunk file {} is larger than expected ({} > {}) – treating as corruption",
            task.chunk_path.display(), existing_bytes, chunk_size
        );
        // Cleanup corrupted chunk file immediately, so that the next retry starts with a pristine
        // chunk file and does not pick up invalid bytes that could cause persistent size mismatches.
        if task.chunk_path.exists() {
            match fs::remove_file(&task.chunk_path) {
                Ok(_) => log::debug!(
                    "check_chunk_completion: removed corrupt chunk file {} after size check",
                    task.chunk_path.display()
                ),
                Err(e) => log::warn!(
                    "check_chunk_completion: failed to remove corrupt chunk file {}: {}",
                    task.chunk_path.display(),
                    e
                ),
            }
            // Reset progress counters so resumed/received math is correct on retry
            task.resumed_bytes.store(0, Ordering::Relaxed);
            task.received_bytes.store(0, Ordering::Relaxed);
        }
        return Err(eyre!(
            "Corrupted chunk file: size {} exceeds expected {} for {}",
            existing_bytes, chunk_size, task.chunk_path.display()
        ));
    }
    Ok(false) // Task is not complete
}

/// Log download completion statistics
fn log_download_completion(
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
        if let Err(e) = mirror::append_download_log(
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

/// Clean cache decision logic replacing complex nested conditionals
fn should_redownload(
    task: &DownloadTask,
    server_metadata: &ServerMetadata
) -> Result<CacheDecision> {
    use std::time::Duration;

    let local_path = &task.final_path;
    if !local_path.exists() {
        return Ok(CacheDecision::RedownloadDueTo { reason: "Local file doesn't exist".to_string() });
    }

    // Get local file metadata
    let local_metadata = map_io_error(fs::metadata(local_path), "get local file metadata", local_path)?;
    let local_size = local_metadata.len();
    let local_last_modified_sys_time = local_metadata.modified()
        .map_err(|e| eyre!("Failed to get local file modification time: {}", e))?;
    let local_last_modified: OffsetDateTime = local_last_modified_sys_time.into();

    let remote_size_opt = server_metadata.remote_size;
    let remote_size = remote_size_opt.unwrap_or(0);

    // Detect 0-byte files early - always redownload them
    if local_size == 0 {
        log::warn!("Local file {} is 0 bytes - triggering redownload", local_path.display());
        return Ok(CacheDecision::RedownloadDueTo { reason: "Local file is 0 bytes".to_string() });
    }

    // For immutable files, we already know beforehand whether to UseCache/AppendDownload
    // So these are double checks serving as validation
    if task.file_type == FileType::Immutable {
        if let Some(remote_size_val) = remote_size_opt {
            if local_size == remote_size_val {
                return Ok(CacheDecision::UseCache { reason: "Immutable file size matches".to_string() });
            }
            if local_size < remote_size_val {
                return Ok(CacheDecision::AppendDownload { reason: format!("Append immutable file: local_size {} < remote_size {}", local_size, remote_size_val) });
            }

            // local_size > remote_size is a corruption case
            return Ok(CacheDecision::RedownloadDueTo { reason: format!("Corrupt immutable file: local_size {} > remote_size {}", local_size, remote_size_val) });
        } else {
            // Remote size unknown - can't validate, so redownload
            return Ok(CacheDecision::RedownloadDueTo { reason: "Remote size unknown, cannot validate immutable file".to_string() });
        }
    }

    // For mutable files, check timestamps if available
    let remote_ts_opt = server_metadata.last_modified.as_ref()
        .and_then(|s| parse_http_date(s).ok())
        .map(|st| OffsetDateTime::from(st));

    match remote_ts_opt {
        Some(remote_ts) if remote_size_opt.is_some() && remote_size == local_size => {
            let time_diff = if local_last_modified > remote_ts {
                (local_last_modified - remote_ts).unsigned_abs()
            } else {
                (remote_ts - local_last_modified).unsigned_abs()
            };

            // If local time is more recent than remote time, assume local file is newer
            if local_last_modified > remote_ts {
                Ok(CacheDecision::UseCache {
                    reason: format!("Local file is newer than remote (local: {}, remote: {})", local_last_modified, remote_ts)
                })
            }
            // If timestamps are within 10 minutes of each other, consider them the same
            else if time_diff <= Duration::from_secs(600) {
                Ok(CacheDecision::UseCache {
                    reason: format!("Size and timestamp match within 10min tolerance (remote: {}, local: {})", remote_ts, local_last_modified)
                })
            }
            else {
                let mut reasons = Vec::new();
                if remote_size != local_size {
                    reasons.push(format!("size mismatch: remote {}, local {}", remote_size, local_size));
                }
                if time_diff > Duration::from_secs(600) {
                    reasons.push(format!("timestamp mismatch (tolerance: 10min): remote {}, local {}", remote_ts, local_last_modified));
                }
                Ok(CacheDecision::RedownloadDueTo { reason: reasons.join(" and ") })
            }
        }
        Some(remote_ts) => {
            let mut reasons = Vec::new();
            if let Some(remote_size_val) = remote_size_opt {
                if remote_size_val != local_size {
                    reasons.push(format!("size mismatch: remote {}, local {}", remote_size_val, local_size));
                }
            } else {
                reasons.push("remote size unknown".to_string());
            }
            let time_diff = if local_last_modified > remote_ts {
                (local_last_modified - remote_ts).unsigned_abs()
            } else {
                (remote_ts - local_last_modified).unsigned_abs()
            };
            if time_diff > Duration::from_secs(600) {
                reasons.push(format!("timestamp mismatch (tolerance: 10min): remote {}, local {}", remote_ts, local_last_modified));
            }
            Ok(CacheDecision::RedownloadDueTo { reason: reasons.join(" and ") })
        }
        None if remote_size_opt.is_some() && remote_size == local_size => {
            // Only use cache if we actually know the remote size and it matches
            Ok(CacheDecision::UseCache { reason: "Size matches, no timestamp available".to_string() })
        }
        None => {
            // Remote size unknown or doesn't match - redownload
            if remote_size_opt.is_none() {
                Ok(CacheDecision::RedownloadDueTo {
                    reason: "Remote size unknown and no timestamp available".to_string()
                })
            } else {
                Ok(CacheDecision::RedownloadDueTo {
                    reason: format!("Size differs (remote {}, local {}) and no timestamp", remote_size, local_size)
                })
            }
        }
    }
}

// Helper function to parse HTTP date headers
fn parse_http_date(date_str: &str) -> Result<SystemTime> {
    log::debug!("Parsing HTTP date: {}", date_str);

    // Try parsing RFC 2822 format (most common HTTP date format)
    if let Ok(datetime) = OffsetDateTime::parse(date_str, &Rfc2822) {
        return Ok(datetime.into());
    }

    // Try parsing ISO format as fallback
    if let Ok(datetime) = OffsetDateTime::parse(date_str, &time::format_description::well_known::Iso8601::DEFAULT) {
        return Ok(datetime.into());
    }

    // Try parsing simple date formats
    let formats = [
        "%a, %d %b %Y %H:%M:%S GMT",
        "%A, %d-%b-%y %H:%M:%S GMT",
        "%a %b %d %H:%M:%S %Y",
    ];

    for format in &formats {
        if let Ok(parsed) = time::PrimitiveDateTime::parse(date_str, &time::format_description::parse(format).unwrap_or_default()) {
            let offset_dt = parsed.assume_utc();
            return Ok(offset_dt.into());
        }
    }

    Err(eyre!("Failed to parse HTTP date: {}", date_str))
}

/// Helper to safely remove files with consistent error handling
#[allow(dead_code)]
fn safe_remove_file(path: &Path, context: &str) -> Result<()> {
    fs::remove_file(path).map_err(|e| eyre!("Failed to remove {} file '{}': {}", context, path.display(), e))
}

/// Helper to safely log HTTP events with error handling
fn log_http_event_safe(url: &str, event: mirror::HttpEvent) {
    if let Err(e) = mirror::append_http_log(url, event) {
        log::warn!("Failed to log HTTP event for {}: {}", url, e);
    }
}

/// Helper for consistent error mapping patterns
fn map_io_error<T>(result: std::io::Result<T>, context: &str, path: &Path) -> Result<T> {
    result.map_err(|e| DownloadError::DiskError {
        details: format!("Failed to {} '{}': {} (line: {})", context, path.display(), e, line!())
    }.into())
}

/// Helper function to log errors with optional backtrace
fn log_error_with_backtrace<E: std::fmt::Display + std::fmt::Debug>(url: &str, error: &E) {
    log::error!("Download task failed for {}: {}", url, error);

    // Check if we should dump backtraces
    let should_dump_backtrace = cfg!(debug_assertions) ||
                               std::env::var("RUST_BACKTRACE").is_ok() ||
                               std::env::var("EPKG_BACKTRACE").is_ok();

    if should_dump_backtrace {
        log::error!("Full backtrace:\n{:?}", error);
    }
}

/// Helper to format progress messages with chunk counts
fn format_progress_message(resolved_url: &str, downloading_chunks: usize) -> String {
    if downloading_chunks == 0 {
        resolved_url.to_string()
    } else {
        format!("+{} {}", downloading_chunks, resolved_url)
    }
}

/// Setup file for download content writing
fn setup_download_file(task: &DownloadTask, existing_bytes: u64) -> Result<File> {
    let chunk_path = &task.chunk_path;

    if existing_bytes == 0 {
        if let Some(parent) = chunk_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| DownloadError::DiskError {
                    details: format!("Failed to create directory '{}': {}", parent.display(), e)
                })?;
        }
    };

    let mut file = map_io_error(
        OpenOptions::new()
            .create(true)
            .write(true)
            .append(false)              // Never use O_APPEND to prevent race conditions
            .open(chunk_path),
        "open file",
        chunk_path
    ).map_err(|e| DownloadError::DiskError {
        details: format!("setup_download_file failed for chunk_path {}: {}", chunk_path.display(), e)
    })?;

    // If file exists and we need to append, seek to the end to prevent overwriting
    if existing_bytes > 0 {
        file.seek(std::io::SeekFrom::Start(existing_bytes))
            .map_err(|e| DownloadError::DiskError {
                details: format!("Failed to seek to end of file {}: {}", chunk_path.display(), e)
            })?;
    }

    Ok(file)
}

/// Calculate bytes to write considering chunk boundaries
fn calculate_write_bytes(
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
fn write_chunk_data(
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

    return Ok(write_len);
}

/// Update progress tracking for download tasks
fn update_download_progress(
    task: &DownloadTask,
    last_update: &mut std::time::Instant
) {
    let now = std::time::Instant::now();
    if now.duration_since(*last_update) > Duration::from_millis(PROGRESS_UPDATE_INTERVAL_MS) {
        if task.is_master_task() {
            // For master tasks, show total progress across all chunks (reused + network bytes)
            let (total_received, total_reused, downloading_chunks) = task.get_total_progress_bytes();
            task.set_position(total_received + total_reused);

            // Update progress bar message with chunk count if there are downloading chunks
            let resolved_url = task.resolved_url.lock()
                .map(|r| r.clone())
                .unwrap_or_else(|_| task.url.clone());
            let message = format_progress_message(&resolved_url, downloading_chunks);
            task.set_message(message);
        }
        *last_update = now;
    }
}

/// Check for on-demand chunking execution (master tasks and L2 chunks)
///
/// This function checks if the task has been marked for ondemand chunking by the global
/// scheduler and executes the chunking if needed. It now supports both master tasks creating
/// L2 chunks and L2 chunks creating L3 chunks.
fn check_ondemand_chunking(
    task: &DownloadTask,
    chunk_append_offset: u64,
    _last_ondemand_check: &mut std::time::Instant
) {
    // Early return if not flagged for ondemand chunking
    if task.get_chunk_status() != ChunkStatus::NeedOndemandChunk {
        return;
    }

    let file_size_val = task.file_size.load(Ordering::Relaxed);
    // Early return if file size is invalid
    if file_size_val <= 0 {
        return;
    }

    let remaining_size = task.remaining();

    // Create on-demand chunks for remaining data
    match create_ondemand_chunks(task, chunk_append_offset, remaining_size) {
        Ok(chunk_count) => {
            let level = if task.is_master_task() { "L2" } else { "L3" };
            log::info!("Created {} on-demand {} chunks for {} bytes remaining", chunk_count, level, remaining_size);
            // Note: chunk status is now set by add_chunk_tasks() inside create_ondemand_chunks()
        }
        Err(_) => {
            log::warn!("Failed to create ondemand chunks, resetting status to NoChunk for {}", task.chunk_path.display());
            if let Err(e) = task.set_chunk_status(ChunkStatus::NoChunk) {
                log::warn!("Failed to reset chunk status: {}", e);
            }
        }
    }
}

/// Handle 304 Not Modified response
fn handle_304_not_modified_response(
    task: &DownloadTask,
) -> Result<()> {
    log::debug!("Received 304 Not Modified - file unchanged on server");
    task.set_message(format!("File unchanged, checking local copy - {}", task.final_path.display()));

    send_file_to_channel(task)
        .map_err(|e| eyre!("Failed to send cached file to channel: {}", e))?;

    Err(DownloadError::AlreadyComplete.into())
}


/// Check existing file size and validate chunk completion
/// Returns existing bytes and whether the chunk is already complete
fn check_existing_partfile(task: &DownloadTask) -> Result<(u64, bool)> {
    let chunk_path = &task.chunk_path;

    // Check existing file size for resumption
    let existing_bytes = get_existing_file_size(chunk_path)?;
    if existing_bytes > 0 {
        task.resumed_bytes.store(existing_bytes, Ordering::Relaxed);
        log::debug!("Resuming download from {} bytes for {}", existing_bytes, &task.url);

        // Only master task has data channel
        if let Some(channel) = task.get_data_channel() {
            log::debug!("Sending master task resumed data to channel for {}", task.chunk_path.display());
            send_chunk_to_channel(&task, &task.chunk_path, &channel)?;
        }
    }

    // Check if chunk task is already complete
    let is_complete = check_chunk_completion(task, existing_bytes)?;
    Ok((existing_bytes, is_complete))
}



/// Resolve mirror URL and update task with resolved URL and mirror
fn resolve_mirror_and_update_task(task: &DownloadTask) -> Result<String> {
    let url = &task.url;
    let need_range = task.get_range_request() != RangeRequest::None;

    // If URL doesn't contain $mirror, just update resolved URL
    if !url.contains("$mirror") {
        log::debug!("resolve_mirror_and_update_task: URL {} doesn't contain $mirror, using as-is", url);
        if let Ok(mut resolved) = task.resolved_url.lock() {
            *resolved = url.to_string();
        }
        return Ok(url.to_string());
    }

    log::debug!("resolve_mirror_and_update_task: Resolving mirror for URL {}", url);

    // Select mirror with usage tracking
    let selected_mirror = {
        let mut mirrors = mirror::MIRRORS.lock()
            .map_err(|e| eyre!("Failed to lock mirrors: {}", e))?;

        let mirror = mirrors.select_mirror_with_usage_tracking(need_range, Some(&task.url), &task.repodata_name)
            .map_err(|e| DownloadError::MirrorResolution {
                details: format!("{}", e)
            })?;

        log::debug!("resolve_mirror_and_update_task: Selected mirror {} for URL {} {}", mirror.url, url, &task.repodata_name);
        mirror
    };

    // Get distro directory for the selected mirror
    let distro = &channel_config().distro;
    let arch = &channel_config().arch;
    let distro_dir = mirror::Mirrors::find_distro_dir(&selected_mirror, distro, arch, &task.repodata_name);
    let final_distro_dir = if distro_dir.is_empty() { distro.to_string() } else { distro_dir };

    // Format mirror URL
    let url_formatted = {
        let mirrors = mirror::MIRRORS.lock()
            .map_err(|e| eyre!("Failed to lock mirrors: {}", e))?;
        mirrors.format_mirror_url(&selected_mirror.url, selected_mirror.top_level, &final_distro_dir)?
    };

    let resolved_url = url.replace("$mirror", &url_formatted);

    // Store the selected mirror in the task
    if let Ok(mut mirror_guard) = task.mirror_inuse.lock() {
        *mirror_guard = Some(selected_mirror);
    }

    // Update resolved URL in task
    if let Ok(mut resolved) = task.resolved_url.lock() {
        *resolved = resolved_url.clone();
    }

    Ok(resolved_url)
}

// ===========================
// File Validation Logic
// ===========================

/// Resolve mirror placeholder in URL with smart mirror selection
///
/// Uses pre-filtered mirrors and intelligent retry logic for optimal performance
/// Determine if a file is immutable based on its file path
/// Immutable files are those whose content won't change over time
fn is_immutable_filename(file_path: &str) -> bool {
    file_path.ends_with(".deb") ||
    file_path.ends_with(".rpm") ||
    file_path.ends_with(".apk") ||
    file_path.ends_with(".epkg") ||
    file_path.ends_with(".conda") ||
    file_path.contains("/by-hash/") ||
    file_path.ends_with(".gz") ||
    file_path.ends_with(".xz") ||
    file_path.ends_with(".zst")
}

/// Classify file type for integrity handling based on filename and path
fn classify_file_type(final_path: &Path, file_size: Option<u64>) -> FileType {
    let path_str = final_path.to_string_lossy();

    // Check for mutable repository metadata files first
    let file_name = final_path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");

    // Known mutable files
    if matches!(file_name,
        "Release" | "Release.gpg" | "InRelease" |
        "repomd.xml" | "repomd.xml.asc" |
        "APKINDEX.tar.gz" | "APKINDEX.tar.gz.sig" |
        "elf-loader" | "elf-loader.sig"
    ) {
        return FileType::Mutable;
    }

    // Check by path patterns for mutable files
    if path_str.contains("/Release") ||
       path_str.contains("/repomd.xml") ||
       path_str.contains("/APKINDEX") ||
       path_str.contains("/elf-loader") {
        return FileType::Mutable;
    }

    // Immutable files (packages) - require known size
    if file_size.is_some() && is_immutable_filename(&path_str) {
        return FileType::Immutable;
    }

    // Append-only files (future extension)
    // if path_str.contains("/epkg-index") {
    //     return FileType::AppendOnly;
    // }

    // Default classification based on size availability
    // Files with known size are more likely to be immutable packages
    if file_size.is_some() {
        FileType::Immutable
    } else {
        FileType::Mutable
    }
}

/// Validate existing final_file and determine appropriate download action
fn validate_existing_file(task: &DownloadTask) -> Result<ValidationResult> {
    let final_path = &task.final_path;
    let file_type = &task.file_type;
    let expected_size = task.file_size.load(Ordering::Relaxed);

    // Early return if file doesn't exist
    if !final_path.exists(){
        return Ok(ValidationResult::StartFresh);
    }

    // Get local file metadata
    let local_metadata = match fs::metadata(final_path) {
        Ok(meta) => meta,
        Err(e) => {
            log::warn!("Failed to read local file metadata for {}: {}", final_path.display(), e);
            return Ok(ValidationResult::StartFresh);
        }
    };

    let local_size = local_metadata.len();

    match file_type {
        FileType::Immutable | FileType::AppendOnly => {
            // For immutable and append-only files, we can trust size-based validation
            validate_immutable_file(task, local_size, expected_size, file_type)
        },
        FileType::Mutable => {
            // For mutable files, we need to check server metadata
            // This will be handled by download_file_with_integrity() which gets server metadata first
            log::info!("Mutable file {} exists, will validate against server metadata",
                      final_path.display());
            // the SkipDownload case will be checked after being able to resolve mirror and make request
            Ok(ValidationResult::StartFresh)
        }
    }
}

/// Handle size-based validation for immutable and append-only files
fn validate_immutable_file(
    task: &DownloadTask,
    local_size: u64,
    expected_size: u64,
    file_type: &FileType,
) -> Result<ValidationResult> {
    let final_path = &task.final_path;

    match file_type {
        FileType::Immutable => {
            if local_size == expected_size {
                log::info!("Immutable file {} already exists with correct size {}, treating as already downloaded",
                          final_path.display(), local_size);

                return Ok(ValidationResult::SkipDownload("File exists with correct size".to_string()));
            } else if local_size > expected_size {
                log::warn!("Immutable file {} has larger size than expected ({} > {}), file may be corrupt",
                          final_path.display(), local_size, expected_size);
                return Ok(ValidationResult::CorruptionDetected);
            } else {
                // local_size < expected_size - can resume from partial
                log::info!("Immutable file {} exists but incomplete ({} < {}), will resume download",
                          final_path.display(), local_size, expected_size);
                return Ok(ValidationResult::ResumeFromPartial);
            }
        }

        FileType::AppendOnly => {
            if local_size >= expected_size {
                log::info!("Append-only file {} already exists with sufficient size ({} >= {}), treating as complete",
                          final_path.display(), local_size, expected_size);

                return Ok(ValidationResult::SkipDownload("File exists with sufficient size".to_string()));
            } else {
                // local_size < expected_size - can resume from partial
                log::info!("Append-only file {} exists but incomplete ({} < {}), will resume download",
                          final_path.display(), local_size, expected_size);
                return Ok(ValidationResult::ResumeFromPartial);
            }
        }

        _ => unreachable!("This function only handles Immutable and AppendOnly file types")
    }
}

fn recover_parto_files(task: &DownloadTask) -> Result<ValidationResult> {
    let mut expected_size = task.file_size.load(Ordering::Relaxed);

    // Mutable files have no expected_size beforehand
    if task.file_type == FileType::Mutable {
        if let Some(pget_status) = load_pget_status(task)? {
            match fetch_server_metadata(task, &pget_status.url) {
                Ok(server_metadata) => {
                    // Check part files consistency using final_path since it's per-file not per-chunk info
                    if !server_metadata.matches_with(&pget_status.metadata) {
                        log::warn!("Server metadata conflicts with existing part files");
                    } else {
                        expected_size = server_metadata.remote_size.unwrap_or(0);
                    }
                }
                Err(e) => {
                    log::debug!("Failed to fetch server metadata for {}: {}", pget_status.url, e);
                    log::debug!("Will start fresh download due to metadata fetch failure");
                }
            }
        }
    }

    if expected_size == 0 {
        // cleanup_related_part_files() cleans up the pget status file together with
        // the part files, since they are tied together
        cleanup_related_part_files(task)?;
        return Ok(ValidationResult::StartFresh);
    }

    let parto_files = find_parto_files(task)?;

    if parto_files.is_empty() {
        return Ok(ValidationResult::StartFresh);
    }

    if let Err(e) = recover_chunks_for_parto_files(task, parto_files, expected_size) {
        log::warn!("Failed to recover from part files: {}", e);
        log::info!("Cleaning up invalid part files and starting fresh download");
        cleanup_related_part_files(task)?;
        return Ok(ValidationResult::StartFresh);
    }
    Ok(ValidationResult::ResumeFromPartial)
}

fn fetch_server_metadata(task: &DownloadTask, url: &str) -> Result<ServerMetadata> {
    let request_start = std::time::Instant::now();

    let client = task.get_client()?;
    let response = client.head(url).call()
        .with_context(|| format!("Failed to make HEAD request to {}", url))?;

    let latency = request_start.elapsed().as_millis() as u64;
    log_http_event_safe(url, mirror::HttpEvent::Latency(latency));

    if let Ok(mut guard) = task.range_request.lock() {
        *guard = RangeRequest::None;  // reset for correct get_remote_size()
    }
    Ok(extract_server_metadata(task, &response))
}

/// Get server metadata from HTTP response headers
fn extract_server_metadata(task: &DownloadTask, response: &http::Response<ureq::Body>) -> ServerMetadata {
    let remote_size = get_remote_size(task, response);
    let last_modified = response.headers().get("last-modified").map(|s| s.to_str().unwrap_or("").to_string());
    let etag = parse_etag(response);

    // Parse timestamp from last_modified, or use 0 if not available
    let timestamp = if let Some(ref lm) = last_modified {
        parse_http_date(lm)
            .map(|st| st.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())
            .unwrap_or(0)
    } else {
        0
    };

    ServerMetadata {
        remote_size,
        last_modified,
        timestamp,
        etag,
    }
}

/// Load .pget-status file if it exists
fn load_pget_status(task: &DownloadTask) -> Result<Option<PgetStatus>> {
    let status_path = task.pget_status_path();

    if !status_path.exists() {
        return Ok(None);
    }

    match fs::read_to_string(&status_path) {
        Ok(content) => {
            match serde_json::from_str::<PgetStatus>(&content) {
                Ok(status) => Ok(Some(status)),
                Err(e) => {
                    log::warn!("Failed to parse .pget-status file {}: {}", status_path.display(), e);
                    Ok(None)
                }
            }
        }
        Err(e) => {
            log::warn!("Failed to read .pget-status file {}: {}", status_path.display(), e);
            Ok(None)
        }
    }
}

/// Save .pget-status file with current metadata
fn save_pget_status(task: &DownloadTask, metadata: &ServerMetadata) -> Result<()> {
    // Only few Mutable files need .pget-status validation
    if task.file_type != FileType::Mutable {
        return Ok(());
    }

    // Check if metadata has valid information before saving
    // Skip saving if all metadata fields are empty/null
    if metadata.remote_size.is_none() &&
       metadata.last_modified.is_none() &&
       metadata.etag.is_none() {
        log::debug!("Skipping pget-status save for {} - no valid metadata available", task.url);
        return Ok(());
    }

    let status_path = task.pget_status_path();

    let pget_status = PgetStatus {
        url: task.get_resolved_url(),
        file_type: task.file_type.clone(),
        metadata: metadata.clone(),
    };

    let json_content = serde_json::to_string_pretty(&pget_status)
        .with_context(|| "Failed to serialize PgetStatus to JSON")?;

    fs::write(&status_path, json_content)
        .with_context(|| error_context!(format!("save_pget_status failed for status_path: {}", status_path.display())))?;

    Ok(())
}

/// Handle corruption detection by renaming corrupted files
fn handle_corruption_detection(task: &DownloadTask) -> Result<()> {
    utils::mark_file_bad(&task.final_path)?;
    cleanup_related_part_files(task)?;
    Ok(())
}

/// Clean up all files related to a download task (main part file, pget-status, and chunk files)
fn cleanup_related_part_files(task: &DownloadTask) -> Result<()> {
    cleanup_pget_status_file(task)?;
    cleanup_main_part_file(task)?;
    cleanup_chunk_files(task)?;
    Ok(())
}

fn cleanup_pget_status_file(task: &DownloadTask) -> Result<()> {
    let status_path = task.pget_status_path();
    if status_path.exists() {
        fs::remove_file(&status_path)?;
    }
    Ok(())
}

/// Clean up the main part file and pget-status file
fn cleanup_main_part_file(task: &DownloadTask) -> Result<()> {
    // Remove .part file
    if task.chunk_path.exists() {
        fs::remove_file(&task.chunk_path)?;
    }

    Ok(())
}

/// Clean up any chunk files with -O suffix that belong to this download
fn cleanup_chunk_files(task: &DownloadTask) -> Result<()> {
    let part_path = &task.chunk_path;

    // Remove any chunk files (.part-O*) by globbing filesystem
    if let Some(parent) = part_path.parent() {
        let chunk_prefix = part_path.file_name()
            .and_then(|n| n.to_str())
            .map(|s| format!("{}-O", s))
            .unwrap_or_default();

        if let Ok(entries) = fs::read_dir(parent) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.starts_with(&chunk_prefix) {
                        if let Err(e) = fs::remove_file(entry.path()) {
                            log::warn!("Failed to remove file {}: {}", entry.path().display(), e);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Validate chunk merge integrity: check for gaps, overlaps, and file size consistency
fn validate_chunk_merge_integrity(
    master_task: &DownloadTask,
    chunk_task: &DownloadTask,
    prev_end: u64,
) -> Result<u64> {
    let current_offset = chunk_task.chunk_offset.load(Ordering::Relaxed);
    let chunk_size = chunk_task.chunk_size.load(Ordering::Relaxed);

    // 1. Order validation (non-decreasing sequence)
    if current_offset < prev_end {
        log::error!(
            "validate_chunk_merge_integrity: overlapping chunk – current offset {} < previous end {} for {}",
            current_offset, prev_end, master_task.chunk_path.display()
        );
        // return Err(eyre!("Chunk overlap detected"));
    } else if current_offset > prev_end {
        log::error!(
            "validate_chunk_merge_integrity: gap between chunks – current offset {} > previous end {} for {} (gap {} bytes)",
            current_offset, prev_end, master_task.chunk_path.display(), current_offset - prev_end
        );
        return Err(eyre!("Chunk gap detected"));
    }

    // 2. Validate target file size after merge equals start + chunk_size
    let expected_after = current_offset + chunk_size;
    let file_size_after = fs::metadata(&master_task.chunk_path)
        .map(|m| m.len())
        .unwrap_or(0);
    if file_size_after != expected_after {
        log::error!(
            "validate_chunk_merge_integrity: target file size after merge {} != expected {} (offset {} + size {}) for {}",
            file_size_after, expected_after, current_offset, chunk_size, master_task.chunk_path.display()
        );
        return Err(eyre!("Target file size mismatch after merge"));
    }

    Ok(expected_after)
}

/// Validate that each part file's offset+size matches its chunk offset+size
/// This ensures that each part file is contained within its designated byte range
/// and prevents overlapping data during merging and streaming operations.
fn validate_chunk_file_boundaries(task: &DownloadTask, chunk_append_offset: u64) -> Result<()> {
    let chunk_offset = task.chunk_offset.load(Ordering::Relaxed);
    let chunk_size = task.chunk_size.load(Ordering::Relaxed);

    // Skip validation if chunk_size is 0 (unlimited)
    if chunk_size == 0 {
        return Ok(());
    }

    // Validate that the chunk append offset matches the expected chunk size
    if chunk_append_offset > 0 && chunk_append_offset != chunk_size {
        log::error!(
            "Chunk append offset mismatch: {} has {} bytes but expected {} bytes for {}",
            task.chunk_path.display(),
            chunk_append_offset,
            chunk_size,
            task.url
        );
        return Err(eyre!(
            "Chunk append offset mismatch: {} bytes != {} bytes for {}",
            chunk_append_offset,
            chunk_size,
            task.chunk_path.display()
        ));
    }

    // Validate that the actual file size on disk matches the expected chunk size
    if let Ok(metadata) = fs::metadata(&task.chunk_path) {
        let actual_file_size = metadata.len();
        if actual_file_size != chunk_size {
            log::error!(
                "Chunk file size mismatch: {} has {} bytes on disk but expected {} bytes for {}",
                task.chunk_path.display(),
                actual_file_size,
                chunk_size,
                task.url
            );
            return Err(DownloadError::ContentValidation {
                expected: format!("{} bytes", chunk_size),
                actual: format!("{} bytes", actual_file_size)
            }.into());
        }
    } else {
        log::warn!(
            "Could not read file metadata for {} to validate size",
            task.chunk_path.display()
        );
        return Err(DownloadError::DiskError {
            details: format!("Failed to read file metadata for {}", task.chunk_path.display())
        }.into());
    }

    // Validate that the sum of resumed and received bytes equals the chunk size
    let resumed_bytes = task.resumed_bytes.load(Ordering::Relaxed);
    let received_bytes = task.received_bytes.load(Ordering::Relaxed);
    if resumed_bytes + received_bytes != chunk_size {
        log::error!(
            "Chunk byte count mismatch: resumed {} + received {} = {} but expected {} for {}",
            resumed_bytes,
            received_bytes,
            resumed_bytes + received_bytes,
            chunk_size,
            task.chunk_path.display()
        );
        return Err(eyre!(
            "Chunk byte count mismatch: {} + {} = {} != {} for {}",
            resumed_bytes,
            received_bytes,
            resumed_bytes + received_bytes,
            chunk_size,
            task.chunk_path.display()
        ));
    }

    // Log successful validation for debugging
    log::debug!(
        "Chunk file boundary validation passed: {} has {} bytes within boundary {} (offset {} + size {})",
        task.chunk_path.display(),
        chunk_append_offset,
        chunk_size,
        chunk_offset,
        chunk_size
    );

    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct ChunkInfo {
    offset: u64,
    size: u64,      // Total chunk size (from offset to end of chunk)
    filesize: u64,  // Existing file size (bytes already downloaded)
}

/// This is a pure function that processes existing chunks and creates new chunks to cut down too large to_download areas.
///
/// # input
/// - `input_chunks`: the first chunk represents master task chunk, all are continouse and covers the whole file
///
/// # Returns
/// A vector of ChunkInfo representing adjusted chunks and new chunks in order, which are continouse and covers the whole file
///
/// # Chunk Creation Rules:
/// - Existing chunks keep their original offsets and may reduce size to make room for new chunks
///   if to_download area is too large; but do not over-reduce existing chunk's to_download area to < PGET_CHUNK_SIZE/4
/// - New chunks created are aligned to PGET_CHUNK_SIZE boundaries
/// - New chunks' to_download area is normally exact PGET_CHUNK_SIZE, or around it (for the first/last ones)
/// - New chunks' filesize = 0 (no existing files for them, so no filesize)
pub(crate) fn split_download_areas(
    input_chunks: &[&ChunkInfo],
) -> Vec<ChunkInfo> {
    let mut result = Vec::new();

    for chunk in input_chunks {
        let to_download_start = chunk.offset + chunk.filesize;
        let to_download_end = chunk.offset + chunk.size;
        let to_download_size = to_download_end - to_download_start;

        // If to_download area is small enough, keep the chunk as is
        if to_download_size <= PGET_CHUNK_SIZE {
            result.push((*chunk).clone());
            continue;
        }

        // Split large to_download areas
        let min_existing_download = PGET_CHUNK_SIZE / 4;
        let available_for_split = if to_download_size > min_existing_download {
            to_download_size - min_existing_download
        } else {
            0
        };

        if available_for_split < PGET_CHUNK_SIZE {
            // Not enough space to create a new chunk, keep original
            result.push((*chunk).clone());
            continue;
        }

        // Find the first PGET_CHUNK_SIZE boundary after the minimum existing download
        let min_boundary = to_download_start + min_existing_download;
        let first_boundary = ((min_boundary + PGET_CHUNK_SIZE - 1) / PGET_CHUNK_SIZE) * PGET_CHUNK_SIZE;

        // Adjust the original chunk to end at the first boundary
        let adjusted_chunk = ChunkInfo {
            offset: chunk.offset,
            size: first_boundary - chunk.offset,
            filesize: chunk.filesize,
        };
        result.push(adjusted_chunk);

        // Create new chunks from first_boundary to to_download_end
        let mut current_offset = first_boundary;
        while current_offset < to_download_end {
            let chunk_end = std::cmp::min(current_offset + PGET_CHUNK_SIZE, to_download_end);
            let new_chunk = ChunkInfo {
                offset: current_offset,
                size: chunk_end - current_offset,
                filesize: 0, // No existing file for new chunks
            };
            result.push(new_chunk);
            current_offset = chunk_end;
        }
    }

    result
}

// 1. Collect and sort chunks
fn collect_and_sort_chunks(chunk_files: Vec<PathBuf>) -> Result<Vec<ChunkInfo>> {
    let mut chunks: Vec<ChunkInfo> = chunk_files
        .into_iter()
        .filter_map(|path| {
            let offset = extract_offset(&path);
            match fs::metadata(&path) {
                Ok(meta) => {
                    let filesize = meta.len();
                    // For existing chunks, we don't know the total size yet, so we set it to filesize initially
                    // This will be corrected later when we have the expected_size
                    let size = filesize;
                    Some(ChunkInfo{offset, size, filesize})
                }
                Err(e) => {
                    log::warn!("Failed to read metadata for {}: {}", path.display(), e);
                    None // Skip this chunk file
                }
            }
        })
        .collect();

    // Sort chunks by offset
    chunks.sort_by_key(|c| c.offset);
    Ok(chunks)
}

// 2. Validate chunks and master file
fn validate_chunks<'a>(master_task: &DownloadTask, chunks: &'a [ChunkInfo], expected_size: u64) -> Result<Vec<&'a ChunkInfo>> {
    let mut prev_end = 0;
    let mut has_errors = false;
    let mut valid_chunks = Vec::new();

    let master_partfile_size = match fs::metadata(&master_task.chunk_path) {
        Ok(metadata) => metadata.len(),
        Err(e) => {
            log::warn!("Failed to read master partfile metadata for {}: {}", master_task.chunk_path.display(), e);
            1 // Default to 1 to avoid skipping all chunks
        }
    };

    // Validate chunk overlaps and boundaries
    for chunk in chunks {
        // Skip chunks that overlap with the master part file
        if chunk.offset < master_partfile_size {
            log::debug!("Skipping chunk at offset {} (overlaps with master ending at {})", chunk.offset, master_partfile_size);
            continue;
        }

        if chunk.offset < prev_end {
            log::error!("Overlapping chunks detected at offset {} (ends at {})", chunk.offset, prev_end);
            has_errors = true;
            continue;
        }

        if chunk.offset >= expected_size {
            log::error!("Chunk starts beyond expected file size: {} >= {}", chunk.offset, expected_size);
            has_errors = true;
            continue;
        }

        let chunk_end = chunk.offset + chunk.size;
        if chunk_end > expected_size {
            log::error!("Chunk exceeds expected file size: {} > {}", chunk_end, expected_size);
            has_errors = true;
            continue;
        }

        valid_chunks.push(chunk);
        prev_end = chunk_end;
    }

    if has_errors {
        log::error!("==== recover_chunks_for_parto_files debug dump ====");
        log::error!("task.url           : {}", master_task.url);
        log::error!("task.file_size     : {}", master_task.file_size.load(Ordering::Relaxed));
        log::error!("expected_size      : {}", expected_size);
        log::error!("Collected {} chunk files:", chunks.len());
        for c in chunks {
            log::error!("  offset {:>10}  size {:>10}  filesize {:>10}", c.offset, c.size, c.filesize);
        }
        log::error!("==== end debug dump ====");

        return Err(eyre!("Invalid chunk files detected. See log for details. Please remove existing part files and retry."));
    }

    Ok(valid_chunks)
}

// 3. Adjust and create chunk tasks
fn adjust_and_create_chunks(
    master_task: &DownloadTask,
    valid_chunks: Vec<&ChunkInfo>,
    expected_size: u64,
) -> Result<()> {
    // Create a complete set of chunks that cover the entire file continuously
    let mut complete_chunks = Vec::new();

    // Start with master task chunk
    let first_existing_offset = valid_chunks.first().map(|c| c.offset).unwrap_or(expected_size);
    let master_chunk = ChunkInfo {
        offset: 0,
        size: first_existing_offset,
        filesize: std::cmp::min(first_existing_offset, get_existing_file_size(&master_task.chunk_path).unwrap_or(0)),
    };
    complete_chunks.push(master_chunk);
    let mut current_offset = first_existing_offset;

    // Add existing chunks and fill gaps between them
    for (i, chunk) in valid_chunks.iter().enumerate() {
        // Fill gap if there is one
        if current_offset < chunk.offset {
            let gap_chunk = ChunkInfo {
                offset: current_offset,
                size: chunk.offset - current_offset,
                filesize: 0, // No existing file for gap
            };
            complete_chunks.push(gap_chunk);
        }

        // Add the existing chunk with corrected size
        let chunk_end = if i == valid_chunks.len() - 1 {
            // Last chunk extends to the end of the file
            expected_size
        } else {
            // Non-last chunk extends to the start of the next chunk
            valid_chunks[i + 1].offset
        };

        let corrected_chunk = ChunkInfo {
            offset: chunk.offset,
            size: chunk_end - chunk.offset,  // Total chunk size
            filesize: chunk.filesize,        // Existing file size
        };
        complete_chunks.push(corrected_chunk);
        current_offset = chunk_end;
    }

    // Fill any remaining gap to the end of file
    if current_offset < expected_size {
        let final_chunk = ChunkInfo {
            offset: current_offset,
            size: expected_size - current_offset,
            filesize: 0, // No existing file for gap
        };
        complete_chunks.push(final_chunk);
    }

    // Convert to references for split_download_areas
    let complete_refs: Vec<&ChunkInfo> = complete_chunks.iter().collect();

    // Split large download areas into properly sized chunks
    let split_chunks = split_download_areas(&complete_refs);

    // Adjust master task's chunk size to match the first chunk
    if let Some(first_chunk) = split_chunks.first() {
        master_task.chunk_size.store(first_chunk.size, Ordering::Relaxed);
        log::debug!("Adjusted master chunk size to {}", first_chunk.size);
    }

    // Create chunk tasks from split chunks (skip first one which is master task)
    let mut chunk_tasks = Vec::new();
    for chunk in split_chunks.iter().skip(1) {
        let chunk_task = master_task.create_chunk_task(chunk.offset, chunk.size);
        chunk_tasks.push(chunk_task);
    }

    add_chunk_tasks(master_task, chunk_tasks, ChunkStatus::HasBeforehandChunk)
}

fn add_url_to_mirror_skip_list(task: &DownloadTask) {
    // Get resolved URL; do not use task.mirror_inuse -- it's None at the call time
    let resolved_url = {
        if let Ok(resolved_guard) = task.resolved_url.lock() {
            resolved_guard.clone()
        } else {
            log::warn!("Failed to lock resolved_url for task URL {}", task.url);
            return;
        }
    };

    // Return early if resolved URL is the same as original URL or still contains $mirror
    if resolved_url == task.url || resolved_url.contains("$mirror") {
        log::debug!("add_url_to_mirror_skip_list: No resolved URL found for task URL {}", task.url);
        return;
    }

    // Extract mirror site from resolved URL
    let site_key = mirror::url2site(&resolved_url);
    log::debug!("add_url_to_mirror_skip_list: task.url={}, resolved_url={}, site_key={}", task.url, resolved_url, site_key);

    // Add URL to mirror skip list
    let url = &task.url;
    if let Ok(mut mirrors) = mirror::MIRRORS.lock() {
        if let Some(mirror_in_collection) = mirrors.mirrors.get_mut(&site_key) {
            mirror_in_collection.add_skip_url(url);
            log::debug!("Successfully added {} to skip_urls for mirror site {}", url, site_key);
        } else {
            log::warn!("Mirror site {} not found in mirrors collection for URL {}", site_key, url);
        }
    } else {
        log::warn!("Failed to lock mirrors collection for URL {}", url);
    }
}
