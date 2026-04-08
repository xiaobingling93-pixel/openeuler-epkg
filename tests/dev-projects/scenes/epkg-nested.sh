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

# Check if running on macOS with VM mode - inside VM, host paths are not accessible
# Inside VM, epkg is available at /usr/bin/epkg or /opt/epkg/envs/self/usr/bin/epkg
case "$(uname -s)" in
    Darwin)
        # On macOS, epkg runs in a VM. Inside the VM, the epkg binary is at a different path.
        # Use just 'epkg' since it's in PATH inside the VM
        epkg_cmd="epkg"
        log "macOS detected: using 'epkg' from PATH inside VM"
        ;;
    *)
        epkg_cmd="$EPKG_BIN"
        log "Using epkg binary: $epkg_cmd"
        ;;
esac

# Test nested epkg list
log "Testing epkg list via nested bash command"
# On macOS VM, nested epkg uses 'root' user, so environment paths differ.
# Use EPKG_ACTIVE_ENV (already set by parent epkg) instead of -e option.
case "$(uname -s)" in
    Darwin)
        # Inside VM, EPKG_ACTIVE_ENV is set, so just run 'epkg list' without -e
        if ! "$EPKG_BIN" -e "$TEST_ENV" run bash -c "\"$epkg_cmd\" list" >/dev/null 2>&1; then
            error "Nested epkg list command failed"
        fi
        ;;
    *)
        if ! "$EPKG_BIN" -e "$TEST_ENV" run bash -c "\"$epkg_cmd\" -e \"$TEST_ENV\" list" >/dev/null 2>&1; then
            error "Nested epkg list command failed"
        fi
        ;;
esac
log "Nested epkg list works"

# Compare output between direct and nested
log "Comparing direct vs nested epkg list output"
list1=$("$EPKG_BIN" -e "$TEST_ENV" list 2>/dev/null | tail -n +5 | grep -v '^Total' | grep -v '^$' | sort)

case "$(uname -s)" in
    Darwin)
        # Inside VM, EPKG_ACTIVE_ENV is set, so just run 'epkg list' without -e
        list2=$("$EPKG_BIN" -e "$TEST_ENV" run bash -c "\"$epkg_cmd\" list" 2>/dev/null | tail -n +5 | grep -v '^Total' | grep -v '^$' | sort)
        ;;
    *)
        list2=$("$EPKG_BIN" -e "$TEST_ENV" run bash -c "\"$epkg_cmd\" -e \"$TEST_ENV\" list" 2>/dev/null | tail -n +5 | grep -v '^Total' | grep -v '^$' | sort)
        ;;
esac

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

# Test nested epkg install via 'epkg run epkg install'
log "Testing nested epkg install via 'epkg run epkg install'"
TEST_PKG="tree"
case "$(uname -s)" in
    Darwin)
        # On macOS VM, EPKG_ACTIVE_ENV is set, so nested epkg auto-detects the environment
        if ! "$EPKG_BIN" -e "$TEST_ENV" run "$epkg_cmd" --assume-yes install "$TEST_PKG" >/dev/null 2>&1; then
            error "Nested epkg install command failed"
        fi
        ;;
    *)
        if ! "$EPKG_BIN" -e "$TEST_ENV" run "$epkg_cmd" -e "$TEST_ENV" --assume-yes install "$TEST_PKG" >/dev/null 2>&1; then
            error "Nested epkg install command failed"
        fi
        ;;
esac
log "Nested epkg install works"

# Verify install succeeded with nested epkg list
log "Verifying nested epkg install succeeded"
case "$(uname -s)" in
    Darwin)
        if ! "$EPKG_BIN" -e "$TEST_ENV" run "$epkg_cmd" list 2>/dev/null | grep -qw "$TEST_PKG"; then
            error "Package $TEST_PKG not found after nested install"
        fi
        ;;
    *)
        if ! "$EPKG_BIN" -e "$TEST_ENV" run "$epkg_cmd" -e "$TEST_ENV" list 2>/dev/null | grep -qw "$TEST_PKG"; then
            error "Package $TEST_PKG not found after nested install"
        fi
        ;;
esac
log "Nested epkg install verification passed"

log "All nested epkg tests passed for $TARGET_OS"
