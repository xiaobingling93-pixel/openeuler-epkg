#!/bin/sh
# Run all e2e tests (excluding build-from-source and install-remove-upgrade tests).
# Supports debug mode with -d/-dd/-ddd flags.

. "$(dirname "$0")/host-vars.sh"
. "$(dirname "$0")/lib.sh"

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
        echo "Usage: $0 [-d|--debug|-dd|-ddd]"
        echo "Run all e2e tests (excluding build-from-source and install-remove-upgrade tests)."
        exit 0
        ;;
esac

SCRIPT_DIR="$(dirname "$0")"
FAILED_TESTS=""
PASSED_TESTS=""

if [ -z "$(find "$SCRIPT_DIR/cases" -maxdepth 1 -type f -name '*.sh' | head -n 1)" ]; then
    echo "No tests under cases/" >&2
    exit 1
fi

for test_script in $(find "$SCRIPT_DIR/cases" -maxdepth 1 -type f -name '*.sh' | sort); do
    # Skip install-remove-upgrade (heavy; use test-iur.sh)
    if [ "$(basename "$test_script")" = "install-remove-upgrade.sh" ]; then
        echo "========================================="
        echo "Skipping heavy test: $(basename "$test_script")"
        echo "Use ./test-iur.sh for install-remove-upgrade tests"
        echo "========================================="
        echo ""
        continue
    fi

    # Skip build-from-source (slow download; test-dev.sh)
    if [ "$(basename "$test_script")" = "build-from-source.sh" ]; then
        echo "========================================="
        echo "Skipping slow download test: $(basename "$test_script")"
        echo "Use ./test-dev.sh for build-from-source tests"
        echo "========================================="
        echo ""
        continue
    fi

    test_name=$(basename "$test_script")
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
        echo "Failed tests:$FAILED_TESTS"
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
