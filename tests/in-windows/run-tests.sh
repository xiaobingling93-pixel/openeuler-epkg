#!/bin/bash
# Entry point script to run Windows native tests from WSL2
#
# Usage: ./run-tests.sh [-d|--debug|-dd|-ddd] [test-name]
#
# Arguments:
#   test-name       Test to run: env-path-auto-discovery, export-import,
#                   history-restore, install-remove-upgrade (or iur), all
#                   (default: all)
#
# Options:
#   -d, --debug    Interactive debug mode (pause on error)
#   -dd            Debug logging (RUST_LOG=debug, RUST_BACKTRACE=1)
#   -ddd           Trace logging (RUST_LOG=trace, RUST_BACKTRACE=1)
#
# Environment:
#   EPKG_BIN        Path to epkg.exe (optional, auto-detected if not set)
#
# Examples:
#   ./run-tests.sh                           # Run all tests, auto-detect binary
#   ./run-tests.sh export-import             # Run single test
#   ./run-tests.sh -dd install-remove-upgrade # Run IUR test with debug logging
#   EPKG_BIN=dist/epkg.exe ./run-tests.sh -d history-restore
#
# This script copies the tests and binary to a Windows-accessible location
# and runs them using cmd.exe, because cmd.exe cannot work with WSL UNC paths.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

# Parse arguments
DEBUG_FLAG=""
TEST_NAME=""

parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            -d|--debug)
                DEBUG_FLAG="-d"
                shift
                ;;
            -dd)
                DEBUG_FLAG="-dd"
                shift
                ;;
            -ddd)
                DEBUG_FLAG="-ddd"
                shift
                ;;
            -h|--help)
                show_usage
                exit 0
                ;;
            -*)
                echo "ERROR: Unknown option: $1" >&2
                show_usage
                exit 1
                ;;
            *)
                # Positional arg is test name
                if [ -z "$TEST_NAME" ]; then
                    TEST_NAME="$1"
                else
                    echo "ERROR: Unexpected argument: $1" >&2
                    show_usage
                    exit 1
                fi
                shift
                ;;
        esac
    done
}

show_usage() {
    cat <<'USAGE'
Usage: run-tests.sh [-d|--debug|-dd|-ddd] [test-name]

Run Windows native tests from WSL2.

Arguments:
  test-name       Test to run: env-path-auto-discovery, export-import,
                  history-restore, install-remove-upgrade (or iur), all
                  (default: all)

Options:
  -d, --debug     Interactive debug mode (pause on error)
  -dd             Debug logging (RUST_LOG=debug)
  -ddd            Trace logging (RUST_LOG=trace, RUST_BACKTRACE=1)
  -h, --help      Show this help message

Environment:
  EPKG_BIN        Path to epkg.exe (optional, auto-detected)

Examples:
  ./run-tests.sh                           # Run all tests
  ./run-tests.sh export-import             # Run single test
  ./run-tests.sh -dd install-remove-upgrade # Run IUR with debug logging
  EPKG_BIN=dist/epkg.exe ./run-tests.sh -d

Note: Debug logging flags (-dd/-ddd) set environment variables but may not
fully apply to the Windows cmd.exe environment. Check test logs for details.
USAGE
}

parse_args "$@"

# Set default test name if not provided
if [ -z "$TEST_NAME" ]; then
    TEST_NAME="all"
fi

# Apply debug settings based on DEBUG_FLAG
case "$DEBUG_FLAG" in
    -ddd)
        export RUST_LOG=trace
        export RUST_BACKTRACE=1
        echo "Debug mode: RUST_LOG=trace, RUST_BACKTRACE=1"
        ;;
    -dd)
        export RUST_LOG=debug
        export RUST_BACKTRACE=1
        echo "Debug mode: RUST_LOG=debug, RUST_BACKTRACE=1"
        ;;
    -d|--debug)
        echo "Debug mode: Interactive (will pause on error if supported)"
        ;;
esac

# Determine epkg binary path
if [ -n "$EPKG_BIN" ]; then
    EPKG_BINARY="$EPKG_BIN"
elif [ -f "$PROJECT_ROOT/dist/epkg-windows-x86_64.exe" ]; then
    EPKG_BINARY="$PROJECT_ROOT/dist/epkg-windows-x86_64.exe"
elif [ -f "$PROJECT_ROOT/target/debug/epkg.exe" ]; then
    EPKG_BINARY="$PROJECT_ROOT/target/debug/epkg.exe"
else
    echo "ERROR: EPKG_BIN not set and epkg.exe not found"
    echo ""
    echo "Expected locations:"
    echo "  - dist/epkg-windows-x86_64.exe"
    echo "  - target/debug/epkg.exe"
    echo ""
    echo "Set EPKG_BIN to specify the binary path:"
    echo "  EPKG_BIN=/path/to/epkg.exe ./run-tests.sh"
    echo ""
    show_usage
    exit 1
fi

if [ ! -f "$EPKG_BINARY" ]; then
    echo "ERROR: epkg.exe not found at: $EPKG_BINARY"
    exit 1
fi

# Validate test name
case "$TEST_NAME" in
    all|env-path-auto-discovery|export-import|history-restore|install-remove-upgrade|iur)
        : # Valid test name
        ;;
    *)
        echo "ERROR: Unknown test name: $TEST_NAME"
        echo "Available tests: env-path-auto-discovery, export-import, history-restore, install-remove-upgrade (or iur), all"
        exit 1
        ;;
esac

# Normalize iur alias
if [ "$TEST_NAME" = "iur" ]; then
    TEST_NAME="install-remove-upgrade"
fi

# Check if we're in WSL
if [ ! -d "/mnt/c" ]; then
    echo "ERROR: This script is designed to run from WSL2 with Windows C: drive mounted at /mnt/c"
    exit 1
fi

# Create temp directory on Windows C: drive
TEMP_DIR="/mnt/c/temp_epkg_test_$$"
WIN_TEMP_DIR="C:\\temp_epkg_test_$$"

echo "Preparing Windows test environment..."
echo "  Source: $SCRIPT_DIR"
echo "  Binary: $EPKG_BINARY"
echo "  Test: $TEST_NAME"
echo "  Target: $TEMP_DIR"

mkdir -p "$TEMP_DIR"
cp -r "$SCRIPT_DIR"/* "$TEMP_DIR/"
cp "$EPKG_BINARY" "$TEMP_DIR/epkg.exe"

echo ""
echo "Running Windows native tests from C: drive..."
echo ""

# Change to C: drive and run tests
# We need to cd to /mnt/c first to avoid UNC path issues
cd /mnt/c
TEST_RESULT=0
cmd.exe /c "cd /d $WIN_TEMP_DIR && run-tests.cmd epkg.exe $TEST_NAME" || TEST_RESULT=$?

# Cleanup
echo ""
echo "Cleaning up temporary files..."
rm -rf "$TEMP_DIR"

# Restore original directory
cd "$PROJECT_ROOT"

if [ $TEST_RESULT -eq 0 ]; then
    echo ""
    if [ "$TEST_NAME" = "all" ]; then
        echo "All Windows native tests PASSED!"
    else
        echo "Test '$TEST_NAME' PASSED!"
    fi
    exit 0
else
    echo ""
    if [ "$TEST_NAME" = "all" ]; then
        echo "Some tests FAILED!"
    else
        echo "Test '$TEST_NAME' FAILED!"
    fi
    exit 1
fi
