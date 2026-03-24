#!/bin/sh
# Test nested epkg commands via bash in an epkg environment
#
# This test verifies that:
# - epkg can be invoked from within a bash command inside an environment
# - The output matches between direct and nested invocations
# - Nested epkg correctly uses the same environment
#
# Usage:
#   E2E_OS=debian ./test-epkg-nested.sh [-d|--debug|-dd|-ddd]
#   ./test-epkg-nested.sh debian [-d|--debug|-dd|-ddd]

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
        echo "Test nested epkg commands via bash"
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

# Determine target OS (skip conda - runs in host OS, not environment)
TARGET_OS="${1:-${E2E_OS:-debian}}"
if [ "$TARGET_OS" = "conda" ]; then
    warn "Skipping test for conda (epkg run runs in host OS, not environment)"
    exit 0
fi

TEST_ENV="test-nested-${TARGET_OS}-$$"

log "Starting nested epkg test for OS: $TARGET_OS"
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

# Determine epkg command to use inside environment
# In nested environments, we need to use the same binary
log "Determining epkg command for nested execution"
epkg_cmd="$EPKG_BIN"
log "Using epkg binary: $epkg_cmd"

# Test nested epkg list
log "Testing epkg list via nested bash command"
if ! "$EPKG_BIN" -e "$TEST_ENV" run bash -c "\"$epkg_cmd\" -e \"$TEST_ENV\" list" >/dev/null 2>&1; then
    error "Nested epkg list command failed"
fi
log "Nested epkg list works"

# Compare output between direct and nested
log "Comparing direct vs nested epkg list output"
list1=$("$EPKG_BIN" -e "$TEST_ENV" list 2>/dev/null | tail -n +5 | grep -v '^Total' | grep -v '^$' | sort)
list2=$("$EPKG_BIN" -e "$TEST_ENV" run bash -c "\"$epkg_cmd\" -e \"$TEST_ENV\" list" 2>/dev/null | tail -n +5 | grep -v '^Total' | grep -v '^$' | sort)

if [ "$list1" != "$list2" ]; then
    log "ERROR: epkg list output differs between direct and nested"
    diff_tmp1=$(mktemp)
    diff_tmp2=$(mktemp)
    echo "$list1" > "$diff_tmp1"
    echo "$list2" > "$diff_tmp2"
    diff -u "$diff_tmp1" "$diff_tmp2" >&2 || true
    rm -f "$diff_tmp1" "$diff_tmp2"
    error "Output mismatch between direct and nested epkg list"
fi
log "Direct and nested epkg list outputs match"

log "All nested epkg tests passed for $TARGET_OS"
