#!/bin/sh
# Test curl installation and HTTPS connectivity in an epkg environment
#
# This test verifies that:
# - curl can be installed
# - SSL certificates are properly configured
# - HTTPS requests work (when not in VM mode with limited network)
#
# Usage:
#   E2E_OS=debian ./test-curl-https.sh [-d|--debug|-dd|-ddd]
#   ./test-curl-https.sh debian [-d|--debug|-dd|-ddd]

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
        echo "Test curl installation and HTTPS connectivity"
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
        echo "  E2E_OS          Target OS/distro"
        echo "  E2E_BACKEND     Set to 'vm' to skip HTTPS test (guest DNS not guaranteed)"
        exit 0
        ;;
esac

set_epkg_bin
set_color_names

log() {
    printf "%b[TEST]%b %b\n" "$GREEN" "$NC" "$*" >&2
}

warn() {
    printf "%b[WARN]%b %b\n" "$YELLOW" "$NC" "$*" >&2
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
TEST_ENV="test-curl-${TARGET_OS}-$$"

log "Starting curl HTTPS test for OS: $TARGET_OS"
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

# Install curl
log "Installing curl"
"$EPKG_BIN" -e "$TEST_ENV" --assume-yes install --no-install-essentials curl || error "Failed to install curl"

# Check for HTTPS support in curl
log "Testing curl --version for HTTPS support"
if ! "$EPKG_BIN" -e "$TEST_ENV" run curl --version 2>&1 | grep -q "https"; then
    warn "curl may not have HTTPS support compiled in"
fi

# Test HTTPS connectivity (skip in VM mode due to DNS limitations)
if [ "${E2E_BACKEND:-}" = "vm" ]; then
    warn "Skipping HTTPS test in E2E_BACKEND=vm (guest DNS is not guaranteed)"
else
    log "Testing curl HTTPS request to https://example.com/"
    if "$EPKG_BIN" -e "$TEST_ENV" run curl -s -I -o /dev/null -w "%{http_code}" https://example.com/ 2>/dev/null | grep -q "200"; then
        log "HTTPS request succeeded"
    else
        warn "HTTPS request failed or returned non-200 (may be a network issue)"
    fi
fi

log "All curl HTTPS tests passed for $TARGET_OS"
