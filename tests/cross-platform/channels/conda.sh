#!/bin/sh
# Conda channel test: install and run packages

ENV_NAME="${ENV_NAME:-test-conda}"
EPKG_BIN="${EPKG_BIN:-}"
SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
. "$SCRIPT_DIR/common.sh"

setup

# Update repo
epkg update

#========================================
# Test 1: Utility packages
#========================================
echo ""
echo "=== Test 1: Utility packages ==="

# jq - JSON processor
test_util_jq || channel_skip "no jq for channel=$CHANNEL_NAME"

# wget - file downloader
run_install wget || channel_skip "no wget for channel=$CHANNEL_NAME"
run wget --version || channel_skip "wget not found"

# curl - network tool
run_install curl
run curl --version 2>&1 | head -2

# sed - stream editor
run_install sed
run sed --version 2>&1 | head -2

#========================================
# Test 2: Programming Languages
#========================================
echo ""
echo "=== Test 2: Programming Languages ==="

# Python
test_lang_python

# Perl
test_lang_perl

# Ruby
test_lang_ruby

# Node.js (has dylib loading issue, skip version check)
run_install nodejs node
run node -e "console.log('Hello from Node.js')"

#========================================
# Test 3: Build Systems
#========================================
echo ""
echo "=== Test 3: Build Systems ==="

# cmake
test_build_cmake

#========================================
# Test 4: Scientific Computing
#========================================
echo ""
echo "=== Test 4: Scientific Computing ==="

# numpy
test_scipy_numpy

# scipy
test_scipy_scipy

# pandas
test_scipy_pandas

#========================================
# Test 5: Machine Learning
#========================================
echo ""
echo "=== Test 5: Machine Learning ==="

# scikit-learn
test_ml_scikit

#========================================
# Test 6: Package Management
#========================================
echo ""
echo "=== Test 6: Package Management ==="

# Remove wget
run_remove wget

# List installed packages
epkg list | head -30

# Search for package
epkg search jq | head -20

channel_ok