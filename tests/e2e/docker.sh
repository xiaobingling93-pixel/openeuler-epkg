#!/bin/sh
# Docker run command for e2e tests

. "$(dirname "$0")/host-vars.sh"

# Calculate relative path from E2E_DIR to test script
TEST_REL_PATH="${TEST_SCRIPT#$E2E_DIR/}"

# Determine if we should run interactively (for debugging)
INTERACTIVE="${INTERACTIVE:-}"

if [ -n "$INTERACTIVE" ]; then
    DOCKER_FLAGS="-it"
else
    DOCKER_FLAGS=""
fi

# Create /opt/epkg directory structure on host if it doesn't exist
# This ensures cache, store, and envs are all on the same filesystem
mkdir -p "$PERSISTENT_OPT_EPKG/cache" "$PERSISTENT_OPT_EPKG/store" "$PERSISTENT_OPT_EPKG/envs"

# Detect host timezone and sync with Docker to avoid timestamp issues
# This helps prevent "Another download process is already active" errors due to timezone differences
HOST_TZ="${TZ:-}"
if [ -z "$HOST_TZ" ] && [ -f /etc/timezone ]; then
    HOST_TZ=$(cat /etc/timezone)
elif [ -z "$HOST_TZ" ] && [ -L /etc/localtime ]; then
    HOST_TZ=$(readlink /etc/localtime | sed 's|.*/zoneinfo/||')
fi

# Run docker with proper mounts
# Mount E2E_DIR at the same path inside docker for easier debugging
# Mount entire /opt/epkg/ as a single mount to avoid cross-device link errors
# Sync timezone to prevent timestamp-related download conflicts
docker run --name epkg-e2e --privileged --rm $DOCKER_FLAGS \
    -v "$PROJECT_ROOT:$PROJECT_ROOT:ro" \
    -v "$PERSISTENT_OPT_EPKG:/opt/epkg:rw" \
    -v "$PERSISTENT_CACHE:/root/.cache/epkg:rw" \
    -v "$PERSISTENT_STORE:/root/.epkg/store:rw" \
    -v "$TMPFS_ENVS_ROOT:/root/.epkg/envs:rw" \
    ${HOST_TZ:+-e TZ="$HOST_TZ"} \
    ${LIGHT_TEST:+-e LIGHT_TEST="$LIGHT_TEST"} \
    -e E2E_DIR="$E2E_DIR" \
    -e EPKG_BINARY=$EPKG_BINARY \
    -e TEST_REL_PATH="$TEST_REL_PATH" \
    -e ADDITIONAL_ARGS="$ADDITIONAL_ARGS" \
    ${INTERACTIVE:+-e INTERACTIVE="$INTERACTIVE"} \
    ${RUST_LOG:+-e RUST_LOG="$RUST_LOG"} \
    ${RUST_BACKTRACE:+-e RUST_BACKTRACE="$RUST_BACKTRACE"} \
    "$DOCKER_IMAGE" \
    "$E2E_DIR/entry.sh"

