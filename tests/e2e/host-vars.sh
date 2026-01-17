#!/bin/sh
# Host-side e2e test variables (for scripts running on the host)

# Get the project root directory (parent of tests/)
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
E2E_DIR="$SCRIPT_DIR"
export PROJECT_ROOT="${SCRIPT_DIR%/tests/e2e*}"

# Determine epkg binary path on host (external, for mounting into docker)
# Use static binary for Docker compatibility (Alpine uses musl, not glibc)
ARCH=$(uname -m)
case "$ARCH" in
	x86_64|aarch64|riscv64|loongarch64) EPKG_BINARY="$PROJECT_ROOT/dist/epkg-$ARCH" ;;
	*) exit 1 ;;
esac

# If EPKG_BINARY doesn't exist, build it automatically
if [ ! -x "$EPKG_BINARY" ]; then
	make -C $PROJECT_ROOT static-$ARCH
fi

# Docker volume mounts (host paths)
TMPFS_ENVS_ROOT="/tmp/epkg-envs-root"

# Mount entire /opt/epkg/ as a single mount point to avoid cross-device link errors
# This ensures cache/unpack and store are on the same filesystem
PERSISTENT_OPT_EPKG="/srv/os-repo/epkg-opt"
[ -w $PERSISTENT_OPT_EPKG ] || {
PERSISTENT_OPT_EPKG="/tmp/epkg-opt"
LIGHT_TEST=1
}

PERSISTENT_CACHE="$PERSISTENT_OPT_EPKG/cache"
PERSISTENT_STORE="$PERSISTENT_OPT_EPKG/store"

# Docker configuration
DOCKER_IMAGE=alpine
BUSYBOX_DOCKER=busybox
