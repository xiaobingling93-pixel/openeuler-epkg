# VM Virtiofs Mount Architecture

## Overview

When running nested `epkg` commands inside a VM (e.g., `epkg run --isolate=vm`), the guest
needs to correctly detect the environment layout. This document explains the mount strategy
that makes nested epkg work seamlessly.

## The Problem

Host users can have different `shared_store` settings:

- **Private store** (`shared_store=false`): self env at `$HOME/.epkg/envs/self`
- **Shared store** (`shared_store=true`): self env at `/opt/epkg/envs/root/self`

When a non-root host user runs a VM with guest running as root:

1. Host has `shared_store=false` (non-root can't write to `/opt/epkg`)
2. Old code mounted `home_epkg` → `/opt/epkg` in guest
3. Guest's `determine_shared_store()` saw `/opt/epkg/envs` exists → returned true
4. Guest expected self at `/opt/epkg/envs/root/self`, but it was at `/opt/epkg/envs/self`

Result: **Path mismatch!** Nested `epkg -e self` would fail.

## The Solution

Mount `home_epkg` to the location that aligns with host's `shared_store` setting:

### Host `shared_store=false` (non-root host)

Mount `home_epkg` → `/root/.epkg` (guest private location):

```
Guest's determine_shared_store() checks:
1. is_root → true (guest is root)
2. /opt/epkg/envs exists? → false (not mounted there)
3. $HOME/.epkg/envs exists? → true (mounted to /root/.epkg)
Result: shared_store=false ✓

self env: /root/.epkg/envs/self ✓
```

### Host `shared_store=true` (root host)

Mount `opt_epkg` → `/opt/epkg` (guest shared location):

```
Guest's determine_shared_store() checks:
1. is_root → true (guest is root)
2. /opt/epkg/envs exists? → true
Result: shared_store=true ✓

self env: /opt/epkg/envs/root/self ✓
```

## Implementation

### Key Code: `build_virtiofs_mount_specs()` in libkrun.rs

```rust
if is_host_root {
    // Host root: mount to same paths
    try_add_mount(&dirs().home_epkg, None, false, true);
    try_add_mount(&dirs().opt_epkg, None, false, true);
} else if is_guest_root {
    // Non-root host + root guest: mount to /root/.epkg for private layout
    try_add_mount(&dirs().home_epkg, Some(Path::new("/root/.epkg")), false, true);
    try_add_mount(&dirs().home_cache, Some(Path::new("/root/.cache")), false, true);
} else {
    // Non-root host + non-root guest: mount to same paths
    try_add_mount(&dirs().home_epkg, None, false, true);
}
```

### Why This Works

1. **Consistent detection**: Guest's `determine_shared_store()` returns the same value as host
2. **Correct paths**: Guest finds self env at the expected location
3. **No fallbacks needed**: Clean design without special-case path lookups

## Key Insight

The core insight is: **mount paths must create a directory layout that makes
`determine_shared_store()` return the correct value for the guest**.

This is achieved by:
- Understanding how `determine_shared_store()` works (checks `/opt/epkg/envs` and `$HOME/.epkg/envs`)
- Mounting to the location that triggers the correct detection logic
- Avoiding post-mount symlinks or fallback paths in dirs.rs

## Related Files

- `src/libkrun.rs`: `build_virtiofs_mount_specs()` - mount path logic
- `src/utils.rs`: `determine_shared_store()` - shared store detection
- `src/dirs.rs`: `get_env_base_path()` - env path resolution
- `docs/zh/architecture/virtiofs-rootfs.md`: VM rootfs architecture