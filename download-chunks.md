# Chunked Parallel Download Design

## Overview

This document describes the design for adding chunked parallel download functionality to `download.rs`, similar to lftp's pget feature. The design reuses the existing `DownloadTask` structure and download functions while adding support for master-chunk task coordination.

## Architecture

### Task Types

There are two types of tasks:
1. **Master Task**: Coordinates chunked downloads, manages progress bar, and handles data channel
2. **Chunk Task**: Downloads a specific byte range of the file

The distinction is made by checking `chunk_tasks.is_empty()` - master tasks have non-empty chunk_tasks vector.

### File Naming Convention

We use a single pattern for chunk files:
- Master task: `filename.part` (existing pattern)
- Chunk tasks: `filename.part-O{offset-in-bytes}` where offset is the starting byte position

Examples:
- `package.deb.part` (master task, first chunk)
- `package.deb.part-O1048576` (chunk starting at 1MB)
- `package.deb.part-O2097152` (chunk starting at 2MB)

### Extended DownloadTask Structure

```rust
pub struct DownloadTask {
    // Existing fields
    pub url: String,
    pub output_dir: PathBuf,
    pub max_retries: usize,
    pub data_channel: Option<Sender<Vec<u8>>>,
    pub status: Arc<std::sync::Mutex<DownloadStatus>>,
    pub final_path: PathBuf,
    pub size: Option<u32>,

    // New fields for chunking
    pub chunk_tasks: Arc<std::sync::Mutex<Vec<Arc<DownloadTask>>>>,
    pub chunk_path: PathBuf,
    pub chunk_offset: u64,
    pub chunk_size: Option<u64>,
    pub thread_handle: Arc<std::sync::Mutex<Option<std::thread::JoinHandle<()>>>>,
    pub start_time: Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    pub received_bytes: Arc<std::sync::atomic::AtomicU64>,
}
```

## Chunking Policies

### 1. Beforehand Chunking (Files > 1.5MB)

For files larger than 1.5MB with known size:
- Split into 1MB-aligned chunks
- Create chunk tasks during `download_file()` before starting download
- Master task handles the first chunk (0 to 1MB)
- Additional chunk tasks handle subsequent 1MB chunks

### 2. On-demand Chunking

During download, if conditions are met:
- Estimated remaining time > 5 seconds
- Remaining data > 200KB
- Available threads in thread pool

Create additional 100KB-aligned chunk tasks dynamically.

## Implementation Plan

### Phase 1: Extend DownloadTask Structure

1. Add new fields to `DownloadTask`
2. Update constructors to initialize new fields
3. Add helper methods for chunk management

### Phase 2: Chunk File Management

1. Add functions for chunk file path generation
2. Implement chunk file cleanup on completion
3. Add atomic file operations for crash recovery

### Phase 3: Master Task Logic

1. Modify `download_file()` to detect large files and create chunks
2. Implement chunk task creation (beforehand)
3. Add chunk coordination in `download_content()`
4. Implement data channel ordering logic

### Phase 4: Chunk Task Processing

1. Create `start_chunks_processing()` function
2. Implement chunk task execution
3. Add progress reporting to master task
4. Handle chunk task completion

### Phase 5: On-demand Chunking

1. Add estimation logic in `download_content()`
2. Implement dynamic chunk creation
3. Handle master task range adjustment

### Phase 6: Process Coordination

1. Add PID file management
2. Implement atomic file operations
3. Add crash recovery logic

## Key Functions

### New Functions

- `create_chunk_tasks(task: &DownloadTask, total_size: u64) -> Vec<Arc<DownloadTask>>`
- `start_chunks_processing(chunks: Vec<Arc<DownloadTask>>)`
- `wait_for_chunks_and_merge(master_task: &DownloadTask)`
- `estimate_remaining_time(task: &DownloadTask) -> Duration`
- `create_ondemand_chunk(master_task: &DownloadTask, offset: u64, size: u64) -> Arc<DownloadTask>`
- `merge_chunk_data_to_channel(master_task: &DownloadTask)`
- `cleanup_chunk_files(task: &DownloadTask)`

### Modified Functions

- `download_file()`: Add chunk creation logic
- `download_content()`: Add chunk coordination and on-demand logic
- `DownloadTask::new()`: Initialize chunk-related fields

## Data Flow

### Master Task Flow

1. Check file size in `download_file()`
2. If > 1.5MB, create chunk tasks
3. Start chunk tasks via `start_chunks_processing()`
4. Download own chunk (first chunk)
5. Wait for other chunks to complete
6. Merge chunk data in order to data channel
7. Clean up chunk files

### Chunk Task Flow

1. Download assigned byte range
2. Write to chunk file (`filename.part-O{offset}`)
3. Update received_bytes counter
4. Signal completion to master task

## Thread Management

- Master tasks use existing `DownloadManager::pool`
- Chunk tasks spawn individual threads to avoid pool exhaustion
- Maximum 2 * nr_parallel total chunk threads
- Thread handles stored in master task for joining

## Progress Reporting

- Master task manages single ProgressBar
- Collects stats from all chunk tasks via `received_bytes`
- Updates progress based on total downloaded across all chunks

## Error Handling

- Individual chunk failures don't fail entire download
- Master task can retry failed chunks
- Graceful degradation to single-threaded download
- Proper cleanup of partial chunk files

## Process Coordination

### PID File Management

- Create `{final_path}.download.pid` during active download
- Contains process ID and start timestamp
- Cleaned up on successful completion
- Stale PID files (process not running) are automatically removed

### Atomic Operations

- Use temporary file + rename for final completion
- Chunk files written atomically
- Master task coordinates final merge

### Crash Recovery

- Detect existing chunk files on restart
- Validate chunk integrity
- Resume from last valid state
- Clean up corrupted chunks

## Compatibility

- Fully backward compatible with existing code
- Non-chunked downloads work exactly as before
- Chunk tasks reuse all existing download logic
- No changes to public API

## Performance Expectations

- 2-4x speedup for large files (>1.5MB) on fast connections
- Automatic fallback to single-threaded for small files
- Efficient resource usage with thread pool management
- Minimal overhead for files that don't benefit from chunking
