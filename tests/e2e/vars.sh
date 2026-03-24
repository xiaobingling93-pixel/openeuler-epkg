#!/bin/sh
# Guest-side e2e test variables (scripts running inside epkg run --isolate=vm)

# Common test variables
ALL_OS="openeuler fedora  debian ubuntu  alpine archlinux conda"

# E2E_DIR is set by vm.sh to the same path as on the host (project tree mounted read-only)
if [ -z "$E2E_DIR" ]; then
    # Fallback: try to detect from script location
    SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
    E2E_DIR="$SCRIPT_DIR"
fi
