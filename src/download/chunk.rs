// ============================================================================
// DOWNLOAD CHUNKING - Parallel Chunked Download System
//
// This module implements a sophisticated parallel chunked download system similar
// to LFTP, with master-child task coordination, intelligent resumption, and
// real-time streaming capabilities. It enables efficient downloading of large
// files by splitting them into chunks that can be downloaded concurrently.
//
// Key Features:
// - 3-level task hierarchy (master, L2 chunks, L3 chunks)
// - Intelligent ondemand chunking for slow downloads
// - Resume capability for interrupted chunk downloads
// - Real-time chunk merging and validation
// - Thread-safe coordination between chunk tasks
// ============================================================================

use color_eyre::eyre::{eyre, Result};
use std::fs::{self, File, OpenOptions};
use crate::lfs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::Ordering, mpsc::SyncSender as Sender};
use super::types::*;
use crate::download::http::execute_download_request;
use crate::download::http::process_download_response;
use crate::download::http::process_chunk_download_stream;
use crate::download::http::finalize_chunk_download;
use crate::download::http::validate_download_size;
use crate::download::utils::send_chunk_to_all_channels;
use crate::download::utils::map_io_error;
use crate::download::progress::log_download_completion;
use crate::download::progress::update_chunk_progress;
use crate::download::mirror::resolve_mirror_and_update_task;
use crate::download::file_ops::get_existing_file_size;
use crate::download::file_ops::check_existing_partfile;
use crate::download::file_ops::extract_offset;

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

pub(crate) fn create_chunk_tasks(task: &DownloadTask) -> Result<()> {
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
pub(crate) fn download_chunk_task(task: &DownloadTask) -> Result<()> {
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
pub(crate) fn validate_chunk_file_boundaries(task: &DownloadTask, chunk_append_offset: u64) -> Result<()> {
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
pub fn split_download_areas(
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
pub(crate) fn collect_and_sort_chunks(chunk_files: Vec<PathBuf>) -> Result<Vec<ChunkInfo>> {
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
pub(crate) fn validate_chunks<'a>(master_task: &DownloadTask, chunks: &'a [ChunkInfo], expected_size: u64) -> Result<Vec<&'a ChunkInfo>> {
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
pub(crate) fn adjust_and_create_chunks(
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

/// Check for on-demand chunking execution (master tasks and L2 chunks)
///
/// This function checks if the task has been marked for ondemand chunking by the global
/// scheduler and executes the chunking if needed. It now supports both master tasks creating
/// L2 chunks and L2 chunks creating L3 chunks.
pub(crate) fn check_ondemand_chunking(
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
pub(crate) fn may_ondemand_chunking(task: &DownloadTask) -> bool {

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
            send_chunk_to_all_channels(&chunk_task, &chunk_task.chunk_path, data_channels, false)?;
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
        if let Err(e) = lfs::remove_file(&chunk_task.chunk_path) {
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

pub(crate) fn wait_for_chunks_and_merge(master_task: &DownloadTask) -> Result<()> {
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

        update_chunk_progress(chunk, master_task);

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

                // Remove this chunk from the local processing list so we don't
                // re-process it, but keep it in the parent's `chunk_tasks` so
                // global progress accounting (get_total_progress_bytes) remains
                // accurate and monotonic.
                chunks.remove(0);
            },
            DownloadStatus::Failed(ref err) => {
                // Handle chunk failure and retry logic
                if handle_failed_chunk(chunk, parent_task, chunk_index, err) {
                    // Chunk will be retried - don't remove it
                    // Sleep to allow retry to happen
                    std::thread::sleep(std::time::Duration::from_millis(CHUNK_SLEEP_DURATION_MS));
                } else {
                    // Max retries exceeded - record failure and remove chunk
                    // from the local processing list. We intentionally keep the
                    // failed chunk in the parent's `chunk_tasks` so that
                    // progress calculations based on the master task's view of
                    // all chunks do not suddenly drop when chunks are removed.
                    *any_fail = true;
                    chunks.remove(0);
                }
            },
            DownloadStatus::Pending | DownloadStatus::Downloading => {
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
            lfs::create_dir_all(parent)?;
        }
        lfs::file_create(target_path)?.sync_all()?;
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

