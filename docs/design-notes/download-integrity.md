# Download Data Integrity Design

## Overview

This document outlines the data integrity principles for `download_task()`, `download_file()`, and `download_chunk_task()` functions to handle mismatches between different remote mirror sites and sync delays between repodata and package files.

## Core Challenge

Remote mirrors may have inconsistencies due to:
- **Mirror sync delays**: Different mirrors may be at different sync states
- **Repodata vs package file delays**: Repository metadata may be newer than actual package files or vice versa
- **Corrupted local cache**: Previously downloaded files may be corrupted
- **Mixed content from different mirrors**: Chunk tasks downloading from different mirrors with different file versions

## Data Integrity Principles

### 1. Immutable Files (Package files, by-hash content)

**Characteristics:**
- Content never changes for the same filename/hash
- Majority of well known package files (.deb, .rpm, .apk, .epkg)
- Files in `/by-hash/` directories
- Have known expected `task.file_size` from repository metadata

**Integrity Policy:**
```
if has_final_path:
    if expected_size == local_final_path_size:
        → SKIP_DOWNLOAD (file is complete and correct)
    elif expected_size != local_final_path_size:
        → VERIFY_CHECKSUM (determine if local or remote data is corrupt)
        → RECOMMEND_RERUN_UPDATE (either repodata or package file is corrupt)
elif has_part_files:
    → RESUME_WITH_CHUNKING (partial files are always valid prefixes)
```

### 2. Mutable Files (Repository entrance metadata, etc.)

**Characteristics:**
- Content can change over time with same filename
- Examples: `Release`, `repomd.xml`, `APKINDEX.tar.gz`, `elf-loader` etc. can be arbitrary names
- Unknown expected file size beforehand (includes rpm/deb from index.html directory listing)
- Require timestamp/size/ETag validation to detect changes

**Integrity Policy:**
```
// Phase 1: Cleanup and Initial Request
recover/delete part files based on pget status file
send_initial_request() → get remote_metadata{last_modified, size, etag}

// Phase 2: Cache Validation
if final_path_exists:
    if remote_metadata matches local_metadata(timestamp, size, etag):
        → SKIP_DOWNLOAD (file is up-to-date)
    else:
        → PROCEED_WITH_DOWNLOAD
elif chunk_path_exists:
    verify_part_file_consistency_with_remote_metadata()
    if pget metadata mismatch:
        → RETRY_OTHER_MIRROR (avoid mixing old/new content)
    else:
        → RESUME_DOWNLOAD

// Phase 3: Download Execution
save_master_metadata_to_pget_status_file{last_modified, size, etag}
verify_chunk_server_info_matches_master_metadata()
```

### 3. Append-Only Files (Future epkg repository files)

**Characteristics:**
- Content only grows, never changes existing content
- Expected size indicates minimum expected content
- Partial files are always valid prefixes

**Integrity Policy:**
```
Same as immutable files, except:
if has_final_path:
    if expected_size > local_size:
        → MOVE_TO_PART_FILE_AND_RESUME_CHUNKING
    elif expected_size == local_size:
        → SKIP_DOWNLOAD
    elif expected_size < local_size:
        → CORRUPTION_ERROR (local file has more content than expected), maybe data corrupt or repodata is older
```

## Mirror Validation and Switching

### Range Request Response Handling
```
if response_416_invalid_range:
    → RETRY_OTHER_MIRROR (current mirror may have different file version)

if response_206_partial_content:
    verify_content_range_header_matches_expectations()
    if mismatch:
        → RETRY_OTHER_MIRROR
```

### Metadata Consistency Checking
```
for chunk_task in chunk_tasks:
    if chunk_server_metadata != master_server_metadata:
        → ABORT_CHUNK_RETRY_OTHER_MIRROR
        → LOG_MIRROR_INCONSISTENCY(mirror_url, file_path)
```

## .pget-status File Format

Store master download metadata for verification:

```json
{
    "url": "https://mirror.example.com/file.tar.gz",
    "file_type": "Mutable|Immutable|Append_only",
    "metadata": {
      "remote_size": 77401140,
      "last_modified": "Fri, 04 Jul 2025 22:07:28 GMT",
      "etag": "686850a0-49d0c34",
      "timestamp": 1751666848
    }
}
```

Note: Chunk information is discovered by globbing `$prefix.part`, `$prefix.part-O*` from filesystem files, allowing the status file to be saved early without needing updates during downloading.

## Error Recovery Strategies

### Size Mismatch Detection
```
if is_immutable_file && local_size != expected_size:
    if local_size > expected_size:
        → DELETE_LOCAL_FILE_REDOWNLOAD (local corruption)
    elif local_size < expected_size:
        → RESUME_DOWNLOAD (partial file)

if is_mutable_file:
    → ALWAYS_VALIDATE_WITH_SERVER_METADATA_FIRST
```

### Mirror Consistency Validation
```
before_chunk_download(chunk_task, master_task):
    if chunk_task.server_metadata != master_task.server_metadata:
        → MARK_MIRROR_INCONSISTENT(chunk_task.mirror)
        → SELECT_DIFFERENT_MIRROR_FOR_CHUNK
        → UPDATE_MIRROR_PENALTY_SCORE
```

### Checksum Verification
```
if size_mismatch_detected && is_immutable_file:
    local_checksum = calculate_file_checksum(local_file)
    expected_checksum = get_checksum_from_repository_metadata()

    if local_checksum != expected_checksum:
        → DELETE_LOCAL_FILE_REDOWNLOAD
        → LOG_LOCAL_CORRUPTION(file_path)
    else:
        → LOG_REMOTE_METADATA_INCONSISTENCY(mirror_url)
        → RECOMMEND_EPKG_UPDATE
```

## Implementation Benefits

1. **Robust Mirror Handling**: Prevents content mixing from mirrors at different sync states
2. **Early Corruption Detection**: Identifies local vs remote data corruption quickly
3. **Bandwidth Optimization**: Avoids unnecessary re-downloads when files are already correct
4. **Debugging Support**: Comprehensive logging for troubleshooting mirror and sync issues
5. **Future-Proof**: Extensible to new file types and repository formats

## Channel Streaming Details

### send_file_to_channel()
- **Called from**: `download_file_with_retries()` for AlreadyComplete Immutable file
- **Called from**: `handle_304_not_modified_response()` for AlreadyComplete Mutable file
- **Purpose**: Streams existing local file to data channel
- **Context**: When file validation determines download can be skipped

### send_chunk_to_channel()
- **Called from**: `merge_completed_chunk()` for each completed chunk
- **Called from**: `check_existing_partfile()` for existing master .part file
- **Purpose**: Streams chunk file data to data channel
- **Context**: When a chunk completes and needs to be streamed to consumer

### channel.send() (Real-time streaming)
- **Called from**: `process_chunk_download_stream()` during active download
- **Purpose**: Streams data in real-time as it's received from network
- **Context**: During active download loop, sends each buffer of received data
