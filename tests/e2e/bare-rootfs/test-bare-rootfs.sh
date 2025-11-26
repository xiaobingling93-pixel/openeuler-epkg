#!/bin/sh
# Test bare rootfs on /

set -e

. "$(dirname "$0")/../host-vars.sh"
. "$(dirname "$0")/../lib.sh"

log "Starting bare rootfs test"

# Prepare empty scratch image
log "Preparing empty scratch image"
# Create an empty tar to import as a scratch-based image
TAR_DIR=$(mktemp -d) || error "Failed to create temporary directory"
TAR_TMP=$(mktemp) || error "Failed to create temporary tar"
# Create an empty tar archive from empty directory
tar -C "$TAR_DIR" -cf "$TAR_TMP" . 2>/dev/null || error "Failed to create empty tar"
# Import the empty tar as a Docker image (creates a scratch-based image)
docker import "$TAR_TMP" epkg-scratch-temp >/dev/null 2>&1 || error "Failed to import image"
rm -rf "$TAR_DIR" "$TAR_TMP"

# Start a long-running docker container with sleep to persist state
log "Starting long-running docker container"
# Built-in commands like sleep work without initialization, so we can run directly
# Pre-create directories on host (they're mounted as volumes)
mkdir -p "$PERSISTENT_OPT_EPKG/envs" "$PERSISTENT_CACHE" "$PERSISTENT_STORE" "$TMPFS_ENVS_ROOT"

CONTAINER_NAME="epkg-e2e"
docker run -d --name="$CONTAINER_NAME" --privileged --rm \
	-v "$PROJECT_ROOT:$PROJECT_ROOT:ro" \
	-v "$PERSISTENT_OPT_EPKG:/opt/epkg:rw" \
        -v "$PERSISTENT_CACHE:/root/.cache/epkg:rw" \
	-v "$PERSISTENT_STORE:/root/.epkg/store:rw" \
	-v "$TMPFS_ENVS_ROOT:/root/.epkg/envs:rw" \
        epkg-scratch-temp \
        $EPKG_BINARY run --builtin sleep 10000

# Check if container is running
if ! docker ps --format '{{.Names}}' | grep -q "^${CONTAINER_NAME}$"; then
    log "Container $CONTAINER_NAME is not running, checking status:"
    docker ps -a --format 'table {{.Names}}\t{{.Status}}\t{{.Command}}' | grep -E "(NAMES|${CONTAINER_NAME})" || true
    log "Container logs:"
    docker logs "$CONTAINER_NAME" 2>&1 | tail -20 || true
    error "Container failed to start or exited immediately"
fi

log "Container $CONTAINER_NAME is running"

# Cleanup function to remove container and image on exit
cleanup_container() {
    if [ -n "$CONTAINER_NAME" ]; then
        log "Stopping and removing container $CONTAINER_NAME"
        docker stop "$CONTAINER_NAME" >/dev/null 2>&1 || true
        docker rm "$CONTAINER_NAME" >/dev/null 2>&1 || true
    fi
    # Remove the scratch image after container is cleaned up
    docker rmi epkg-scratch-temp >/dev/null 2>&1 || true
}
trap cleanup_container EXIT

# Helper function to exec epkg commands in the running container
exec_epkg() {
    docker exec "$CONTAINER_NAME" $EPKG_BINARY "$@" || error "epkg command failed: $*"
}

exec_epkg run --builtin ls /
exec_epkg run --builtin ls /etc
# Initialize epkg in the running container
log "Initializing epkg in container"
exec_epkg init -c alpine

# Setup bare rootfs environment using epkg's environment feature with --path /
log "Creating sys environment with --path /"
exec_epkg env create sys -c alpine --path /

log "Installing jq"
exec_epkg run --builtin cat /etc/resolv.conf
exec_epkg -e sys --assume-yes install jq coreutils bash

# Verify that jq
log "Verifying jq command"
exec_epkg -e sys run jq --version || error "jq failed"
docker exec "$CONTAINER_NAME" /usr/bin/jq --version || error "/usr/bin/jq failed"

log "Bare rootfs test completed successfully"

# Cleanup: stop container first, then remove image (trap will handle final cleanup on exit)
cleanup_container
