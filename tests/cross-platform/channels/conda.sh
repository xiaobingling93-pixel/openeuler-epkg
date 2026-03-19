#!/bin/sh
# Conda channel test: install and run packages

ENV_NAME="${ENV_NAME:-test-conda}"
EPKG_BIN="${EPKG_BIN:-}"
SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
. "$SCRIPT_DIR/common.sh"

setup

# Update repo
epkg update

# If a specific test is requested, run only that test
if [ -n "$SELECT_TEST" ]; then
    echo ""
    run_test_suite "$SELECT_TEST"
    channel_ok
    exit 0
fi

#========================================
# Test 1: Utility packages
#========================================
echo ""
echo "=== Test 1: Utility packages ==="

# Skip tree on conda (not commonly available)
test_suite_utils "tree"

# Additional conda-specific utilities
test_util_curl
# wget not available in conda (only pywget Python library)
# test_util_wget
# sed is part of MSYS2/Cygwin, not conda
# test_util_sed

#========================================
# Test 2: Programming Languages
#========================================
echo ""
echo "=== Test 2: Programming Languages ==="

# Skip go on conda (not commonly available)
test_suite_langs "go"

#========================================
# Test 3: Build Systems
#========================================
echo ""
echo "=== Test 3: Build Systems ==="

# Only cmake on conda
test_suite_build "make ninja"

#========================================
# Test 4: Scientific Computing
#========================================
echo ""
echo "=== Test 4: Scientific Computing ==="

# Skip scipy and pandas on conda - Python 3.14 has _ctypes module issue
test_suite_scipy "scipy pandas"

#========================================
# Test 5: Machine Learning
#========================================
echo ""
echo "=== Test 5: Machine Learning ==="

# Skip scikit-learn on conda - scipy has _ctypes module issue on Python 3.14
# test_suite_ml "scikit"
echo "  (skipped - scipy has _ctypes module issue on Python 3.14)"

#========================================
# Test 6: Package Management
#========================================
echo ""
echo "=== Test 6: Package Management ==="

# Remove curl (installed in Test 1)
test_suite_pkgmgr "curl"

#========================================
# Test 7: Query Commands
#========================================
echo ""
echo "=== Test 7: Query Commands ==="

test_suite_queries

#========================================
# Test 8: History and Restore
#========================================
echo ""
echo "=== Test 8: History and Restore ==="

test_suite_history

#========================================
# Test 9: Environment Export/Import
#========================================
echo ""
echo "=== Test 9: Environment Export/Import ==="

test_suite_env_io

#========================================
# Test 10: Garbage Collection
#========================================
echo ""
echo "=== Test 10: Garbage Collection ==="

test_suite_gc

channel_ok
