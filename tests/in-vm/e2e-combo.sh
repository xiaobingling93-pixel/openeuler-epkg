#!/bin/sh
# Run one in-vm test with explicit VMM / memory / per-test knobs (thin wrapper around test-one.sh).
#
# Examples:
#   E2E_VMM=qemu E2E_VM_MEMORY=8G ./e2e-combo.sh cases/bare-rootfs.sh
#   E2E_OS=ubuntu ./e2e-combo.sh cases/env-register-activate.sh
#   ./e2e-combo.sh -d cases/history-restore.sh

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
export E2E_COMBO="${E2E_COMBO:-manual}"

exec "$SCRIPT_DIR/test-one.sh" "$@"
