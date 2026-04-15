#!/bin/bash
# Run dev-projects tests from WSL2 to test native Windows epkg.exe
#
# Usage: ./run-wsl2-windows.sh [-o OS] [-t LANG] [-e EPKG_EXE_PATH]
#   -o OS       Run only this OS (e.g. alpine, ubuntu)
#   -t LANG     Run only this lang test (e.g. python, go)
#   -e PATH     Path to Windows epkg.exe (default: /mnt/c/Users/$USER/epkg.exe)
#
# This script runs from WSL2 but tests a native Windows epkg.exe binary.
# The epkg.exe manages Windows environments (libkrun VM or native Windows packages).
#
# Examples:
#   # Test all languages on alpine from WSL2
#   ./run-wsl2-windows.sh -o alpine
#
#   # Test only python on ubuntu
#   ./run-wsl2-windows.sh -o ubuntu -t python
#
#   # Use custom epkg.exe location
#   ./run-wsl2-windows.sh -e /mnt/c/ProgramData/epkg/epkg.exe -o alpine

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Default path to Windows epkg.exe
DEFAULT_EPKG_EXE="/mnt/c/Users/${USER}/epkg.exe"
EPKG_EXE=""
SELECT_OS=""
SELECT_TEST=""
DEBUG_FLAG=""

# Parse arguments
while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help)
            echo "Usage: $0 [-d|--debug|-dd|-ddd] [-o OS] [-t LANG] [-e EPKG_EXE_PATH]"
            echo ""
            echo "Run dev-projects tests from WSL2 against native Windows epkg.exe"
            echo ""
            echo "Options:"
            echo "  -o OS       Run only this OS (e.g. alpine, ubuntu)"
            echo "  -t LANG     Run only this lang test (e.g. python, go)"
            echo "  -e PATH     Path to Windows epkg.exe (default: $DEFAULT_EPKG_EXE)"
            echo "  -d          Enable debug mode (set -x)"
            echo "  -dd         Enable debug mode + RUST_LOG=debug"
            echo "  -ddd        Enable debug mode + RUST_LOG=trace"
            echo ""
            echo "Examples:"
            echo "  $0 -o alpine"
            echo "  $0 -o ubuntu -t python"
            echo "  $0 -e /mnt/c/ProgramData/epkg/epkg.exe -o alpine -t go"
            exit 0
            ;;
        -o|--os)
            [ $# -gt 1 ] || { echo "Missing value for $1" >&2; exit 1; }
            SELECT_OS="$2"
            shift 2
            ;;
        -t|--test)
            [ $# -gt 1 ] || { echo "Missing value for $1" >&2; exit 1; }
            SELECT_TEST="$2"
            shift 2
            ;;
        -e|--epkg)
            [ $# -gt 1 ] || { echo "Missing value for $1" >&2; exit 1; }
            EPKG_EXE="$2"
            shift 2
            ;;
        -ddd) DEBUG_FLAG="-ddd"; shift ;;
        -dd)  DEBUG_FLAG="-dd"; shift ;;
        -d|--debug) DEBUG_FLAG="-d"; shift ;;
        *)
            echo "Unknown option: $1" >&2
            echo "Use -h for help" >&2
            exit 1
            ;;
    esac
done

# Set default epkg.exe path if not provided
if [ -z "$EPKG_EXE" ]; then
    EPKG_EXE="$DEFAULT_EPKG_EXE"
fi

# Verify epkg.exe exists
if [ ! -f "$EPKG_EXE" ]; then
    echo "Error: epkg.exe not found at: $EPKG_EXE" >&2
    echo "" >&2
    echo "Please build epkg for Windows first, or specify the path with -e:" >&2
    echo "  $0 -e /path/to/epkg.exe -o alpine" >&2
    exit 1
fi

# Verify we're running in WSL2
if [ -z "$WSL_DISTRO_NAME" ] && [ -z "$WSL_INTEROP" ]; then
    echo "Warning: This script is designed for WSL2. Detected environment may not be WSL2." >&2
fi

echo "[WSL2-Windows] Testing native Windows epkg.exe"
echo "[WSL2-Windows] epkg.exe: $EPKG_EXE"
echo "[WSL2-Windows] OS: ${SELECT_OS:-all}"
echo "[WSL2-Windows] Test: ${SELECT_TEST:-all}"

# Export environment variables for the underlying run.sh
export EPKG_BIN="$EPKG_EXE"
export EPKG_WSL2_MODE=1

# Build argument list for run.sh
RUN_ARGS=""
[ -n "$SELECT_OS" ] && RUN_ARGS="$RUN_ARGS -o $SELECT_OS"
[ -n "$SELECT_TEST" ] && RUN_ARGS="$RUN_ARGS -t $SELECT_TEST"
[ -n "$DEBUG_FLAG" ] && RUN_ARGS="$RUN_ARGS $DEBUG_FLAG"

# Run the actual test script
cd "$PROJECT_ROOT"
exec "$SCRIPT_DIR/run.sh" $RUN_ARGS
