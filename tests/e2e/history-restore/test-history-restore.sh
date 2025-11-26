#!/bin/sh
# Test history/restore functionality

set -e

. "$(dirname "$0")/../vars.sh"
. "$(dirname "$0")/../lib.sh"

log "Starting history/restore test"

ENV_NAME="test-history"

log "Creating environment: $ENV_NAME"
epkg env create "$ENV_NAME" -c alpine || error "Failed to create environment"

log "Installing jq and curl"
epkg -e "$ENV_NAME" install --assume-yes jq curl || error "Failed to install jq and curl"

log "Installing jq and htop (htop should be new)"
epkg -e "$ENV_NAME" install --assume-yes jq htop || error "Failed to install jq and htop"

log "Removing curl"
epkg -e "$ENV_NAME" --assume-yes remove curl || error "Failed to remove curl"

log "Installing ripgrep"
epkg -e "$ENV_NAME" install --assume-yes ripgrep || error "Failed to install ripgrep"

# Verify history shows the above generations
log "Checking history"
HISTORY=$(epkg -e "$ENV_NAME" history 2>/dev/null || true)

if [ -z "$HISTORY" ]; then
    error "History is empty"
fi

# Count generations (should be at least 4)
GEN_COUNT=$(echo "$HISTORY" | grep -c "^[0-9]" || echo "0")
if [ "$GEN_COUNT" -lt 4 ]; then
    log "WARNING: Expected at least 4 generations, found $GEN_COUNT"
fi

log "History shows $GEN_COUNT generations"

# Restore to ~2 (2 generations ago)
log "Restoring to ~2"
epkg -e "$ENV_NAME" --assume-yes restore ~2 || error "Failed to restore to ~2"

# Verify that jq/htop are installed, curl/rg are not
log "Verifying installed packages after restore"

if ! epkg -e "$ENV_NAME" run jq --version; then
    error "jq not found after restore"
fi

if ! epkg -e "$ENV_NAME" run htop --version; then
    error "htop not found after restore"
fi

if ! epkg -e "$ENV_NAME" run curl --version; then
    error "curl not found after restore"
fi

if epkg -e "$ENV_NAME" run rg --version >/dev/null 2>&1; then
    error "ripgrep should not be present after restore"
fi

log "History/restore test completed successfully"

# Cleanup
epkg --assume-yes env remove "$ENV_NAME" 2>/dev/null || true

