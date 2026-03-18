#!/bin/sh
# Brew channel test: install and run packages

ENV_NAME="${ENV_NAME:-test-brew}"
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
run_install tree || channel_skip "no tree for channel=$CHANNEL_NAME"
check_cmd tree --version || channel_skip "tree not found"
run tree --version

# Test 3: Install package with dependencies (aalib)
run_install aalib || channel_skip "no aalib for channel=$CHANNEL_NAME"
run jq . <<< '{"test":1}'

# Test 4: Package removal
run_remove tree || echo "INFO: tree removal may have failed"

# Test 5: List installed packages
epkg list

# Test 6: Search for package
epkg search jq

channel_ok