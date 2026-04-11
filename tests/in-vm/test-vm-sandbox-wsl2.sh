#!/bin/bash
# Test epkg run VM sandbox modes using Windows native epkg.exe from WSL2
# Usage: test-vm-sandbox-wsl2.sh [--vmm=libkrun] [-d|--debug|-dd|-ddd]
#
# This script runs VM sandbox tests on Windows native epkg.exe from WSL2.
# It copies the binary to a Windows-accessible location and runs tests via cmd.exe.
#
# Principles:
# - Supports debug mode with -d/-dd/-ddd flags
# - Assumes epkg is already installed
# - Creates new env with non-random name for testing
# - Run tests with 'timeout' prefix and '-y|--assume-yes' for automation
# - Leaves the env for human/agent debug (removed at start if exists)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
. "$PROJECT_ROOT/tests/common.sh"

# Configuration
VMM_BACKEND="libkrun"
ENV_NAME="test-vm-sandbox-wsl2"
TEMP_DIR=""
WIN_TEMP_DIR=""
EPKG_WIN_BINARY=""

# Default VMM backend for Windows
VMM_BACKEND=""

# Parse arguments
filtered_args=""
for arg in "$@"; do
    case "$arg" in
        --vmm=*)
            VMM_BACKEND="${arg#--vmm=}"
            ;;
        *)
            filtered_args="$filtered_args $arg"
            ;;
    esac
done

# Parse debug flags
eval set -- $filtered_args
parse_debug_flags "$@"
parse_ret=$?
case $parse_ret in
    0)
        eval set -- "$PARSE_DEBUG_FLAGS_REMAINING"
        ;;
    1)
        exit 1
        ;;
    2)
        echo "Usage: $0 [--vmm=libkrun] [-d|--debug|-dd|-ddd]"
        echo ""
        echo "Test epkg run VM sandbox modes using Windows native epkg.exe from WSL2"
        echo ""
        echo "Options:"
        echo "  --vmm=libkrun  Use libkrun backend (Windows with Hyper-V)"
        echo "  -d, --debug    Interactive debug mode (pause on error)"
        echo "  -dd            Debug logging (RUST_LOG=debug)"
        echo "  -ddd           Trace logging (RUST_LOG=trace)"
        exit 0
        ;;
esac

# Set debug environment variables
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
    -d|--debug)
        export RUST_LOG=debug
        INTERACTIVE=1
        ;;
esac
export INTERACTIVE

set_color_names

# Find Windows native epkg binary
find_windows_epkg_bin() {
    if [ -n "$EPKG_BIN" ] && [ -f "$EPKG_BIN" ]; then
        EPKG_BINARY="$EPKG_BIN"
    elif [ -f "$PROJECT_ROOT/dist/epkg-windows-x86_64.exe" ]; then
        EPKG_BINARY="$PROJECT_ROOT/dist/epkg-windows-x86_64.exe"
    elif [ -f "$PROJECT_ROOT/target/debug/epkg.exe" ]; then
        EPKG_BINARY="$PROJECT_ROOT/target/debug/epkg.exe"
    else
        error "Windows epkg.exe not found. Please build for Windows target or set EPKG_BIN"
    fi
}

find_windows_epkg_bin

# Check we're in WSL
if [ ! -d "/mnt/c" ]; then
    error "This script is designed to run from WSL2 with Windows C: drive mounted at /mnt/c"
fi

# Setup Windows temp directory
setup_windows_env() {
    local pid=$$
    TEMP_DIR="/mnt/c/temp_epkg_vm_test_${pid}"
    WIN_TEMP_DIR="C:\\temp_epkg_vm_test_${pid}"

    echo "Setting up Windows test environment..."
    echo "  Temp: $TEMP_DIR"

    mkdir -p "$TEMP_DIR"

    # Copy epkg binary to temp location
    cp "$EPKG_BINARY" "$TEMP_DIR/epkg.exe"
    EPKG_WIN_BINARY="$TEMP_DIR/epkg.exe"

    # Also need vmlinux kernel
    local kernel_src=""
    if [ -f "$HOME/.epkg/envs/self/boot/vmlinux" ]; then
        kernel_src="$HOME/.epkg/envs/self/boot/vmlinux"
    elif [ -f "/opt/epkg/boot/vmlinux" ]; then
        kernel_src="/opt/epkg/boot/vmlinux"
    elif [ -f "/mnt/c/epkg/boot/vmlinux" ]; then
        kernel_src="/mnt/c/epkg/boot/vmlinux"
    elif [ -f "/mnt/c/Users/$(whoami)/.epkg/envs/self/boot/vmlinux" ]; then
        kernel_src="/mnt/c/Users/$(whoami)/.epkg/envs/self/boot/vmlinux"
    fi

    if [ -n "$kernel_src" ] && [ -f "$kernel_src" ]; then
        mkdir -p "$TEMP_DIR/boot"
        cp "$kernel_src" "$TEMP_DIR/boot/vmlinux"
        echo "  Kernel: $kernel_src -> $TEMP_DIR/boot/vmlinux"
    fi

    export EPKG_VM_KERNEL="C:/temp_epkg_vm_test_${pid}/boot/vmlinux"
}

# Cleanup Windows temp directory
cleanup_windows_env() {
    if [ -n "$TEMP_DIR" ] && [ -d "$TEMP_DIR" ]; then
        echo "Cleaning up temporary files..."
        rm -rf "$TEMP_DIR"
    fi
}

trap cleanup_windows_env EXIT INT HUP

log() {
    printf "%b[TEST]%b %b\n" "$GREEN" "$NC" "$*" >&2
}

error() {
    printf "%b[ERROR]%b %b\n" "$RED" "$NC" "$*" >&2
    if [ -n "$INTERACTIVE" ]; then
        printf "\n=== Debug Mode ===\n" >&2
        printf "Press Enter to continue (or Ctrl+C to exit)...\n" >&2
        read dummy || true
    fi
    exit 1
}

skip() {
    printf "%b[SKIP]%b %b\n" "$YELLOW" "$NC" "$*" >&2
    exit 0
}

# Run command via cmd.exe on Windows epkg.exe
# Converts output from Windows format (CRLF) to Unix format
run_epkg_cmd() {
    local cmd="$*"
    local output
    local exit_code

    # Build the Windows command
    # Note: We need to escape backslashes and quotes for cmd.exe
    local win_cmd="cd /d $WIN_TEMP_DIR && epkg.exe $cmd"

    log "Running: epkg.exe $cmd"

    # Run via cmd.exe and capture output
    output=$(cd /mnt/c && cmd.exe /c "$win_cmd" 2>&1) || exit_code=$?
    exit_code=${exit_code:-0}

    # Convert CRLF to LF and strip carriage returns
    output=$(echo "$output" | tr -d '\r')

    # Return the output
    echo "$output"
    return $exit_code
}

# Run command with timeout
run_with_timeout() {
    local timeout_secs=60
    local cmd="$*"

    log "Running with timeout ${timeout_secs}s: epkg.exe $cmd"

    # Use timeout command on the cmd.exe execution
    local win_cmd="cd /d $WIN_TEMP_DIR && epkg.exe $cmd"
    local output
    local exit_code=0

    output=$(cd /mnt/c && timeout --foreground "$timeout_secs" cmd.exe /c "$win_cmd" 2>&1) || exit_code=$?

    # Convert CRLF to LF
    output=$(echo "$output" | tr -d '\r')

    case $exit_code in
        0)
            log "Command succeeded"
            ;;
        124|142)
            error "Command timed out after ${timeout_secs}s"
            ;;
        *)
            error "Command failed with exit code $exit_code"
            ;;
    esac

    echo "$output"
}

# Capture output with timeout
capture_with_timeout() {
    local timeout_secs=60
    local cmd="$*"

    # Check if first argument is a number (timeout override)
    if [ $# -gt 0 ] && [ "${1##*[!0-9]}" = "$1" ]; then
        timeout_secs="$1"
        shift
        cmd="$*"
    fi

    log "Running with timeout ${timeout_secs}s (capture): epkg.exe $cmd"

    local win_cmd="cd /d $WIN_TEMP_DIR && epkg.exe $cmd"
    local output
    local exit_code=0

    output=$(cd /mnt/c && timeout --foreground "$timeout_secs" cmd.exe /c "$win_cmd" 2>&1) || exit_code=$?

    # Convert CRLF to LF
    output=$(echo "$output" | tr -d '\r')

    case $exit_code in
        0)
            log "Command succeeded"
            ;;
        124|142)
            error "Command timed out after ${timeout_secs}s"
            ;;
        *)
            error "Command failed with exit code $exit_code: $output"
            ;;
    esac

    echo "$output"
}

# Check libkrun requirements for Windows
check_libkrun_requirements() {
    log "Checking libkrun requirements for Windows..."

    # Check if epkg was built with libkrun feature
    local help_output
    help_output=$(run_epkg_cmd "run --help")
    if ! echo "$help_output" | grep -q "isolate"; then
        skip "epkg run --isolate not available (libkrun feature not enabled)"
    fi

    # Check for kernel
    check_kernel_for_libkrun
}

# Check for kernel suitable for libkrun
check_kernel_for_libkrun() {
    # Check for default kernel locations
    local default_kernel="$HOME/.epkg/envs/self/boot/vmlinux"

    if [ -n "$EPKG_VM_KERNEL" ]; then
        if [ ! -f "$EPKG_VM_KERNEL" ]; then
            skip "EPKG_VM_KERNEL set but file not found: $EPKG_VM_KERNEL"
        fi
        log "Using kernel from EPKG_VM_KERNEL: $EPKG_VM_KERNEL"
        export EPKG_VM_KERNEL
        return 0
    fi

    if [ -f "$default_kernel" ]; then
        export EPKG_VM_KERNEL="C:/temp_epkg_vm_test_$$/boot/vmlinux"
        log "Using kernel: $default_kernel (copied to Windows temp)"
        return 0
    fi

    # Check Windows path (via WSL mount)
    if [ -f "/mnt/c/epkg/boot/vmlinux" ]; then
        export EPKG_VM_KERNEL="C:/epkg/boot/vmlinux"
        log "Using kernel: C:/epkg/boot/vmlinux"
        return 0
    fi

    skip "No kernel found. Run 'epkg.exe self install' on Windows or set EPKG_VM_KERNEL"
}

# Auto-select VMM backend (Windows only supports libkrun)
auto_select_vmm() {
    echo "libkrun"
}

# Select VMM backend
if [ -z "$VMM_BACKEND" ]; then
    VMM_BACKEND=$(auto_select_vmm)
fi

log "Testing with VMM backend: $VMM_BACKEND"
log "Platform: Windows (via WSL2)"

# Setup Windows environment
setup_windows_env

# Check requirements
case "$VMM_BACKEND" in
    libkrun)
        check_libkrun_requirements
        ;;
    *)
        error "Unknown VMM backend: $VMM_BACKEND. Windows only supports 'libkrun'"
        ;;
esac

# Build isolate options
ISOLATE_OPTS="--isolate=vm --vmm=$VMM_BACKEND"

log "Isolate options: $ISOLATE_OPTS"

# Remove possible old envs
run_epkg_cmd "env remove $ENV_NAME" >/dev/null 2>&1 || true

log "Creating test environment $ENV_NAME"
output=$(capture_with_timeout env create "$ENV_NAME" -c alpine)
if ! echo "$output" | grep -qi "created\|success"; then
    # Check if it was actually created despite output
    if ! run_epkg_cmd "env list" | grep -q "$ENV_NAME"; then
        error "Failed to create sandbox env: $output"
    fi
fi

log "Installing bash coreutils into $ENV_NAME"
output=$(capture_with_timeout 120 -e "$ENV_NAME" --assume-yes install bash coreutils) || {
    error "Failed to install coreutils in sandbox env: $output"
}

# Ensure /etc/passwd has root so whoami prints "root" in the VM
ENV_ROOT_WIN="C:\\Users\\$(whoami)\\.epkg\\envs\\$ENV_NAME"
ENV_ROOT="/mnt/c/Users/$(whoami)/.epkg/envs/$ENV_NAME"

# ============================================
# Test Suite: VM Sandbox (WSL2 + Windows)
# ============================================

PASS_COUNT=0
FAIL_COUNT=0

# Helper to mark test passed
test_passed() {
    log "Test $1: PASSED"
    PASS_COUNT=$((PASS_COUNT + 1))
}

# Helper to mark test failed
test_failed() {
    log "Test $1: FAILED - $2"
    FAIL_COUNT=$((FAIL_COUNT + 1))
    if [ -n "$INTERACTIVE" ]; then
        printf "\n=== Debug Mode ===\n" >&2
        printf "Press Enter to continue (or Ctrl+C to exit)...\n" >&2
        read dummy || true
    fi
}

# Test 1: Basic echo command
log "Test 1: Running echo test"
output=$(capture_with_timeout -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch echo "hello from vm")
output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
if [ "$output" = "hello from vm" ]; then
    test_passed 1
else
    test_failed 1 "Expected 'hello from vm', got '$output'"
fi

# Test 2: whoami command (check user context)
log "Test 2: Running whoami"
output=$(capture_with_timeout -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch whoami)
output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
if [ "$output" = "root" ]; then
    test_passed 2
else
    test_failed 2 "Expected whoami output 'root', got '$output'"
fi

# Test 3: File I/O - create and read file
log "Test 3: Testing file I/O (create/read file)"
output=$(capture_with_timeout -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch sh -c 'echo "test content" > /tmp/testfile && cat /tmp/testfile')
output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
if [ "$output" = "test content" ]; then
    test_passed 3
else
    test_failed 3 "Expected 'test content', got '$output'"
fi

# Test 4: Symlink handling (critical for epkg!)
log "Test 4: Testing symlink creation and resolution"
output=$(capture_with_timeout -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch sh -c 'echo data > /tmp/sf && ln -sf /tmp/sf /tmp/sl && readlink /tmp/sl')
output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
if [ "$output" = "/tmp/sf" ]; then
    test_passed 4
else
    test_failed 4 "Expected '/tmp/sf', got '$output'"
fi

# Test 5: Exit code propagation
log "Test 5: Testing exit code propagation"
run_epkg_cmd "-e $ENV_NAME run $ISOLATE_OPTS --io=batch sh -c 'exit 42'" >/dev/null 2>&1
exit_code=$?
if [ "$exit_code" = "42" ]; then
    test_passed 5
else
    test_failed 5 "Expected exit code 42, got $exit_code"
fi

# Test 6: uname -a (check kernel info)
log "Test 6: Running uname -a"
output=$(capture_with_timeout -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch uname -a)
if echo "$output" | grep -q "Linux"; then
    test_passed 6
else
    test_failed 6 "Expected 'Linux' in uname output, got '$output'"
fi

# Test 7: Environment variable passing
log "Test 7: Testing environment variable passing"
output=$(capture_with_timeout -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch env)
if echo "$output" | grep -q "PATH="; then
    test_passed 7
else
    test_failed 7 "Expected PATH in environment, got '$output'"
fi

# Test 8: Working directory (inherits from host)
log "Test 8: Testing working directory (pwd)"
output=$(capture_with_timeout -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch pwd)
output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
if [ -n "$output" ]; then
    test_passed 8
    log "Test 8: pwd=$output"
else
    test_failed 8 "pwd returned empty"
fi

# Test 9: Shell script execution
log "Test 9: Testing shell script execution"
output=$(capture_with_timeout -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch sh -c 'for i in 1 2 3; do echo "iter$i"; done')
if echo "$output" | grep -q "iter1" && echo "$output" | grep -q "iter3"; then
    test_passed 9
else
    test_failed 9 "Expected iteration output, got '$output'"
fi

# Test 10: Process signals
log "Test 10: Testing process signal handling"
output=$(capture_with_timeout -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch sh -c 'trap "echo caught" SIGTERM; kill -TERM \$\$; echo done')
if echo "$output" | grep -q "caught"; then
    test_passed 10
else
    test_failed 10 "Expected 'caught' in output, got '$output'"
fi

# Test 11: Large output handling (small)
log "Test 11: Testing large output handling (100 lines)"
output=$(capture_with_timeout -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch sh -c 'seq 1 100')
line_count=$(echo "$output" | grep -v '^\[' | grep -v '^$' | wc -l | tr -d ' ')
if [ "$line_count" = "100" ]; then
    test_passed 11
else
    test_failed 11 "Expected 100 lines, got $line_count"
fi

# Test 11b: Large output handling (batch mode, 10000 lines - reduced for Windows)
log "Test 11b: Testing large output handling (batch mode, 10000 lines)"
output=$(capture_with_timeout 120 -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch sh -c 'seq 10000')
line_count=$(echo "$output" | grep -v '^\[' | grep -v '^$' | wc -l | tr -d ' ')
if [ "$line_count" = "10000" ]; then
    test_passed 11b
else
    test_failed 11b "Expected 10000 lines, got $line_count"
fi

# Test 12: Check /proc filesystem
log "Test 12: Checking /proc filesystem in VM"
output=$(capture_with_timeout -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch ls /proc)
found=0
for entry in self cpuinfo meminfo; do
    if echo "$output" | grep -q "$entry"; then
        found=$((found + 1))
    fi
done
if [ $found -ge 2 ]; then
    test_passed 12
else
    test_failed 12 "Expected entries in /proc, got '$output'"
fi

# Test 13: Check memory info
log "Test 13: Checking /proc/meminfo"
output=$(capture_with_timeout -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch cat /proc/meminfo)
if echo "$output" | grep -q "MemTotal"; then
    test_passed 13
else
    test_failed 13 "Expected 'MemTotal' in /proc/meminfo"
fi

# Test 14: Directory creation and listing
log "Test 14: Testing directory operations"
output=$(capture_with_timeout -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch sh -c 'mkdir -p /tmp/a/b/c && ls /tmp/a/b')
if echo "$output" | grep -q "c"; then
    test_passed 14
else
    test_failed 14 "Expected 'c' in directory listing, got '$output'"
fi

# Test 15: Binary execution (coreutils)
log "Test 15: Testing binary execution with coreutils"
output=$(capture_with_timeout -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch /usr/bin/env echo "binary works")
output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
if [ "$output" = "binary works" ]; then
    test_passed 15
else
    test_failed 15 "Expected 'binary works', got '$output'"
fi

# Test 16: stdin handling
log "Test 16: Testing stdin handling"
output=$(capture_with_timeout -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch sh -c 'cat <<EOF
multiline
input
EOF')
if echo "$output" | grep -q "multiline"; then
    test_passed 16
else
    test_failed 16 "Expected 'multiline' in output, got '$output'"
fi

# Test 17: Host filesystem isolation (verify /sys is virtualized)
log "Test 17: Verifying /sys virtualization"
output=$(capture_with_timeout -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch ls /sys)
if echo "$output" | grep -q "class"; then
    test_passed 17
else
    test_failed 17 "Expected 'class' in /sys"
fi

# ============================================
# Test Suite: VM Lifecycle Subcommands
# ============================================

# Note: VM session detection on Windows is different from Unix
# We'll use simpler checks based on command success/failure

# Test VM-1: vm start basic
log "Test VM-1: vm start basic functionality"
run_epkg_cmd "vm stop $ENV_NAME" >/dev/null 2>&1 || true
output=$(capture_with_timeout vm start "$ENV_NAME")
if echo "$output" | grep -qi "started\|running"; then
    test_passed VM-1
else
    # Check if vm list shows it
    if run_epkg_cmd "vm list" | grep -q "$ENV_NAME"; then
        test_passed VM-1
    else
        test_failed VM-1 "VM session not active after vm start: $output"
    fi
fi

# Test VM-2: vm list shows running VM
log "Test VM-2: vm list shows running VM"
output=$(capture_with_timeout vm list)
if echo "$output" | grep -q "$ENV_NAME"; then
    test_passed VM-2
else
    test_failed VM-2 "vm list should show $ENV_NAME, got: $output"
fi

# Test VM-3: vm status shows YAML output
log "Test VM-3: vm status shows YAML output"
output=$(capture_with_timeout vm status "$ENV_NAME")
found_fields=0
for field in "env_name:" "env_root:" "daemon_pid:" "socket_path:" "backend:"; do
    if echo "$output" | grep -q "$field"; then
        found_fields=$((found_fields + 1))
    fi
done
if [ $found_fields -ge 3 ]; then
    test_passed VM-3
else
    test_failed VM-3 "YAML output missing expected fields, got: $output"
fi

# Test VM-4: VM reuse - run command in existing VM
log "Test VM-4: VM reuse - run command in existing VM session"
output=$(capture_with_timeout -e "$ENV_NAME" run --isolate=vm --io=batch echo "reuse test")
output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
if [ "$output" = "reuse test" ]; then
    test_passed VM-4
else
    test_failed VM-4 "Expected 'reuse test', got '$output'"
fi

# Test VM-5: vm start rejects duplicate
log "Test VM-5: vm start rejects duplicate session"
output=$(run_epkg_cmd "vm start $ENV_NAME" 2>&1) || exit_code=$?
exit_code=${exit_code:-0}
if [ "$exit_code" -ne 0 ] || echo "$output" | grep -qi "already running"; then
    test_passed VM-5
else
    test_failed VM-5 "vm start should fail when VM already running, got: $output"
fi

# Test VM-6: vm stop
log "Test VM-6: vm stop functionality"
output=$(capture_with_timeout vm stop "$ENV_NAME")
if echo "$output" | grep -qi "stopped\|done"; then
    test_passed VM-6
else
    # Check if vm list no longer shows it
    sleep 2
    if ! run_epkg_cmd "vm list" | grep -q "$ENV_NAME"; then
        test_passed VM-6
    else
        test_failed VM-6 "VM should be stopped"
    fi
fi

# Test VM-7: vm stop on non-existent VM fails
log "Test VM-7: vm stop on non-existent VM fails"
output=$(run_epkg_cmd "vm stop $ENV_NAME" 2>&1) || exit_code=$?
exit_code=${exit_code:-0}
if [ "$exit_code" -ne 0 ] || echo "$output" | grep -qi "no vm\|not found\|not running"; then
    test_passed VM-7
else
    test_failed VM-7 "vm stop should fail when no VM running, got: $output"
fi

# Test VM-8: vm status on non-existent VM fails
log "Test VM-8: vm status on non-existent VM fails"
output=$(run_epkg_cmd "vm status $ENV_NAME" 2>&1) || exit_code=$?
exit_code=${exit_code:-0}
if [ "$exit_code" -ne 0 ] || echo "$output" | grep -qi "no vm\|not found\|not running"; then
    test_passed VM-8
else
    test_failed VM-8 "vm status should fail when no VM running, got: $output"
fi

# Test VM-9: vm start with parameters
log "Test VM-9: vm start with custom parameters"
output=$(capture_with_timeout vm start "$ENV_NAME" -s cpus=2 -s memory=1024)
if echo "$output" | grep -qi "started\|running"; then
    # Verify parameters in vm status
    output=$(capture_with_timeout vm status "$ENV_NAME")
    if echo "$output" | grep -q "cpus:" && echo "$output" | grep -q "memory_mib:"; then
        test_passed VM-9
    else
        test_failed VM-9 "Expected cpus and memory in status, got: $output"
    fi
else
    test_failed VM-9 "VM session not active after vm start: $output"
fi

# Test VM-10: whoami in VM
log "Test VM-10: whoami in VM session"
output=$(capture_with_timeout -e "$ENV_NAME" run --isolate=vm --io=batch whoami)
output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
if [ "$output" = "root" ]; then
    test_passed VM-10
else
    test_failed VM-10 "whoami should return 'root', got '$output'"
fi

# Cleanup VM session
log "Stopping VM session"
run_epkg_cmd "vm stop $ENV_NAME" >/dev/null 2>&1 || true

# Remove test environment
log "Removing test environment $ENV_NAME"
run_epkg_cmd "env remove $ENV_NAME" >/dev/null 2>&1 || true

# ============================================
# Summary
# ============================================
log "============================================"
log "VM sandbox tests completed!"
log "VMM backend: $VMM_BACKEND"
log "Platform: Windows (via WSL2)"
log "============================================"
log "Results: $PASS_COUNT passed, $FAIL_COUNT failed"

if [ $FAIL_COUNT -eq 0 ]; then
    log "All tests PASSED!"
    exit 0
else
    log "Some tests FAILED!"
    exit 1
fi
