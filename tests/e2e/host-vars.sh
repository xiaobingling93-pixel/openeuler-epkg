#!/bin/sh
# Host-side e2e test variables (for scripts running on the host)

# Get the project root directory (parent of tests/)
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
E2E_DIR="$SCRIPT_DIR"
export PROJECT_ROOT="${SCRIPT_DIR%/tests/e2e*}"

# Determine epkg binary path on host (external, for mounting into docker)
# Use static binary for Docker compatibility (Alpine uses musl, not glibc)
# make static puts binary at target/<rust_target>/debug/epkg (see bin/make.sh)
ARCH=$(uname -m)
case "$ARCH" in
	x86_64) RUST_TARGET=x86_64-unknown-linux-musl ;;
	aarch64) RUST_TARGET=aarch64-unknown-linux-musl ;;
	riscv64) RUST_TARGET=riscv64gc-unknown-linux-musl ;;
	loongarch64) RUST_TARGET=loongarch64-unknown-linux-musl ;;
	*) exit 1 ;;
esac
EPKG_BINARY="$PROJECT_ROOT/target/$RUST_TARGET/debug/epkg"

# If EPKG_BINARY doesn't exist, build it automatically
if [ ! -x "$EPKG_BINARY" ]; then
	make -C $PROJECT_ROOT static-$ARCH
fi

# Mount entire /opt/epkg/ as a single mount point to avoid cross-device link errors
# This ensures cache/unpack and store are on the same filesystem
PERSISTENT_OPT_EPKG="/opt/epkg"
[ -z "$LIGHT_TEST" ] && LIGHT_TEST=1

# Docker configuration
# Allow DOCKER_IMAGE to be set from environment, default to debian
# Note: Alpine uses musl libc which cannot run conda (relies on host __glibc)
DOCKER_IMAGE="${DOCKER_IMAGE:-debian}"
