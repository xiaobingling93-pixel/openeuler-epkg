#!/bin/sh
# Test history/restore functionality

. "$(dirname "$0")/../vars.sh"
. "$(dirname "$0")/../lib.sh"

log "Starting history/restore test"

# Detect platform and set appropriate channel
if [ "$(uname -s)" = "Darwin" ]; then
    TEST_CHANNEL="brew"
    TEST_PKG1="jq"
    TEST_PKG2="tree"
    TEST_PKG3="curl"
else
    TEST_CHANNEL="alpine"
    TEST_PKG1="jq"
    TEST_PKG2="htop"
    TEST_PKG3="curl"
fi
log "Using channel: $TEST_CHANNEL"

ENV_NAME="test-history"

log "Creating environment: $ENV_NAME"
epkg env remove "$ENV_NAME" 2>/dev/null
epkg env create "$ENV_NAME" -c $TEST_CHANNEL || error "Failed to create environment"

log "Installing $TEST_PKG1 and $TEST_PKG3"
epkg -e "$ENV_NAME" --assume-yes install $TEST_PKG1 $TEST_PKG3 || error "Failed to install $TEST_PKG1 and $TEST_PKG3"

log "Installing $TEST_PKG1 and $TEST_PKG2 ($TEST_PKG2 should be new)"
epkg -e "$ENV_NAME" --assume-yes install $TEST_PKG1 $TEST_PKG2 || error "Failed to install $TEST_PKG1 and $TEST_PKG2"

log "Removing $TEST_PKG3"
epkg -e "$ENV_NAME" --assume-yes remove $TEST_PKG3 || error "Failed to remove $TEST_PKG3"

log "Installing ripgrep"
epkg -e "$ENV_NAME" --assume-yes install ripgrep || error "Failed to install ripgrep"

# Verify history shows the above generations
log "Checking history"
HISTORY=$(epkg -e "$ENV_NAME" history)

if [ -z "$HISTORY" ]; then
    error "History is empty"
fi

# Count generations (should be at least 4)
GEN_COUNT=$(echo "$HISTORY" | grep -c "^[0-9]" || echo "0")
if [ "$GEN_COUNT" -lt 4 ]; then
    log "WARNING: Expected at least 4 generations, found $GEN_COUNT"
fi

log "History shows $GEN_COUNT generations"

# Restore to -2 (2 generations ago)
log "Restoring to -2"
epkg -e "$ENV_NAME" --assume-yes restore -2 || error "Failed to restore to -2"

# Verify that packages are in expected state after restore
log "Verifying installed packages after restore"

if ! epkg -e "$ENV_NAME" run $TEST_PKG1 --version; then
    error "$TEST_PKG1 not found after restore"
fi

if ! epkg -e "$ENV_NAME" run $TEST_PKG2 --version; then
    error "$TEST_PKG2 not found after restore"
fi

if ! epkg -e "$ENV_NAME" run $TEST_PKG3 --version; then
    error "$TEST_PKG3 not found after restore"
fi

if epkg -e "$ENV_NAME" run rg --version >/dev/null 2>&1; then
    error "ripgrep should not be present after restore"
fi

log "History/restore test completed successfully"

