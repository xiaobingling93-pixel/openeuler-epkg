#!/bin/sh
# Test bash installation and /bin/sh usability in an epkg environment
#
# This test verifies that:
# - Bash can be installed in an environment
# - /bin/sh exists and is usable
# - The shell can execute simple commands
#
# Usage:
#   E2E_OS=debian ./test-bash.sh [-d|--debug|-dd|-ddd]
#   ./test-bash.sh debian [-d|--debug|-dd|-ddd]

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
        echo "Test bash installation and /bin/sh usability"
        echo ""
        echo "Arguments:"
        echo "  OS              Target OS/distro (default: from E2E_OS env var, or debian)"
        echo ""
        echo "Options:"
        echo "  -d, --debug    Interactive debug mode"
        echo "  -dd            Debug logging"
        echo "  -ddd           Trace logging"
        echo ""
        echo "Environment:"
        echo "  E2E_OS          Target OS/distro (e.g., debian, alpine, fedora)"
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
TARGET_OS="${1:-${E2E_OS:-debian}}"
TEST_ENV="test-bash-${TARGET_OS}-$$"

log "Starting bash installation test for OS: $TARGET_OS"
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

# Test /bin/sh usability
log "Testing /bin/sh usability"
if ! "$EPKG_BIN" -e "$TEST_ENV" run /bin/sh -c 'exit 0'; then
    error "/bin/sh not usable in environment"
fi
log "/bin/sh is usable"

# Test /bin/sh can execute a command
log "Testing /bin/sh command execution"
output=$("$EPKG_BIN" -e "$TEST_ENV" run /bin/sh -c 'echo hello-from-sh' 2>&1)
if ! echo "$output" | grep -q "hello-from-sh"; then
    error "/bin/sh command execution failed"
fi
log "/bin/sh command execution works"

# Test epkg info bash
log "Testing epkg info bash"
if ! "$EPKG_BIN" -e "$TEST_ENV" info bash >/dev/null 2>&1; then
    error "epkg info bash failed"
fi
log "epkg info bash works"

log "All bash tests passed for $TARGET_OS"
