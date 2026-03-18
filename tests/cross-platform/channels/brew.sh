#!/bin/sh
# Brew channel test: install and run packages

ENV_NAME="${ENV_NAME:-test-brew}"
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

# tree - directory listing
run_install tree || channel_skip "no tree for channel=$CHANNEL_NAME"
run tree --version || channel_skip "tree not found"

#========================================
# Test 2: Programming Languages
#========================================
echo ""
echo "=== Test 2: Programming Languages ==="

# Python (from brew, has symlink issue - skip for now)
# run_install python@3.13 python@3.12 python@3.11
# run python3 --version 2>&1 || run python --version 2>&1

# Perl - has unpack issue, skip for now
# test_lang_perl

# Node.js
run_install node
run node --version

# Go
test_lang_go

#========================================
# Test 3: Build Systems
#========================================
echo ""
echo "=== Test 3: Build Systems ==="

# cmake
test_build_cmake

# make
test_build_make

# ninja
test_build_ninja

#========================================
# Test 4: Scientific Computing (brew bottles)
#========================================
echo ""
echo "=== Test 4: Scientific Computing ==="

# numpy - requires python, skip for now
# run_install numpy
# run python3 -c "import numpy; print('numpy:', numpy.__version__)" 2>&1

# scipy - requires python, skip for now
# run_install scipy
# run python3 -c "import scipy; print('scipy:', scipy.__version__)" 2>&1

#========================================
# Test 5: ML/AI
#========================================
echo ""
echo "=== Test 5: ML/AI ==="

# pytorch - requires python, skip for now
# run_install pytorch
# run python3 -c "import torch; print('torch:', torch.__version__)" 2>&1

#========================================
# Test 6: Package Management
#========================================
echo ""
echo "=== Test 6: Package Management ==="

# Remove tree
run_remove tree

# List installed packages
epkg list | head -30

# Search for package
epkg search jq | head -20

channel_ok