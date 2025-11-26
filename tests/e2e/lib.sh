#!/bin/sh
# Common shell functions for e2e tests

# Source vars.sh if not already sourced (container-side)
if [ -z "$E2E_DIR" ]; then
    . "$(dirname "$0")/vars.sh"
fi

# Log and run epkg command
epkg() {
    local cmd="epkg $*"
    echo "$cmd" >&2
    "$EPKG_BINARY" "$@"
    local exit_code=$?
    if [ $exit_code -ne 0 ]; then
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

    if grep -qa -- '--help' "$cmd_path" 2>/dev/null; then
        has_help=1
    fi

    if grep -qa -- '--version' "$cmd_path" 2>/dev/null; then
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
    if [ -n "${INTERACTIVE:-}" ] && [ -n "$IN_DOCKER$CONTAINER_ID$CONTAINER_NAME" ]; then
        # Try to get container ID from cgroup or hostname
        local container_id="$CONTAINER_ID$CONTAINER_NAME"
        if [ -z "$container_id" ]; then
            if [ -f /proc/self/cgroup ]; then
                # Try to extract container ID from cgroup
                container_id=$(cat /proc/self/cgroup 2>/dev/null | head -1 | sed 's/.*\///' | cut -c1-12 2>/dev/null || echo "")
            fi
	fi
        if [ -z "$container_id" ]; then
            # Fallback to hostname (often set to container ID)
            container_id=$(hostname 2>/dev/null | cut -c1-12 || echo "")
        fi

        echo "" >&2
        echo "=== Debug Mode ===" >&2
        if [ -n "$container_id" ]; then
            echo "To debug, run:" >&2
            echo "  docker exec -it $container_id /bin/sh" >&2
        else
            echo "To debug, find the container ID and run:" >&2
            echo "  docker exec -it <container_id> /bin/sh" >&2
        fi
        echo "" >&2
        echo "Press Enter to continue (or Ctrl+C to exit)..." >&2
        read dummy || true
    fi

    exit 1
}

if [ "$INTERACTIVE" = 2 ]; then
    set -x
fi
