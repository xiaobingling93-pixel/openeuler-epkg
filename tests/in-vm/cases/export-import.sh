#!/bin/sh
# Test export/import functionality

. "$(dirname "$0")/../vars.sh"
. "$(dirname "$0")/../lib.sh"

log "Starting export/import test"

# Create a test environment
ENV_NAME="test-export"
ENV2_NAME="test-import"

log "Creating environment: $ENV_NAME"
epkg env remove "$ENV_NAME" 2>/dev/null
epkg env remove "$ENV2_NAME" 2>/dev/null
epkg env create "$ENV_NAME" -c alpine || error "Failed to create environment"

log "Installing jq and htop"
epkg -e "$ENV_NAME" --assume-yes install jq htop || error "Failed to install packages"

# Export to a file
EXPORT_FILE="/tmp/epkg-export-$ENV_NAME.yaml"
log "Exporting environment to $EXPORT_FILE"
epkg env export "$ENV_NAME" --output "$EXPORT_FILE" || error "Failed to export environment"

if [ ! -f "$EXPORT_FILE" ]; then
    error "Export file not found"
fi

log "Export file created: $EXPORT_FILE"

# Get list of packages from original environment
log "Getting package list from original environment"
LIST1=$(epkg -e "$ENV_NAME" list --installed | sort)

log "Creating new environment with import"
epkg --assume-yes env create "$ENV2_NAME" --import "$EXPORT_FILE" || error "Failed to create environment with import"

# Get list of packages from imported environment
log "Getting package list from imported environment"
LIST2=$(epkg -e "$ENV2_NAME" list --installed | sort)

# Compare lists
if [ "$LIST1" != "$LIST2" ]; then
    log "ERROR: Package lists differ"
    log "Original environment packages:"
    echo "$LIST1"
    log "Imported environment packages:"
    echo "$LIST2"
    error "Package lists do not match"
fi

log "Package lists match"

# Verify that jq and htop commands are installed in env2
log "Verifying jq command in env2"
if ! epkg -e "$ENV2_NAME" run jq --version; then
    error "jq not found in env2"
fi

log "Verifying htop command in env2"
if ! epkg -e "$ENV2_NAME" run htop --version; then
    error "htop not found in env2"
fi

log "Export/import test completed successfully"

