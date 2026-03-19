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
test_util_wget
test_util_sed

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

test_suite_scipy

#========================================
# Test 5: Machine Learning
#========================================
echo ""
echo "=== Test 5: Machine Learning ==="

test_suite_ml

#========================================
# Test 6: Package Management
#========================================
echo ""
echo "=== Test 6: Package Management ==="

# Remove curl (installed in Test 1)
test_suite_pkgmgr "curl"

channel_ok
