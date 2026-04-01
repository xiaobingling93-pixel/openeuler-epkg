# Windows WHPX VM Performance Analysis

## Current Baseline (2026-04-01, after sleep optimization)

### Timing Breakdown for `epkg run -e alpine echo hello`

| Phase | Time | Description |
|-------|------|-------------|
| VM config | ~1.5ms | Create and configure VM context |
| Guest connect | ~1.4s | VM boot + kernel init + vsock connect |
| Write+flush | ~0.1ms | Send command to guest |
| Response wait | ~1.4s | Guest execute + return result |
| **Total** | **~2.8-3.0s** | End-to-end latency |

### Previous Baseline (before sleep optimization)

| Phase | Time | Description |
|-------|------|-------------|
| VM config | ~1.5ms | Create and configure VM context |
| Guest connect | ~1.4s | VM boot + kernel init + vsock connect |
| **Sleep delays** | **~1.1s** | Two fixed sleeps removed |
| Write+flush | ~0.1ms | Send command to guest |
| Response wait | ~1.8-2.0s | Guest execute + return result |
| **Total** | **~3.3-3.5s** | End-to-end latency |

### Key Findings

1. **VM boot time (1.4s)** is dominated by:
   - WHPX VM creation
   - Kernel boot and init
   - This is hard to optimize without kernel changes

2. **Sleep delays removed (1.1s)**:
   - 1000ms vsock handshake wait → 10ms (pipe buffer propagation)
   - 100ms guest ready wait → removed (guest is already ready)
   - WaitNamedPipeA already ensures named pipe is ready

3. **Response wait (1.4s)** after optimization:
   - For `echo hello`, this includes command execution + vsock round-trip
   - Virtiofs overhead for reading init binary and libraries

4. **Virtiofs optimizations already applied**:
   - File handle caching
   - readdirplus implementation
   - FUSE operations reduced from ~26,000 to ~1,100 (fixed 191MB debug init)

### Remaining Bottlenecks

1. **VM boot time (1.4s)** - WHPX overhead, difficult to optimize
2. **Response wait (~1.4s)** - Includes virtiofs operations for command execution
3. **Virtiofs overhead** - Each command needs to read binaries/libraries from host

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

## Future Optimization Opportunities

1. **VM reuse mode**: Keep VM running for multiple commands (--reuse_vm)
2. **Virtiofs cache warming**: Pre-cache frequently used files
3. **Init binary optimization**: Further reduce size or use compressed init
4. **WHPX alternatives**: Consider other virtualization backends if available