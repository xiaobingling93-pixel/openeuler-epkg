#!/bin/sh
# Test building epkg from source and self-installing in Docker container OS

. "$(dirname "$0")/../vars.sh"
. "$(dirname "$0")/../lib.sh"

log "Starting build-from-source test in Docker container"

# Determine project root directory (same as outside Docker)
# E2E_DIR is set by vars.sh
PROJECT_ROOT="${E2E_DIR%/tests/e2e}"
export PROJECT_ROOT

# Detect and log current OS
if [ -f /etc/os-release ]; then
    . /etc/os-release
    log "Current OS: $ID $VERSION_ID"
else
    log "Could not detect OS from /etc/os-release"
fi

# Helper to build epkg from source in current environment (Docker container)
build_epkg() {
    log "Building epkg from source in current environment"

    # Change to project root directory (mounted at same path as outside Docker)
    cd "$PROJECT_ROOT" || error "Failed to cd to project root: $PROJECT_ROOT"

    # Run bin/make.sh dev-depends to install build dependencies
    log "Installing build dependencies"
    if ! ./bin/make.sh dev-depends; then
        error "Failed to install build dependencies"
    fi

    # Build epkg using bin/make.sh
    log "Running bin/make.sh build"
    if ! ./bin/make.sh build; then
        error "bin/make.sh build failed"
    fi

    # Verify the binary exists
    if ! test -f target/debug/epkg; then
        error "Built binary not found at target/debug/epkg"
    fi
    log "Build successful"
}

# Helper to self-install and test the built binary
test_built_epkg() {
    log "Self-installing built epkg"
    # Ensure we're in project root directory
    cd "$PROJECT_ROOT" || error "Failed to cd to project root: $PROJECT_ROOT"

    if ! target/debug/epkg self install; then
        error "Self-install failed"
    fi

    # Verify epkg is now available in PATH
    log "Testing installed epkg"
    # Try to run epkg info bash using the installed epkg
    if ! epkg info bash; then
        error "epkg info bash failed with installed epkg"
    fi
    log "Installed epkg works correctly"
}

# Main test - build in current Docker container environment
log "Testing build-from-source in current Docker container environment"
build_epkg
test_built_epkg

log "Build-from-source test passed in Docker container"
