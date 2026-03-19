#!/bin/sh
# Brew channel test: install and run packages

ENV_NAME="${ENV_NAME:-test-brew}"
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

test_suite_utils

#========================================
# Test 2: Programming Languages
#========================================
echo ""
echo "=== Test 2: Programming Languages ==="

test_suite_langs

#========================================
# Test 3: Build Systems
#========================================
echo ""
echo "=== Test 3: Build Systems ==="

test_suite_build

#========================================
# Test 4: Scientific Computing
#========================================
echo ""
echo "=== Test 4: Scientific Computing ==="

# Skip pandas for brew (not available in homebrew-core)
test_suite_scipy "pandas"

#========================================
# Test 5: Machine Learning
#========================================
echo ""
echo "=== Test 5: Machine Learning ==="

# scikit-learn is not available in homebrew-core
echo "SKIP: scikit-learn not available in homebrew-core"
# test_suite_ml "scikit"

#========================================
# Test 6: Package Management
#========================================
echo ""
echo "=== Test 6: Package Management ==="

# Remove tree (installed in Test 1)
test_suite_pkgmgr "tree"

channel_ok
