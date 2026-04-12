#!/bin/bash
# Cross-platform package manager test runner for WSL2 with Windows epkg.exe
# This script runs tests using the Windows native epkg.exe from WSL2
#
# Usage:
#   ./run-wsl2-windows.sh                    # Run all applicable channel tests
#   ./run-wsl2-windows.sh -c conda           # Run conda channel tests only
#   ./run-wsl2-windows.sh -c conda -k        # Keep environment (don't cleanup)
#   ./run-wsl2-windows.sh -d                 # Debug mode (set -x)
#
# Requirements:
#   - WSL2 with Windows interop enabled
#   - Windows epkg.exe built (target/x86_64-pc-windows-gnu/debug/epkg.exe)
#   - Windows epkg environment at %USERPROFILE%\.epkg (e.g., C:\Users\<user>\.epkg)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Source common functions
. "$PROJECT_ROOT/tests/common.sh"

# Colors
set_color_names

# Find Windows epkg.exe
WIN_EPKG_EXE=""
find_windows_epkg() {
    # Try debug build first
    if [ -x "$PROJECT_ROOT/target/x86_64-pc-windows-gnu/debug/epkg.exe" ]; then
        WIN_EPKG_EXE="$PROJECT_ROOT/target/x86_64-pc-windows-gnu/debug/epkg.exe"
        return 0
    fi
    # Try release build
    if [ -x "$PROJECT_ROOT/target/x86_64-pc-windows-gnu/release/epkg.exe" ]; then
        WIN_EPKG_EXE="$PROJECT_ROOT/target/x86_64-pc-windows-gnu/release/epkg.exe"
        return 0
    fi
    return 1
}

if ! find_windows_epkg; then
    echo "Error: Windows epkg.exe not found" >&2
    echo "Tried locations:" >&2
    echo "  - $PROJECT_ROOT/target/x86_64-pc-windows-gnu/debug/epkg.exe" >&2
    echo "  - $PROJECT_ROOT/target/x86_64-pc-windows-gnu/release/epkg.exe" >&2
    echo "Please build the Windows target first: make build-windows" >&2
    exit 1
fi

log() {
    echo -e "[$(date '+%Y-%m-%d %H:%M:%S')] $*" >&2
}

# Detect Windows user profile path for epkg
get_windows_epkg_root() {
    # In WSL2, Windows user profile is typically at /mnt/c/Users/<username>
    # Try to find it
    local win_home=""

    # Try to get from WSLENV or default location
    if [ -n "$USERPROFILE" ]; then
        # Convert Windows path to WSL path
        win_home="$(echo "$USERPROFILE" | sed 's|\\|/|g; s|^C:|/mnt/c|')"
    elif [ -d "/mnt/c/Users/$USER" ]; then
        win_home="/mnt/c/Users/$USER"
    else
        # Try to find any user directory
        win_home="$(ls -d /mnt/c/Users/*/ 2>/dev/null | grep -v 'Public\|Default\|All Users' | head -1)"
        win_home="${win_home%/}"
    fi

    if [ -n "$win_home" ] && [ -d "$win_home" ]; then
        echo "$win_home/.epkg"
        return 0
    fi
    return 1
}

# Get Windows epkg root directory
WIN_EPKG_ROOT=""
if ! WIN_EPKG_ROOT="$(get_windows_epkg_root)"; then
    log "${YELLOW}Warning: Could not determine Windows epkg root directory${NC}"
    log "Tests may fail if Windows epkg environment is not set up"
fi

log "===================================="
log "WSL2 Windows epkg.exe Test Runner"
log "===================================="
log "Windows epkg.exe: $WIN_EPKG_EXE"
log "Windows epkg root: ${WIN_EPKG_ROOT:-not found}"
log ""

# Check if we're in WSL2
if [ -z "$WSL_DISTRO_NAME" ] && [ -z "$WSLENV" ]; then
    log "${YELLOW}Warning: WSL_DISTRO_NAME and WSLENV not set${NC}"
    log "This script is designed to run in WSL2 environment"
    log ""
fi

# Parse arguments - same as run.sh
DEBUG_FLAG=""
CLEANUP_ENV=""
SELECT_CHANNEL=""
SELECT_TEST=""

while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  -c, --channel CHANNEL     Run tests for specific channel (conda, msys2)"
            echo "  -t, --test TEST           Run specific test suite"
            echo "  -r, --remove              Remove test environment after tests"
            echo "  -d, --debug               Enable debug mode"
            echo "  -h, --help                Show this help"
            echo ""
            echo "This script runs cross-platform tests using Windows native epkg.exe"
            echo "from WSL2 via Windows interoperability."
            echo ""
            echo "Channels supported on Windows:"
            echo "  conda    - Conda packages (via Windows epkg.exe)"
            echo "  msys2    - MSYS2 packages (Windows native)"
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
        -d|--debug)
            DEBUG_FLAG="-d"
            set -x
            ;;
        -r|--remove)
            CLEANUP_ENV="1"
            ;;
    esac
    shift
done

export EPKG_BIN="$WIN_EPKG_EXE"
export LOG_DIR="${LOG_DIR:-/tmp}"
export SKIP_CLEANUP="${CLEANUP_ENV}"
export SELECT_TEST="$SELECT_TEST"

# For Windows epkg.exe, we need to use Windows paths
# The epkg.exe will handle path translation internally
export EPKG_WINDOWS_MODE=1

log "Running tests with Windows epkg.exe..."

# Run the standard run.sh with our settings
exec "$SCRIPT_DIR/run.sh" ${SELECT_CHANNEL:+-c "$SELECT_CHANNEL"} ${SELECT_TEST:+-t "$SELECT_TEST"} ${DEBUG_FLAG}
