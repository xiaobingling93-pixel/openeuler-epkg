#!/bin/bash
# Shared routines for dev-projects tests.
# Source from run.sh after common.sh. Expects PROJECT_ROOT, set_epkg_bin, set_color_names, ALL_OS.
# Provides env helpers, logging, timeout; run.sh exports ENV_NAME, OS, LOG_DIR.

# Env name for a given OS (non-random, for reproduce/debug)
env_name_for() {
    echo "dev-$1"
}

create_env() {
    local os="$1"
    local name
    name=$(env_name_for "$os")
    log "Creating environment $name (os=$os)"
    "$EPKG_BIN" --assume-yes env create "$name" -c "$os" || error "Failed to create env $name"
    bootstrap_shell "$name"
}

# Ensure /bin/sh exists in env so lang scripts can run /bin/sh -c '...'
# On macOS/Windows (libkrun), also create busybox applet symlinks for VM.
bootstrap_shell() {
    local name="$1"
    log "Bootstrapping shell in $name"
    if [ "$OS" = "msys2" ]; then
        # msys2: install bash (no busybox on Windows)
        "$EPKG_BIN" -e "$name" --assume-yes --ignore-missing install bash
    else
        "$EPKG_BIN" -e "$name" --assume-yes --ignore-missing install busybox bash
    fi

    # On macOS/Windows (libkrun), create busybox applet symlinks in the environment.
    # The epkg binary has busybox applets built-in, but needs symlinks.
    # These symlinks are used when running commands inside the VM.
    local host_os
    host_os=$(uname -s)
    if [ "$host_os" = "Darwin" ] || [ "$host_os" = "MINGW" ] || [ "$host_os" = "MSYS" ] || [ "$host_os" = "CYGWIN" ]; then
        log "Creating busybox applet symlinks for VM (host=$host_os)"
        local env_root="${EPKG_ENVS_DIR:-$HOME/.epkg/envs}/$name"
        local bin_dir="$env_root/usr/bin"
        if [ -f "$bin_dir/epkg" ]; then
            # Create symlinks for common busybox applets
            for applet in mkdir ls cat cp mv rm rmdir echo printf sleep true false test pwd; do
                if [ ! -e "$bin_dir/$applet" ]; then
                    ln -sf epkg "$bin_dir/$applet" 2>/dev/null || true
                fi
            done
        fi
    fi
}

# Remove env if it exists (idempotent). Use before create to get a fresh env; we never remove at end (leave for debug).
# If env is not registered but the directory exists (e.g. leftover from a bad state), remove the directory so create can succeed.
remove_env() {
    local os="$1"
    local name
    name=$(env_name_for "$os")
    log "Removing environment $name (if exists)"
    "$EPKG_BIN" --assume-yes env remove "$name" 2>/dev/null || true

    local env_root="${EPKG_ENVS_DIR:-$HOME/.epkg/envs}/$name"
    if [ -d "$env_root" ]; then
        log "Removing leftover env directory $env_root"
        rm -rf "$env_root"
    fi
}

# Run command inside current ENV_NAME (must be set by caller)
run_in_env() {
    "$EPKG_BIN" -e "$ENV_NAME" run "$@"
}

# Run with timeout; exit 124 on timeout, propagate exit code otherwise
run_with_timeout() {
    local t="$1"
    shift
    case "$(uname -s)" in
        CYGWIN*|MINGW*|MSYS*)
            # Windows MSYS2 timeout doesn't support --foreground
            timeout "$t" "$@"
            ;;
        Linux*)
            timeout --foreground "$t" "$@"
            ;;
        Darwin*)
            # macOS doesn't have timeout, use perl as fallback
            perl -e 'alarm shift; exec @ARGV' "$t" "$@"
            ;;
        *)
            timeout --foreground "$t" "$@"
            ;;
    esac
}

log() {
    echo -e "${GREEN}[${OS:-dev-projects}]${NC} $*" >&2
}

error() {
    echo -e "${RED}[ERROR]${NC} $*" >&2
    exit 1
}

skip() {
    echo -e "${YELLOW}[SKIP]${NC} $*" >&2
    exit 0
}

# Parse run.sh arguments: -o OS, -t TEST
# Sets SELECT_OS (empty = all), SELECT_TEST (empty = all)
parse_run_args() {
    SELECT_OS=""
    SELECT_TEST=""
    while [ $# -gt 0 ]; do
        case "$1" in
            -o|--os)
                [ $# -gt 1 ] || { echo "Missing value for $1" >&2; return 1; }
                SELECT_OS="$2"
                shift 2
                ;;
            -t|--test)
                [ $# -gt 1 ] || { echo "Missing value for $1" >&2; return 1; }
                SELECT_TEST="$2"
                shift 2
                ;;
            *)
                break
                ;;
        esac
    done
    return 0
}

# Return 0 if we should run this OS
should_run_os() {
    local os="$1"
    [ -z "$SELECT_OS" ] && return 0
    [ "$os" = "$SELECT_OS" ] && return 0
    return 1
}

# Return 0 if we should run this test (lang name)
should_run_test() {
    local lang="$1"
    [ -z "$SELECT_TEST" ] && return 0
    [ "$lang" = "$SELECT_TEST" ] && return 0
    return 1
}

# List of lang test names (scripts in langs/*.sh)
list_lang_tests() {
    local dir
    dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/langs"
    local f
    for f in "$dir"/*.sh; do
        [ -f "$f" ] || continue
        [ -x "$f" ] || continue
        echo "$(basename "$f" .sh)"
    done
}
