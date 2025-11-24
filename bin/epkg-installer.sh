#!/bin/sh
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# Global variables
ARCH=$(uname -m)
EPKG_STATIC="epkg"
EPKG_CACHE="$HOME/.cache/epkg/downloads/epkg"
GITEE_API_BASE="https://gitee.com/api/v5/repos"
GITEE_OWNER="openeuler"
GITEE_REPO="epkg"

# Default values
CHANNEL=""
STORE_MODE="auto"

# Parse command line arguments
parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            -c|--channel)
                if [ -z "$2" ] || [ "${2#-}" != "$2" ]; then
                    print_error "Option $1 requires an argument"
                fi
                CHANNEL="$2"
                shift 2
                ;;
            --store)
                if [ -z "$2" ] || [ "${2#-}" != "$2" ]; then
                    print_error "Option $1 requires an argument"
                fi
                case "$2" in
                    shared|private|auto)
                        STORE_MODE="$2"
                        ;;
                    *)
                        print_error "Invalid store mode: $2. Must be one of: shared, private, auto"
                        ;;
                esac
                shift 2
                ;;
            -h|--help)
                echo "Usage: $0 [OPTIONS]"
                echo
                echo "Options:"
                echo "  -c, --channel CHANNEL   Set the channel for the main environment"
                echo "  --store MODE            Store mode: shared, private, or auto (default: auto)"
                echo "  -h, --help              Show this help message"
                echo
                echo "Examples:"
                echo "  $0 --channel alpine:3.21"
                echo "  $0 --store shared"
                echo "  $0 --channel debian:trixie --store private"
                exit 0
                ;;
            *)
                print_error "Unknown option: $1"
                ;;
        esac
    done
}

print_step() {
    echo ">> $1"
}

print_info() {
    echo "$1"
}

print_error() {
    echo "ERROR: $1" >&2
    exit 1
}

check_architecture() {
    case "$ARCH" in
        x86_64|aarch64|riscv64|loongarch64)
            ;;
        *)
            print_error "Unsupported architecture: $ARCH"
            ;;
    esac
}

setup_environment() {
    # Create cache directory structure
    mkdir -p "$EPKG_CACHE" || exit
}

check_git_tree() {
    # Get script directory using realpath
    local SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
    # Go up one level since script is in bin/
    local PROJECT_ROOT=$(dirname "$SCRIPT_DIR")

    if [ -d "$PROJECT_ROOT/.git" ] && [ -x "$PROJECT_ROOT/target/debug/epkg" ]; then
        EPKG_PATH="$PROJECT_ROOT/target/debug/epkg"
        return 0
    fi
    return 1
}

fetch_show_latest_release() {
    # Fetch latest release from Gitee API
    local api_url="${GITEE_API_BASE}/${GITEE_OWNER}/${GITEE_REPO}/releases/latest"
    local response

    response=$(curl -s --connect-timeout 15 --max-time 30 "$api_url") || {
        print_error "Failed to fetch release info from Gitee API"
    }

    # Extract tag_name from JSON response
    # Using a simple approach that works with common JSON parsers
    local tag_name
    if command -v jq >/dev/null 2>&1; then
        tag_name=$(echo "$response" | jq -r '.tag_name // empty')
    elif command -v python3 >/dev/null 2>&1; then
        tag_name=$(echo "$response" | python3 -c "import sys, json; print(json.load(sys.stdin).get('tag_name', ''))" 2>/dev/null)
    elif command -v python >/dev/null 2>&1; then
        tag_name=$(echo "$response" | python -c "import sys, json; print(json.load(sys.stdin).get('tag_name', ''))" 2>/dev/null)
    fi

    if [ -z "$tag_name" ] || [ "$tag_name" = "null" ] || [ "$tag_name" = "" ]; then
        # Fallback: use grep/sed to extract tag_name (less robust but works without dependencies)
        tag_name=$(echo "$response" | grep -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' | sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')
    fi

    if [ -z "$tag_name" ] || [ "$tag_name" = "null" ] || [ "$tag_name" = "" ]; then
        print_error "Failed to parse release tag from Gitee API response"
    fi

    echo "$tag_name"
}

download_files() {
    # Skip download if running from git tree
    if check_git_tree; then
        echo
        print_info "Using local binary from git tree: $EPKG_PATH"
        return
    fi

    # Fetch latest release version
    print_info "Fetching latest release from Gitee..."
    local latest_version
    latest_version=$(fetch_show_latest_release) || exit 1

    # Construct download URLs based on latest release
    # Format: https://gitee.com/openeuler/epkg/releases/download/{tag_name}/epkg-{arch}
    local EPKG_BINARY_URL="https://gitee.com/${GITEE_OWNER}/${GITEE_REPO}/releases/download/${latest_version}/${EPKG_STATIC}-${ARCH}"
    local EPKG_SHA_URL="${EPKG_BINARY_URL}.sha256"

    cd "$EPKG_CACHE" || exit

    echo
    print_info "Latest release: $latest_version"
    print_info "Source URL: $EPKG_BINARY_URL"
    print_info "Destination: $EPKG_CACHE"

    echo
    echo "Downloading $EPKG_STATIC-$ARCH.sha256 ..."
    curl -L -# -o "$EPKG_STATIC-$ARCH.sha256" "$EPKG_SHA_URL" --connect-timeout 15 --max-time 30 || print_error "Failed to download checksum file"

    # Validate checksum file
    if [ ! -s "$EPKG_STATIC-$ARCH.sha256" ]; then
        print_error "Checksum file is empty or not found"
    fi

    # Check if file contains HTML (error page) - look for common HTML tags
    if grep -q -i '<html\|<!DOCTYPE\|<body' "$EPKG_STATIC-$ARCH.sha256" 2>/dev/null; then
        print_error "Checksum file appears to be an HTML error page. URL may be incorrect."
    fi

    echo "Downloading $EPKG_STATIC-$ARCH ..."
    curl -L -# -o "$EPKG_STATIC-$ARCH" "$EPKG_BINARY_URL" --retry 5 --connect-timeout 15 --max-time 300 || print_error "Failed to download binary"
    chmod +x "./$EPKG_STATIC-$ARCH"
    EPKG_PATH=./$EPKG_STATIC-$ARCH

    command -v sha256sum >/dev/null || return
    sha256sum -c "$EPKG_STATIC-$ARCH.sha256" || exit
}

initialize_epkg() {
    # Build the init command with options
    local init_cmd="$EPKG_PATH init --store=$STORE_MODE"

    # Add channel option if specified
    if [ -n "$CHANNEL" ]; then
        init_cmd="$init_cmd --channel=$CHANNEL"
    fi

    # Set store mode based on user (for display purposes)
    echo
    if [ "$USER" = "root" ] || [ "$LOGNAME" = "root" ] || [ "$(id -u)" = "0" ]; then
        print_info "Installation mode: shared (system-wide)"
    else
        print_info "Installation mode: private (user-local)"
    fi

    # Show what we're doing
    if [ -n "$CHANNEL" ]; then
        print_info "Initializing epkg with channel: $CHANNEL"
    fi
    print_info "Store mode: $STORE_MODE"

    # Initialize epkg
    $init_cmd || exit
}

print_completion() {
    echo
    echo "================================================="
    echo "              Installation Complete              "
    echo "================================================="
    print_info "Usage:"
    print_info "  epkg search [pattern]  - Search for packages"
    print_info "  epkg install [pkg]     - Install packages"
    print_info "  epkg remove [pkg]      - Remove packages"
    print_info "  epkg list              - List packages"
    print_info "  epkg update            - Update repo data"
    print_info "  epkg upgrade           - Upgrade packages"
    print_info "  epkg --help            - Show detailed help"
}

check_duplicate_install()
{
    test -d $HOME/.epkg/envs/main && {
        echo "epkg was already initialized for current user"
        echo "TO upgrade epkg: epkg init --upgrade"
        echo "TO uninstall epkg: epkg deinit"
        exit 1
    }
}

# Main execution flow
main() {
    check_duplicate_install
    parse_args "$@"
    check_architecture
    setup_environment
    download_files
    initialize_epkg
    print_completion
}

main "$@"

# vim: sw=4 ts=4 et
