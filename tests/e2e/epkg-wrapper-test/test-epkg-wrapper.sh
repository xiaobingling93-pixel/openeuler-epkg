#!/bin/sh
# Test epkg-rc.sh wrapper function for proper option parsing and eval handling

. "$(dirname "$0")/../vars.sh"
. "$(dirname "$0")/../lib.sh"

log "Starting epkg-rc.sh wrapper function tests"

# We'll create a mock epkg binary to test the wrapper without side effects
MOCK_HOME="$(mktemp -d)"
ORIG_HOME="$HOME"
HOME="$MOCK_HOME"

# Create directory structure for epkg binary
mkdir -p "$HOME/.epkg/envs/self/usr/bin"
MOCK_EPKG="$HOME/.epkg/envs/self/usr/bin/epkg"

# Create mock epkg binary
cat > "$MOCK_EPKG" <<'EOF'
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
EOF
chmod +x "$MOCK_EPKG"

log "Mock epkg binary created at $MOCK_EPKG"

# Source the actual epkg-rc.sh wrapper
EPKG_RC_PATH="$(dirname "$0")/../../../lib/epkg-rc.sh"
if [ ! -f "$EPKG_RC_PATH" ]; then
    error "epkg-rc.sh not found at $EPKG_RC_PATH"
fi
. "$EPKG_RC_PATH"

# Helper to check for eval errors in output
check_no_eval_errors() {
    local output="$1"
    if echo "$output" | grep -q "command not found"; then
        error "Eval error detected: $output"
    fi
}

log "=== Test 1: Options before command (should not treat -e as cmd) ==="
output=$(epkg -e myenv install bash 2>&1)
check_no_eval_errors "$output"
log "✓ -e option before command works"

log "=== Test 2: env -h (help flag at subcommand level) ==="
output=$(epkg env -h 2>&1)
check_no_eval_errors "$output"
if ! echo "$output" | grep -q "USAGE: mock epkg help"; then
    error "Help output not shown: $output"
fi
log "✓ env -h works without eval errors"

log "=== Test 3: env path -h (help flag after subcommand) ==="
output=$(epkg env path -h 2>&1)
check_no_eval_errors "$output"
if ! echo "$output" | grep -q "USAGE: mock epkg help"; then
    error "Help output not shown: $output"
fi
log "✓ env path -h works without eval errors"

log "=== Test 4: env path (should output eval-able code) ==="
output=$(epkg env path 2>&1)
check_no_eval_errors "$output"
if ! echo "$output" | grep -q 'export MOCK_TEST="passed"'; then
    error "env path output not eval-able: $output"
fi
log "✓ env path outputs eval-able code"

# Cleanup
log "Cleaning up mock home"
rm -rf "$MOCK_HOME"
HOME="$ORIG_HOME"

log "All epkg-rc.sh wrapper tests passed"