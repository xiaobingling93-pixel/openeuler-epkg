# In-VM Tests for epkg

This directory contains tests that run **inside a microVM** via `epkg run --isolate=vm` (see `vm.sh`), not Docker. These tests are isolated from the host OS and can safely modify system state.

## Directory Structure

```
/c/epkg/tests/
├── in-vm/                    # In-VM tests (microVM-based isolation)
│   ├── cases/                # Test case scripts (one per feature)
│   ├── host-vars.sh          # Host-side variables (E2E_* defaults)
│   ├── vars.sh               # Guest-side variables (ALL_OS list)
│   ├── lib.sh                # Shared shell helpers
│   ├── vm.sh                 # VM launcher (epkg run --isolate=vm)
│   ├── entry.sh              # Guest-side entry point
│   ├── test-one.sh           # Run a single test
│   ├── test-all.sh           # Run all tests in cases/
│   ├── test-iur.sh           # Install/Remove/Upgrade matrix
│   ├── e2e-combo.sh          # Thin wrapper for VMM/memory knobs
│   ├── run-alpine-vm.sh      # Alpine-focused VM runs
│   └── test-dev.sh           # Build-from-source test runner
│
├── sandbox/                  # Sandbox/isolation tests (moved from e2e)
│   ├── test-vm-sandbox.sh    # VM sandbox backend tests
│   └── test-isolation-modes.sh  # env/fs/vm isolation tests
│
├── dev-projects/
│   ├── langs/                # Language toolchain tests (go.sh, python.sh, rust.sh, etc.)
│   └── scenes/               # Test scenarios (split from bash-sh.sh)
│       ├── bash.sh           # Bash installation and /bin/sh tests
│       ├── curl-https.sh     # curl installation and HTTPS tests
│       ├── epkg-nested.sh    # Nested epkg via bash tests
│       ├── package-queries.sh   # rpm/dpkg query tests
│       └── search-paths.sh   # epkg search --paths tests
│
└── misc/                     # Miscellaneous tests (moved from e2e)
    └── test-shell-wrapper.sh # epkg.sh shell wrapper tests
```

## Architecture

### Three-Layer Test Architecture

**Layer 1: Host-side scripts** (`test-one.sh`, `test-all.sh`, `host-vars.sh`)
- Run on the host Linux machine
- Parse debug flags (`-d`, `-dd`, `-ddd`)
- Set environment variables (`E2E_VMM`, `E2E_VM_MEMORY`, `E2E_OS`)
- Locate the static musl epkg binary

**Layer 2: VM launcher** (`vm.sh`)
- Creates harness environment `bare-alpine-e2e` if not exists
- Mounts: tmpfs paths, download cache, log dir, resolv.conf, project root
- Runs `epkg -e bare-alpine-e2e run --isolate=vm` with user `root`
- Sets `E2E_BACKEND=vm` in guest for nested-microVM behavior

**Layer 3: Guest entry** (`entry.sh`)
- Runs inside the VM guest
- Sets timezone from host
- Runs `epkg self install` if not already installed
- Sources `vars.sh` and `lib.sh`
- Executes the actual test script

## Prerequisites

- Linux host with working KVM/QEMU user networking, virtiofsd, and a static `epkg`
- Harness environment `bare-alpine-e2e` created automatically by `vm.sh`
- First guest `self install` needs outbound HTTPS
- Host `~/.cache/epkg/downloads` mounted at guest `/opt/epkg/cache/downloads`

## Quick Start

### Run All In-VM Tests

```bash
cd /c/epkg/tests/in-vm
./test-all.sh
```

### Run a Single Test

```bash
./test-one.sh cases/env-register-activate.sh
./test-one.sh -d cases/bare-rootfs.sh    # Debug mode
```

### Run with Specific OS

```bash
E2E_OS=alpine ./test-one.sh cases/bare-rootfs.sh
./test-one.sh cases/bare-rootfs.sh debian    # As argument
```

### Run with Different VMM/Memory

```bash
E2E_VMM=qemu E2E_VM_MEMORY=8G ./e2e-combo.sh cases/install-remove-upgrade.sh
```

### Run Alpine-Focused Tests

```bash
./run-alpine-vm.sh
```

## Test Categories

### Core In-VM Tests (`cases/`)

| Test | Purpose |
|------|---------|
| `bare-rootfs.sh` | Minimal chroot test with busybox + epkg |
| `install-remove-upgrade.sh` | Package lifecycle with batch processing |
| `build-from-source.sh` | Build epkg from source in containers |
| `env-register-activate.sh` | Environment management and PATH |
| `env-path-auto-discovery.sh` | `--root` option and `.eenv` discovery |
| `history-restore.sh` | History tracking and restore |
| `export-import.sh` | Environment export/import |
| `public-multi-user.sh` | Multi-user public/private environments |

**Note:** Some tests have been moved:
- `bash-sh.sh` → Split into `tests/dev-projects/scenes/*.sh`
- `sandbox-run.sh` → `tests/sandbox/test-isolation-modes.sh`
- `epkg-wrapper.sh` → `tests/misc/test-shell-wrapper.sh`

### Sandbox Tests (`tests/sandbox/`)

```bash
# Test VM sandbox backends
./tests/sandbox/test-vm-sandbox.sh [--vmm=qemu|libkrun]

# Test isolation modes (env/fs/vm)
E2E_OS=debian ./tests/sandbox/test-isolation-modes.sh
```

### Scenario Tests (`tests/dev-projects/scenes/`)

```bash
# Test bash installation
E2E_OS=debian ./tests/dev-projects/scenes/bash.sh

# Test curl HTTPS
E2E_OS=alpine ./tests/dev-projects/scenes/curl-https.sh

# Test nested epkg
E2E_OS=fedora ./tests/dev-projects/scenes/epkg-nested.sh

# Test package queries
E2E_OS=debian ./tests/dev-projects/scenes/package-queries.sh

# Test file path search
E2E_OS=openeuler ./tests/dev-projects/scenes/search-paths.sh
```

### Shell Wrapper Tests (`tests/misc/`)

```bash
# Test epkg.sh wrapper
./tests/misc/test-shell-wrapper.sh
```

## Environment Variables

| Variable | Purpose |
|----------|---------|
| `E2E_BARE_ENV` | Harness env name (default: `bare-alpine-e2e`) |
| `E2E_VMM` | VMM preference: `qemu` or `libkrun,qemu` |
| `E2E_VM_MEMORY` | VM RAM (default: `16G`) |
| `E2E_VM_CPUS` | vCPU count (optional) |
| `E2E_OS` | Target OS for multi-OS tests |
| `E2E_DOWNLOAD_CACHE` | Host download cache path |
| `E2E_LOG_DIR` | Host log directory (guest: `/var/log/epkg-in-vm`) |
| `E2E_RESOLV_CONF` | Custom resolv.conf for guest |
| `E2E_COMBO` | Label for combo test logs |

## Debug Flags

All test scripts support consistent debug flags:

| Flag | Effect |
|------|--------|
| `-d` / `--debug` | Interactive mode (pause on error) |
| `-dd` | Debug logging (`RUST_LOG=debug`) |
| `-ddd` | Trace logging (`RUST_LOG=trace`, `RUST_BACKTRACE=1`) |

## VM Reuse

After a command finishes, the guest can wait for another connection:

```bash
epkg -e myenv run --isolate=vm --vmm=qemu --vm-keep-timeout 120 bash
# In another terminal:
epkg -e myenv run --isolate=vm --vmm=qemu --reuse --vm-keep-timeout 60 -- sh -c 'echo hi'
```

## Implementation Details

- `E2E_BACKEND=vm` is set in the guest for nested-microVM behavior
- Host project root is mounted read-only at the same path in guest
- Guest DNS uses public resolvers (8.8.8.8, 1.1.1.1) before QEMU slirp
- Debug pause only prompts when stdin is a TTY (non-blocking for CI)
