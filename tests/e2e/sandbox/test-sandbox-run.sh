#!/bin/sh
# Test epkg run sandbox modes (env and fs)

. "$(dirname "$0")/../vars.sh"
. "$(dirname "$0")/../lib.sh"

log "Starting sandbox run test"

ENV_NAME="sandbox-debian"
EPKG_BIN=$(realpath $EPKG_BINARY)
EPKG_BIN_DIR=$(dirname $EPKG_BIN)
SANDBOX_MOUNT_OPTS="--mount $EPKG_BIN_DIR"

log "Creating test environment $ENV_NAME"
epkg env create "$ENV_NAME" -c alpine || error "Failed to create sandbox env"

log "Running epkg ls / with --sandbox=fs"
epkg -e "$ENV_NAME" run --sandbox=fs $SANDBOX_MOUNT_OPTS ls / || error "epkg run --sandbox=fs ls / failed"

log "Installing bash into $ENV_NAME"
epkg -e "$ENV_NAME" --assume-yes install bash coreutils || error "Failed to install bash in sandbox env"

log "Running ls /sys with default sandbox (env)"
epkg -e "$ENV_NAME" run ls /sys || error "epkg run ls /sys failed in default sandbox"

log "Running ls /sys with --sandbox=env"
epkg -e "$ENV_NAME" run --sandbox=env ls /sys || error "epkg run --sandbox=env ls /sys failed"

log "Running ls /sys with --sandbox=fs"
epkg -e "$ENV_NAME" run --sandbox=fs $SANDBOX_MOUNT_OPTS ls /sys || error "epkg run --sandbox=fs ls /sys failed"

log "Setting sandbox_mode=fs in env config"
epkg -e "$ENV_NAME" env config set sandbox.sandbox_mode fs || error "Failed to set sandbox_mode in env config"

log "Running ls /sys with env sandbox_mode=fs (no --sandbox flag)"
epkg -e "$ENV_NAME" run $SANDBOX_MOUNT_OPTS ls /sys || error "epkg run ls /sys failed with sandbox_mode=fs"

log "Sandbox run test completed successfully"

log "Cleaning up environment $ENV_NAME"
epkg env remove "$ENV_NAME"

