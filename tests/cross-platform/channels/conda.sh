#!/bin/sh
# Conda channel test: install and run packages

ENV_NAME="${ENV_NAME:-test-conda}"
EPKG_BIN="${EPKG_BIN:-}"
. "$(dirname "$0")/../common.sh"

setup

# Install and run jq
run_install jq || channel_skip "no jq for channel=$CHANNEL_NAME"
check_cmd jq --version || channel_skip "jq not found"
run jq --version

# Install and run python
run_install python python3 || channel_skip "no python for channel=$CHANNEL_NAME"
check_cmd python --version 2>&1 || check_cmd python3 --version 2>&1 || channel_skip "python not found"

channel_ok