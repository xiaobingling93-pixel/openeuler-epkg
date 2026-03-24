#!/bin/sh
# Host-side in-vm test variables (scripts running on the host before epkg run --isolate=vm)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
E2E_DIR="$SCRIPT_DIR"
export PROJECT_ROOT="${SCRIPT_DIR%/tests/in-vm*}"

# Static epkg for the VM guest (musl); host uses the same binary to run `epkg run`
ARCH=$(uname -m)
case "$ARCH" in
	x86_64) RUST_TARGET=x86_64-unknown-linux-musl ;;
	aarch64) RUST_TARGET=aarch64-unknown-linux-musl ;;
	riscv64) RUST_TARGET=riscv64gc-unknown-linux-musl ;;
	loongarch64) RUST_TARGET=loongarch64-unknown-linux-musl ;;
	*) exit 1 ;;
esac
EPKG_BINARY="$PROJECT_ROOT/target/$RUST_TARGET/debug/epkg"

if [ ! -x "$EPKG_BINARY" ]; then
	make -C "$PROJECT_ROOT" static-$ARCH
fi

[ -z "$LIGHT_TEST" ] && LIGHT_TEST=1

# VM harness: environment used only to supply a rootfs + bash for `epkg run --isolate=vm`
E2E_BARE_ENV="${E2E_BARE_ENV:-bare-alpine-e2e}"
# Comma-separated VMM preference (see `epkg run --help` / --vmm)
# E2E_VMM="${E2E_VMM:-qemu}"
E2E_VMM="${E2E_VMM:-libkrun}"
E2E_VM_MEMORY="${E2E_VM_MEMORY:-16G}"
# Optional: pin vCPU count, e.g. E2E_VM_CPUS=4
# E2E_VM_CPUS=

# Optional: host dir bound read-write to guest /var/log/epkg-e2e (default ~/.cache/epkg/e2e-logs)
# E2E_LOG_DIR="$HOME/.cache/epkg/e2e-logs"

# Optional: full path to resolv.conf to mount in the guest (overrides vm.sh auto-generated DNS list)
# E2E_RESOLV_CONF=/path/to/resolv.conf

# Optional: host download cache bound to guest /opt/epkg/cache/downloads
# E2E_DOWNLOAD_CACHE="$HOME/.cache/epkg/downloads"

# Per-test filters (see cases/bash-sh.sh, test-iur.sh, e2e-combo.sh)
# E2E_OS=debian
# E2E_COMBO=label-for-logs

# Set by vm.sh in the guest only (not on the host): E2E_BACKEND=vm — epkg uses it for nested-VM behavior.
