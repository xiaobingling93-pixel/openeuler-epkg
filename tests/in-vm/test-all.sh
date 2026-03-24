#!/bin/sh
# Run all e2e tests in the cases/ directory.
#
# This script runs all test cases in the e2e/cases/ directory,
# excluding heavy tests (install-remove-upgrade) and slow download
# tests (build-from-source) which have their own dedicated runners.
#
# Usage:
#   ./test-all.sh [-d|--debug|-dd|-ddd]
#
# Options:
#   -d, --debug    Interactive debug mode (pause on error)
#   -dd            Debug logging (RUST_LOG=debug)
#   -ddd           Trace logging (RUST_LOG=trace)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Source host variables and common library
. "$SCRIPT_DIR/host-vars.sh"
. "$SCRIPT_DIR/lib.sh"

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
        cat <<'USAGE'
Usage: test-all.sh [-d|--debug|-dd|-ddd]

Run all in-vm tests (excluding heavy and slow tests).

This script runs all test cases in the cases/ directory:
  - Excludes install-remove-upgrade.sh (heavy; use test-iur.sh)
  - Excludes build-from-source.sh (slow download; use test-dev.sh)

For individual test categories, see:
  - tests/sandbox/test-isolation-modes.sh    Sandbox isolation tests
  - tests/dev-projects/langs/test-*.sh       Language/package tests
  - tests/misc/test-shell-wrapper.sh         Shell wrapper tests

Options:
  -d, --debug    Interactive debug mode (pause on error)
  -dd            Debug logging (RUST_LOG=debug, RUST_BACKTRACE=1)
  -ddd           Trace logging (RUST_LOG=trace, RUST_BACKTRACE=1)
USAGE
        exit 0
        ;;
esac

FAILED_TESTS=""
PASSED_TESTS=""

# Check for test cases
if [ -z "$(find "$SCRIPT_DIR/cases" -maxdepth 1 -type f -name '*.sh' 2>/dev/null | head -n 1)" ]; then
    echo "No tests found in cases/" >&2
    exit 1
fi

# Run each test case
for test_script in $(find "$SCRIPT_DIR/cases" -maxdepth 1 -type f -name '*.sh' | sort); do
    test_name=$(basename "$test_script")

    # Skip install-remove-upgrade (heavy; use test-iur.sh)
    case "$test_name" in
        install-remove-upgrade.sh)
            echo "========================================="
            echo "Skipping heavy test: $test_name"
            echo "Use ./test-iur.sh for install-remove-upgrade tests"
            echo "========================================="
            echo ""
            continue
            ;;
        build-from-source.sh)
            echo "========================================="
            echo "Skipping slow download test: $test_name"
            echo "Use ./test-dev.sh for build-from-source tests"
            echo "========================================="
            echo ""
            continue
            ;;
    esac

    echo "========================================="
    echo "Running test: $test_name"
    echo "========================================="

    if "$SCRIPT_DIR/test-one.sh" $DEBUG_FLAG "$test_script"; then
        echo "PASSED: $test_name"
        PASSED_TESTS="$PASSED_TESTS $test_name"
    else
        echo "FAILED: $test_name"
        FAILED_TESTS="$FAILED_TESTS $test_name"
        echo ""
        echo "Aborting on first failure."
        echo "========================================="
        echo "Test Summary"
        echo "========================================="
        echo "Passed: $(echo $PASSED_TESTS | wc -w)"
        echo "Failed: $(echo $FAILED_TESTS | wc -w)"
        if [ -n "$FAILED_TESTS" ]; then
            echo "Failed tests:$FAILED_TESTS"
        fi
        exit 1
    fi
    echo ""
done

# Summary
echo "========================================="
echo "Test Summary"
echo "========================================="
echo "Passed: $(echo $PASSED_TESTS | wc -w)"
echo "Failed: $(echo $FAILED_TESTS | wc -w)"

if [ -n "$FAILED_TESTS" ]; then
    echo "Failed tests:$FAILED_TESTS"
    exit 1
fi

exit 0
