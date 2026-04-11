#!/bin/bash
# Test runner for posix lua function tests
# Tries to use test_all.lua (auto-discovers tests), falls back to individual test execution
#
# Usage:
#   ./test_lua.sh                    	  # Tests epkg rpmlua (default)
#   ./test_lua.sh <test_name>             # Run single test (e.g., ./test_lua.sh access)
#   RPMLUA=/usr/bin/rpmlua ./test_lua.sh  # Uses system rpmlua, useful for verifying correctness of the lua scripts
#
# Note: System rpmlua may have compatibility issues with some tests due to differences
# in POSIX function implementations. The epkg rpmlua is the primary supported implementation.
# WARNING: System rpmlua may segfault when running the full test suite due to memory corruption
# or other issues that accumulate over multiple test executions.

# Source common variables and setup
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Determine which rpmlua to use
if [ -n "$RPMLUA" ]; then
    # Use explicitly specified rpmlua
    RPMLUA_CMD="$RPMLUA"
    echo "Using rpmlua: $RPMLUA_CMD"
else
    # Try to use epkg rpmlua via common.sh
    source "$SCRIPT_DIR/common.sh"
    RPMLUA_CMD="$EPKG_RPMLUA"
    echo "Using epkg rpmlua: $RPMLUA_CMD"
fi

# Check if rpmlua works
if ! $RPMLUA_CMD -e "print('test')" &> /dev/null; then
    echo -e "ERROR: rpmlua not working: $RPMLUA_CMD"
    exit 1
fi

TEST_ALL="$SCRIPT_DIR/test_all.lua"

# Check if a specific test was requested
if [ $# -gt 0 ]; then
    TEST_NAME="$1"
    echo "Running single test: $TEST_NAME"
    $RPMLUA_CMD "$TEST_ALL" "$TEST_NAME"
else
    echo "Running all tests"
    $RPMLUA_CMD "$TEST_ALL"
fi
