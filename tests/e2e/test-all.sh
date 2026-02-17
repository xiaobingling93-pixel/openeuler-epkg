#!/bin/sh
# Run all e2e tests

. "$(dirname "$0")/host-vars.sh"

SCRIPT_DIR="$(dirname "$0")"
FAILED_TESTS=""
PASSED_TESTS=""

(
	cd $PROJECT_ROOT
	make static-$ARCH
)

# Find all test scripts
for test_dir in "$SCRIPT_DIR"/*/; do
    if [ ! -d "$test_dir" ]; then
        continue
    fi

    for test_script in "$test_dir"test*.sh; do
        if [ ! -f "$test_script" ]; then
            continue
        fi

        test_name=$(basename "$test_script")
        echo "========================================="
        echo "Running test: $test_name"
        echo "========================================="

        if "$SCRIPT_DIR/test.sh" "$test_script"; then
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

