#!/bin/bash
# Cross-platform package manager test runner
# Runs tests for package channels (conda, homebrew, msys2, etc.) based on host OS
#
# Usage:
#   ./run.sh                    # Run all applicable channel tests
#   ./run.sh -c conda           # Run conda channel tests only
#   ./run.sh -c conda -k        # Keep environment (don't cleanup)
#   ./run.sh -d                 # Debug mode (set -x)
#
# Supported channels:
#   - conda     (Linux, macOS, Windows)
#   - homebrew  (macOS, Linux) - planned
#   - msys2     (Windows) - planned

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Source common functions
. "$PROJECT_ROOT/tests/common.sh"

# Colors
set_color_names

# Parse arguments
DEBUG_FLAG=""
CLEANUP_ENV=""
SELECT_CHANNEL=""
SELECT_TEST=""
REMAINING=""

while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  -c, --channel CHANNEL     Run tests for specific channel (conda, brew)"
            echo "  -t, --test TEST           Run specific test suite (utils, langs, build, scipy, ml, pkgmgr)"
            echo "  -r, --remove              Remove test environment after tests (cleanup)"
            echo "  -d, --debug               Enable debug mode (set -x)"
            echo "  -dd                       More verbose debug"
            echo "  -h, --help                Show this help"
            echo ""
            echo "Channels:"
            echo "  conda    - Conda package channel (Linux, macOS, Windows)"
            echo "  brew     - Homebrew package channel (macOS, Linux)"
            echo ""
            echo "Test suites:"
            echo "  utils    - Utility packages (jq, tree)"
            echo "  langs    - Programming languages (python, perl, ruby, nodejs, go)"
            echo "  build    - Build systems (cmake, make, ninja)"
            echo "  scipy    - Scientific computing (numpy, scipy, pandas)"
            echo "  ml       - Machine learning (scikit-learn)"
            echo "  pkgmgr   - Package management operations"
            echo ""
            echo "Examples:"
            echo "  $0                        # Run all channel tests"
            echo "  $0 -c conda               # Run conda channel tests only"
            echo "  $0 -c brew -t langs       # Run brew languages test only"
            echo ""
            exit 0
            ;;
        -c|--channel)
            shift
            SELECT_CHANNEL="$1"
            ;;
        -t|--test)
            shift
            SELECT_TEST="$1"
            ;;
        -dd)
            DEBUG_FLAG="-dd"
            export RUST_LOG=debug
            set -x
            ;;
        -d|--debug)
            DEBUG_FLAG="-d"
            set -x
            ;;
        *)
            REMAINING="$REMAINING $1"
            ;;
    esac
    shift
done

# Find epkg binary
set_epkg_bin
export EPKG_BIN

export LOG_DIR="${LOG_DIR:-/tmp}"
export SKIP_CLEANUP="${KEEP_ENV}"

log() {
    echo -e "[$(date '+%Y-%m-%d %H:%M:%S')] $*" >&2
}

# Detect host OS
# Returns: linux, macos, windows, or unknown
detect_host_os() {
    case "$(uname -s)" in
        Linux*)     echo "linux" ;;
        Darwin*)    echo "macos" ;;
        CYGWIN*|MINGW*|MSYS*) echo "windows" ;;
        *)          echo "unknown" ;;
    esac
}

HOST_OS=$(detect_host_os)
log "Detected host OS: $HOST_OS"
log "EPKG binary: $EPKG_BIN"

# List available channel tests
list_channel_tests() {
    for script in "$SCRIPT_DIR/channels/"*.sh; do
        [ -f "$script" ] && basename "$script" .sh
    done
}

# Check if channel test should run
should_run_channel() {
    local channel="$1"

    # If specific channel requested, only run that
    if [ -n "$SELECT_CHANNEL" ]; then
        [ "$channel" = "$SELECT_CHANNEL" ]
        return $?
    fi

    # Otherwise, check if channel is applicable for this host OS
    case "$channel" in
        conda)
            # Conda works on all OSes
            return 0
            ;;
        homebrew)
            # Homebrew only on macOS and Linux
            [ "$HOST_OS" = "macos" ] || [ "$HOST_OS" = "linux" ]
            return $?
            ;;
        msys2)
            # MSYS2 only on Windows
            [ "$HOST_OS" = "windows" ]
            return $?
            ;;
        *)
            return 0
            ;;
    esac
}

# Run channel test
run_channel_test() {
    local channel="$1"
    local script="$SCRIPT_DIR/channels/${channel}.sh"

    if [ ! -x "$script" ]; then
        log "${YELLOW}Channel test script not found: $script${NC}"
        return 1
    fi

    log "${GREEN}Running $channel tests...${NC}"

    export ENV_NAME="test-${channel}"
    export CHANNEL_NAME="$channel"
    export SELECT_TEST="$SELECT_TEST"

    if ! "$script"; then
        log "${RED}$channel tests failed${NC}"
        return 1
    fi

    log "${GREEN}$channel tests passed${NC}"
    return 0
}

# Main
main() {
    log "Starting cross-platform tests"
    log "===================================="

    local failed=0
    local failed_channel=""

    for channel in $(list_channel_tests); do
        if should_run_channel "$channel"; then
            if ! run_channel_test "$channel"; then
                failed=$((failed + 1))
                if [ -z "$failed_channel" ]; then
                    failed_channel="$channel"
                fi
            fi
        else
            log "${YELLOW}Skipping $channel (not applicable for $HOST_OS)${NC}"
        fi
    done

    log "===================================="

    if [ $failed -gt 0 ]; then
        log "${RED}Tests failed for: $failed_channel${NC}"
        if [ -n "$SELECT_CHANNEL" ]; then
            log "Reproduce with: $SCRIPT_DIR/run.sh -c $SELECT_CHANNEL"
        fi
        exit 1
    fi

    log "${GREEN}All cross-platform tests passed${NC}"
}

# Run main
main
