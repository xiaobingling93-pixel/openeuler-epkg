#!/bin/bash
# Cross-platform package manager test runner
# Runs tests for conda, homebrew, msys2, etc. based on host platform
#
# Usage:
#   ./run.sh                    # Run all applicable platform tests
#   ./run.sh -p conda           # Run conda tests only
#   ./run.sh -p conda -k        # Keep environment (don't cleanup)
#   ./run.sh -d                 # Debug mode (set -x)
#
# Supported platforms:
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
KEEP_ENV=""
SELECT_PLATFORM=""
REMAINING=""

while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  -p, --platform PLATFORM   Run tests for specific platform (conda)"
            echo "  -k, --keep                Keep test environment after tests (no cleanup)"
            echo "  -d, --debug               Enable debug mode (set -x)"
            echo "  -dd                       More verbose debug"
            echo "  -h, --help                Show this help"
            echo ""
            echo "Platforms:"
            echo "  conda    - Conda package manager (Linux, macOS, Windows)"
            echo ""
            echo "Examples:"
            echo "  $0                        # Run all platform tests"
            echo "  $0 -p conda               # Run conda tests only"
            echo "  $0 -p conda -k            # Run conda tests, keep environment"
            exit 0
            ;;
        -p|--platform)
            shift
            SELECT_PLATFORM="$1"
            ;;
        -k|--keep)
            KEEP_ENV="1"
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
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*" >&2
}

# Detect host platform
detect_host_platform() {
    case "$(uname -s)" in
        Linux*)     echo "linux" ;;
        Darwin*)    echo "macos" ;;
        CYGWIN*|MINGW*|MSYS*) echo "windows" ;;
        *)          echo "unknown" ;;
    esac
}

HOST_PLATFORM=$(detect_host_platform)
log "Detected host platform: $HOST_PLATFORM"
log "EPKG binary: $EPKG_BIN"

# List available platform tests
list_platform_tests() {
    for script in "$SCRIPT_DIR/platforms/"*.sh; do
        [ -f "$script" ] && basename "$script" .sh
    done
}

# Check if platform test should run
should_run_platform() {
    local platform="$1"

    # If specific platform requested, only run that
    if [ -n "$SELECT_PLATFORM" ]; then
        [ "$platform" = "$SELECT_PLATFORM" ]
        return $?
    fi

    # Otherwise, check if platform is applicable for this host
    case "$platform" in
        conda)
            # Conda works on all platforms
            return 0
            ;;
        homebrew)
            # Homebrew only on macOS and Linux
            [ "$HOST_PLATFORM" = "macos" ] || [ "$HOST_PLATFORM" = "linux" ]
            return $?
            ;;
        msys2)
            # MSYS2 only on Windows
            [ "$HOST_PLATFORM" = "windows" ]
            return $?
            ;;
        *)
            return 0
            ;;
    esac
}

# Run platform test
run_platform_test() {
    local platform="$1"
    local script="$SCRIPT_DIR/platforms/${platform}.sh"

    if [ ! -x "$script" ]; then
        log "${YELLOW}Platform test script not found: $script${NC}"
        return 1
    fi

    log "${GREEN}Running $platform tests...${NC}"

    export ENV_NAME="test-${platform}"
    export PLATFORM_NAME="$platform"

    if ! "$script"; then
        log "${RED}$platform tests failed${NC}"
        return 1
    fi

    log "${GREEN}$platform tests passed${NC}"
    return 0
}

# Main
main() {
    log "Starting cross-platform tests"
    log "===================================="

    local failed=0
    local failed_platform=""

    for platform in $(list_platform_tests); do
        if should_run_platform "$platform"; then
            if ! run_platform_test "$platform"; then
                failed=$((failed + 1))
                if [ -z "$failed_platform" ]; then
                    failed_platform="$platform"
                fi
            fi
        else
            log "${YELLOW}Skipping $platform (not applicable for $HOST_PLATFORM)${NC}"
        fi
    done

    log "===================================="

    if [ $failed -gt 0 ]; then
        log "${RED}Tests failed for: $failed_platform${NC}"
        if [ -n "$SELECT_PLATFORM" ]; then
            log "Reproduce with: $SCRIPT_DIR/run.sh -p $SELECT_PLATFORM"
        fi
        exit 1
    fi

    log "${GREEN}All cross-platform tests passed${NC}"
}

# Run main
main
