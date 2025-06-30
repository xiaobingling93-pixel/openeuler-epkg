# Mirror Time Skew Fix

## Problem Description

The original `should_redownload()` function had a 2-second tolerance for timestamp comparison between local and remote files. This caused issues when:

1. **Local time is more recent than remote time**: When a local file was downloaded from a mirror with a more recent timestamp, and then epkg tries to download from a mirror with an older timestamp, it would unnecessarily re-download the file.

2. **Mirror time skew**: Different mirrors can have different system times, causing false cache invalidation when the timestamp difference exceeds the 2-second tolerance.

## Example from Log

```
[2025-06-30 15:59:04 +0800 DEBUG src/download.rs:3845] Cache validation failed: timestamp mismatch:
remote 2025-06-30 2:10:26.0 +00:00:00,
local 2025-06-30 2:15:48.200590639 +00:00:00
```

In this case:
- Local time: `2025-06-30 2:15:48` (more recent)
- Remote time: `2025-06-30 2:10:26` (older)
- The local file is actually newer than what's on the server

## Solution

Modified the `should_redownload()` function in `src/download.rs` to:

1. **Handle local time being more recent**: If `local_ts > remote_ts`, assume the local file is newer and use the cache.

2. **Extend tolerance to 10 minutes**: Increased the timestamp tolerance from 2 seconds to 600 seconds (10 minutes) to handle mirror time skew.

3. **Improved logic flow**: Restructured the timestamp comparison to be more intelligent about when to use cache vs. re-download.

## Code Changes

### Before:
```rust
Some(ts) if remote_size == local_size && (ts - local_ts).unsigned_abs() <= Duration::from_secs(2) => {
    CacheDecision::UseCache {
        reason: format!("Size and timestamp match (remote: {}, local: {})", ts, local_ts)
    }
}
```

### After:
```rust
Some(ts) if remote_size == local_size => {
    let time_diff = (ts - local_ts).unsigned_abs();

    // If local time is more recent than remote time, assume local file is newer
    if local_ts > ts {
        CacheDecision::UseCache {
            reason: format!("Local file is newer than remote (local: {}, remote: {})", local_ts, ts)
        }
    }
    // If timestamps are within 10 minutes of each other, consider them the same
    else if time_diff <= Duration::from_secs(600) {
        CacheDecision::UseCache {
            reason: format!("Size and timestamp match within 10min tolerance (remote: {}, local: {})", ts, local_ts)
        }
    }
    else {
        // ... handle re-download case
    }
}
```

## Benefits

1. **Reduced unnecessary downloads**: Prevents re-downloading files when local version is actually newer
2. **Better mirror compatibility**: Handles time differences between mirrors more gracefully
3. **Improved performance**: Fewer unnecessary network requests and file operations
4. **Better user experience**: Less waiting time for downloads that aren't actually needed

## Testing

The fix should resolve the specific case shown in the log where:
- Local file timestamp: `2025-06-30 2:15:48`
- Remote file timestamp: `2025-06-30 2:10:26`
- Result: Cache will be used instead of re-downloading
