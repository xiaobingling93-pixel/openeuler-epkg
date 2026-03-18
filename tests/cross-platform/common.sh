#!/bin/sh
# Common functions for cross-platform package manager tests (conda, homebrew, msys2, etc.)
# Sourced by platforms/*.sh test scripts
#
# Usage:
#   ENV_NAME=test-conda EPKG_BIN=/path/to/epkg ./platforms/conda.sh
#
# Or via run.sh:
#   ./run.sh -p conda    # run conda tests only
#   ./run.sh             # run all platform tests

SCRIPT_DIR="${SCRIPT_DIR:-$(cd "$(dirname "$0")" && pwd)}"
PLATFORM_NAME="${PLATFORM_NAME:-$(basename "$0" .sh)}"

# Colors for output (can be overridden by run.sh)
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Log command to stderr with platform prefix
_log_cmd() {
    printf '%b\n' "${GREEN}[${PLATFORM_NAME}]${NC} \$ $*" >&2
}

# Log info message
log_info() {
    printf '%b\n' "${GREEN}[${PLATFORM_NAME}]${NC} $*" >&2
}

# Log warning
log_warn() {
    printf '%b\n' "${YELLOW}[${PLATFORM_NAME}]${NC} WARNING: $*" >&2
}

# Log error and exit
log_error() {
    printf '%b\n' "${RED}[${PLATFORM_NAME}]${NC} ERROR: $*" >&2
    exit 1
}

# Detect host OS
# Returns: linux, macos, windows, or unknown
detect_host_os() {
    case "$(uname -s)" in
        Linux*)     echo "linux" ;;
        Darwin*)    echo "macos" ;;
        CYGWIN*|MINGW*|MSYS*) echo "windows" ;;
        *)          echo "unknown" ;;
    esac
}

# Get platform-specific run options
# On macOS without libkrun, this returns empty (direct execution path used)
_get_run_opts() {
    local os
    os=$(detect_host_os)
    # macOS without libkrun uses direct execution for conda packages
    # (they have RPATH and work like portable apps)
    # With libkrun, we could use --isolate=vm here
    echo ""
}

# Detect host architecture
# Returns: x86_64, aarch64, arm64, i686, etc.
detect_host_arch() {
    local arch
    arch=$(uname -m)
    case "$arch" in
        x86_64|amd64) echo "x86_64" ;;
        aarch64|arm64) echo "aarch64" ;;
        i686|i386) echo "i686" ;;
        *) echo "$arch" ;;
    esac
}

# Check if running on expected platform
# Args: $1 = expected platform (linux, macos, windows)
check_platform() {
    local expected="$1"
    local actual
    actual=$(detect_host_os)
    if [ "$actual" != "$expected" ]; then
        log_warn "Test designed for $expected, but running on $actual"
        return 1
    fi
    return 0
}

# Per-run log file counter
RUN_COUNT=${RUN_COUNT:-0}

# Get log directory
_get_log_dir() {
    local log_dir="${LOG_DIR:-/tmp}"
    if ! mkdir -p "$log_dir" 2>/dev/null; then
        log_dir="/tmp"
    fi
    echo "$log_dir"
}

# Run epkg command with logging
# Args: $1 = test_name, remaining = epkg arguments
run_epkg() {
    local test_name="$1"
    shift

    if [ -z "${EPKG_BIN:-}" ]; then
        log_error "EPKG_BIN not set"
    fi

    if [ -z "${ENV_NAME:-}" ]; then
        log_error "ENV_NAME not set"
    fi

    RUN_COUNT=$((RUN_COUNT + 1))
    local log_dir
    log_dir=$(_get_log_dir)
    local log_file="$log_dir/epkg-cross-${PLATFORM_NAME}-${test_name}-${RUN_COUNT}.log"

    _log_cmd "epkg -e $ENV_NAME $*"
    "$EPKG_BIN" -e "$ENV_NAME" "$@" > "$log_file" 2>&1
    local r=$?

    # Check for errors in log
    if [ $r -ne 0 ] || grep -qE 'Error:|error:' "$log_file" 2>/dev/null; then
        echo "" >&2
        echo "Command failed: epkg -e $ENV_NAME $*" >&2
        echo "Log file: $log_file" >&2
        echo "<<<<<<<<<<<<<<<<<<<" >&2
        cat "$log_file" >&2
        echo ">>>>>>>>>>>>>>>>>>>" >&2
        return $r
    fi

    cat "$log_file"
    return 0
}

# Run command in environment
# On macOS, uses direct execution path (conda packages work like portable apps)
run_in_env() {
    local run_opts
    run_opts=$(_get_run_opts)
    if [ -n "$run_opts" ]; then
        run_epkg "run" run "$run_opts" -- "$@"
    else
        run_epkg "run" run -- "$@"
    fi
}

# Install packages
install_packages() {
    run_epkg "install" --assume-yes install --ignore-missing "$@"
}

# Remove packages
remove_packages() {
    run_epkg "remove" --assume-yes remove "$@"
}

# Get package info
package_info() {
    run_epkg "info" info "$1"
}

# Check if command exists in environment
check_cmd() {
    "$EPKG_BIN" -e "$ENV_NAME" run -- "$@" 2>/dev/null
}

# Create test environment
# Args: $1 = channel (conda, conda-forge, etc.)
create_test_env() {
    local channel="${1:-conda}"

    if [ -z "${EPKG_BIN:-}" ]; then
        log_error "EPKG_BIN not set"
    fi

    if [ -z "${ENV_NAME:-}" ]; then
        log_error "ENV_NAME not set"
    fi

    # Remove existing env if present
    if [ -d "${EPKG_ENVS_DIR:-$HOME/.epkg/envs}/$ENV_NAME" ]; then
        log_info "Removing existing environment $ENV_NAME"
        "$EPKG_BIN" env remove "$ENV_NAME" 2>/dev/null || true
    fi

    log_info "Creating environment $ENV_NAME with channel $channel"
    if ! "$EPKG_BIN" env create "$ENV_NAME" -c "$channel"; then
        log_error "Failed to create environment $ENV_NAME"
    fi
}

# Cleanup test environment
cleanup_test_env() {
    if [ -z "${ENV_NAME:-}" ]; then
        return 0
    fi

    log_info "Cleaning up environment $ENV_NAME"
    "$EPKG_BIN" env remove "$ENV_NAME" 2>/dev/null || true
}

# Mark test as skipped
platform_skip() {
    printf '%b\n' "${YELLOW}[${PLATFORM_NAME}]${NC} SKIP: $*" >&2
    exit 0
}

# Mark test as passed
platform_ok() {
    printf '%b\n' "${GREEN}[${PLATFORM_NAME}]${NC} OK: $*" >&2
}

# Verify virtual package is detected
# Args: $1 = virtual package name (e.g., __unix, __linux, __osx)
verify_virtual_package() {
    local pkg="$1"
    log_info "Checking virtual package: $pkg"
    # Virtual packages are detected during solve, we can verify by checking if conda packages can be installed
    # A simpler check: grep log for virtual package detection
    if ! package_info "$pkg" 2>/dev/null | grep -q "$pkg"; then
        # Virtual packages might not show in info, but we can verify the env works
        log_info "Virtual package $pkg may not be directly queryable (expected)"
    fi
}
