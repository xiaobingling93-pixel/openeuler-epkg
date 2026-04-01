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

**Main Bottleneck: READ operations (81% of FUSE time)**

1. **READ virtio overhead: ~922ms (85% of READ time)**
   - This is inherent to virtio/FUSE protocol
   - Includes: queue pop/add, memory mapping, guest-host communication
   - Difficult to optimize without architecture changes
   - Tracked components: 159ms (15%) - write 130ms, I/O 16ms, buffer 12ms

2. **OPEN operations: ~195ms (15% of FUSE time)**
   - 20 OPEN operations, ~9.5ms each
   - 97% of time is File::open (Windows kernel)
   - API alternatives tested (CreateFileW, NtCreateFile): no significant improvement
   - Windows ACL, antivirus, filesystem driver are the bottleneck

3. **WHPX exits: ~54ms (4% of total)**
   - Not a significant bottleneck
   - MemoryAccess: 17ms, Canceled: 17ms, X64Cpuid: 12ms

### Performance Summary

| Category | Time | % of Total |
|----------|------|------------|
| **READ virtio overhead** | 922ms | 66% |
| READ tracked operations | 159ms | 11% |
| OPEN operations | 195ms | 14% |
| Other FUSE | 54ms | 4% |
| WHPX exits | 54ms | 4% |
| **Total** | **~1400ms** | **100%** |

**Key Insight:** 66% of time is virtio queue overhead in READ operations.

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

### End-to-End Timing Analysis (warm run: ~1.4s)

#### Complete Timeline from Logs

```
Time (ms)    Event                                              Source
--------     -----                                              ------
0            Program start (main.rs)                            Rust log
13           VM config setup                                     Rust log
17           krun_start_enter (VM start)                        Rust log
70           Kernel loaded, devices attached                     Rust log
88           vCPU starting execution (kernel boot)              Rust log

--- Guest kernel boot (no logs, loglevel=1) ---

~370         Guest: vsock client start                          guest-debug.log
~437         Guest: vsock connect (0.87ms)                      guest-debug.log
~458         Guest: READY signal sent                           guest-debug.log
~513         Guest: handle_connection ready                     guest-debug.log
~587         Guest: read command from host                      guest-debug.log
~690         Guest: execute_batch response sent                 guest-debug.log

--- Host processing ---

1330         FUSE operations complete                           FUSE stats
~1400        Program end
```

#### Phase Duration Breakdown

| Phase | Duration | Description |
|-------|----------|-------------|
| Host setup | 17ms | Program start → VM start |
| VM device init | 71ms | VM start → vCPU exec |
| **Kernel boot** | **~280ms** | vCPU exec → guest vsock start |
| **Guest init** | **~143ms** | vsock start → vsock ready |
| **FUSE operations** | **~1100ms** | Command execution + file I/O |
| **Total** | **~1400ms** | |

#### Guest-Side Timing (from guest-debug.log)

```
00:00:00.371 - REVERSE_VSOCK_CLIENT START
00:00:00.437 - vsock connect (0.87ms)
00:00:00.458 - READY sent
00:00:00.513 - handle_connection ready (total: 118ms)
00:00:00.587 - read command from host
00:00:00.690 - execute_batch response sent
```

Guest vsock initialization: ~143ms (from start to ready)

### End-to-End Timing Analysis (warm run: ~1.4s)

```
Total Wall Time: ~1.4s
├── FUSE Operations: 1330ms (95%)
│   ├── READ:  1081ms (81% of FUSE)
│   ├── OPEN:   195ms (15% of FUSE)
│   ├── WRITE:   29ms (2%)
│   └── Other:   25ms (2%)
└── WHPX Exits:    54ms (4%)
```

### READ Operation Breakdown (1081ms total)

| Component | Time | Percentage |
|-----------|------|------------|
| **Untracked (virtio overhead)** | **922ms** | **85%** |
| write (virtio queue) | 130ms | 12% |
| alloc_buffer | 12ms | 1% |
| read (file I/O) | 16ms | 1.5% |
| Other (lock, seek, handle) | 1ms | <1% |

**Key Finding:** 85% of READ time is untracked virtio queue overhead, not the actual file I/O.

### OPEN Operation Breakdown (195ms total, 20 ops)

| Component | Time | Per Op |
|-----------|------|--------|
| File::open | 189ms | 9.5ms |
| symlink_metadata | 0.7ms | 35μs |
| read_reparse_kind | 0.02ms | 1μs |
| metadata | 0.01ms | <1μs |

**Key Finding:** 97% of OPEN time is File::open (Windows kernel overhead).

### Cold vs Warm Run Comparison

| Run | Time | Notes |
|-----|------|-------|
| Run 1 (cold) | 4.2s | First run, all caches cold |
| Run 2 (warm) | 1.4s | Windows file system cache warmed |
| Run 3 (warm) | 1.4s | Stable performance |

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
12. **OPEN metadata optimization**: Avoid redundant symlink_metadata calls
    - Added `read_reparse_kind_from_metadata()` to reuse fetched metadata
    - Removed separate `fs::metadata()` call for non-symlinks
    - Negligible improvement vs 10ms File::open bottleneck
13. **virtio queue timing**: Identified inherent protocol overhead
    - pop: 200-700μs (avail ring access)
    - rw: 400-3500μs (Reader/Writer memory mapping)
    - add: 700-4300μs (used ring update)
    - Total ~2ms per READ operation (inherent cost)

## Future Optimization Opportunities

1. **VM reuse mode**: Keep VM running for multiple commands (--reuse_vm)
   - Eliminates cold start overhead (4.2s → 1.4s)
   - Most effective optimization for repeated commands

2. **READ virtio optimization**: 66% of total time
   - Inherent virtio/FUSE protocol overhead
   - Difficult to optimize without architecture changes
   - Consider: larger READ sizes, batching, DAX mapping

3. **OPEN optimization**: 14% of total time
   - Windows kernel bottleneck, limited optimization potential
   - Consider: file handle pooling, aggressive caching

4. **Virtiofs cache warming**: Pre-cache frequently used files

5. **Init binary optimization**: Further reduce size or use compressed init