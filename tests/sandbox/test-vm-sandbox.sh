#!/bin/sh
# Test epkg run sandbox=vm mode (QEMU VMM)
# Supports debug mode with -d/-dd/-ddd flags.
# - assumes epkg is already installed
# - creates new env for testing
# - leaves the env for human debug

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
. "$PROJECT_ROOT/tests/common.sh"

# Parse command line flags

parse_debug_flags "$@"
case $? in
    0)
        eval set -- "$PARSE_DEBUG_FLAGS_REMAINING"
        ;;
    1)
        exit 1
        ;;
    2)
        echo "Usage: $0 [-d|--debug|-dd|-ddd]"
        echo "Test epkg run sandbox=vm mode (QEMU VMM)"
        exit 0
        ;;
esac

# Check for extra arguments
if [ $# -gt 0 ]; then
    echo "Error: Unexpected arguments: $*" >&2
    echo "Usage: $0 [-d|--debug|-dd|-ddd]" >&2
    exit 1
fi

# Set debug environment variables based on DEBUG_FLAG
case "$DEBUG_FLAG" in
    -ddd)
        export RUST_LOG=trace
        export RUST_BACKTRACE=1
        INTERACTIVE=2
        set -x
        ;;
    -dd)
        export RUST_LOG=debug
        export RUST_BACKTRACE=1
        INTERACTIVE=2
        ;;
    -d)
        export RUST_LOG=debug
        INTERACTIVE=1
        ;;
    *)
        ;;
esac
export INTERACTIVE

set_epkg_bin
set_color_names

log() {
    echo "${GREEN}[TEST]${NC} $*" >&2
}

error() {
    echo "${RED}[ERROR]${NC} $*" >&2
    if [ -n "$INTERACTIVE" ]; then
        echo "" >&2
        echo "=== Debug Mode ===" >&2
        echo "Press Enter to continue (or Ctrl+C to exit)..." >&2
        read dummy || true
    fi
    exit 1
}

skip() {
    echo "${YELLOW}[SKIP]${NC} $*" >&2
    exit 0
}

# Check QEMU log for error patterns and kernel panics
check_qemu_log() {
    local log_file="$HOME/.cache/epkg/vmm-logs/latest-qemu.log"
    if [ ! -f "$log_file" ]; then
        log "QEMU log not found at $log_file (maybe not created yet)"
        return 0
    fi
    log "Checking QEMU log $log_file for errors..."
    # Check for kernel panic indicators
    if grep -w "Kernel panic\|Panic\|Oops\|BUG\|Call Trace" "$log_file"; then
        error "Kernel panic or serious error detected in QEMU log. See $log_file"
    fi
    # Check for other error patterns (warn only)
    if grep "Error:\|failed\|WARN" "$log_file"; then
        echo "${YELLOW}[WARN]${NC} Some errors/warnings found in QEMU log (may be benign). Check $log_file" >&2
    fi
}

# Run command with timeout and exit code checking
run_with_timeout() {
    local timeout=30
    local cmd=("$@")
    log "Running with timeout ${timeout}s: ${cmd[*]}"
    # Run command with timeout, capture exit code
    set +e
    timeout --foreground "$timeout" "${cmd[@]}"
    local exit_code=$?
    set -e
    case $exit_code in
        0)
            log "Command succeeded"
            ;;
        124)
            error "Command timed out after ${timeout}s"
            ;;
        *)
            error "Command failed with exit code $exit_code"
            ;;
    esac
    # Check QEMU log after each command
    check_qemu_log
}

# Run command with timeout and capture output
capture_with_timeout() {
    local timeout=30
    local cmd=("$@")
    log "Running with timeout ${timeout}s (capture output): ${cmd[*]}"
    set +e
    output=$(timeout --foreground "$timeout" "${cmd[@]}" 2>/dev/null)
    local exit_code=$?
    set -e
    case $exit_code in
        0)
            log "Command succeeded"
            ;;
        124)
            error "Command timed out after ${timeout}s"
            ;;
        *)
            error "Command failed with exit code $exit_code"
            ;;
    esac
    check_qemu_log
    echo "$output"
}

# Check dependencies
check_dependency() {
    if ! command -v "$1" >/dev/null 2>&1; then
        skip "$1 not found, skipping VM sandbox test"
    fi
}

check_dependency qemu-system-x86_64
check_dependency timeout
# Check for virtiofsd in PATH or common location
if ! command -v virtiofsd >/dev/null 2>&1 && [ ! -x "/usr/libexec/virtiofsd" ]; then
    skip "virtiofsd not found in PATH or /usr/libexec/virtiofsd, skipping VM sandbox test"
fi

# Check for kernel image
if [ -z "$EPKG_VM_KERNEL" ]; then
    # Try some common locations
    if [ -f "/boot/vmlinuz-$(uname -r)" ]; then
        EPKG_VM_KERNEL="/boot/vmlinuz-$(uname -r)"
    elif [ -f "/boot/vmlinuz" ]; then
        EPKG_VM_KERNEL="/boot/vmlinuz"
    else
        skip "EPKG_VM_KERNEL not set and no kernel found in /boot, skipping VM sandbox test"
    fi
fi

if [ ! -f "$EPKG_VM_KERNEL" ]; then
    skip "Kernel image not found at $EPKG_VM_KERNEL, skipping VM sandbox test"
fi

# Export environment variables for qemu.rs
export EPKG_VM_KERNEL
export EPKG_VM_QEMU="qemu-system-x86_64"
# Try to find virtiofsd
if command -v virtiofsd >/dev/null 2>&1; then
    export EPKG_VM_VIRTIOFSD="virtiofsd"
elif [ -x "/usr/libexec/virtiofsd" ]; then
    export EPKG_VM_VIRTIOFSD="/usr/libexec/virtiofsd"
else
    skip "virtiofsd not found in PATH or /usr/libexec/virtiofsd"
fi

# Optional initrd
if [ -n "$EPKG_VM_INITRD" ]; then
    export EPKG_VM_INITRD
fi

log "Starting VM sandbox test"
log "Using kernel: $EPKG_VM_KERNEL"

ENV_NAME="test-vm-sandbox"

# Remove possible old envs
"$EPKG_BIN" env remove "$ENV_NAME" 2>/dev/null || true

log "Creating test environment $ENV_NAME"
"$EPKG_BIN" env create "$ENV_NAME" -c alpine || error "Failed to create sandbox env"

log "Installing coreutils into $ENV_NAME"
"$EPKG_BIN" -e "$ENV_NAME" --assume-yes install bash coreutils || error "Failed to install coreutils in sandbox env"

# Ensure /etc/passwd has root so whoami prints "root" in the VM (Alpine env may omit it)
ENV_ROOT="${HOME}/.epkg/envs/${ENV_NAME}"
if [ -d "$ENV_ROOT" ] && [ -f "$ENV_ROOT/etc/passwd" ] && ! grep -q "^root:" "$ENV_ROOT/etc/passwd" 2>/dev/null; then
    log "Adding root entry to /etc/passwd for whoami test"
    (printf '%s\n' "root:x:0:0:root:/root:/bin/sh"; cat "$ENV_ROOT/etc/passwd") > "$ENV_ROOT/etc/passwd.new"
    mv "$ENV_ROOT/etc/passwd.new" "$ENV_ROOT/etc/passwd"
fi

# epkg binary dir is auto-mounted into env for VM mode (when epkg is outside env)
log "Running echo test with --isolate=vm"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --no-tty echo "hello from vm")
if [ "$output" != "hello from vm" ]; then
    error "Expected 'hello from vm', got '$output'"
fi

# Manual test:
# % epkg run --isolate=vm --no-tty whoami|xxd
# 00000000: 726f 6f74 0a                             root.
log "Running whoami with --no-tty"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --no-tty whoami)
if [ "$output" != "root" ]; then
    error "Expected whoami output 'root', got '$output'"
fi

log "Running ls / with --isolate=vm"
run_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --no-tty ls /

log "Checking ls / directory layout"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --no-tty ls /)
# Check for expected directories in root filesystem
for dir in bin etc home proc root sys tmp usr var; do
    if ! echo "$output" | grep -q "\b$dir\b"; then
        error "Expected directory '$dir' not found in ls / output"
    fi
done

log "VM sandbox test completed successfully"
check_qemu_log
