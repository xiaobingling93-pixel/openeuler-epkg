#!/bin/sh
# Test epkg.sh shell wrapper for proper option parsing and eval handling
#
# This test verifies that the shell wrapper correctly:
# - Parses options before commands (e.g., -e myenv install)
# - Handles help flags at various levels
# - Evaluates output from env path/register/activate/deactivate/remove
#
# Usage:
#   ./test-shell-wrapper.sh [-d|--debug|-dd|-ddd]

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
. "$PROJECT_ROOT/tests/common.sh"

# Parse command line flags
parse_debug_flags "$@"
case $? in
    0)
        eval set -- "$PARSE_DEBUG_FLAGS_REMAINING"
        ;;
    1)
        exit 1
        ;;
    2)
        echo "Usage: $0 [-d|--debug|-dd|-ddd]"
        echo ""
        echo "Test epkg.sh shell wrapper function"
        echo ""
        echo "Options:"
        echo "  -d, --debug    Interactive debug mode"
        echo "  -dd            Debug logging"
        echo "  -ddd           Trace logging"
        exit 0
        ;;
esac

set_epkg_bin
set_color_names

log() {
    printf "%b[TEST]%b %b\n" "$GREEN" "$NC" "$*" >&2
}

error() {
    printf "%b[ERROR]%b %b\n" "$RED" "$NC" "$*" >&2
    if [ -n "$DEBUG_FLAG" ]; then
        printf "\n=== Debug Mode ===\n" >&2
        if [ -t 0 ]; then
            printf "Press Enter to continue (or Ctrl+C to exit)...\n" >&2
            read dummy || true
        fi
    fi
    exit 1
}

# Helper to check for eval errors in output
check_no_eval_errors() {
    local output="$1"
    if echo "$output" | grep -q "command not found"; then
        error "Eval error detected: $output"
    fi
}

log "Starting epkg.sh wrapper function tests"

# Create mock environment for testing
MOCK_HOME="$(mktemp -d)"
ORIG_HOME="$HOME"
HOME="$MOCK_HOME"
export HOME

# Create directory structure for mock epkg binary
mkdir -p "$HOME/.epkg/envs/self/usr/bin"
MOCK_EPKG="$HOME/.epkg/envs/self/usr/bin/epkg"

# Create mock epkg binary
cat > "$MOCK_EPKG" <<'MOCK_EOF'
#!/bin/sh
# Mock epkg binary for testing wrapper
# Detect help flags anywhere in arguments
for arg in "$@"; do
    case "$arg" in
        -h|--help)
            echo "USAGE: mock epkg help"
            exit 0
            ;;
    esac
done

# Process arguments
case "$1" in
    env)
        case "$2" in
            path|register|unregister|activate|deactivate|remove)
                # Output eval-able shell code
                echo 'export MOCK_TEST="passed"'
                ;;
            *)
                echo "env subcommand: $2"
                ;;
        esac
        ;;
    *)
        # For any other command, just succeed
        exit 0
        ;;
esac
MOCK_EOF
chmod +x "$MOCK_EPKG"

log "Mock epkg binary created at $MOCK_EPKG"

# Source the actual epkg.sh wrapper
EPKG_RC_PATH="$PROJECT_ROOT/assets/shell/epkg.sh"
if [ ! -f "$EPKG_RC_PATH" ]; then
    # Restore HOME before error
    HOME="$ORIG_HOME"
    error "epkg.sh not found at $EPKG_RC_PATH"
fi
. "$EPKG_RC_PATH"

# Run tests
log "=== Test 1: Options before command (should not treat -e as cmd) ==="
output=$(epkg -e myenv install bash 2>&1)
check_no_eval_errors "$output"
log "  -e option before command works"

log "=== Test 2: env -h (help flag at subcommand level) ==="
output=$(epkg env -h 2>&1)
check_no_eval_errors "$output"
if ! echo "$output" | grep -q "USAGE: mock epkg help"; then
    HOME="$ORIG_HOME"
    error "Help output not shown: $output"
fi
log "  env -h works without eval errors"

log "=== Test 3: env path -h (help flag after subcommand) ==="
output=$(epkg env path -h 2>&1)
check_no_eval_errors "$output"
if ! echo "$output" | grep -q "USAGE: mock epkg help"; then
    HOME="$ORIG_HOME"
    error "Help output not shown: $output"
fi
log "  env path -h works without eval errors"

log "=== Test 4: env path (should output eval-able code) ==="
output=$(epkg env path 2>&1)
check_no_eval_errors "$output"
if ! echo "$output" | grep -q 'export MOCK_TEST="passed"'; then
    HOME="$ORIG_HOME"
    error "env path output not eval-able: $output"
fi
log "  env path outputs eval-able code"

# Cleanup
log "Cleaning up mock home directory"
rm -rf "$MOCK_HOME"
HOME="$ORIG_HOME"
export HOME

log "All epkg-rc.sh wrapper tests passed"
