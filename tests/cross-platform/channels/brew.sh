#!/bin/sh
# Brew channel test: install and run packages

ENV_NAME="${ENV_NAME:-test-brew}"
EPKG_BIN="${EPKG_BIN:-}"
. "$(dirname "$0")/../common.sh"

setup

# Update repo
epkg update

# Install and run jq
run_install jq || channel_skip "no jq for channel=$CHANNEL_NAME"
check_cmd jq --version || channel_skip "jq not found"
run jq --version

# Install package with dependencies
run_install aalib && run jq . <<< '{"test":1}'

channel_ok