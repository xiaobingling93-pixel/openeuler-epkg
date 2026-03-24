#!/bin/sh
# Run a single in-vm test inside the VM harness.
#
# This script sets up the test environment and runs the specified test
# script inside an epkg microVM (--isolate=vm).
#
# Usage:
#   ./test-one.sh [-d|--debug|-dd|-ddd] <test_script> [additional_args...]
#
# Arguments:
#   test_script       Path to the test script (relative or absolute)
#   additional_args   Arguments passed to the test script
#
# Options:
#   -d, --debug    Interactive debug mode (pause on error)
#   -dd            Debug logging (RUST_LOG=debug, RUST_BACKTRACE=1)
#   -ddd           Trace logging (RUST_LOG=trace, RUST_BACKTRACE=1)
#
# Environment:
#   E2E_VMM          VMM backend (qemu, libkrun; default: libkrun)
#   E2E_VM_MEMORY    VM memory (default: 16G)
#   E2E_OS           Default OS for tests that iterate over OSes

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Source host variables
. "$SCRIPT_DIR/host-vars.sh"

# Parse command line flags using common library
. "$SCRIPT_DIR/lib.sh"

parse_debug_flags "$@"
case $? in
    0)
        eval set -- "$PARSE_DEBUG_FLAGS_REMAINING"
        ;;
    1)
        exit 1
        ;;
    2)
        cat <<'USAGE'
Usage: test-one.sh [-d|--debug|-dd|-ddd] <test_script> [additional_args...]

Run a single e2e test inside the VM harness.

This script sets up the test environment and runs the specified test
script inside an epkg microVM (--isolate=vm).

Arguments:
  test_script       Path to the test script
  additional_args   Arguments passed to the test script

Options:
  -d, --debug       Interactive debug mode (pause on error)
  -dd               Debug logging (RUST_LOG=debug)
  -ddd              Trace logging (RUST_LOG=trace, RUST_BACKTRACE=1)

Environment:
  E2E_VMM           VMM backend (qemu, libkrun; default: libkrun)
  E2E_VM_MEMORY     VM memory (default: 16G)
  E2E_OS            Default OS for tests

Examples:
  ./test-one.sh cases/env-register-activate.sh
  ./test-one.sh -d cases/bare-rootfs.sh
  E2E_OS=alpine ./test-one.sh cases/bash-sh.sh
USAGE
        exit 0
        ;;
esac

# Apply debug settings based on DEBUG_FLAG
INTERACTIVE=""
case "$DEBUG_FLAG" in
    -ddd)
        INTERACTIVE="2"
        export RUST_LOG=trace
        export RUST_BACKTRACE=1
        ;;
    -dd)
        INTERACTIVE="2"
        export RUST_LOG=debug
        export RUST_BACKTRACE=1
        ;;
    -d|--debug)
        INTERACTIVE="1"
        ;;
    *)
        ;;
esac

if [ $# -lt 1 ]; then
    echo "Usage: $0 [-d|--debug|-dd|-ddd] <test_script> [additional_args...]" >&2
    exit 1
fi

TEST_SCRIPT="$1"
shift
ADDITIONAL_ARGS="$*"

if [ ! -x "$TEST_SCRIPT" ]; then
    echo "ERROR: Test script not found or not executable: $TEST_SCRIPT" >&2
    exit 1
fi

export TEST_SCRIPT
export INTERACTIVE
export ADDITIONAL_ARGS

# Source and execute the VM launcher
. "$SCRIPT_DIR/vm.sh"
