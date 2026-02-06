#!/bin/bash
# Run BusyBox testsuite runtest in place, using epkg as the "busybox" implementation.
# Only applets implemented by epkg are tested; others are skipped.
#
# Usage: busybox-runtest.sh [applet1 [applet2 ...]]
#   With no args: run all tests for all epkg applets that have tests in the suite.
#   With args: run tests only for those applets.
#
# Env: BUSYBOX_TESTSUITE, EPKG_BIN, EPKG_BUSYBOX_SKIP_FEATURES, VERBOSE (see README).

set -e

# Get script directory and project root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

. "$PROJECT_ROOT/tests/common.sh"
set_epkg_bin

BUSYBOX_TS="${BUSYBOX_TESTSUITE:-$PROJECT_ROOT/../busybox/testsuite}"

if [ ! -d "$BUSYBOX_TS" ]; then
    echo "busybox-runtest.sh: BusyBox testsuite not found at $BUSYBOX_TS (set BUSYBOX_TESTSUITE)" >&2
    exit 1
fi

# Build epkg bindir with wrapper and .config (exports EPKG_BUSYBOX_SKIP_FEATURES for .config)
BINDIR=/tmp/epkg-busybox-bindir

export EPKG_BIN

bash "$SCRIPT_DIR/mk-epkg-bindir.sh" "$BINDIR" "$EPKG_BIN" >/dev/null

cd "$BUSYBOX_TS"
tsdir="$BUSYBOX_TS"
bindir="$BINDIR"
PATH="$bindir:$PATH"
export bindir tsdir VERBOSE

if [ $# -eq 0 ]; then
    ./runtest ${VERBOSE:+-v}
else
    ./runtest ${VERBOSE:+-v} "$@"
fi
