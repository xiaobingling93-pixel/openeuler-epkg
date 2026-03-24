#!/bin/sh
# Run multiple e2e cases against the Alpine channel inside the VM harness (E2E_OS=alpine).
# Requires: built musl epkg (make), QEMU. Uses E2E_BACKEND=vm from vm.sh in the guest.

. "$(dirname "$0")/host-vars.sh"

export E2E_OS=alpine

run_one() {
	echo "========== $1 ==========" >&2
	./test-one.sh "$@" || exit 1
}

cd "$(dirname "$0")" || exit 1

# Order: fast / focused on alpine + VM behavior first
run_one cases/bash-sh.sh
run_one cases/sandbox-run.sh
run_one cases/epkg-wrapper.sh

echo "All Alpine VM cases passed." >&2
