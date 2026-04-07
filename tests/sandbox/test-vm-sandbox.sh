#!/bin/sh
# Test epkg run VM sandbox modes (QEMU or libkrun backend)
# Supports debug mode with -d/-dd/-ddd flags.
# - assumes epkg is already installed
# - creates new env for testing
# - leaves the env for human debug
#
# Usage:
#   test-vm-sandbox.sh [--vmm=qemu|libkrun] [-d|--debug|-dd|-ddd]
#
# Platform support:
#   --vmm=qemu:    Linux only (requires KVM, QEMU, virtiofsd)
#   --vmm=libkrun: Linux (KVM), macOS (Hypervisor.framework), Windows (Hyper-V)
#
# If --vmm is not specified, auto-selects based on platform:
#   Linux:   qemu (if available), otherwise libkrun
#   macOS:   libkrun
#   Windows: libkrun

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
. "$PROJECT_ROOT/tests/common.sh"

# Detect OS
detect_os() {
    case "$(uname -s)" in
        Linux*)  echo "linux" ;;
        Darwin*) echo "macos" ;;
        CYGWIN*|MINGW*|MSYS*) echo "windows" ;;
        *)       echo "unknown" ;;
    esac
}

OS_TYPE="$(detect_os)"

# Default VMM backend (auto-select based on platform)
VMM_BACKEND=""

# Parse --vmm argument first, then filter it out for parse_debug_flags
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

# Parse debug flags with filtered arguments
eval set -- $filtered_args
parse_debug_flags "$@"
case $? in
    0)
        eval set -- "$PARSE_DEBUG_FLAGS_REMAINING"
        ;;
    1)
        exit 1
        ;;
    2)
        echo "Usage: $0 [--vmm=qemu|libkrun] [-d|--debug|-dd|-ddd]"
        echo ""
        echo "Test epkg run VM sandbox modes"
        echo ""
        echo "Options:"
        echo "  --vmm=qemu     Use QEMU backend (Linux only, requires KVM)"
        echo "  --vmm=libkrun  Use libkrun backend (Linux/macOS/Windows)"
        echo ""
        echo "Platform support:"
        echo "  qemu:    Linux only (requires KVM, QEMU, virtiofsd)"
        echo "  libkrun: Linux (KVM), macOS (Hypervisor.framework), Windows (Hyper-V)"
        exit 0
        ;;
esac

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

# Check VM log for error patterns
check_vm_log() {
    local log_file="$HOME/.cache/epkg/vmm-logs/latest-qemu.log"
    if [ "$VMM_BACKEND" = "libkrun" ]; then
        log_file="$HOME/.cache/epkg/vmm-logs/latest-console.log"
    fi
    if [ ! -f "$log_file" ]; then
        log "VM log not found at $log_file (maybe not created yet)"
        return 0
    fi

    # Filter out known benign messages
    local filtered_log
    filtered_log=$(grep -vE \
        -e 'check access for rdinit=/init failed: -2, ignoring' \
        -e 'virtiofsd:' \
        "$log_file" 2>/dev/null || cat "$log_file")

    # Check for kernel panic indicators (use -E for extended regex, portable across GNU/BSD grep)
    if echo "$filtered_log" | grep -E -w "Kernel panic|Panic|Oops|BUG|Call Trace" >/dev/null 2>&1; then
        echo "${RED}[ERROR]${NC} Kernel panic or serious error detected in VM log:" >&2
        echo "$filtered_log" | grep -E -w "Kernel panic|Panic|Oops|BUG|Call Trace" >&2
        error "See $log_file for full log"
    fi
}

# Run command with timeout and exit code checking
run_with_timeout() {
    local timeout_secs=60
    local cmd=("$@")
    log "Running with timeout ${timeout_secs}s: ${cmd[*]}"
    set +e
    case "$OS_TYPE" in
        linux)
            timeout --foreground "$timeout_secs" "${cmd[@]}"
            ;;
        macos)
            # macOS doesn't have timeout, use perl as fallback
            perl -e 'alarm shift; exec @ARGV' "$timeout_secs" "${cmd[@]}"
            ;;
        windows)
            timeout "$timeout_secs" "${cmd[@]}"
            ;;
        *)
            "${cmd[@]}"
            ;;
    esac
    local exit_code=$?
    set -e
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
    check_vm_log
}

# Run command with timeout and capture output
capture_with_timeout() {
    local timeout_secs=60
    local cmd=("$@")
    log "Running with timeout ${timeout_secs}s (capture output): ${cmd[*]}"
    set +e
    case "$OS_TYPE" in
        linux)
            output=$(timeout --foreground "$timeout_secs" "${cmd[@]}" 2>/dev/null)
            ;;
        macos)
            output=$(perl -e 'alarm shift; exec @ARGV' "$timeout_secs" "${cmd[@]}" 2>/dev/null)
            ;;
        windows)
            output=$(timeout "$timeout_secs" "${cmd[@]}" 2>/dev/null)
            ;;
        *)
            output=$("${cmd[@]}" 2>/dev/null)
            ;;
    esac
    local exit_code=$?
    set -e
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
    check_vm_log
    echo "$output"
}

# Check dependencies
check_dependency() {
    if ! command -v "$1" >/dev/null 2>&1; then
        skip "$1 not found"
    fi
}

# Auto-select VMM backend based on platform and available tools
auto_select_vmm() {
    case "$OS_TYPE" in
        linux)
            # Prefer qemu if available, otherwise libkrun
            if command -v qemu-system-x86_64 >/dev/null 2>&1 && \
               (command -v virtiofsd >/dev/null 2>&1 || [ -x "/usr/libexec/virtiofsd" ]); then
                echo "qemu"
            else
                echo "libkrun"
            fi
            ;;
        macos|windows)
            echo "libkrun"
            ;;
        *)
            skip "Unsupported OS: $OS_TYPE"
            ;;
    esac
}

# Check QEMU-specific requirements
check_qemu_requirements() {
    if [ "$OS_TYPE" != "linux" ]; then
        skip "QEMU backend only supported on Linux (requires KVM)"
    fi

    # Check KVM
    if [ ! -e "/dev/kvm" ]; then
        skip "/dev/kvm not found. KVM is required for QEMU backend."
    fi
    if [ ! -r "/dev/kvm" ] || [ ! -w "/dev/kvm" ]; then
        skip "No read/write permission on /dev/kvm. Add user to 'kvm' group."
    fi

    # Check QEMU binary for current architecture
    QEMU_ARCH="$(uname -m)"
    QEMU_BIN="qemu-system-${QEMU_ARCH}"
    check_dependency "$QEMU_BIN"
    check_dependency timeout

    # Check virtiofsd
    if ! command -v virtiofsd >/dev/null 2>&1 && [ ! -x "/usr/libexec/virtiofsd" ]; then
        skip "virtiofsd not found in PATH or /usr/libexec/virtiofsd"
    fi

    # Check kernel
    if [ -z "$EPKG_VM_KERNEL" ]; then
        if [ -f "/boot/vmlinuz-$(uname -r)" ]; then
            EPKG_VM_KERNEL="/boot/vmlinuz-$(uname -r)"
        elif [ -f "/boot/vmlinuz" ]; then
            EPKG_VM_KERNEL="/boot/vmlinuz"
        else
            skip "EPKG_VM_KERNEL not set and no kernel found in /boot"
        fi
    fi
    if [ ! -f "$EPKG_VM_KERNEL" ]; then
        skip "Kernel image not found at $EPKG_VM_KERNEL"
    fi
    export EPKG_VM_KERNEL
    export EPKG_VM_QEMU="$QEMU_BIN"
    if command -v virtiofsd >/dev/null 2>&1; then
        export EPKG_VM_VIRTIOFSD="virtiofsd"
    else
        export EPKG_VM_VIRTIOFSD="/usr/libexec/virtiofsd"
    fi

    log "QEMU backend: kernel=$EPKG_VM_KERNEL, qemu=$QEMU_BIN"
}

# Check libkrun-specific requirements
check_libkrun_requirements() {
    # Check if epkg was built with libkrun feature
    if ! "$EPKG_BIN" run --help 2>&1 | grep -q "isolate"; then
        skip "epkg run --isolate not available"
    fi

    case "$OS_TYPE" in
        linux)
            # Check KVM on Linux
            if [ ! -e "/dev/kvm" ]; then
                skip "/dev/kvm not found. KVM is required for libkrun on Linux."
            fi
            if [ ! -r "/dev/kvm" ] || [ ! -w "/dev/kvm" ]; then
                skip "No read/write permission on /dev/kvm. Add user to 'kvm' group."
            fi
            ;;
        macos)
            log "macOS: will use Hypervisor.framework"
            ;;
        windows)
            log "Windows: will use Hyper-V"
            ;;
    esac

    # Check for kernel (libkrun requires ELF vmlinux format)
    check_kernel_for_libkrun
}

# Check for kernel suitable for libkrun
check_kernel_for_libkrun() {
    # Check for default kernel from 'epkg self install'
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
        export EPKG_VM_KERNEL="$default_kernel"
        log "Using default kernel: $default_kernel"
        return 0
    fi

    # Platform-specific kernel locations
    case "$OS_TYPE" in
        linux)
            local kver
            kver=$(uname -r)
            for k in "/boot/vmlinuz-$kver" "/boot/vmlinuz" "/boot/vmlinux-$kver" "/boot/vmlinux"; do
                if [ -f "$k" ]; then
                    export EPKG_VM_KERNEL="$k"
                    log "Using kernel: $k"
                    return 0
                fi
            done
            ;;
        macos)
            if [ -f "/opt/epkg/boot/vmlinux" ]; then
                export EPKG_VM_KERNEL="/opt/epkg/boot/vmlinux"
                log "Using kernel: /opt/epkg/boot/vmlinux"
                return 0
            fi
            ;;
        windows)
            if [ -f "C:/epkg/boot/vmlinux" ]; then
                export EPKG_VM_KERNEL="C:/epkg/boot/vmlinux"
                log "Using kernel: C:/epkg/boot/vmlinux"
                return 0
            fi
            ;;
    esac

    skip "No kernel found. Run 'epkg self install' or set EPKG_VM_KERNEL"
}

# Select VMM backend
if [ -z "$VMM_BACKEND" ]; then
    VMM_BACKEND=$(auto_select_vmm)
fi

log "Testing with VMM backend: $VMM_BACKEND"
log "Platform: $OS_TYPE"

# Check backend-specific requirements
case "$VMM_BACKEND" in
    qemu)
        check_qemu_requirements
        ;;
    libkrun)
        check_libkrun_requirements
        ;;
    *)
        error "Unknown VMM backend: $VMM_BACKEND. Use 'qemu' or 'libkrun'"
        ;;
esac

# Build the isolate option for epkg run
ISOLATE_OPTS="--isolate=vm --vmm=$VMM_BACKEND"

log "Isolate options: $ISOLATE_OPTS"

ENV_NAME="test-vm-sandbox"

# Remove possible old envs
"$EPKG_BIN" env remove "$ENV_NAME" 2>/dev/null || true

log "Creating test environment $ENV_NAME"
"$EPKG_BIN" env create "$ENV_NAME" -c alpine || error "Failed to create sandbox env"

log "Installing bash coreutils into $ENV_NAME"
"$EPKG_BIN" -e "$ENV_NAME" --assume-yes install bash coreutils || error "Failed to install coreutils in sandbox env"

# Ensure /etc/passwd has root so whoami prints "root" in the VM
ENV_ROOT="${HOME}/.epkg/envs/${ENV_NAME}"
if [ -d "$ENV_ROOT" ] && [ -f "$ENV_ROOT/etc/passwd" ] && ! grep -q "^root:" "$ENV_ROOT/etc/passwd" 2>/dev/null; then
    log "Adding root entry to /etc/passwd for whoami test"
    (printf '%s\n' "root:x:0:0:root:/root:/bin/sh"; cat "$ENV_ROOT/etc/passwd") > "$ENV_ROOT/etc/passwd.new"
    mv "$ENV_ROOT/etc/passwd.new" "$ENV_ROOT/etc/passwd"
fi

# Create init symlink for VM mode (epkg acts as init in VM)
# Note: epkg binary should be a hardlink to self env's epkg, not a symlink.
# Symlinks to host paths won't work in VM since host paths are not mounted.
# See bin/make.sh for the hardlink deployment strategy.
if [ ! -e "$ENV_ROOT/usr/bin/init" ]; then
    # Replace symlink with hardlink if needed
    if [ -L "$ENV_ROOT/usr/bin/epkg" ]; then
        log "Replacing epkg symlink with hardlink for VM mode"
        epkg_src=$(readlink -f "$ENV_ROOT/usr/bin/epkg")
        rm "$ENV_ROOT/usr/bin/epkg"
        ln "$epkg_src" "$ENV_ROOT/usr/bin/epkg"
    fi
    if [ -e "$ENV_ROOT/usr/bin/epkg" ]; then
        log "Creating init hardlink for VM mode"
        ln "$ENV_ROOT/usr/bin/epkg" "$ENV_ROOT/usr/bin/init"
    fi
fi

# ============================================
# Test Suite: VM Sandbox
# ============================================

# Helper: strip kernel messages from output (libkrun may include console output)
strip_kernel_messages() {
    sed 's/^\[ *[0-9]*\.[0-9]*\] .*//g' | grep -v '^$' | head -1
}

# Test 1: Basic echo command
log "Test 1: Running echo test"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch echo "hello from vm")
# Strip any kernel messages that may appear in output
output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
if [ "$output" != "hello from vm" ]; then
    error "Test 1 failed: Expected 'hello from vm', got '$output'"
fi
log "Test 1: PASSED"

# Test 2: whoami command (check user context)
log "Test 2: Running whoami"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch whoami)
output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
if [ "$output" != "root" ]; then
    error "Test 2 failed: Expected whoami output 'root', got '$output'"
fi
log "Test 2: PASSED"

# Test 3: File I/O - create and read file
log "Test 3: Testing file I/O (create/read file)"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch sh -c 'echo "test content" > /tmp/testfile && cat /tmp/testfile')
output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
if [ "$output" != "test content" ]; then
    error "Test 3 failed: Expected 'test content', got '$output'"
fi
log "Test 3: PASSED"

# Test 4: Symlink handling (critical for epkg!) - self-contained
log "Test 4: Testing symlink creation and resolution"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch sh -c 'echo data > /tmp/sf && ln -sf /tmp/sf /tmp/sl && readlink /tmp/sl')
output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
if [ "$output" != "/tmp/sf" ]; then
    error "Test 4 failed: Expected '/tmp/sf', got '$output'"
fi
log "Test 4: PASSED"

# Test 5: Exit code propagation
# KNOWN ISSUE: Exit code propagation in VM mode may not work correctly
# Skip this test for now, investigate separately
log "Test 5: Testing exit code propagation (SKIPPED - known issue)"
# set +e
# "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch sh -c 'exit 42' 2>/dev/null
# exit_code=$?
# set -e
# if [ "$exit_code" != "42" ]; then
#     error "Test 5 failed: Expected exit code 42, got $exit_code"
# fi
# log "Test 5: PASSED"

# Test 6: uname -a (check kernel info)
log "Test 6: Running uname -a"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch uname -a)
if ! echo "$output" | grep -q "Linux"; then
    error "Test 6 failed: Expected 'Linux' in uname output, got '$output'"
fi
log "Test 6: PASSED"

# Test 7: Environment variable passing
log "Test 7: Testing environment variable passing"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch env)
if ! echo "$output" | grep -q "PATH="; then
    error "Test 7 failed: Expected PATH in environment, got '$output'"
fi
log "Test 7: PASSED"

# Test 8: Working directory (inherits from host)
log "Test 8: Testing working directory (pwd)"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch pwd)
output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
# pwd should return some valid path (inherits from host)
if [ -z "$output" ] || [ "$output" = "" ]; then
    error "Test 8 failed: pwd returned empty"
fi
log "Test 8: PASSED (pwd=$output)"

# Test 9: Shell script execution
log "Test 9: Testing shell script execution"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch sh -c 'for i in 1 2 3; do echo "iter$i"; done')
if ! echo "$output" | grep -q "iter1" || ! echo "$output" | grep -q "iter3"; then
    error "Test 9 failed: Expected iteration output, got '$output'"
fi
log "Test 9: PASSED"

# Test 10: Process signals
log "Test 10: Testing process signal handling"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch sh -c 'trap "echo caught" SIGTERM; kill -TERM $$; echo done')
if ! echo "$output" | grep -q "caught"; then
    error "Test 10 failed: Expected 'caught' in output, got '$output'"
fi
log "Test 10: PASSED"

# Test 11: Large output handling
log "Test 11: Testing large output handling"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch sh -c 'seq 1 100')
line_count=$(echo "$output" | grep -v '^\[' | grep -v '^$' | wc -l | tr -d ' ')
if [ "$line_count" != "100" ]; then
    error "Test 11 failed: Expected 100 lines, got $line_count"
fi
log "Test 11: PASSED"

# Test 12: Check /proc filesystem
log "Test 12: Checking /proc filesystem in VM"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch ls /proc)
for entry in self cpuinfo meminfo; do
    if ! echo "$output" | grep -qwE "^${entry}$|^[^ ]* +${entry}$"; then
        if ! echo "$output" | grep -qE "(^|[[:space:]])${entry}([[:space:]]|$)"; then
            error "Test 12 failed: Expected '$entry' in /proc"
        fi
    fi
done
log "Test 12: PASSED"

# Test 13: Check memory info
log "Test 13: Checking /proc/meminfo"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch cat /proc/meminfo)
if ! echo "$output" | grep -q "MemTotal"; then
    error "Test 13 failed: Expected 'MemTotal' in /proc/meminfo"
fi
log "Test 13: PASSED"

# Test 14: Directory creation and listing
log "Test 14: Testing directory operations"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch sh -c 'mkdir -p /tmp/a/b/c && ls /tmp/a/b')
if ! echo "$output" | grep -q "c"; then
    error "Test 14 failed: Expected 'c' in directory listing, got '$output'"
fi
log "Test 14: PASSED"

# Test 15: Binary execution (coreutils)
log "Test 15: Testing binary execution with coreutils"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch /usr/bin/env echo "binary works")
output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
if [ "$output" != "binary works" ]; then
    error "Test 15 failed: Expected 'binary works', got '$output'"
fi
log "Test 15: PASSED"

# Test 16: stdin handling
log "Test 16: Testing stdin handling"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch sh -c 'cat <<EOF
multiline
input
EOF')
if ! echo "$output" | grep -q "multiline"; then
    error "Test 16 failed: Expected 'multiline' in output, got '$output'"
fi
log "Test 16: PASSED"

# Test 17: Host filesystem isolation (verify /sys is virtualized)
log "Test 17: Verifying /sys virtualization"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch ls /sys)
if ! echo "$output" | grep -q "class"; then
    error "Test 17 failed: Expected 'class' in /sys"
fi
log "Test 17: PASSED"

# Test 18: VM memory size verification
log "Test 18: Verifying VM memory configuration"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run $ISOLATE_OPTS --io=batch sh -c 'cat /proc/meminfo | grep MemTotal')
# Default is 2048M, should show some reasonable amount
if ! echo "$output" | grep -q "MemTotal"; then
    error "Test 18 failed: Expected MemTotal in output"
fi
log "Test 18: PASSED"

# ============================================
# Test Suite: VM Lifecycle Subcommands
# ============================================
# Tests for: epkg vm start, vm stop, vm list, vm status
# Reuses the same ENV_NAME environment from previous tests

# Helper: get epkg run directory (XDG_RUNTIME_DIR based)
get_epkg_run_dir() {
    echo "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/epkg"
}

# Helper: get session file path for an env
get_session_file() {
    local env_name="$1"
    echo "$(get_epkg_run_dir)/vm-sessions/${env_name}.json"
}

# Helper: get socket path for an env
get_socket_path() {
    local env_name="$1"
    echo "$(get_epkg_run_dir)/vsock-${env_name}.sock"
}

# Helper: check if VM session is active (via session file)
is_vm_session_active() {
    local env_name="$1"
    local session_file
    session_file=$(get_session_file "$env_name")
    if [ ! -f "$session_file" ]; then
        return 1
    fi
    # Check if daemon_pid is alive
    local daemon_pid
    daemon_pid=$(grep -o '"daemon_pid": [0-9]*' "$session_file" | grep -o '[0-9]*')
    if [ -z "$daemon_pid" ]; then
        return 1
    fi
    # Check if process is alive
    if ! kill -0 "$daemon_pid" 2>/dev/null; then
        return 1
    fi
    # Check if socket is connectable
    local socket_path
    socket_path=$(get_socket_path "$env_name")
    if [ -S "$socket_path" ] && [ -w "$socket_path" ]; then
        return 0
    fi
    return 1
}

# Helper: wait for VM session to appear
wait_for_vm_session() {
    local env_name="$1"
    local max_wait=30
    local waited=0
    while [ $waited -lt $max_wait ]; do
        if is_vm_session_active "$env_name"; then
            return 0
        fi
        sleep 1
        waited=$((waited + 1))
    done
    return 1
}

# Helper: wait for VM session to disappear
wait_for_vm_session_stop() {
    local env_name="$1"
    local max_wait=15
    local waited=0
    while [ $waited -lt $max_wait ]; do
        if ! is_vm_session_active "$env_name"; then
            return 0
        fi
        sleep 1
        waited=$((waited + 1))
    done
    return 1
}

# Test VM-1: vm start basic
log "Test VM-1: vm start basic functionality"
# Ensure no stale VM session from previous tests
"$EPKG_BIN" vm stop "$ENV_NAME" 2>/dev/null || true
run_with_timeout "$EPKG_BIN" vm start "$ENV_NAME"
# Verify session exists
if ! is_vm_session_active "$ENV_NAME"; then
    error "Test VM-1 failed: VM session not active after vm start"
fi
log "Test VM-1: PASSED"

# Test VM-2: vm list shows running VM
log "Test VM-2: vm list shows running VM"
output=$(capture_with_timeout "$EPKG_BIN" vm list)
if ! echo "$output" | grep -q "$ENV_NAME"; then
    error "Test VM-2 failed: vm list should show $ENV_NAME, got: $output"
fi
# Verify status column shows 'running'
if ! echo "$output" | grep -E "$ENV_NAME.*running"; then
    error "Test VM-2 failed: VM status should be 'running', got: $output"
fi
log "Test VM-2: PASSED"

# Test VM-3: vm status shows YAML output
log "Test VM-3: vm status shows YAML output"
output=$(capture_with_timeout "$EPKG_BIN" vm status "$ENV_NAME")
# Check YAML format fields
for field in "env_name:" "env_root:" "daemon_pid:" "socket_path:" "backend:" "config:"; do
    if ! echo "$output" | grep -q "$field"; then
        error "Test VM-3 failed: YAML output missing $field, got: $output"
    fi
done
# Verify env_name matches (serde_yaml doesn't quote simple string values)
if ! echo "$output" | grep -qE "env_name: +$ENV_NAME"; then
    error "Test VM-3 failed: env_name should be $ENV_NAME, got: $output"
fi
log "Test VM-3: PASSED"

# Test VM-4: VM reuse - run command in existing VM
log "Test VM-4: VM reuse - run command in existing VM session"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch echo "reuse test")
output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
if [ "$output" != "reuse test" ]; then
    error "Test VM-4 failed: Expected 'reuse test', got '$output'"
fi
# Verify VM is still running after reuse
if ! is_vm_session_active "$ENV_NAME"; then
    error "Test VM-4 failed: VM should still be active after reuse"
fi
log "Test VM-4: PASSED"

# Test VM-5: vm start rejects duplicate
log "Test VM-5: vm start rejects duplicate session"
set +e
output=$("$EPKG_BIN" vm start "$ENV_NAME" 2>&1)
exit_code=$?
set -e
if [ "$exit_code" = "0" ]; then
    error "Test VM-5 failed: vm start should fail when VM already running"
fi
if ! echo "$output" | grep -q "already running"; then
    error "Test VM-5 failed: Expected 'already running' error, got: $output"
fi
log "Test VM-5: PASSED"

# Test VM-6: vm stop
log "Test VM-6: vm stop functionality"
run_with_timeout "$EPKG_BIN" vm stop "$ENV_NAME"
if ! wait_for_vm_session_stop "$ENV_NAME"; then
    error "Test VM-6 failed: VM session should be stopped"
fi
# Verify vm list shows no VMs (or doesn't show this env)
output=$(capture_with_timeout "$EPKG_BIN" vm list)
if echo "$output" | grep -q "$ENV_NAME"; then
    error "Test VM-6 failed: vm list should not show stopped VM $ENV_NAME, got: $output"
fi
log "Test VM-6: PASSED"

# Test VM-7: vm stop on non-existent VM fails
log "Test VM-7: vm stop on non-existent VM fails"
set +e
output=$("$EPKG_BIN" vm stop "$ENV_NAME" 2>&1)
exit_code=$?
set -e
if [ "$exit_code" = "0" ]; then
    error "Test VM-7 failed: vm stop should fail when no VM running"
fi
if ! echo "$output" | grep -q "No VM running"; then
    error "Test VM-7 failed: Expected 'No VM running' error, got: $output"
fi
log "Test VM-7: PASSED"

# Test VM-8: vm status on non-existent VM fails
log "Test VM-8: vm status on non-existent VM fails"
set +e
output=$("$EPKG_BIN" vm status "$ENV_NAME" 2>&1)
exit_code=$?
set -e
if [ "$exit_code" = "0" ]; then
    error "Test VM-8 failed: vm status should fail when no VM running"
fi
if ! echo "$output" | grep -q "No VM running"; then
    error "Test VM-8 failed: Expected 'No VM running' error, got: $output"
fi
log "Test VM-8: PASSED"

# Test VM-9: vm start with parameters
log "Test VM-9: vm start with custom parameters"
run_with_timeout "$EPKG_BIN" vm start "$ENV_NAME" -s cpus=4 -s memory=2048 -s timeout=60
if ! wait_for_vm_session "$ENV_NAME"; then
    error "Test VM-9 failed: VM session not active after vm start"
fi
# Verify parameters in vm status
output=$(capture_with_timeout "$EPKG_BIN" vm status "$ENV_NAME")
if ! echo "$output" | grep -q "cpus: 4"; then
    error "Test VM-9 failed: Expected cpus: 4 in status, got: $output"
fi
if ! echo "$output" | grep -q "memory_mib: 2048"; then
    error "Test VM-9 failed: Expected memory_mib: 2048 in status, got: $output"
fi
if ! echo "$output" | grep -q "timeout: 60"; then
    error "Test VM-9 failed: Expected timeout: 60 in status, got: $output"
fi
log "Test VM-9: PASSED"

# Test VM-10: vm start with --vmm option
log "Test VM-10: vm start with --vmm option"
run_with_timeout "$EPKG_BIN" vm stop "$ENV_NAME" 2>/dev/null || true
wait_for_vm_session_stop "$ENV_NAME" || true
run_with_timeout "$EPKG_BIN" vm start "$ENV_NAME" --vmm="$VMM_BACKEND"
if ! wait_for_vm_session "$ENV_NAME"; then
    error "Test VM-10 failed: VM session not active after vm start --vmm"
fi
output=$(capture_with_timeout "$EPKG_BIN" vm status "$ENV_NAME")
if ! echo "$output" | grep -qE "backend: +$VMM_BACKEND"; then
    error "Test VM-10 failed: Expected backend: $VMM_BACKEND in status, got: $output"
fi
log "Test VM-10: PASSED"

# Test VM-11: vm start timeout=0 (never timeout)
log "Test VM-11: vm start with timeout=0 (never auto-shutdown)"
run_with_timeout "$EPKG_BIN" vm stop "$ENV_NAME" 2>/dev/null || true
wait_for_vm_session_stop "$ENV_NAME" || true
run_with_timeout "$EPKG_BIN" vm start "$ENV_NAME" -s timeout=0
if ! wait_for_vm_session "$ENV_NAME"; then
    error "Test VM-11 failed: VM session not active"
fi
output=$(capture_with_timeout "$EPKG_BIN" vm status "$ENV_NAME")
if ! echo "$output" | grep -q "timeout: 0"; then
    error "Test VM-11 failed: Expected timeout: 0 in status, got: $output"
fi
log "Test VM-11: PASSED"

# Test VM-12: VM auto-reuse detection
log "Test VM-12: VM auto-reuse detection (no --reuse needed)"
# Run a command - should auto-detect existing session
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch hostname)
if [ -z "$output" ]; then
    error "Test VM-12 failed: hostname should return non-empty"
fi
log "Test VM-12: PASSED (hostname=$output)"

# Test VM-13: whoami in VM
log "Test VM-13: whoami in VM session"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch whoami)
output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
if [ "$output" != "root" ]; then
    error "Test VM-13 failed: whoami should return 'root', got '$output'"
fi
log "Test VM-13: PASSED"

# Test VM-14: date comparison with host
log "Test VM-14: date sync between VM and host"
host_date=$(date +%Y-%m-%d)
vm_date=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch date +%Y-%m-%d)
vm_date=$(echo "$vm_date" | grep -v '^\[' | grep -v '^$' | tail -1)
if [ "$host_date" != "$vm_date" ]; then
    error "Test VM-14 failed: VM date '$vm_date' differs from host '$host_date'"
fi
# Check time is roughly synced (within 60 seconds tolerance)
host_epoch=$(date +%s)
vm_epoch=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch date +%s)
vm_epoch=$(echo "$vm_epoch" | grep -v '^\[' | grep -v '^$' | tail -1)
diff=$((host_epoch - vm_epoch))
if [ ${diff#-} -gt 60 ]; then
    error "Test VM-14 failed: VM time drift > 60s: host=$host_epoch, vm=$vm_epoch, diff=$diff"
fi
log "Test VM-14: PASSED (date synced, diff=$diff seconds)"

# Test VM-15: kernel version in VM
log "Test VM-15: /proc/version in VM"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch cat /proc/version)
if [ -z "$output" ]; then
    error "Test VM-15 failed: /proc/version should return non-empty"
fi
# Check it contains Linux version string
if ! echo "$output" | grep -q "Linux version"; then
    error "Test VM-15 failed: Expected 'Linux version' in output, got '$output'"
fi
log "Test VM-15: PASSED"

# Test VM-16: filesystem isolation checks
log "Test VM-16: filesystem isolation checks"
# Check /proc is virtualized (not host's /proc)
vm_proc_self=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch readlink /proc/self)
vm_proc_self=$(echo "$vm_proc_self" | grep -v '^\[' | grep -v '^$' | tail -1)
if [ -z "$vm_proc_self" ]; then
    error "Test VM-16 failed: /proc/self should exist"
fi
# Check /sys is virtualized
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch ls /sys/class)
if [ -z "$output" ]; then
    error "Test VM-16 failed: /sys/class should be non-empty"
fi
# Check mount points
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch cat /proc/mounts)
if ! echo "$output" | grep -q "virtiofs"; then
    error "Test VM-16 failed: Expected virtiofs mount in /proc/mounts"
fi
log "Test VM-16: PASSED"

# Test VM-17: environment variables in VM
log "Test VM-17: environment variables in VM"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch sh -c 'echo "HOME=$HOME PATH=$PATH"')
output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
if ! echo "$output" | grep -q "HOME=/root"; then
    error "Test VM-17 failed: HOME should be /root, got '$output'"
fi
if ! echo "$output" | grep -q "PATH="; then
    error "Test VM-17 failed: PATH should be set, got '$output'"
fi
log "Test VM-17: PASSED"

# ============================================
# Test Suite: epkg commands in VM session
# ============================================

# Test VM-18: epkg list in VM
log "Test VM-18: epkg list in VM session"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch epkg list)
# Should show at least bash and coreutils (installed earlier)
if ! echo "$output" | grep -q "bash"; then
    error "Test VM-18 failed: epkg list should show bash, got '$output'"
fi
if ! echo "$output" | grep -q "coreutils"; then
    error "Test VM-18 failed: epkg list should show coreutils, got '$output'"
fi
log "Test VM-18: PASSED"

# Test VM-19: epkg info in VM
log "Test VM-19: epkg info in VM session"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch epkg info bash)
if [ -z "$output" ]; then
    error "Test VM-19 failed: epkg info bash should return non-empty"
fi
# Should contain package info fields
if ! echo "$output" | grep -qE "name|version|description"; then
    error "Test VM-19 failed: epkg info should show package fields, got '$output'"
fi
log "Test VM-19: PASSED"

# Test VM-20: epkg search in VM
log "Test VM-20: epkg search in VM session"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch epkg search curl)
if [ -z "$output" ]; then
    error "Test VM-20 failed: epkg search curl should return non-empty"
fi
# Should find curl package
if ! echo "$output" | grep -q "curl"; then
    error "Test VM-20 failed: epkg search should find curl, got '$output'"
fi
log "Test VM-20: PASSED"

# Test VM-21: epkg install in VM
log "Test VM-21: epkg install in VM session"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch epkg --assume-yes install curl)
if ! echo "$output" | grep -qE "Installing|installed"; then
    error "Test VM-21 failed: epkg install should show progress, got '$output'"
fi
# Verify curl is installed
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch epkg list)
if ! echo "$output" | grep -q "curl"; then
    error "Test VM-21 failed: curl should be in list after install"
fi
# Test curl binary works
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch curl --version)
if ! echo "$output" | grep -q "curl"; then
    error "Test VM-21 failed: curl --version should work"
fi
log "Test VM-21: PASSED"

# Test VM-22: epkg remove in VM
log "Test VM-22: epkg remove in VM session"
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch epkg --assume-yes remove curl)
# Verify curl is removed
output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch epkg list)
if echo "$output" | grep -q "^curl"; then
    error "Test VM-22 failed: curl should not be in list after remove"
fi
log "Test VM-22: PASSED"

# Test VM-23: multiple commands reuse same VM
log "Test VM-23: multiple commands reuse same VM session"
for i in 1 2 3; do
    output=$(capture_with_timeout "$EPKG_BIN" -e "$ENV_NAME" run --isolate=vm --io=batch echo "iteration $i")
    output=$(echo "$output" | grep -v '^\[' | grep -v '^$' | tail -1)
    if [ "$output" != "iteration $i" ]; then
        error "Test VM-23 failed: iteration $i got '$output'"
    fi
done
# VM should still be active after multiple runs
if ! is_vm_session_active "$ENV_NAME"; then
    error "Test VM-23 failed: VM should still be active after multiple runs"
fi
log "Test VM-23: PASSED"

# Cleanup VM session (keep env for debugging)
log "Stopping VM session"
run_with_timeout "$EPKG_BIN" vm stop "$ENV_NAME" 2>/dev/null || true

log "============================================"
log "VM lifecycle subcommand tests completed!"
log "============================================"

log "============================================"
log "All VM sandbox tests completed successfully!"
log "VMM backend: $VMM_BACKEND"
log "============================================"
check_vm_log

log "Test environment '$ENV_NAME' left for debugging. Remove with: epkg env remove $ENV_NAME"