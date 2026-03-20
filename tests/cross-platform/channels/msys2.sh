#!/bin/sh
# MSYS2 channel test: install and run packages

ENV_NAME="${ENV_NAME:-test-msys2}"
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

# Skip nodejs for msys2 - mingw packages install to mingw64/bin/ which is not in PATH
# Skip go for msys2 - not available in msys2 repos
test_suite_langs "nodejs go"

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

# Skip scipy tests for msys2 - numpy/scipy not available in msys2 repos
echo "SKIP: numpy/scipy not available in msys2 repos"
# test_suite_scipy

#========================================
# Test 5: Machine Learning
#========================================
echo ""
echo "=== Test 5: Machine Learning ==="

# Skip ML tests for msys2 - scikit-learn not available in msys2 repos
echo "SKIP: scikit-learn not available in msys2 repos"
# test_suite_ml

#========================================
# Test 6: Package Management
#========================================
echo ""
echo "=== Test 6: Package Management ==="

# Remove tree (installed in Test 1)
test_suite_pkgmgr "tree"

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

#========================================
# Test 11: Package Upgrade
#========================================
echo ""
echo "=== Test 11: Package Upgrade ==="

test_suite_upgrade

#========================================
# Test 12: List Variants
#========================================
echo ""
echo "=== Test 12: List Variants ==="

test_suite_list_variants

#========================================
# Test 13: Environment Management
#========================================
echo ""
echo "=== Test 13: Environment Management ==="

test_suite_env

#========================================
# Test 14: Repo Commands
#========================================
echo ""
echo "=== Test 14: Repo Commands ==="

test_suite_repo

#========================================
# Test 15: Run Variants
#========================================
echo ""
echo "=== Test 15: Run Variants ==="

test_suite_run

#========================================
# Test 16: Search Variants
#========================================
echo ""
echo "=== Test 16: Search Variants ==="

test_suite_search

#========================================
# Test 17: Info Variants
#========================================
echo ""
echo "=== Test 17: Info Variants ==="

test_suite_info

#========================================
# Test 18: Dry Run
#========================================
echo ""
echo "=== Test 18: Dry Run ==="

test_suite_dry_run

channel_ok