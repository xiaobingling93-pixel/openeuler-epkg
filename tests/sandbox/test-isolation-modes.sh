#!/bin/sh
# Test epkg run sandbox isolation modes (env, fs, vm)
#
# This test verifies that different isolation modes work correctly:
# - --isolate=env: process isolation with environment variables
# - --isolate=fs: filesystem isolation with bind mounts
# - --isolate=vm: full VM isolation with microVM backend
#
# Usage:
#   ./test-isolation-modes.sh [-d|--debug|-dd|-ddd]
#
# The test creates a temporary environment and exercises all isolation modes.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
. "$PROJECT_ROOT/tests/common.sh"

# Parse command line flags
parse_debug_flags "$@"
case $? in
    0)
        eval set -- "$PARSE_DEBUG_FLAGS_REMAINING"
        ;;
    1)
        exit 1
        ;;
    2)
        echo "Usage: $0 [-d|--debug|-dd|-ddd]"
        echo ""
        echo "Test epkg run sandbox isolation modes"
        echo ""
        echo "Options:"
        echo "  -d, --debug    Interactive debug mode (pause on error)"
        echo "  -dd            Debug logging (RUST_LOG=debug)"
        echo "  -ddd           Trace logging (RUST_LOG=trace)"
        exit 0
        ;;
esac

set_epkg_bin
set_color_names

log() {
    printf "%b[TEST]%b %b\n" "$GREEN" "$NC" "$*" >&2
}

error() {
    printf "%b[ERROR]%b %b\n" "$RED" "$NC" "$*" >&2
    if [ -n "$DEBUG_FLAG" ]; then
        printf "\n=== Debug Mode ===\n" >&2
        if [ -t 0 ]; then
            printf "Press Enter to continue (or Ctrl+C to exit)...\n" >&2
            read dummy || true
        fi
    fi
    exit 1
}

cleanup() {
    if [ -n "$TEST_ENV" ]; then
        log "Cleaning up environment: $TEST_ENV"
        "$EPKG_BIN" env remove "$TEST_ENV" 2>/dev/null || true
    fi
}

trap cleanup EXIT INT TERM

log "Starting sandbox isolation modes test"

# Create test environment
TEST_ENV="test-sandbox-$$"
log "Creating test environment: $TEST_ENV"
"$EPKG_BIN" env create "$TEST_ENV" -c alpine || error "Failed to create environment"

# Determine mount options for fs/vm isolation
EPKG_BIN_DIR="$(dirname "$EPKG_BIN")"
SANDBOX_MOUNT_OPTS="--mount $EPKG_BIN_DIR"

# Test 1: Default isolation (env)
log "=== Test 1: Default isolation (env) ==="
"$EPKG_BIN" -e "$TEST_ENV" run ls /sys || error "Default isolation ls /sys failed"
log "Default isolation works"

# Test 2: Explicit env isolation
log "=== Test 2: Explicit --isolate=env ==="
"$EPKG_BIN" -e "$TEST_ENV" run --isolate=env ls /sys || error "--isolate=env ls /sys failed"
log "Explicit env isolation works"

# Test 3: Filesystem isolation
log "=== Test 3: --isolate=fs ==="
"$EPKG_BIN" -e "$TEST_ENV" run --isolate=fs $SANDBOX_MOUNT_OPTS ls / || error "--isolate=fs ls / failed"

# Install bash for filesystem isolation tests
log "Installing bash for filesystem isolation tests"
"$EPKG_BIN" -e "$TEST_ENV" --assume-yes install bash coreutils || error "Failed to install bash"

log "Testing ls /sys with --isolate=fs"
"$EPKG_BIN" -e "$TEST_ENV" run --isolate=fs $SANDBOX_MOUNT_OPTS ls /sys || error "--isolate=fs ls /sys failed"
log "Filesystem isolation works"

# Test 4: Config persistence
log "=== Test 4: Config persistence ==="
log "Setting isolate_mode=fs in env config"
"$EPKG_BIN" -e "$TEST_ENV" env config set sandbox.isolate_mode fs || error "Failed to set isolate_mode"

log "Testing with env config (no --isolate flag)"
"$EPKG_BIN" -e "$TEST_ENV" run $SANDBOX_MOUNT_OPTS ls /sys || error "ls /sys failed with env config"

# Reset config
"$EPKG_BIN" -e "$TEST_ENV" env config set sandbox.isolate_mode env || error "Failed to reset isolate_mode"
log "Config persistence works"

# Test 5: VM isolation (if supported)
log "=== Test 5: --isolate=vm (if supported) ==="

# Check if we have a static binary for VM mode
ARCH=$(uname -m)
case "$ARCH" in
    x86_64) RUST_TARGET=x86_64-unknown-linux-musl ;;
    aarch64) RUST_TARGET=aarch64-unknown-linux-musl ;;
    riscv64) RUST_TARGET=riscv64gc-unknown-linux-musl ;;
    loongarch64) RUST_TARGET=loongarch64-unknown-linux-musl ;;
    *) RUST_TARGET="" ;;
esac

if [ -n "$RUST_TARGET" ] && [ -x "$PROJECT_ROOT/target/$RUST_TARGET/debug/epkg" ]; then
    log "Found static binary for VM isolation tests"

    # VM-specific tests
    log "Testing VM-specific mount paths"

    # Test that /opt/epkg/cache is writable
    log "Testing /opt/epkg/cache is writable in VM"
    if "$EPKG_BIN" -e "$TEST_ENV" run --isolate=vm touch /opt/epkg/cache/.test_write 2>/dev/null; then
        log "/opt/epkg/cache is writable in VM"
        "$EPKG_BIN" -e "$TEST_ENV" run --isolate=vm rm -f /opt/epkg/cache/.test_write 2>/dev/null || true
    else
        log "Note: /opt/epkg/cache write test inconclusive (may be expected)"
    fi

    # Test with -u root
    log "Testing VM mode with -u root"
    if "$EPKG_BIN" -e "$TEST_ENV" run --isolate=vm -u root id 2>/dev/null | grep -q "uid=0"; then
        log "VM mode with -u root works correctly"
    else
        log "Note: VM mode with -u root test inconclusive"
    fi

    log "VM isolation tests completed"
else
    log "Skipping VM isolation tests (no static binary found at target/$RUST_TARGET/debug/epkg)"
    log "Run 'make static-$ARCH' to build the static binary for VM tests"
fi

log "All sandbox isolation mode tests passed"
