#!/bin/sh
# Test script for interactive bash in VM mode
#
# This test verifies that:
# - Non-interactive bash commands work
# - PTY devices are available in VM
# - Interactive stdin works
#
# Usage:
#   E2E_OS=alpine ./interactive-bash.sh [-d|--debug|-dd|-ddd]
#   ./interactive-bash.sh alpine [-d|--debug|-dd|-ddd]

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
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
        echo "Usage: $0 [OS] [-d|--debug|-dd|-ddd]"
        echo ""
        echo "Test interactive bash in VM mode"
        echo ""
        echo "Arguments:"
        echo "  OS              Target OS/distro (default: from E2E_OS env var, or alpine)"
        echo ""
        echo "Options:"
        echo "  -d, --debug    Interactive debug mode"
        echo "  -dd            Debug logging"
        echo "  -ddd           Trace logging"
        echo ""
        echo "Environment:"
        echo "  E2E_OS          Target OS/distro"
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

# Determine target OS
TARGET_OS="${1:-${E2E_OS:-alpine}}"
TEST_ENV="test-interactive-${TARGET_OS}-$$"

log "Starting interactive bash test for OS: $TARGET_OS"
log "Test environment: $TEST_ENV"

cleanup() {
    if [ -n "$TEST_ENV" ]; then
        log "Cleaning up environment: $TEST_ENV"
        "$EPKG_BIN" --assume-yes env remove "$TEST_ENV" 2>/dev/null || true
    fi
}

trap cleanup EXIT INT TERM

# Create environment
log "Creating environment for $TARGET_OS"
"$EPKG_BIN" env remove "$TEST_ENV" 2>/dev/null || true
"$EPKG_BIN" env create "$TEST_ENV" -c "$TARGET_OS" || error "Failed to create environment"

# Install bash
log "Installing bash"
"$EPKG_BIN" -e "$TEST_ENV" --assume-yes install --no-install-essentials bash || error "Failed to install bash"

EPKG_CMD="$EPKG_BIN -e $TEST_ENV"

log "=== Test 1: Non-interactive command (should work) ==="
$EPKG_CMD run -- bash -c "echo HELLO" || error "Test 1 failed"

log "=== Test 2: Multiple commands via -c (should work) ==="
$EPKG_CMD run -- bash -c "id; whoami; pwd" || error "Test 2 failed"

log "=== Test 3: Check PTY devices in VM ==="
$EPKG_CMD run -- stat /dev/ptmx || error "Test 3 failed"

log "=== Test 4: Check /dev/pts/ ==="
$EPKG_CMD run -- ls -la /dev/pts/ || error "Test 4 failed"

log "=== Test 5: Check stdin in VM (piped) ==="
echo "test" | $EPKG_CMD run -- bash -c "cat" || error "Test 5 failed"

log "=== Test 6: Interactive stdin test ==="
echo "id" | $EPKG_CMD run bash || error "Test 6 failed"

log "=== Test 7: Check if bash is available ==="
$EPKG_CMD run -- which bash || error "Test 7 failed: bash not found"
$EPKG_CMD run -- bash --version | head -1 || error "Test 7 failed: bash --version"

log "All interactive bash tests passed for $TARGET_OS"