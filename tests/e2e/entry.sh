#!/bin/sh
# Entry script for docker container

# E2E_DIR should be set by docker.sh as an environment variable
# Fallback: try to detect from script location (for manual debugging)
if [ -z "$E2E_DIR" ]; then
    SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
    E2E_DIR="$SCRIPT_DIR"
fi

export IN_DOCKER=1

# Initialize tmpfs mounts
mount -t tmpfs tmpfs /root/.epkg/envs 2>/dev/null || true
mount -t tmpfs tmpfs /opt/epkg/envs 2>/dev/null || true

# Create directories
mkdir -p /root/.epkg/envs /opt/epkg/envs /root/.cache/epkg /opt/epkg/cache /opt/epkg/store

# Set up timezone symlink if TZ is set and zoneinfo exists
if [ -n "$TZ" ] && [ -f "/usr/share/zoneinfo/$TZ" ]; then
    ln -sf "/usr/share/zoneinfo/$TZ" /etc/localtime
    echo "Set /etc/localtime to /usr/share/zoneinfo/$TZ"
elif [ -n "$TZ" ] && [ ! -f "/usr/share/zoneinfo/$TZ" ]; then
    echo "Warning: TZ=$TZ but zoneinfo file not found, timezone may not work correctly"
fi

# Initialize epkg
"$EPKG_BINARY" --version
"$EPKG_BINARY" self install
ls -l /opt/epkg/envs/root/self/usr/bin/epkg

# Source vars and lib
. "$E2E_DIR/vars.sh"
. "$E2E_DIR/lib.sh"

# Run the test script with additional args if provided
if [ -n "$ADDITIONAL_ARGS" ]; then
    "$E2E_DIR/$TEST_REL_PATH" $ADDITIONAL_ARGS
else
    "$E2E_DIR/$TEST_REL_PATH"
fi

