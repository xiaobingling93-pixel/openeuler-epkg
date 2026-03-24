#!/bin/bash
# Entry script for e2e (runs inside epkg run --isolate=vm guest). Bash is required (lib.sh uses `local`).

# E2E_DIR should be set by vm.sh launch wrapper
# Fallback: try to detect from script location (for manual debugging)
if [ -z "$E2E_DIR" ]; then
    SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
    E2E_DIR="$SCRIPT_DIR"
fi

export IN_E2E=1

# Set up timezone symlink if TZ is set and zoneinfo exists
if [ -n "$TZ" ] && [ -f "/usr/share/zoneinfo/$TZ" ]; then
    ln -sf "/usr/share/zoneinfo/$TZ" /etc/localtime
    echo "Set /etc/localtime to /usr/share/zoneinfo/$TZ"
elif [ -n "$TZ" ] && [ ! -f "/usr/share/zoneinfo/$TZ" ]; then
    echo "Warning: TZ=$TZ but zoneinfo file not found, timezone may not work correctly"
fi

# Initialize epkg
"$EPKG_BINARY" --version
if [ ! -x "$HOME/.epkg/envs/self/usr/bin/epkg" ] && [ ! -x "/opt/epkg/envs/root/self/usr/bin/epkg" ]; then
	"$EPKG_BINARY" self install || exit 1
fi
if [ -x /opt/epkg/envs/root/self/usr/bin/epkg ]; then
	ls -l /opt/epkg/envs/root/self/usr/bin/epkg
elif [ -x "$HOME/.epkg/envs/self/usr/bin/epkg" ]; then
	ls -l "$HOME/.epkg/envs/self/usr/bin/epkg"
fi

# Source vars and lib
. "$E2E_DIR/vars.sh"
. "$E2E_DIR/lib.sh"

# Run the test script with additional args if provided
if [ -n "$ADDITIONAL_ARGS" ]; then
    /bin/bash "$E2E_DIR/$TEST_REL_PATH" $ADDITIONAL_ARGS
else
    /bin/bash "$E2E_DIR/$TEST_REL_PATH"
fi

