#!/bin/bash

# Find and set epkg binary path
set_epkg_bin() {
    # Find epkg binary - try multiple locations
    if [ -n "$EPKG_BIN" ] && [ -x "$EPKG_BIN" ]; then
        # Use EPKG_BIN if explicitly set and exists
        EPKG_BIN="$EPKG_BIN"
    elif [ -x "$HOME/.epkg/envs/self/usr/bin/epkg" ]; then
        # Try installed location (from Makefile)
        EPKG_BIN="$HOME/.epkg/envs/self/usr/bin/epkg"
    elif [ -x "$PROJECT_ROOT/target/debug/epkg" ]; then
        # Try debug build
        EPKG_BIN="$PROJECT_ROOT/target/debug/epkg"
    elif [ -x "$PROJECT_ROOT/target/release/epkg" ]; then
        # Try release build
        EPKG_BIN="$PROJECT_ROOT/target/release/epkg"
    else
        echo "Error: epkg binary not found" >&2
        echo "Tried locations:" >&2
        echo "  - EPKG_BIN environment variable: ${EPKG_BIN:-not set}" >&2
        echo "  - $HOME/.epkg/envs/self/usr/bin/epkg" >&2
        echo "  - $PROJECT_ROOT/target/debug/epkg" >&2
        echo "  - $PROJECT_ROOT/target/release/epkg" >&2
        echo "Please build the project first or set EPKG_BIN environment variable" >&2
        exit 1
    fi
}

set_color_names() {
    # Colors for output
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[1;33m'
    NC='\033[0m' # No Color
}
