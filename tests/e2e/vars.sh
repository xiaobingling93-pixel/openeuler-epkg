#!/bin/sh
# Container-side e2e test variables (for scripts running inside docker)

# Common test variables
ALL_OS="debian ubuntu fedora alpine archlinux"

# E2E_DIR is set by docker.sh to the same path as on host
# This allows the same directory layout inside and outside docker for easier debugging
if [ -z "$E2E_DIR" ]; then
    # Fallback: try to detect from script location
    SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
    E2E_DIR="$SCRIPT_DIR"
fi
