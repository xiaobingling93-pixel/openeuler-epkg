#!/bin/bash

# OS list for tests that iterate over multiple distros (sandbox dev-projects, e2e, etc.)
ALL_OS="openeuler fedora debian ubuntu alpine archlinux conda"

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

# Parse debug flags (-d, --debug, -dd, -ddd) and shift arguments
# Usage: parse_debug_flags "$@"
# Sets DEBUG_FLAG to "", "-d", "-dd" or "-ddd"
# Sets PARSE_DEBUG_FLAGS_REMAINING to remaining arguments (space-separated)
# Returns:
#   0 - success
#   1 - unknown option
#   2 - help requested (-h or --help)
parse_debug_flags() {
    DEBUG_FLAG=""
    local _remaining=""
    while [ $# -gt 0 ] && [ "${1#-}" != "$1" ]; do
        case "$1" in
            -h|--help)
                PARSE_DEBUG_FLAGS_REMAINING=""
                return 2
                ;;
            -ddd)
                DEBUG_FLAG="-ddd"
                ;;
            -dd)
                DEBUG_FLAG="-dd"
                ;;
            -d|--debug)
                DEBUG_FLAG="-d"
                ;;
            *)
                echo "Unknown option: $1" >&2
                PARSE_DEBUG_FLAGS_REMAINING=""
                return 1
                ;;
        esac
        shift
    done
    # Store remaining arguments
    PARSE_DEBUG_FLAGS_REMAINING="$*"
    return 0
}
