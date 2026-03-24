#!/bin/sh
# Test building epkg from source and self-installing in Docker container OS

. "$(dirname "$0")/../vars.sh"
. "$(dirname "$0")/../lib.sh"

log "Starting build-from-source test in Docker container"

# Determine project root directory (same as outside Docker)
# E2E_DIR is set by vars.sh
PROJECT_ROOT="${E2E_DIR%/tests/in-vm}"
export PROJECT_ROOT

# Detect and log current OS
if [ -f /etc/os-release ]; then
    . /etc/os-release
    log "Current OS: $ID $VERSION_ID"
else
    log "Could not detect OS from /etc/os-release"
fi

# Helper function: install OS packages via dev-pkgs
install_packages() {
    log "Installing OS packages (git, wget, compilers, etc.)"
    if ! $PROJECT_ROOT/bin/make.sh dev-pkgs; then
        error "Failed to install OS packages"
    fi
    log "OS packages installed successfully"
}

# Helper function: clone project to writable directory
clone_project_to_writable_dir() {
    local BUILD_DIR GIT_URL

    # Create writable build directory (project is mounted read-only)
    BUILD_DIR="${PERSISTENT_OPT_EPKG:-/opt/epkg}/build-$(date +%s)"
    log "Creating writable build directory at $BUILD_DIR"
    mkdir -p "$BUILD_DIR" || error "Failed to create build directory: $BUILD_DIR"

    # Clone project to writable location using git
    log "Cloning project to writable directory using git"

    # Verify we have a git repository
    if ! [ -d "$PROJECT_ROOT/.git" ]; then
        error "Project directory is not a git repository: $PROJECT_ROOT"
    fi

    # Clone from local filesystem using file:// protocol
    GIT_URL="file://$PROJECT_ROOT"
    log "Cloning from local git repository: $GIT_URL"
    if ! git clone "$GIT_URL" "$BUILD_DIR"; then
        error "Failed to clone repository from $GIT_URL"
    fi
    log "Git clone successful"

    # Change to writable build directory
    cd "$BUILD_DIR" || error "Failed to cd to build directory: $BUILD_DIR"
    PROJECT_ROOT="$BUILD_DIR"  # Update PROJECT_ROOT for subsequent operations
    echo "$BUILD_DIR"  # Return build directory path
}

# Configure git safe.directory to avoid dubious ownership errors
# Fixes:
#+ git clone file:///c/epkg /opt/epkg/build-1771692286
# Cloning into '/opt/epkg/build-1771692286'...
# fatal: detected dubious ownership in repository at '/c/epkg/.git'
# To add an exception for this directory, call:
#
#         git config --global --add safe.directory /c/epkg/.git
configure_git_safe_directories() {
    git config --global --add safe.directory $PROJECT_ROOT/.git
}

clone_repos() {
    log "Cloning required repositories (rpm-rs, resolvo, elf-loader)"
    if ! ./bin/make.sh clone-repos; then
        error "Failed to clone required repositories"
    fi
    log "Repositories cloned successfully"
}

# Helper function: build epkg binary
build_epkg_binary() {
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

# Helper to build epkg from source in current environment (Docker container)
build_epkg() {
    log "Building epkg from source in current environment"

    install_packages

    configure_git_safe_directories

    # Step 2: Clone epkg project to writable directory
    clone_project_to_writable_dir

    # Step 3: Clone resolvo, rpm-rs, elf-loader
    clone_repos

    # Step 4: Build the binary
    build_epkg_binary
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

# Build static binary via make.sh static (requires Lua musl lib first)
build_static_binary() {
    log "Building Lua musl library for static build"
    if ! ./bin/make.sh lua; then
        error "bin/make.sh lua failed"
    fi
    log "Running bin/make.sh static"
    if ! ./bin/make.sh static; then
        error "bin/make.sh static failed"
    fi
    log "Static build successful"
}

# Verify target/<rust_target>/debug/epkg exists and works (make static output)
test_static_binary() {
    cd "$PROJECT_ROOT" || error "Failed to cd to project root: $PROJECT_ROOT"
    ARCH=$(arch)
    case "$ARCH" in
        x86_64) RUST_TARGET=x86_64-unknown-linux-musl ;;
        aarch64) RUST_TARGET=aarch64-unknown-linux-musl ;;
        riscv64) RUST_TARGET=riscv64gc-unknown-linux-musl ;;
        loongarch64) RUST_TARGET=loongarch64-unknown-linux-musl ;;
        *) error "Unsupported arch for static binary: $ARCH" ;;
    esac
    STATIC_BIN="target/$RUST_TARGET/debug/epkg"
    if ! test -f "$STATIC_BIN"; then
        error "Static binary not found at $STATIC_BIN"
    fi
    if ! test -x "$STATIC_BIN"; then
        error "Static binary $STATIC_BIN is not executable"
    fi
    log "Verifying static binary $STATIC_BIN runs"
    if ! "$STATIC_BIN" --version; then
        error "Static binary $STATIC_BIN --version failed"
    fi
    log "Verifying static binary works (epkg info)"
    if ! "$STATIC_BIN" info bash; then
        error "Static binary $STATIC_BIN info bash failed"
    fi
    log "Static binary $STATIC_BIN works correctly"
}

# Main test - build in current Docker container environment
log "Testing build-from-source in current Docker container environment"
build_epkg
test_built_epkg

log "Testing make.sh static and target/<triple>/debug/epkg"
build_static_binary
test_static_binary

log "Build-from-source test passed in Docker container"
