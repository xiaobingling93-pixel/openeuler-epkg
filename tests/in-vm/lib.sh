#!/bin/sh
# Common shell functions for in-vm tests

# Source vars.sh if not already sourced (guest-side)
if [ -z "$E2E_DIR" ]; then
    . "$(dirname "$0")/vars.sh"
fi

# Set PROJECT_ROOT based on E2E_DIR (needed for common.sh)
if [ -z "$PROJECT_ROOT" ]; then
    PROJECT_ROOT="${E2E_DIR%/tests/in-vm*}"
    # If pattern didn't match, fall back to parent of tests/
    if [ "$PROJECT_ROOT" = "$E2E_DIR" ]; then
        PROJECT_ROOT="$(cd "$E2E_DIR/../.." && pwd)"
    fi
    export PROJECT_ROOT
fi

# Source common.sh for shared functions like parse_debug_flags
. "$PROJECT_ROOT/tests/common.sh"

# Log and run epkg command
epkg() {
    local cmd="epkg $*"
    echo "$cmd" >&2
    "$EPKG_BINARY" "$@"
    local exit_code=$?
    # Ignore SIGPIPE (141 = 128 + 13) - expected when piping to grep -q
    if [ $exit_code -ne 0 ] && [ $exit_code -ne 141 ]; then
        echo "ERROR: epkg command failed with exit code $exit_code" >&2
    fi
    return $exit_code
}

# Test if a command supports --help or --version
# Tests a command in the specified environment to verify it can execute properly.
# Tries --help first if present, and only tests --version when --help is not present
# or when --help failed without a library error (uncertain case).
#
# Args:
#   $1: cmd_path - Path to the command/executable to test
#   $2: os - Environment name to run the command in
#
# Returns:
#   0 - Command executed successfully (--help or --version worked)
#   1 - Command failed or has library errors
#
# Behavior:
#   - If --help is present and succeeds, returns 0 immediately
#   - If --help fails with library error, returns 1 (binary can't run)
#   - If --help is not present or failed without library error, tries --version
#   - Skips testing if neither --help nor --version flags are found in binary
run_cmd_help() {
    local cmd_path="$1"
    local os="$2"

    # Check if command has --help or --version flag
    local has_help=0
    local has_version=0

    if grep -qa -- '--help' "$cmd_path"; then
        has_help=1
    fi

    if grep -qa -- '--version' "$cmd_path"; then
        has_version=1
    fi

    # Skip if neither flag is present
    if [ $has_help -eq 0 ] && [ $has_version -eq 0 ]; then
        return 0
    fi

    # Try --help if present
    if [ $has_help -eq 1 ]; then
        local out
        out=$(epkg -e "$os" run "$cmd_path" --help 2>&1)
        local exit_code=$?

        if [ $exit_code -eq 0 ]; then
            return 0
        fi

        # Check for success indicators
        if echo "$out" | grep -qE "(Usage|--help)"; then
            return 0
        fi

        # Check for failure indicators - if library error, this is a real problem
        if echo "$out" | grep -qE "(error while loading shared libraries|cannot open shared object file|No such file or directory)"; then
            # Library error means the binary can't run, don't try --version
	    echo "$out"
            return 1
        fi
        # If --help failed but not with library error, we're not sure, so fall through to try --version
    fi

    # Try --version only when no --help or not sure about --help
    if [ $has_version -eq 1 ]; then
        local out
        out=$(epkg -e "$os" run "$cmd_path" --version 2>&1)
        local exit_code=$?

        if [ $exit_code -eq 0 ]; then
            return 0
        fi

        if echo "$out" | grep -qE "(version|Version|Copyright)"; then
            return 0
        fi

        if echo "$out" | grep -qE "(error while loading shared libraries|cannot open shared object file|No such file or directory)"; then
	    echo "$out"
            return 1
        fi
    fi

    # If all checks failed
    return 1
}

# Log a message
log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*" >&2
}

# Error handler
error() {
    echo "ERROR: $*" >&2

    # Check if we're in interactive debug mode (-d flag)
    if [ -n "${INTERACTIVE:-}" ] && [ -n "${IN_E2E:-}${E2E_BACKEND:-}$CONTAINER_NAME" ]; then
        local hint_id="${CONTAINER_NAME:-}"
        if [ -z "$hint_id" ]; then
            hint_id=$(hostname | cut -c1-12 || echo "")
        fi

        echo "" >&2
        echo "=== Debug Mode ===" >&2
        if [ "${E2E_BACKEND:-}" = vm ]; then
            echo "Running inside epkg VM sandbox; inspect latest VMM logs under ~/.cache/epkg/vmm-logs/ on the host." >&2
        elif [ -n "$hint_id" ]; then
            echo "If using a container, try: docker exec -it $hint_id /bin/sh" >&2
        else
            echo "Attach to your test environment (container/VM) manually to inspect state." >&2
        fi
        echo "" >&2
        if [ -t 0 ]; then
            echo "Press Enter to continue (or Ctrl+C to exit)..." >&2
            read dummy || true
        else
            echo "Non-interactive stdin; skipping debug pause." >&2
        fi
    fi

    exit 1
}


[ -n "$INTERACTIVE" ] && set -x
