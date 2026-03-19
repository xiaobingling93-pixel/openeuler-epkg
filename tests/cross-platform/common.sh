#!/bin/sh
# Common functions for cross-platform channel tests (conda, brew, etc.)
# Sourced by channels/*.sh test scripts.
#
# Usage:
#   ENV_NAME=test-conda EPKG_BIN=/path/to/epkg ./channels/conda.sh

SCRIPT_DIR="${SCRIPT_DIR:-$(cd "$(dirname "$0")" && pwd)}"
CHANNEL_NAME="${CHANNEL_NAME:-$(basename "$0" .sh)}"

# Source shared test functions
. "$SCRIPT_DIR/lib.sh"

[ -n "$ENV_NAME" ] && [ -n "$EPKG_BIN" ] || {
    echo "[$CHANNEL_NAME] Set ENV_NAME and EPKG_BIN (e.g. ENV_NAME=test-conda EPKG_BIN=/path/to/epkg $0)" >&2
    exit 1
}

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

_log_cmd() {
    printf '%b\n' "${GREEN}[$CHANNEL_NAME]${NC} \$ $*" >&2
}

_check_log_and_fail() {
    local log_file="$1"
    local cmd_display="$2"
    [ ! -f "$log_file" ] && return 0

    if grep -qE 'Error:|error:' "$log_file" 2>/dev/null; then
        echo "" >&2
        echo "Command failed: $cmd_display" >&2
        echo "Log file: $log_file" >&2
        echo "<<<<<<<<<<<<<<<<<<<" >&2
        cat "$log_file" >&2
        echo ">>>>>>>>>>>>>>>>>>>" >&2
        exit 1
    fi
}

RUN_COUNT=${RUN_COUNT:-0}

_run_logged() {
    local tag="$1"
    local cmd_display="$2"
    shift 2
    RUN_COUNT=$((RUN_COUNT + 1))
    local log_dir="${LOG_DIR:-/tmp}"
    local log_file="$log_dir/epkg-channel-$CHANNEL_NAME-$tag-$RUN_COUNT.log"
    _log_cmd "$cmd_display"
    "$EPKG_BIN" -e "$ENV_NAME" "$@" > "$log_file" 2>&1
    local r=$?
    _check_log_and_fail "$log_file" "$cmd_display"
    cat "$log_file"
    return $r
}

run() {
    _run_logged "run" "epkg -e $ENV_NAME run -- $*" run -- "$@"
}

run_install() {
    _run_logged "install" "epkg -e $ENV_NAME --assume-yes install --ignore-missing $*" --assume-yes install --ignore-missing "$@"
}

run_remove() {
    _run_logged "remove" "epkg -e $ENV_NAME --assume-yes remove $*" --assume-yes remove "$@"
}

# Run any epkg command (update, list, search, etc)
epkg() {
    _run_logged "cmd" "epkg -e $ENV_NAME $*" "$@"
}

check_cmd() {
    "$EPKG_BIN" -e "$ENV_NAME" run -- "$@" 2>/dev/null
}

create_env() {
    local channel="${1:-$CHANNEL_NAME}"
    "$EPKG_BIN" env remove "$ENV_NAME" 2>/dev/null || true
    "$EPKG_BIN" env create "$ENV_NAME" -c "$channel" || channel_skip "Failed to create env"
}

cleanup_env() {
    "$EPKG_BIN" env remove "$ENV_NAME" 2>/dev/null || true
}

channel_skip() {
    printf '%b\n' "${YELLOW}[$CHANNEL_NAME]${NC} SKIP: $*" >&2
    exit 0
}

channel_ok() {
    printf '%b\n' "${GREEN}[$CHANNEL_NAME]${NC} OK"
}

# Setup: remove old env if exists, create new env, leave for debug
# Following tests/README.md best practices:
# - Remove env in the beginning, before create, if it already exists
# - Leave env for human/agent debug (do not remove at end)
setup() {
    local channel="${1:-$CHANNEL_NAME}"
    # Clean up old env if exists (idempotent)
    cleanup_env 2>/dev/null || true
    create_env "$channel"
}

# Run a specific test suite by name
run_test_suite() {
    local suite="$1"
    case "$suite" in
        utils)
            echo "=== Test 1: Utility packages ==="
            test_suite_utils
            ;;
        langs)
            echo "=== Test 2: Programming Languages ==="
            test_suite_langs
            ;;
        build)
            echo "=== Test 3: Build Systems ==="
            test_suite_build
            ;;
        scipy)
            echo "=== Test 4: Scientific Computing ==="
            test_suite_scipy
            ;;
        ml)
            echo "=== Test 5: Machine Learning ==="
            test_suite_ml
            ;;
        pkgmgr)
            echo "=== Test 6: Package Management ==="
            test_suite_pkgmgr
            ;;
        queries)
            echo "=== Test 7: Query Commands ==="
            test_suite_queries
            ;;
        *)
            echo "Unknown test suite: $suite" >&2
            return 1
            ;;
    esac
}