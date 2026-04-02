#!/bin/sh
# Test --root DIR option, automatic name generation, and implicit environment discovery

. "$(dirname "$0")/../vars.sh"
. "$(dirname "$0")/../lib.sh"

log "Starting env path auto-discovery test"

# Detect platform and set appropriate channel
# On macOS, use brew for native packages; on Linux, use alpine
if [ "$(uname -s)" = "Darwin" ]; then
    TEST_CHANNEL="brew"
    # Use smaller packages for testing
    TEST_PKG_SMALL="jq"
    TEST_PKG_ALT="tree"
else
    TEST_CHANNEL="alpine"
    TEST_PKG_SMALL="jq"
    TEST_PKG_ALT="htop"
fi
log "Using channel: $TEST_CHANNEL"

# Helper function to mimic env_name_from_path logic
env_name_from_path() {
    local path="$1"
    # Trim leading and trailing slashes
    local trimmed=$(echo "$path" | sed 's|^/*||; s|/*$||')
    if [ -z "$trimmed" ]; then
        echo "root"
        return
    fi
    # Replace '/' with '__'
    local with_underscores=$(echo "$trimmed" | sed 's|/|__|g')
    # Ensure name starts with '__'
    case "$with_underscores" in
        __*) echo "$with_underscores" ;;
        *)   echo "__$with_underscores" ;;
    esac
}

# Create a temporary directory for our test environments
# Use ~/.epkg/tmp to ensure same filesystem as store (required for LinkType::Move)
TEST_DIR="${HOME}/.epkg/tmp/env-path-test-$$"
mkdir -p "$TEST_DIR"
trap "rm -rf '$TEST_DIR'" EXIT

ORIG_DIR=$(pwd)

# ============================================================================
# Test 1: --root DIR option with env create (automatic name generation)
# ============================================================================
log "Test 1: --root DIR option with env create"
ENV_ROOT="$TEST_DIR/myenv"
log "Creating environment at path: $ENV_ROOT"
epkg env create --root "$ENV_ROOT" -c $TEST_CHANNEL || error "Failed to create environment with --root"

# Compute expected auto-generated name
EXPECTED_NAME=$(env_name_from_path "$ENV_ROOT")
log "Expected auto-generated name: $EXPECTED_NAME"

# Verify environment appears in env list
log "Listing environments to verify registration"
ENV_LIST=$(epkg env list)
if ! echo "$ENV_LIST" | grep -q "$EXPECTED_NAME"; then
    error "Environment '$EXPECTED_NAME' not found in env list"
fi

# Verify auto-generated name starts with '__'
if ! echo "$EXPECTED_NAME" | grep -q '^__'; then
    error "Auto-generated name '$EXPECTED_NAME' does not start with '__'"
fi

# Install and run a command using --root flag
log "Installing jq using --root flag"
epkg --root "$ENV_ROOT" --assume-yes install jq || error "Failed to install jq with --root"

log "Running jq with --root flag"
if ! epkg --root "$ENV_ROOT" run jq --version; then
    error "jq not found in environment via --root"
fi

# Also test that we can use -e with the auto-generated name
log "Testing -e flag with auto-generated name"
if ! epkg -e "$EXPECTED_NAME" run jq --version; then
    error "jq not found via -e with auto-generated name"
fi

# ============================================================================
# Test 1b: Complex path with multiple slashes
# ============================================================================
log "Test 1b: Complex path with multiple slashes"
COMPLEX_PATH="$TEST_DIR/some/deep/nested/env"
mkdir -p "$(dirname "$COMPLEX_PATH")"
COMPLEX_NAME=$(env_name_from_path "$COMPLEX_PATH")
epkg env remove "$COMPLEX_NAME" 2>/dev/null
epkg env create --root "$COMPLEX_PATH" -c $TEST_CHANNEL || error "Failed to create environment with complex path"
log "Complex path auto-generated name: $COMPLEX_NAME"
# Verify registration
if ! epkg env list | grep -q "$COMPLEX_NAME"; then
    error "Complex path environment '$COMPLEX_NAME' not found in env list"
fi
# Install TEST_PKG_ALT via --root
epkg --root "$COMPLEX_PATH" --assume-yes install $TEST_PKG_ALT || error "Failed to install $TEST_PKG_ALT in complex env"
if ! epkg --root "$COMPLEX_PATH" run $TEST_PKG_ALT --version; then
    error "$TEST_PKG_ALT not found in complex env via --root"
fi

# ============================================================================
# Test 2: Implicit discovery via .eenv directory
# ============================================================================
log "Test 2: Implicit discovery via .eenv directory"
# Create a .eenv directory as an environment root
EENV_DIR="$TEST_DIR/project/.eenv"
mkdir -p "$EENV_DIR"
EENV_NAME=$(env_name_from_path "$EENV_DIR")
epkg env remove "$EENV_NAME" 2>/dev/null
# Create environment at .eenv path
epkg env create --root "$EENV_DIR" -c $TEST_CHANNEL || error "Failed to create environment at .eenv"

# Install a package in that environment (we'll install TEST_PKG_ALT)
epkg --root "$EENV_DIR" --assume-yes install $TEST_PKG_ALT || error "Failed to install $TEST_PKG_ALT"

# Create a script in a subdirectory (not in .eenv)
SCRIPT_DIR="$TEST_DIR/project/subdir"
mkdir -p "$SCRIPT_DIR"
SCRIPT="$SCRIPT_DIR/test.sh"
cat > "$SCRIPT" <<EOF
#!/bin/sh
$TEST_PKG_ALT --version
EOF
chmod +x "$SCRIPT"

# Run the script without explicit environment; should auto-discover .eenv in parent directory
log "Running script with implicit environment discovery"
cd "$SCRIPT_DIR"
if ! epkg run $PWD/test.sh; then
    error "Failed to run absolute path script with implicit .eenv discovery"
fi
if ! epkg run ./test.sh; then
    error "Failed to run relative path script with implicit .eenv discovery"
fi
cd "$ORIG_DIR"

# ============================================================================
# Test 3: Registered environment search for non--rootath commands
# ============================================================================
log "Test 3: Registered environment search for non--rootath commands"
# Create a new environment with a unique command installed
ENV2_NAME="test-registered-search"
epkg env remove "$ENV2_NAME" 2>/dev/null
epkg env create "$ENV2_NAME" -c $TEST_CHANNEL || error "Failed to create environment $ENV2_NAME"
epkg -e "$ENV2_NAME" --assume-yes install $TEST_PKG_ALT || error "Failed to install $TEST_PKG_ALT"

if ! epkg run -e "$ENV2_NAME" $TEST_PKG_ALT --version; then
    error "Failed to find $TEST_PKG_ALT"
fi

# Run '$TEST_PKG_ALT' without explicit environment; should find in registered environments
# Note: This only works if $TEST_PKG_ALT is not a path (no slash) and no .eenv found.
# We'll run from a directory with no .eenv.
cd "$TEST_DIR"
epkg env register "$ENV2_NAME"
if ! epkg run $TEST_PKG_ALT --version; then
    error "Failed to find $TEST_PKG_ALT in registered environments"
fi
epkg env unregister "$ENV2_NAME"
cd "$ORIG_DIR"

# ============================================================================
# Test 4: Fallback to MAIN_ENV when no environment found
# ============================================================================
log "Test 4: Fallback to MAIN_ENV"
# Run a command that only exist in main environment; should fallback to MAIN_ENV
epkg install coreutils --assume-yes
if ! epkg run echo "test" >/dev/null; then
    error "Failed to fallback to MAIN_ENV"
fi

# ============================================================================
# Test 5: -e overrides --root when both flags present
# ============================================================================
log "Test 5: -e overrides --root precedence"
# Create two environments: one via --root, one via -e name
ENV3_PATH="$TEST_DIR/env3"
ENV3_NAME="explicit-name"
epkg env remove "$ENV3_NAME" 2>/dev/null
epkg env create --root "$ENV3_PATH" -c $TEST_CHANNEL || error "Failed to create env3 via --root"
epkg env create "$ENV3_NAME" -c $TEST_CHANNEL || error "Failed to create env3 via -e"
# Install different packages in each to distinguish
epkg --root "$ENV3_PATH" --assume-yes install jq || error "Failed to install jq in path env"
epkg -e "$ENV3_NAME" --assume-yes install $TEST_PKG_ALT || error "Failed to install $TEST_PKG_ALT in named env"

# Run with both -e and --root; -e should take precedence, so $TEST_PKG_ALT should be found, jq not
if ! epkg -e "$ENV3_NAME" --root "$ENV3_PATH" run $TEST_PKG_ALT --version; then
    error "$TEST_PKG_ALT not found when both -e and --root flags present (-e should win)"
fi
# Verify jq is not found (should error)
if epkg -e "$ENV3_NAME" --root "$ENV3_PATH" run jq --version; then
    error "jq found when -e should have overridden --root"
fi

log "All tests passed!"
