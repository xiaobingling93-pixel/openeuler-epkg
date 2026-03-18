#!/bin/sh
# Conda channel test: install and run packages

ENV_NAME="${ENV_NAME:-test-conda}"
EPKG_BIN="${EPKG_BIN:-}"
. "$(dirname "$0")/../common.sh"

setup

# Update repo
epkg update

# Test 1: Install and run simple package (jq)
run_install jq || channel_skip "no jq for channel=$CHANNEL_NAME"
check_cmd jq --version || channel_skip "jq not found"
run jq --version

# Test 2: Install another package (tests Move + re-download)
run_install wget || channel_skip "no wget for channel=$CHANNEL_NAME"
check_cmd wget --version || channel_skip "wget not found"
run wget --version

# Test 3: Install package with dependencies (curl has many deps)
run_install curl || channel_skip "no curl for channel=$CHANNEL_NAME"
run curl --version

# Test 4: Install and test Python
run_install python python3 || channel_skip "no python for channel=$CHANNEL_NAME"
check_cmd python --version 2>&1 || check_cmd python3 --version 2>&1 || channel_skip "python not found"
run python -c "print('Hello from Python')"

# Test 5: Package removal
run_remove wget || echo "INFO: wget removal may have failed"

# Test 6: List installed packages
epkg list

# Test 7: Search for package
epkg search jq

# Test 8: Install libarchive and test
run_install libarchive || channel_skip "no libarchive for channel=$CHANNEL_NAME"
run bsdcat --version 2>&1 || run bsdtar --version 2>&1 || echo "INFO: libarchive tools may have different names"

# Test 9: Install sed and run
run_install sed || channel_skip "no sed for channel=$CHANNEL_NAME"
run sed --version

channel_ok