Tests shall be able to run freely inside AI agent sandbox, so that
AI agent can freely reproduce-debug-fix bugs in a fully automated loop.

Test scripts shall follow below common principles:

- Supports debug mode with -d/-dd/-ddd flags
- do not 'set -e'
- avoid lots of >/dev/null: we are testing! so preserve context and error info
- Assumes epkg is already installed (except for the more heavier tests/in-vm/ which covers install-from-scratch tests and tests that may pollute host os)
- (Re-)creates new env with non-random names for various testing
- Run tests with 'timeout' prefix and '-y|--assume-yes' for automation w/o blocking
- Log to /tmp/ files with non-random names with backup file for grep based problem analyze and comparison with history behavior
- Leaves the env for human/agent debug; i.e. do not remove env in the end, but remove it in the beginning, before create, if it already exists

## Test Structure

```
/c/epkg/tests/
├── README.md               # This file - common test principles
├── common.sh               # Shared shell functions (parse_debug_flags, set_epkg_bin, etc.)
│
├── in-vm/                  # In-VM tests (microVM-based isolation)
│   ├── README.md           # In-VM test documentation
│   ├── cases/              # Test cases (one per feature)
│   ├── host-vars.sh        # Host-side variables
│   ├── vars.sh             # Guest-side variables
│   ├── lib.sh              # Shared shell helpers
│   ├── vm.sh               # VM launcher
│   ├── entry.sh            # Guest entry point
│   ├── test-one.sh         # Run single test
│   ├── test-all.sh         # Run all tests
│   ├── test-iur.sh         # Install/Remove/Upgrade matrix
│   ├── e2e-combo.sh        # VMM/memory knobs wrapper
│   ├── run-alpine-vm.sh    # Alpine-focused tests
│   └── test-dev.sh         # Build-from-source tests
│
├── sandbox/                # Sandbox/isolation tests
│   ├── test-vm-sandbox.sh  # VM sandbox backend tests
│   └── test-isolation-modes.sh  # env/fs/vm isolation tests
│
├── dev-projects/
│   ├── langs/              # Language toolchain tests
│   │   ├── go.sh           # Go toolchain tests
│   │   ├── python.sh       # Python toolchain tests
│   │   ├── rust.sh         # Rust toolchain tests
│   │   └── [other languages]   # java.sh, node.sh, etc.
│   └── scenes/             # Test scenarios (environment behaviors)
│       ├── bash.sh         # Bash installation and /bin/sh tests
│       ├── curl-https.sh   # curl installation and HTTPS tests
│       ├── epkg-nested.sh  # Nested epkg via bash tests
│       ├── package-queries.sh  # rpm/dpkg query tests
│       └── search-paths.sh     # epkg search --paths tests
│
├── misc/                   # Miscellaneous tests
│   └── test-shell-wrapper.sh  # epkg.sh wrapper tests
│
├── solver/                 # Dependency solver unit tests
├── busybox/                # Busybox-specific tests
├── lua/                    # Lua-specific tests
├── osroot/                 # OS root filesystem tests
└── cross-platform/         # Cross-platform tests
```

## Running Tests

### Quick Tests (host-safe, no VM required)

```bash
# Scenario tests
cd /c/epkg/tests/dev-projects/scenes
E2E_OS=debian ./bash.sh
E2E_OS=alpine ./curl-https.sh

# Sandbox isolation tests
cd /c/epkg/tests/sandbox
E2E_OS=debian ./test-isolation-modes.sh

# Shell wrapper tests
cd /c/epkg/tests/misc
./test-shell-wrapper.sh
```

### In-VM Tests (VM-based isolation)

```bash
cd /c/epkg/tests/in-vm

# Run all tests
./test-all.sh

# Run single test
./test-one.sh cases/env-register-activate.sh
./test-one.sh -d cases/bare-rootfs.sh  # Debug mode

# With specific OS
E2E_OS=alpine ./test-one.sh cases/bare-rootfs.sh

# With specific VMM/Memory
E2E_VMM=qemu E2E_VM_MEMORY=8G ./e2e-combo.sh cases/install-remove-upgrade.sh

# Alpine-focused
./run-alpine-vm.sh

# Install/Remove/Upgrade matrix
./test-iur.sh

# Build-from-source
./test-dev.sh
```

## Debug Flags

All test scripts support consistent debug flags:

| Flag | Effect |
|------|--------|
| `-d` / `--debug` | Interactive mode (pause on error) |
| `-dd` | Debug logging (`RUST_LOG=debug`) |
| `-ddd` | Trace logging (`RUST_LOG=trace`, `RUST_BACKTRACE=1`) |

## Common Functions (common.sh)

- `parse_debug_flags "$@"` - Parse -d/-dd/-ddd flags
- `set_epkg_bin` - Find and set `EPKG_BIN`
- `set_color_names` - Set up color variables

## Environment Variables

| Variable | Used In | Description |
|----------|---------|-------------|
| `E2E_OS` | All | Target OS/distro (debian, alpine, fedora, etc.) |
| `E2E_VMM` | E2E, Sandbox | VMM backend (qemu, libkrun) |
| `E2E_VM_MEMORY` | E2E | VM RAM (default: 16G) |
| `E2E_BACKEND` | Guest | Set to `vm` in VM guests |
| `RUST_LOG` | All | Rust logging level |
| `RUST_BACKTRACE` | All | Enable backtraces |
