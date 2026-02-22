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

# Find all test scripts
for test_dir in "$SCRIPT_DIR"/*/; do
    if [ ! -d "$test_dir" ]; then
        continue
    fi

    for test_script in "$test_dir"test*.sh; do
        if [ ! -f "$test_script" ]; then
            continue
        fi

        # Skip install-remove-upgrade tests as they are heavy weight
        # and their randomness accumulates cache on developer machines
        # Use test-iur.sh instead for predefined matrix testing
        if [ "$(basename "$test_script")" = "test-install-remove-upgrade.sh" ]; then
            echo "========================================="
            echo "Skipping heavy test: $(basename "$test_script")"
            echo "Use ./test-iur.sh for install-remove-upgrade tests"
            echo "========================================="
            echo ""
            continue
        fi

        # Skip build-from-source-test/test-build-from-source.sh
        # because it depends on downloading which is slow and has no cache,
        # and build system won't change frequently over time,
        # and it's handled by test-dev.sh
        if [ "$(basename "$test_script")" = "test-build-from-source.sh" ]; then
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
        fi
        echo ""
    done
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

