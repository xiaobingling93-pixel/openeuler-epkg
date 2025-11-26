# End-to-End Tests for epkg

This directory contains end-to-end tests for the epkg package manager.

## Structure

- `vars.sh` - Common variables and configuration
- `lib.sh` - Common shell functions used by tests
- `test.sh` - Script to run a single test (use `-d` flag for debug mode)
- `test-all.sh` - Script to run all tests
- `docker.sh` - Docker run command for e2e tests
- `entry.sh` - Entry script executed inside docker container

## Test Categories

1. **install-remove-upgrade/** - Tests for install, remove, upgrade, and run --help commands
2. **export-import/** - Tests for environment export and import functionality
3. **history-restore/** - Tests for history and restore functionality
4. **public-multi-user/** - Tests for public mode and multi-user scenarios
5. **env-register-activate/** - Tests for environment registration, activation, and PATH management
6. **bare-rootfs/** - Tests for bare rootfs installation on /

## Prerequisites

- Docker installed and running
- epkg binary built at `target/debug/epkg`
- Static epkg binaries in `dist/` directory (for bare-rootfs test)

## Usage

### Run all tests

```bash
./test-all.sh
```

### Run a single test

```bash
./test.sh install-remove-upgrade/test-install-remove-upgrade.sh
```

### Debug a test

```bash
./test.sh -d install-remove-upgrade/test-install-remove-upgrade.sh
```

## Docker Configuration

Tests run in Docker containers with the following setup:

- **Privileged mode**: Required for namespace operations
- **Tmpfs mounts**: `/root/.epkg/envs` and `/opt/epkg/envs` for efficient test runs
- **Persistent mounts**: `/root/.cache/epkg`, `/opt/epkg/cache`, and `/opt/epkg/store` for caching
- **Test directory**: Mounted as read-only at `/e2e`
- **epkg project dir**: Mounted at same dir layout

## Test Details

### install-remove-upgrade

Tests package installation, removal, upgrade, and command execution across multiple OS distributions. For each OS:
- Creates a test environment
- Gets available packages and processes them in batches
- Installs packages with `--prefer-low-version`
- Runs upgrades
- Tests `--help` on installed executables
- Removes packages and verifies behavior

### export-import

Tests environment export and import:
- Creates an environment and installs packages
- Exports the environment configuration
- Creates a new environment from the export
- Verifies packages match between environments
- Tests command execution in imported environment

### history-restore

Tests generation history and restore:
- Creates multiple generations by installing/removing packages
- Verifies history shows correct generations
- Restores to a previous generation
- Verifies package state matches the restored generation

### public-multi-user

Tests public environments and multi-user scenarios:
- Creates test users
- Initializes epkg with shared store
- Creates public and private environments
- Verifies users can see and use public environments
- Tests command execution across user boundaries

### env-register-activate

Tests environment registration and activation:
- Creates multiple environments
- Registers environments with different priorities
- Activates and deactivates environments
- Verifies PATH ordering matches priorities
- Tests unregistration

### bare-rootfs

Tests installation on bare rootfs:
- Starts empty Docker container
- Initializes epkg
- Creates system environment with `--path /`
- Installs packages
- Verifies commands are usable

## Troubleshooting

If a test fails:

1. Use `test.sh -d` to reproduce the issue interactively
2. Check the reproduce script saved in `reproduce/reproduce.sh`
3. Review logs for specific error messages
4. Check if problematic packages can be isolated

## Notes

- All scripts use POSIX shell for compatibility with minimal Docker images
- Tests are designed to be idempotent and clean up after themselves
- Persistent cache and store directories speed up repeated test runs

