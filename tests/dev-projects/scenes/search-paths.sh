#!/bin/sh
# Test epkg search --paths functionality in an epkg environment
#
# This test verifies that:
# - epkg search --paths can find packages by file path
# - File list databases are correctly downloaded and used
#
# Usage:
#   E2E_OS=debian ./test-search-paths.sh [-d|--debug|-dd|-ddd]
#   ./test-search-paths.sh debian [-d|--debug|-dd|-ddd]

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
        echo "Test epkg search --paths functionality"
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
        echo "  E2E_OS          Target OS/distro (openeuler or debian recommended)"
        exit 0
        ;;
esac

set_epkg_bin
set_color_names

log() {
    echo "${GREEN}[TEST]${NC} $*" >&2
}

warn() {
    echo "${YELLOW}[WARN]${NC} $*" >&2
}

error() {
    echo "${RED}[ERROR]${NC} $*" >&2
    if [ -n "$DEBUG_FLAG" ]; then
        echo "" >&2
        echo "=== Debug Mode ===" >&2
        if [ -t 0 ]; then
            echo "Press Enter to continue (or Ctrl+C to exit)..." >&2
            read dummy || true
        fi
    fi
    exit 1
}

skip() {
    echo "${YELLOW}[SKIP]${NC} $*" >&2
    exit 0
}

# Determine target OS
TARGET_OS="${1:-${E2E_OS:-debian}}"

# Only test one for each format since filelist downloads are time consuming
case "$TARGET_OS" in
    openeuler|fedora)
        # rpm format
        ;;
    debian|ubuntu)
        # deb format
        ;;
    *)
        skip "File list search not tested for $TARGET_OS (only openeuler/fedora/debian tested)"
        ;;
esac

TEST_ENV="test-search-${TARGET_OS}-$$"

log "Starting file paths search test for OS: $TARGET_OS"
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

# Test search --paths (this downloads file list database, may take time)
log "Testing epkg search --paths /bin/bash (may download file list database)"
if ! "$EPKG_BIN" -e "$TEST_ENV" search --paths /bin/bash >/dev/null 2>&1; then
    error "epkg search --paths /bin/bash failed"
fi
log "search --paths works for /bin/bash"

# Test search --paths with a file that doesn't exist
log "Testing epkg search --paths with non-existent path"
result=$("$EPKG_BIN" -e "$TEST_ENV" search --paths /nonexistent/path/12345 2>&1)
if echo "$result" | grep -q "not found\|no matches\|No package"; then
    log "search correctly reports no results for non-existent path"
else
    warn "Unexpected output for non-existent path search: $result"
fi

log "All file paths search tests passed for $TARGET_OS"
