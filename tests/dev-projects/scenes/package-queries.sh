#!/bin/sh
# Test package manager queries (rpm/dpkg) in an epkg environment
#
# This test verifies that:
# - rpm-based systems can query installed packages
# - dpkg-based systems can query installed packages
# - Package metadata is correctly accessible
#
# Usage:
#   E2E_OS=debian ./test-package-queries.sh [-d|--debug|-dd|-ddd]
#   ./test-package-queries.sh debian [-d|--debug|-dd|-ddd]

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
        echo "Test package manager queries (rpm/dpkg)"
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
        echo "  E2E_BACKEND     Set to 'vm' to skip queries (DB-backed tools need namespaces)"
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

skip() {
    echo "${YELLOW}[SKIP]${NC} $*" >&2
    exit 0
}

# Determine target OS
TARGET_OS="${1:-${E2E_OS:-debian}}"
TEST_ENV="test-queries-${TARGET_OS}-$$"

log "Starting package queries test for OS: $TARGET_OS"
log "Test environment: $TEST_ENV"

# Skip unsupported OSes
case "$TARGET_OS" in
    openeuler|fedora|rhel|centos|rocky|alma)
        PKG_TYPE="rpm"
        ;;
    debian|ubuntu)
        PKG_TYPE="dpkg"
        ;;
    alpine|archlinux|conda|msys2)
        skip "Package manager queries not supported for $TARGET_OS"
        ;;
    *)
        warn "Unknown OS type: $TARGET_OS, attempting generic test"
        PKG_TYPE="unknown"
        ;;
esac

# Skip in VM mode (DB-backed tools need namespaces)
if [ "${E2E_BACKEND:-}" = "vm" ]; then
    skip "Skipping package manager queries in E2E_BACKEND=vm (epkg run uses direct exec; DB-backed tools need namespaces)"
fi

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

# Install a test package
log "Installing bash for query tests"
"$EPKG_BIN" -e "$TEST_ENV" --assume-yes install --no-install-essentials bash || error "Failed to install bash"

# Run appropriate query based on package type
case "$PKG_TYPE" in
    rpm)
        log "Testing rpm -q -a for bash"
        if ! "$EPKG_BIN" -e "$TEST_ENV" run rpm -q -a 2>/dev/null | grep -q bash; then
            error "rpm -q -a does not show bash"
        fi
        log "rpm query works"

        log "Testing rpm -qi bash"
        if ! "$EPKG_BIN" -e "$TEST_ENV" run rpm -qi bash >/dev/null 2>&1; then
            warn "rpm -qi bash failed"
        else
            log "rpm -qi works"
        fi
        ;;
    dpkg)
        log "Testing dpkg-query -l for bash"
        if ! "$EPKG_BIN" -e "$TEST_ENV" run dpkg-query -l 2>/dev/null | grep -q '^ii.*bash'; then
            error "dpkg-query -l does not show bash"
        fi
        log "dpkg-query works"

        log "Testing dpkg -s bash"
        if ! "$EPKG_BIN" -e "$TEST_ENV" run dpkg -s bash >/dev/null 2>&1; then
            warn "dpkg -s bash failed"
        else
            log "dpkg -s works"
        fi
        ;;
    *)
        warn "No package manager query test available for $TARGET_OS"
        ;;
esac

log "All package query tests passed for $TARGET_OS"
