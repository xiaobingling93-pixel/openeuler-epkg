#!/bin/bash
# Common variables and setup for posix-lua test scripts
# Source this file in test scripts: source "$(dirname "$0")/common.sh"

# Get script directory and project root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

. $PROJECT_ROOT/tests/common.sh
set_epkg_bin
set_color_names

# Create temporary rpmlua symlink for testing (name must be "rpmlua" for applet to work)
TMP_DIR=$(mktemp -d)
EPKG_RPMLUA="$TMP_DIR/rpmlua"
ln -sf "$EPKG_BIN" "$EPKG_RPMLUA"

# Cleanup function
cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT

