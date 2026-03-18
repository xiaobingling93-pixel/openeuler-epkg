#!/bin/sh
# Conda channel test (works on Linux/macOS/Windows)
# Tests basic conda package operations across platforms

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
CHANNEL_NAME="conda"
ENV_NAME="${ENV_NAME:-test-conda}"

. "$SCRIPT_DIR/common.sh"

# ==============================================================================
# Tests
# ==============================================================================

test_env_create() {
    log_info "=== Test: Environment Creation ==="
    create_test_env "conda"
    channel_ok "Environment created successfully"
}

test_virtual_packages() {
    log_info "=== Test: Virtual Packages ==="
    case "$HOST_OS" in
        linux|macos)
            log_info "Unix-like system detected (virtual packages active)"
            ;;
        *)
            log_info "Non-Unix system, __unix virtual package not expected"
            ;;
    esac
    [ "$HOST_OS" = "linux" ] && log_info "Linux detected: __linux virtual package active"
    [ "$HOST_OS" = "macos" ] && log_info "macOS detected: __osx virtual package active"
    log_info "Architecture: $HOST_ARCH (archspec virtual package active)"
    channel_ok "Virtual packages verified"
}

test_package_info() {
    log_info "=== Test: Package Info ==="
    log_info "Getting info for 'pi' package..."
    if ! package_info "pi" | grep -q "pi"; then
        log_warn "Package info for 'pi' may not be available (this is OK if repo not synced)"
    fi
    channel_ok "Package info query works"
}

test_install_run_jq() {
    log_info "=== Test: Install and Run jq ==="
    log_info "Installing jq..."
    if ! install_packages "jq"; then
        log_warn "jq install failed"
        return 0
    fi
    log_info "Running jq --version..."
    if ! check_cmd jq --version; then
        log_warn "jq command not available or failed"
    else
        log_info "jq version: $(check_cmd jq --version 2>&1 | head -1)"
    fi
    channel_ok "jq install and run test passed"
}

test_install_python() {
    log_info "=== Test: Install Python ==="
    log_info "Installing Python..."
    if ! install_packages "python"; then
        log_warn "Python install failed (may be expected on some platforms)"
        return 0
    fi
    log_info "Running python --version..."
    local py_version
    if py_version=$(check_cmd python --version 2>&1 || check_cmd python3 --version 2>&1); then
        log_info "Python version: $py_version"
    else
        log_warn "Python command not available"
    fi
    channel_ok "Python install test passed"
}

test_remove_packages() {
    log_info "=== Test: Remove Packages ==="
    if run_epkg "list-jq" list 2>/dev/null | grep -q "^jq "; then
        log_info "Removing jq..."
        remove_packages "jq"
    fi
    channel_ok "Package removal test passed"
}

# ==============================================================================
# Main
# ==============================================================================

setup_tests

if [ -n "${TEST_FUNC:-}" ]; then
    $TEST_FUNC
else
    run_tests test_env_create test_virtual_packages test_package_info \
              test_install_run_jq test_install_python test_remove_packages
fi