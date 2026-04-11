#!/bin/sh
# Run BusyBox testsuite for a single applet.
#
# Usage: run-one.sh <applet> [applet ...]
# Example: ./run-one.sh cat

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
if [ $# -eq 0 ]; then
    echo "Usage: $0 <applet> [applet ...]" >&2
    echo "Example: $0 cat ls" >&2
    exit 1
fi
exec "$SCRIPT_DIR/busybox-runtest.sh" "$@"
