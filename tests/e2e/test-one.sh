#!/bin/sh
# Run a single e2e test (supports debug mode with -d flag)

. "$(dirname "$0")/host-vars.sh"

# Check for debug mode flag
INTERACTIVE=""
if [ "$1" = "-ddd" ]; then
    INTERACTIVE="2"
    export RUST_LOG=trace
    export RUST_BACKTRACE=1
    shift
elif [ "$1" = "-dd" ]; then
    INTERACTIVE="2"
    export RUST_LOG=debug
    export RUST_BACKTRACE=1
    shift
elif [ "$1" = "-d" ] || [ "$1" = "--debug" ]; then
    INTERACTIVE="1"
    shift
fi

if [ $# -lt 1 ]; then
    echo "Usage: $0 [-d|--debug|-dd|-ddd] <test_script> [additional_args...]" >&2
    exit 1
fi

TEST_SCRIPT="$1"
shift
ADDITIONAL_ARGS="$*"

if [ ! -x "$TEST_SCRIPT" ]; then
    echo "ERROR: Test script not found: $TEST_SCRIPT" >&2
    exit 1
fi

# Export variables for docker.sh
export TEST_SCRIPT
export INTERACTIVE
export ADDITIONAL_ARGS

if [ "${TEST_SCRIPT#*bare-rootfs/*}" != "$TEST_SCRIPT" ]; then
	$TEST_SCRIPT
else
	# Run docker via docker.sh
	. "$(dirname "$0")/docker.sh"
fi

