#!/bin/sh
# Run in-vm entry inside a microVM (epkg run --isolate=vm) instead of Docker.

. "$(dirname "$0")/host-vars.sh"

TEST_REL_PATH="${TEST_SCRIPT#$E2E_DIR/}"

INTERACTIVE="${INTERACTIVE:-}"

# Host timezone (same rationale as former docker harness)
HOST_TZ="${TZ:-}"
if [ -z "$HOST_TZ" ] && [ -f /etc/timezone ]; then
	HOST_TZ=$(cat /etc/timezone)
elif [ -z "$HOST_TZ" ] && [ -L /etc/localtime ]; then
	HOST_TZ=$(readlink /etc/localtime | sed 's|.*/zoneinfo/||')
fi
if [ -z "$HOST_TZ" ]; then
	HOST_TZ="UTC"
fi
export TZ="$HOST_TZ"

zoneinfo=$(readlink /etc/localtime)
if [ -z "$zoneinfo" ]; then
	zoneinfo="/etc/localtime"
fi

DL_CACHE="${E2E_DOWNLOAD_CACHE:-$HOME/.cache/epkg/downloads}"
mkdir -p "$DL_CACHE"

E2E_LOG_DIR="${E2E_LOG_DIR:-$HOME/.cache/epkg/e2e-logs}"
mkdir -p "$E2E_LOG_DIR"

# Ensure Alpine-based harness environment exists (bash + busybox for tests / sh -c)
ensure_e2e_bare_env() {
	if "$EPKG_BINARY" -e "$E2E_BARE_ENV" list; then
		return 0
	fi
	echo "Creating in-vm harness environment '$E2E_BARE_ENV' (channel alpine)..." >&2
	"$EPKG_BINARY" env create "$E2E_BARE_ENV" -c alpine || exit 1
	"$EPKG_BINARY" -e "$E2E_BARE_ENV" --assume-yes install bash busybox-static || exit 1
}

ensure_e2e_bare_env

# Guest: root + global epkg under tmpfs /opt/epkg; tmpfs /root for HOME.
# Host download cache + optional log dir + resolv.conf for HTTPS/DNS.
#
# IMPORTANT: libkrun has limited IRQs (11 on x86_64). Each virtiofs mount needs one IRQ.
# Rootfs, rng, and vsock each use one IRQ. So we can only have ~8 additional mounts.
# To stay under the limit, mount parent directories instead of multiple separate paths.
#
# Strategy: Mount ~/.cache/epkg as /opt/epkg/cache to cover both downloads and e2e-logs.
# This reduces mount count while preserving functionality.
MOUNTS="-m tmpfs:/opt/epkg"
MOUNTS="$MOUNTS -m tmpfs:/root"
# Mount the entire epkg cache directory to cover downloads and e2e-logs
EPKG_CACHE_DIR="${HOME}/.cache/epkg"
mkdir -p "$EPKG_CACHE_DIR/downloads" "$EPKG_CACHE_DIR/e2e-logs"
MOUNTS="$MOUNTS -m $EPKG_CACHE_DIR:/opt/epkg/cache"
E2E_LOG_DIR=/opt/epkg/cache/e2e-logs  # Guest path (under the mounted cache)
RESOLV_TMP="${TMPDIR:-/tmp}/epkg-e2e-resolv.$$"
# Guest DNS: public resolvers first (work through QEMU NAT), then QEMU slirp (10.0.2.3) and host
# upstreams from systemd. Slirp-only configs often return EAI_AGAIN on some hosts. Do not bind-mount host
# /etc/resolv.conf when it is systemd's 127.0.0.53 stub — wrong inside the guest.
# Override: E2E_RESOLV_CONF=/path/to/resolv.conf
if [ -n "${E2E_RESOLV_CONF:-}" ] && [ -r "$E2E_RESOLV_CONF" ]; then
	cp -a "$E2E_RESOLV_CONF" "$RESOLV_TMP"
else
	{
		# Public DNS first: HTTPS to the internet works even when QEMU slirp (10.0.2.3) is flaky.
		echo "nameserver 8.8.8.8"
		echo "nameserver 1.1.1.1"
		echo "nameserver 10.0.2.3"
		if [ -r /run/systemd/resolve/resolv.conf ]; then
			awk '/^nameserver[[:space:]]+/ { print $2 }' /run/systemd/resolve/resolv.conf | while read -r ns; do
				case "$ns" in
				127.*|0.0.0.0|::1) ;;
				*) echo "nameserver $ns" ;;
				esac
			done
		fi
	} | awk '/^nameserver / { if (!seen[$2]++) print }' >"$RESOLV_TMP"
fi
# Keep resolv.conf writable in VM harness.
# Some kernels/userns combinations reject read-only bind remount for regular files (EPERM),
# which aborts the namespace setup before tests even start.
MOUNTS="$MOUNTS -m $RESOLV_TMP:/etc/resolv.conf"
# Mount project root (read-only) - this provides test scripts and epkg binary
MOUNTS="$MOUNTS -m $PROJECT_ROOT:$PROJECT_ROOT:ro"
# Mount timezone (read-only) - use host timezone in guest
MOUNTS="$MOUNTS -m $zoneinfo:$zoneinfo:ro"

# bare-rootfs case: pre-provision chroot mounts from host-side epkg run.
# Instead of mounting separate tmpfs and resolv.conf, use the existing mounts.
if [ "$TEST_REL_PATH" = "cases/bare-rootfs.sh" ]; then
	E2E_BARE_CHROOT="${E2E_BARE_CHROOT:-/tmp/epkg-bare-chroot}"
	# Note: We don't add extra mounts here to stay under IRQ limit.
	# The test will create the chroot directory within the VM.
fi

trap 'rm -f "$RESOLV_TMP"' EXIT INT HUP

VM_EXTRA=""
if [ -n "$E2E_VM_CPUS" ]; then
	VM_EXTRA="$VM_EXTRA --cpus=$E2E_VM_CPUS"
fi

set -- \
	"$EPKG_BINARY" -e "$E2E_BARE_ENV" run --isolate=vm \
	-u root \
	--vmm="$E2E_VMM" \
	--memory="$E2E_VM_MEMORY" \
	$VM_EXTRA \
	$MOUNTS \
	-- env \
	PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
	HOME=/root \
	USER=root \
	E2E_DIR="$E2E_DIR" \
	EPKG_BINARY="$EPKG_GUEST_BINARY" \
	TEST_REL_PATH="$TEST_REL_PATH" \
	E2E_BACKEND=vm \
	IN_E2E=1 \
	LIGHT_TEST="${LIGHT_TEST:-}" \
	INTERACTIVE="${INTERACTIVE:-}" \
	CONTAINER_NAME="${CONTAINER_NAME:-epkg-e2e-vm}" \
	RUST_LOG="${RUST_LOG:-}" \
	RUST_BACKTRACE="${RUST_BACKTRACE:-}" \
	TZ="$TZ" \
	E2E_OS="${E2E_OS:-}" \
	E2E_VMM="${E2E_VMM:-}" \
	E2E_COMBO="${E2E_COMBO:-}" \
	E2E_BARE_CHROOT="${E2E_BARE_CHROOT:-}" \
	E2E_LOG_DIR=/opt/epkg/cache/e2e-logs \
	/bin/bash "$E2E_DIR/entry.sh" $ADDITIONAL_ARGS

	echo "Running in-vm test: $*" >&2
exec "$@"
