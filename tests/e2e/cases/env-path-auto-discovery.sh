#!/bin/sh
# Test --root DIR option, automatic name generation, and implicit environment discovery

. "$(dirname "$0")/../vars.sh"
. "$(dirname "$0")/../lib.sh"

log "Starting env path auto-discovery test"

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
TEST_DIR=$(mktemp -d)
trap "rm -rf '$TEST_DIR'" EXIT

ORIG_DIR=$(pwd)

# ============================================================================
# Test 1: --root DIR option with env create (automatic name generation)
# ============================================================================
log "Test 1: --root DIR option with env create"
ENV_ROOT="$TEST_DIR/myenv"
log "Creating environment at path: $ENV_ROOT"
epkg env create --root "$ENV_ROOT" -c alpine || error "Failed to create environment with --root"

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
epkg env create --root "$COMPLEX_PATH" -c alpine || error "Failed to create environment with complex path"
COMPLEX_NAME=$(env_name_from_path "$COMPLEX_PATH")
log "Complex path auto-generated name: $COMPLEX_NAME"
# Verify registration
if ! epkg env list | grep -q "$COMPLEX_NAME"; then
    error "Complex path environment '$COMPLEX_NAME' not found in env list"
fi
# Install htop via --root
epkg --root "$COMPLEX_PATH" --assume-yes install htop || error "Failed to install htop in complex env"
if ! epkg --root "$COMPLEX_PATH" run htop --version; then
    error "htop not found in complex env via --root"
fi
# Clean up this environment (remove registration, root dir will be deleted with TEST_DIR)
epkg env remove "$COMPLEX_NAME"

# ============================================================================
# Test 2: Implicit discovery via .eenv directory
# ============================================================================
log "Test 2: Implicit discovery via .eenv directory"
# Create a .eenv directory as an environment root
EENV_DIR="$TEST_DIR/project/.eenv"
mkdir -p "$EENV_DIR"
# Create environment at .eenv path
epkg env create --root "$EENV_DIR" -c alpine || error "Failed to create environment at .eenv"
EENV_NAME=$(env_name_from_path "$EENV_DIR")

# Install a package in that environment (we'll install htop)
epkg --root "$EENV_DIR" --assume-yes install /bin/sh htop || error "Failed to install htop"

# Create a script in a subdirectory (not in .eenv)
SCRIPT_DIR="$TEST_DIR/project/subdir"
mkdir -p "$SCRIPT_DIR"
SCRIPT="$SCRIPT_DIR/test.sh"
cat > "$SCRIPT" <<'EOF'
#!/bin/sh
htop --version
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

# Clean up .eenv environment
epkg env remove "$EENV_NAME"

# ============================================================================
# Test 3: Registered environment search for non--rootath commands
# ============================================================================
log "Test 3: Registered environment search for non--rootath commands"
# Create a new environment with a unique command installed
ENV2_NAME="test-registered-search"
epkg env create "$ENV2_NAME" -c alpine || error "Failed to create environment $ENV2_NAME"
epkg -e "$ENV2_NAME" --assume-yes install htop || error "Failed to install htop"

if ! epkg run -e "$ENV2_NAME" htop --version; then
    error "Failed to find htop"
fi

# Run 'htop' without explicit environment; should find in registered environments
# Note: This only works if htop is not a path (no slash) and no .eenv found.
# We'll run from a directory with no .eenv.
cd "$TEST_DIR"
epkg env register "$ENV2_NAME"
if ! epkg run htop --version; then
    error "Failed to find htop in registered environments"
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
epkg env create --root "$ENV3_PATH" -c alpine || error "Failed to create env3 via --root"
epkg env create "$ENV3_NAME" -c alpine || error "Failed to create env3 via -e"
# Install different packages in each to distinguish
epkg --root "$ENV3_PATH" --assume-yes install jq || error "Failed to install jq in path env"
epkg -e "$ENV3_NAME" --assume-yes install htop || error "Failed to install htop in named env"

# Run with both -e and --root; -e should take precedence, so htop should be found, jq not
if ! epkg -e "$ENV3_NAME" --root "$ENV3_PATH" run htop --version; then
    error "htop not found when both -e and --root flags present (-e should win)"
fi
# Verify jq is not found (should error)
if epkg -e "$ENV3_NAME" --root "$ENV3_PATH" run jq --version; then
    error "jq found when -e should have overridden --root"
fi

# ============================================================================
# Cleanup
# ============================================================================
log "Cleaning up environments"
epkg env remove "$ENV2_NAME"
epkg env remove "$ENV3_NAME"
epkg env remove "$EXPECTED_NAME"
# Environments created with --root will have their root directories removed when TEST_DIR is deleted

log "All tests passed!"
