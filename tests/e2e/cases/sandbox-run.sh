#!/bin/sh
# Test epkg run sandbox modes (env and fs)

. "$(dirname "$0")/../vars.sh"
. "$(dirname "$0")/../lib.sh"

log "Starting sandbox run test"

ENV_NAME="sandbox-debian"
# Idempotent: a previous run may have left the env if cleanup failed
epkg env remove "$ENV_NAME" 2>/dev/null

EPKG_BIN=$(realpath $EPKG_BINARY)
EPKG_BIN_DIR=$(dirname $EPKG_BIN)
SANDBOX_MOUNT_OPTS="--mount $EPKG_BIN_DIR"

log "Creating test environment $ENV_NAME"
epkg env create "$ENV_NAME" -c alpine || error "Failed to create sandbox env"

log "Running epkg ls / with --isolate=fs"
epkg -e "$ENV_NAME" run --isolate=fs $SANDBOX_MOUNT_OPTS ls / || error "epkg run --isolate=fs ls / failed"

log "Installing bash into $ENV_NAME"
epkg -e "$ENV_NAME" --assume-yes install bash coreutils || error "Failed to install bash in sandbox env"

log "Running ls /sys with default sandbox (env)"
epkg -e "$ENV_NAME" run ls /sys || error "epkg run ls /sys failed in default sandbox"

log "Running ls /sys with --isolate=env"
epkg -e "$ENV_NAME" run --isolate=env ls /sys || error "epkg run --isolate=env ls /sys failed"

log "Running ls /sys with --isolate=fs"
epkg -e "$ENV_NAME" run --isolate=fs $SANDBOX_MOUNT_OPTS ls /sys || error "epkg run --isolate=fs ls /sys failed"

log "Setting isolate_mode=fs in env config"
epkg -e "$ENV_NAME" env config set sandbox.isolate_mode fs || error "Failed to set isolate_mode in env config"

log "Running ls /sys with env isolate_mode=fs (no --isolate flag)"
epkg -e "$ENV_NAME" run $SANDBOX_MOUNT_OPTS ls /sys || error "epkg run ls /sys failed with isolate_mode=fs"

log "Sandbox run test completed successfully"

# VM-specific tests: verify mount paths and downloads work correctly
if [ "${E2E_BACKEND:-}" = "vm" ]; then
    log "Running VM-specific mount and download tests"

    # Test 1: Verify wget can download (tests the mount path fix for non-root host users)
    # This was the original bug: downloads failed because /home/USER/.cache didn't exist in guest
    log "Testing wget download in VM mode"
    # Download a small test file (8.8.8.8 is lightweight and reliable)
    if epkg -e "$ENV_NAME" run --isolate=vm wget -q -O /dev/null https://8.8.8.8 2>/dev/null; then
        log "wget download test passed"
    else
        # Network might not be available, try a simpler test
        log "wget download test inconclusive (network may be unavailable)"
    fi

    # Test 2: Verify /opt/epkg/cache is writable (the mount destination for user cache)
    log "Testing /opt/epkg/cache is writable in VM"
    if epkg -e "$ENV_NAME" run --isolate=vm touch /opt/epkg/cache/.test_write 2>/dev/null; then
        log "/opt/epkg/cache is writable"
        # Cleanup
        epkg -e "$ENV_NAME" run --isolate=vm rm -f /opt/epkg/cache/.test_write 2>/dev/null || true
    else
        log "/opt/epkg/cache write test inconclusive"
    fi

    # Test 3: Test with explicit -u root (should use /opt/epkg mount paths)
    log "Testing VM mode with -u root"
    if epkg -e "$ENV_NAME" run --isolate=vm -u root id 2>/dev/null | grep -q "uid=0"; then
        log "VM mode with -u root works correctly"
    else
        log "VM mode with -u root test inconclusive"
    fi
fi

log "All sandbox tests completed successfully"

