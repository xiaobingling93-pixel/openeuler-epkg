# Windows WHPX VM Performance Analysis

## Current Baseline (2026-04-01, after EA batch optimization)

### Timing Breakdown for `epkg run -e alpine echo hello`

| Phase | Time | Description |
|-------|------|-------------|
| VM config | ~1.5ms | Create and configure VM context |
| Guest connect | ~1.4s | VM boot + kernel init + vsock connect |
| Write+flush | ~0.1ms | Send command to guest |
| Response wait | ~1.4s | Guest execute + return result |
| **Total** | **~2.8-3.0s** | End-to-end latency |

### FUSE Operation Statistics (from EPKG_DEBUG_LIBKRUN=1)

**After EA caching optimization:**

```
Operation            Count       Total(ms)      Avg(us)
-------------------------------------------------------
LOOKUP                  27           14.42       533.94
GETATTR                  2            1.56       782.25
READLINK                 4            0.48       119.80
OPEN                    20          235.31     11765.58  <-- SLOW (11.8ms avg)
READ                   565         1125.48      1992.01
WRITE                  171           16.82        98.36
RELEASE                 18            5.59       310.44
GETXATTR               173            1.47         8.48  <-- CACHED (was 10.5ms)
FLUSH                   18            5.47       304.08
INIT                     2            0.08        39.10
-------------------------------------------------------
TOTAL                 1000         1406.68      1406.68
```

**Optimization Impact:**

| Metric | Before | After | Improvement |
|--------|--------|-------|-------------|
| GETXATTR avg | 10.46ms | 8.48μs | ~1200x faster |
| GETXATTR total | 1830ms | 1.47ms | 99.9% reduction |
| FUSE total | 3356ms | 1406ms | 58% reduction |
| End-to-end | ~4.5s | ~2.8s | 38% faster |

### Performance Bottlenecks Identified

1. **GETXATTR (1830ms / 175 ops = 10.46ms avg)**
   - Each GETXATTR requires Windows CreateFileW + NtQueryEaFile + CloseHandle
   - Guest kernel sends individual GETXATTR requests per xattr
   - Cannot batch at FUSE protocol level

2. **OPEN (184ms / 20 ops = 9.18ms avg)**
   - Windows file open overhead
   - Includes creating cached file handle

3. **VM boot time (1.4s)**
   - WHPX VM creation overhead
   - Kernel boot and init

### EA Batch Optimization Impact

The `get_all_file_eas()` function reads all EAs in one file open/close cycle.
This optimization benefits:
- `metadata_to_stat()`: Reduced from 4 EA reads to 1 batch read
- `listxattr()`: Reduced from 4 separate `get_file_ea` calls to 1 batch read

### EA Caching Optimization Impact

Per-inode EA caching eliminates repeated file I/O for same file:
- `get_cached_eas()`: Returns cached EAs if available, otherwise reads and caches
- Cache invalidated on `setxattr()`/`removexattr()` operations
- GETXATTR avg time: 10.46ms → 8.48μs (~1200x faster)

### Remaining Bottlenecks

1. **OPEN operations (~10ms avg)** - Windows CreateFileW overhead
   - `symlink_metadata`: 30μs (negligible)
   - `read_reparse_kind`: 20μs (negligible)
   - `metadata`: 20μs (negligible)
   - `File::open`: **12.3ms** - 97% of OPEN time
   - Windows ACL security checks, antivirus scan, file system driver

2. **READ operations (~2.3ms avg)** - Multiple sources
   - `get_handle`: 1.7μs (negligible)
   - `lock_file`: 0.5μs (negligible)
   - `seek`: 0.8μs (negligible)
   - `alloc_buffer`: 34μs (buffer allocation)
   - `read` (file I/O): 29μs
   - `write` (virtio queue): 256μs
   - **Untracked: ~2ms/operation** - virtio queue transmission overhead

3. **VM boot time (1.4s)** - WHPX overhead
   - WHPX VM creation
   - Kernel boot and init
   - WHPX exit statistics: ~54ms total (not main bottleneck)

### WHPX Exit Statistics

WHPX (unlike KVM) does not provide built-in statistics API.
We track exit counts and processing time at application level using atomic counters.

**Implementation choice:** Using fixed-size array `[ExitStat; 8194]` instead of HashMap:
- Array lookup is O(1) with no hash overhead (important for hot path)
- No lock contention (atomic operations vs RwLock<HashMap>)
- Memory cost is negligible (131KB for VM process)

**Actual statistics (EPKG_DEBUG_LIBKRUN=1):**
```
=== WHPX VM Exit Statistics ===
Exit Reason                    Count       Total(ms)      Avg(us)
-----------------------------------------------------------------
MemoryAccess                    3630           10.92         3.01
X64IoPortAccess                  613            5.59         9.12
X64MsrAccess                      21            0.14         6.67
X64Cpuid                         227           11.61        51.16
Canceled                         756           10.61        14.03
-----------------------------------------------------------------
TOTAL                           5247           38.87         7.41
```

**Analysis:**
- Total WHPX exit time (~39ms) is NOT the main bottleneck
- MemoryAccess exits (3630) are most frequent but fastest (3μs avg)
- X64Cpuid exits (227) take longest per exit (51μs) due to CPUID instruction overhead
- Canceled exits (756) are vCPU interrupt mechanism for forced interrupt delivery

Enable with `EPKG_DEBUG_LIBKRUN=1` to see exit statistics.

### Comparison with macOS

| Platform | Total Time | Notes |
|----------|------------|-------|
| macOS | ~0.15s | Hardware virtualization (HV) |
| Windows WHPX | ~2.8-3.0s | Software virtualization overhead |
| **Difference** | **~2.7s** | WHPX vs HV performance gap |

The macOS baseline uses Hardware Virtualization (HV) which has significantly lower overhead
than Windows WHPX. The VM boot time on macOS is essentially instant (~50ms).

## Virtiofs Operations Breakdown

### Test Results (after init binary fix)

| Command | Total Time | Incremental |
|---------|------------|-------------|
| echo hello | ~2.8-3.0s | baseline |
| ls / | ~3.0s | +0.2s (small dir) |
| ls /usr/bin (~300 files) | ~3.2s | +0.2s (dir traversal) |
| ls -l /usr/bin | ~3.5s | +0.3s (getattr overhead) |

### Analysis

- **Directory traversal overhead**: ~200ms for 300 files (improved with readdirplus)
- **getattr overhead**: ~300ms for 300 files (readdirplus reduces FUSE calls)
- **FUSE operations**: ~1,100 operations (down from ~26,000 with debug init fix)

## Optimization History

1. **virtiofs file handle caching**: Read operations reuse cached file handle
2. **readdirplus implementation**: Reduces FUSE calls for directory listings
3. **Debug init binary fix**: Replaced 191MB debug init with 14MB release init
4. **Sleep optimization**: Removed 1100ms of fixed delays
5. **Runtime socket location**: Moved sockets from cache to epkg_run directory
6. **FUSE operation statistics**: Added tracking for performance analysis
7. **ntdll function pointer caching**: Optimized EA read/write performance
8. **EA batch reading**: Read all EAs in single file open/close cycle
   - `get_all_file_eas()` reads all 4 POSIX EAs (UID, GID, MODE, DEV) at once
   - `metadata_to_stat` and `listxattr` now use batch reading
   - Eliminates redundant file open/close for same file
9. **EA caching**: Per-inode EA caching eliminates repeated file I/O
   - GETXATTR avg time: 10.46ms → 8.48μs (~1200x faster)
   - Cache invalidated on `setxattr()`/`removexattr()` operations
10. **WHPX exit statistics**: Application-level tracking for VM exit analysis
    - Confirmed WHPX exits (~54ms) are NOT the main bottleneck
    - MemoryAccess most frequent, X64Cpuid slowest per exit
11. **FUSE sub-operation statistics**: Detailed timing for OPEN/READ analysis
    - OPEN: `File::open` is 97% of time (Windows CreateFileW overhead)
    - READ: virtio queue write + untracked transmission overhead

## Future Optimization Opportunities

1. **VM reuse mode**: Keep VM running for multiple commands (--reuse_vm)
   - Eliminates 1.4s VM boot overhead per command
2. **OPEN optimization**: Investigate `File::open` alternatives
   - Consider file handle pooling
   - Investigate Windows file API optimizations
3. **READ optimization**: Investigate virtio queue transmission overhead
   - ~2ms untracked per READ operation
   - May be inherent to virtio/FUSE protocol
4. **Virtiofs cache warming**: Pre-cache frequently used files
5. **Init binary optimization**: Further reduce size or use compressed init