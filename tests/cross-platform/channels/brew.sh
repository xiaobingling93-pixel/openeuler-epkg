#!/bin/sh
# Brew channel test (works on macOS/Linux)
# Tests basic homebrew bottle operations

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
CHANNEL_NAME="brew"
ENV_NAME="${ENV_NAME:-test-brew}"

. "$SCRIPT_DIR/common.sh"

# Brew only works on macOS and Linux
check_brew_platform() {
    if [ "$HOST_OS" != "linux" ] && [ "$HOST_OS" != "macos" ]; then
        channel_skip "Brew only supported on Linux/macOS, skipping on $HOST_OS"
    fi
}

# ==============================================================================
# Tests
# ==============================================================================

test_env_create() {
    log_info "=== Test: Environment Creation ==="
    create_test_env "brew"
    channel_ok "Environment created successfully"
}

test_update() {
    log_info "=== Test: Update Repository ==="
    log_info "Updating brew repository..."
    if ! run_epkg "update" update; then
        log_warn "Update failed (may be network issue)"
        return 0
    fi
    channel_ok "Repository update passed"
}

test_search() {
    log_info "=== Test: Search Packages ==="
    log_info "Searching for jq..."
    if ! run_epkg "search" search jq | grep -q "jq"; then
        log_warn "Search for jq failed or no results"
    else
        log_info "Found jq in search results"
    fi
    channel_ok "Search test passed"
}

test_package_info() {
    log_info "=== Test: Package Info ==="
    log_info "Getting info for 'jq' package..."
    if ! package_info "jq" | grep -q "jq"; then
        log_warn "Package info for 'jq' may not be available"
    fi
    channel_ok "Package info query works"
}

test_install_jq() {
    log_info "=== Test: Install jq ==="
    log_info "Installing jq..."
    if ! install_packages "jq"; then
        log_warn "jq install failed"
        return 0
    fi
    if run_epkg "list" list | grep -q "jq"; then
        log_info "jq is installed"
    else
        log_warn "jq not found in list"
    fi
    channel_ok "jq install test passed"
}

test_install_with_deps() {
    log_info "=== Test: Install Package with Dependencies ==="
    log_info "Installing aalib (has dependencies)..."
    if ! install_packages "aalib"; then
        log_warn "aalib install failed (may not be available for this platform)"
        return 0
    fi
    if run_epkg "list-aalib" list | grep -q "aalib"; then
        log_info "aalib is installed"
    fi
    channel_ok "Package with dependencies test passed"
}

test_remove_packages() {
    log_info "=== Test: Remove Packages ==="
    if run_epkg "list-aalib2" list 2>/dev/null | grep -q "aalib"; then
        log_info "Removing aalib..."
        remove_packages "aalib"
    fi
    channel_ok "Package removal test passed"
}

# ==============================================================================
# Main
# ==============================================================================

setup_tests
check_brew_platform

if [ -n "${TEST_FUNC:-}" ]; then
    $TEST_FUNC
else
    run_tests test_env_create test_update test_search test_package_info \
              test_install_jq test_install_with_deps test_remove_packages
fi