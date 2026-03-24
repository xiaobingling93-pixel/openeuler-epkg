#!/bin/sh
# Run one e2e test with explicit VMM / memory / per-test knobs (thin wrapper around test-one.sh).
#
# Examples:
#   E2E_VMM=qemu E2E_VM_MEMORY=8G ./e2e-combo.sh cases/bash-sh.sh debian
#   E2E_OS=ubuntu ./e2e-combo.sh cases/bash-sh.sh
#   ./e2e-combo.sh -d cases/sandbox-run.sh

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
export E2E_COMBO="${E2E_COMBO:-manual}"

exec "$SCRIPT_DIR/test-one.sh" "$@"
