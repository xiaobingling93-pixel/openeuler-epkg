#!/bin/bash
set -e

# Variables
LUA_VERSION=5.4.7
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT_DIR=dist
RUST_TARGET_X86_64=x86_64-unknown-linux-musl
RUST_TARGET_AARCH64=aarch64-unknown-linux-musl
RUST_TARGET_RISCV64=riscv64gc-unknown-linux-musl
RUST_TARGET_LOONGARCH64=loongarch64-unknown-linux-musl
RUST_TARGET_X86_64_DARWIN=x86_64-apple-darwin
RUST_TARGET_AARCH64_DARWIN=aarch64-apple-darwin
# Using GNU target for mingw-w64 toolchain
RUST_TARGET_X86_64_WINDOWS=x86_64-pc-windows-gnu
RUST_TARGET_AARCH64_WINDOWS=aarch64-pc-windows-gnu
BINARY_NAME=epkg

# Cross-platform architecture detection
detect_native_arch() {
    local uname_m=$(uname -m)
    case "$uname_m" in
        x86_64|amd64)
            echo "x86_64"
            ;;
        i386|i686)
            echo "x86_64"  # map 32-bit to 64-bit for simplicity
            ;;
        arm64|aarch64)
            echo "aarch64"
            ;;
        armv7l|armv8l)
            echo "arm"     # not supported, but map
            ;;
        riscv64)
            echo "riscv64"
            ;;
        loongarch64)
            echo "loongarch64"
            ;;
        *)
            echo "$uname_m"
            ;;
    esac
}

# Development environment paths
DEV_ENV_BIN_DIR="$HOME/.epkg/envs/self/usr/bin"
DEV_ENV_SRC_DIR="$HOME/.epkg/envs/self/usr/src/epkg"

# Safe copy with handling for "Text file busy" error
safe_cp() {
    local src="$1"
    local dst="$2"
    local cp_output cp_status
    cp_output=$(cp -v --update "$src" "$dst" 2>&1) && echo "$cp_output" || {
        cp_status=$?
        if echo "$cp_output" | grep -q "Text file busy"; then
            if ! rm -v "$dst"; then
                echo "Error: failed to remove busy file: $dst" >&2
                return 1
            fi
            cp -v --update "$src" "$dst" || return $?
        else
            echo "$cp_output" >&2
            return $cp_status
        fi
    }
}

# Detect OS and version
detect_os() {
    local uname_s=$(uname -s)
    case "$uname_s" in
        Linux)
            OS_FAMILY="linux"
            if [[ -f /etc/os-release ]]; then
                OS_ID=$(grep -E '^ID=' /etc/os-release | cut -d= -f2 | tr -d '"')
                OS_VERSION=$(grep -E '^VERSION_ID=' /etc/os-release | cut -d= -f2 | tr -d '"')
            else
                OS_ID="linux"
                OS_VERSION="unknown"
            fi
            ;;
        Darwin)
            OS_FAMILY="darwin"
            OS_ID="darwin"
            OS_VERSION=$(sw_vers -productVersion 2>/dev/null || echo "unknown")
            ;;
        CYGWIN*|MINGW*|MSYS*)
            OS_FAMILY="windows"
            OS_ID="windows"
            OS_VERSION="unknown"
            ;;
        *)
            OS_FAMILY="unknown"
            OS_ID="unknown"
            OS_VERSION="unknown"
            ;;
    esac
    echo "Detected OS: $OS_FAMILY $OS_ID $OS_VERSION"
}

has_cmd()
{
    command -v "$1" >/dev/null
}

# Detect package manager
detect_package_manager() {
    # Detect available package manager based on OS family
    case "$OS_FAMILY" in
        linux)
            if   has_cmd apt;       then PKG_MANAGER="apt"
            elif has_cmd dnf;       then PKG_MANAGER="dnf"
            elif has_cmd yum;       then PKG_MANAGER="yum"
            elif has_cmd zypper;    then PKG_MANAGER="zypper"
            elif has_cmd pacman;    then PKG_MANAGER="pacman"
            elif has_cmd apk;       then PKG_MANAGER="apk"
            else
                PKG_MANAGER="unknown"
                echo "Warning: Could not detect Linux package manager"
                exit 1
            fi
            ;;
        darwin)
            if has_cmd brew; then
                PKG_MANAGER="brew"
            else
                PKG_MANAGER="unknown"
                echo "Warning: Homebrew not found. Some dependencies may need manual installation."
                # Do not exit, as we can still try to build with system tools
            fi
            ;;
        windows)
            if has_cmd choco; then
                PKG_MANAGER="choco"
            elif has_cmd scoop; then
                PKG_MANAGER="scoop"
            else
                PKG_MANAGER="unknown"
                echo "Warning: No package manager detected (choco/scoop). Some dependencies may need manual installation."
            fi
            ;;
        *)
            PKG_MANAGER="unknown"
            echo "Warning: Unknown OS family, cannot detect package manager"
            ;;
    esac
    echo "Detected package manager: $PKG_MANAGER"
}

# Install libkrunfw for a given architecture.
# This checks if libkrunfw is already installed in the final location,
# and if not, downloads and installs it to both the self env and target/debug.
install_libkrunfw() {
    local arch="$1"

    # Only needed for current arch
    [[ "$arch" != $(arch) ]] && return

    # Check if libkrunfw is supported for this architecture
    case "$arch" in
        x86_64|aarch64|riscv64)
            ;;
        loongarch64)
            echo "Warning: libkrunfw not available for loongarch64, libkrun feature won't be usable" >&2
            return 0
            ;;
        *)
            echo "Warning: libkrunfw not available for $arch, libkrun feature won't be usable" >&2
            return 0
            ;;
    esac

    # Final installation locations
    local dev_env_lib_dir="$HOME/.epkg/envs/self/usr/lib"
    local target_debug_lib_dir="$PROJECT_ROOT/target/debug"
    local rust_target=$(get_rust_target "$arch")
    local target_arch_debug_lib_dir="$PROJECT_ROOT/target/$rust_target/debug"

    # End-to-end check: default kernel exists under self/boot (libs + extraction done)
    local self_boot_kernel="${HOME}/.epkg/envs/self/boot/kernel"
    if [[ -f "$self_boot_kernel" ]]; then
        # echo "libkrunfw already installed for $arch (kernel at $self_boot_kernel)"
        return 0
    fi

    # Download if needed
    local tarball="$PROJECT_ROOT/krun/libkrunfw/libkrunfw-$arch.tgz"
    if [[ ! -f "$tarball" ]]; then
        echo "libkrunfw tarball not found at $tarball, downloading latest release..." >&2
        if ! has_cmd curl || ! has_cmd jq; then
            echo "Error: curl and jq are required to auto-download libkrunfw." >&2
            echo "Please install them or download libkrunfw-$arch.tgz manually into krun/libkrunfw/." >&2
            exit 1
        fi
        local tag
        tag=$(curl -sL https://api.github.com/repos/containers/libkrunfw/releases/latest | jq -r .tag_name)
        if [[ -z "$tag" || "$tag" == "null" ]]; then
            echo "Error: failed to detect latest libkrunfw release tag from GitHub." >&2
            exit 1
        fi
        local url="https://github.com/containers/libkrunfw/releases/download/${tag}/libkrunfw-$arch.tgz"
        echo "Downloading libkrunfw from $url ..."
        mkdir -p "$PROJECT_ROOT/krun/libkrunfw"
        curl -L -o "$tarball" "$url"
    fi

    local tmp_dir="$PROJECT_ROOT/target/libkrunfw-$arch-tmp"
    rm -rf "$tmp_dir"
    mkdir -p "$tmp_dir"

    echo "Unpacking libkrunfw from $tarball..."
    tar xf "$tarball" -C "$tmp_dir"

    local src_lib_dir="$tmp_dir/lib64"
    if [[ ! -d "$src_lib_dir" ]]; then
        echo "Error: expected lib64 directory in extracted libkrunfw payload, but not found at $src_lib_dir" >&2
        exit 1
    fi

    # Install into the self env so the development epkg binary can find it
    mkdir -p "$dev_env_lib_dir"
    echo "Installing libkrunfw into $dev_env_lib_dir..."
    cp -v "$src_lib_dir"/libkrunfw*.so* "$dev_env_lib_dir"/

    # Also symlink to target/debug and target/$rust_target/debug for direct execution
    mkdir -p "$target_debug_lib_dir"
    mkdir -p "$target_arch_debug_lib_dir"
    echo "Creating symlinks in $target_debug_lib_dir and $target_arch_debug_lib_dir..."
    for lib in "$dev_env_lib_dir"/libkrunfw*.so*; do
        local lib_name=$(basename "$lib")
        ln -sf "$lib" "$target_debug_lib_dir/$lib_name"
        ln -sf "$lib" "$target_arch_debug_lib_dir/$lib_name"
    done

    # Extract default kernel image from .so to self/boot (same path as Rust init.rs uses)
    extract_libkrunfw_kernel "$dev_env_lib_dir"
}

# Extract KERNEL_BUNDLE from a libkrunfw .so into $HOME/.epkg/envs/self/boot/kernel.
# First argument: directory containing libkrunfw*.so*
# Uses readelf (binutils); optional Python fallback can be added.
extract_libkrunfw_kernel() {
    local lib_dir="$1"
    local self_boot_dir="${HOME}/.epkg/envs/self/boot"
    local kernel_path="${self_boot_dir}/kernel"

    if ! has_cmd readelf; then
        echo "Warning: readelf not found, skipping kernel extraction (libkrun default kernel will not be written to self/boot)" >&2
        return 0
    fi

    local so_file=""
    for f in "$lib_dir"/libkrunfw.so.5 "$lib_dir"/libkrunfw.so "$lib_dir"/libkrunfw*.so*; do
        [[ -f "$f" ]] || continue
        if readelf -s -W "$f" 2>/dev/null | grep -q ' KERNEL_BUNDLE$'; then
            so_file="$f"
            break
        fi
    done
    if [[ -z "$so_file" ]]; then
        echo "Warning: no libkrunfw .so with KERNEL_BUNDLE found in $lib_dir" >&2
        return 0
    fi

    local sym_line
    sym_line=$(readelf -s -W "$so_file" 2>/dev/null | grep ' KERNEL_BUNDLE$' | head -1)
    if [[ -z "$sym_line" ]]; then
        echo "Warning: KERNEL_BUNDLE symbol not found in $so_file" >&2
        return 0
    fi
    local sym_value sym_size ndx
    sym_value=$(echo "$sym_line" | awk '{print $2}')
    sym_size=$(echo "$sym_line" | awk '{print $3}')
    ndx=$(echo "$sym_line" | awk '{print $7}')
    # Size may be decimal or 0xhex; normalize for dd (decimal)
    sym_size=$((sym_size))
    if [[ -z "$sym_value" || -z "$sym_size" || -z "$ndx" ]]; then
        echo "Warning: could not parse KERNEL_BUNDLE from readelf output" >&2
        return 0
    fi

    local sec_line
    sec_line=$(readelf -S -W "$so_file" 2>/dev/null | awk -v ndx="$ndx" 'index($1, "["ndx"]") > 0 {print; exit}')
    if [[ -z "$sec_line" ]]; then
        echo "Warning: section $ndx not found in $so_file" >&2
        return 0
    fi
    local sec_addr sec_off
    sec_addr=$(echo "$sec_line" | awk '{print $4}')
    sec_off=$(echo "$sec_line" | awk '{print $5}')
    if [[ -z "$sec_addr" || -z "$sec_off" ]]; then
        echo "Warning: could not parse section $ndx from readelf output" >&2
        return 0
    fi

    local file_off
    file_off=$((0x${sec_off} + 0x${sym_value} - 0x${sec_addr}))
    mkdir -p "$self_boot_dir"
    local tmp_kernel="${self_boot_dir}/kernel.tmp.$$"
    if ! dd if="$so_file" of="$tmp_kernel" bs=1 skip="$file_off" count="$sym_size" 2>/dev/null; then
        echo "Warning: failed to extract kernel" >&2
        rm -f "$tmp_kernel"
        return 0
    fi
    # If we can get kernel release (uname -r style), save as kernel-<version> and symlink kernel -> it
    local version
    version=$(strings "$tmp_kernel" 2>/dev/null | grep -m1 "Linux version " | awk '{print $3}')
    version=$(echo "$version" | tr -cd '0-9.-')
    if [[ -n "$version" ]]; then
        local named_kernel="${self_boot_dir}/kernel-${version}"
        mv "$tmp_kernel" "$named_kernel"
        ln -sf "kernel-${version}" "$kernel_path"
        echo "Extracted default kernel image to $named_kernel (kernel -> kernel-$version)"
    else
        mv "$tmp_kernel" "$kernel_path"
        echo "Extracted default kernel image to $kernel_path"
    fi
}

# Clone or update a git repository
clone_or_update_repo() {
    local repo_url="$1"
    local dir_name="$2"
    if ! has_cmd git; then
        echo "Error: git command not found. Please install git or run './make.sh dev-depends' to install dependencies." >&2
        exit 1
    fi
    if [[ -z "$dir_name" ]]; then
        # Extract directory name from repo URL (remove .git suffix if present)
        dir_name="${repo_url##*/}"
        dir_name="${dir_name%.git}"
    fi

    if [[ -d "$dir_name" ]]; then
        if [[ -n "$(ls -A "$dir_name" 2>/dev/null)" ]]; then
            echo "Directory $dir_name already exists and is not empty, attempting to update..."
            if [[ -d "$dir_name/.git" ]]; then
                (cd "$dir_name" && git pull)
            else
                echo "Warning: $dir_name exists but is not a git repository, skipping update"
            fi
        else
            echo "Directory $dir_name exists but is empty, removing..."
            rmdir "$dir_name"
            echo "Cloning $repo_url..."
            git clone "$repo_url" "$dir_name"
        fi
    else
        echo "Cloning $repo_url..."
        git clone "$repo_url" "$dir_name"
    fi
}

# Ensure we operate from project root
cd "$PROJECT_ROOT"

# Helper function for quick develop-debug loop
install_to_dev_env() {
    local binary_path="$1"

    [[ -d "$DEV_ENV_BIN_DIR" ]] || return 0

    if [[ ! -L "$DEV_ENV_SRC_DIR" ]] || [[ "$(readlink "$DEV_ENV_SRC_DIR")" != "$(pwd)" ]]; then
        local src_rc="$PROJECT_ROOT/lib/epkg-rc.sh"
        local dst_rc="$DEV_ENV_SRC_DIR/lib/epkg-rc.sh"
        if [[ "$(readlink -f "$src_rc")" != "$(readlink -f "$dst_rc")" ]]; then
            safe_cp "$src_rc" "$dst_rc"
        fi
    fi

    safe_cp "$binary_path" "$DEV_ENV_BIN_DIR/$BINARY_NAME"
}

# Build Lua library for a specific architecture
# Helper function: download and extract Lua tarball
download_and_extract_lua() {
    local lua_download_dir="$1"
    local lua_build_dir="$2"
    # Check for wget
    if ! has_cmd wget; then
        echo "Error: wget command not found. Please install wget or run './make.sh dev-depends' to install dependencies." >&2
        exit 1
    fi
    # Download tarball once to shared location
    mkdir -p "$lua_download_dir"
    local tarball="$lua_download_dir/lua-$LUA_VERSION.tar.gz"
    [ -f "$tarball" ] || wget --no-verbose "https://www.lua.org/ftp/lua-$LUA_VERSION.tar.gz" -O "$tarball"
    # Extract to architecture-specific build directory
    mkdir -p "$lua_build_dir"
    cd "$lua_build_dir"
    [ -d "lua-$LUA_VERSION" ] || tar xzf "$tarball"
}

# Helper function: build and deploy Lua library
build_and_deploy_lua() {
    local lua_build_dir="$1"
    local lua_lib_dir="$2"

    local compiler=""
    set_lua_compiler

    # Build
    cd "$lua_build_dir/lua-$LUA_VERSION"
    rm -f src/liblua.a
    make clean
    # Add -fPIC for position independent code (required for PIE executables)
    make CC="$compiler" CFLAGS="-O2 -Wall -fPIC -D_FILE_OFFSET_BITS=64 -U_LARGEFILE64_SOURCE" linux
    # Add musl compatibility shims for missing *64 functions
    echo "Adding musl compatibility shims..."
    $compiler -O2 -Wall -fPIC -D_FILE_OFFSET_BITS=64 -U_LARGEFILE64_SOURCE -c -o src/musl_compat.o "$PROJECT_ROOT/lib/musl_compat.c" || echo "Failed to compile musl_compat.c"
    ar rcs src/liblua.a src/musl_compat.o || echo "Failed to add musl_compat.o to liblua.a"
    # Deploy
    mkdir -p "$lua_lib_dir"
    cp src/liblua.a "$lua_lib_dir/"
    cp src/lua.h src/lualib.h src/lauxlib.h src/lua.hpp src/luaconf.h "$lua_lib_dir/"
}

set_lua_compiler() {
    # Deduce compiler based on architecture and library type
    compiler=$(get_c_compiler "$arch" "$lib_type")
    echo "Building Lua library for $arch ($lib_type) using $compiler..."
}

# Usage: build_lua_lib [<arch>] [musl|glibc]
build_lua_lib() {
    local arch=$(get_arch "$1")
    local lib_type="${2:-musl}"  # Default to musl for backward compatibility

    local lua_download_dir="$PROJECT_ROOT/target/lua-download"
    local lua_build_dir="$PROJECT_ROOT/target/lua-build-$arch-$lib_type"
    local lua_lib_dir="$PROJECT_ROOT/target/lua-$lib_type-$arch"

    download_and_extract_lua "$lua_download_dir" "$lua_build_dir"
    build_and_deploy_lua "$lua_build_dir" "$lua_lib_dir"
}

# Helper functions for dependency installation

# Get package manager configuration
get_package_manager_config() {
    local mode="$1"
    local common_packages="git wget curl jq tar"
    packages=""
    update_cmd=""
    install_cmd=""

    case "$PKG_MANAGER" in
        apt)
            update_cmd="apt-get update"
            install_cmd="apt-get install -y"
            case "$mode" in
                dev)
                    packages="rustup build-essential libssl-dev musl-tools liblua5.4-dev"
                    ;;
                crossdev)
                    packages="rustup build-essential libssl-dev musl-tools liblua5.4-dev gcc-aarch64-linux-gnu gcc-riscv64-linux-gnu gcc-loongarch64-linux-gnu gcc-mingw-w64-x86-64 xar libxar-dev clang cmake libxml2-dev fuse3 libfuse3-dev liblzma-dev libbz2-dev zlib1g-dev llvm-dev uuid-dev"
                    ;;
                sandbox)
                    # Tools for user namespace UID/GID mapping and sandbox helpers
                    # newuidmap/newgidmap live in uidmap on Debian/Ubuntu
                    packages="uidmap"
                    ;;
                qemu)
                    # QEMU system emulator and virtiofs daemon
                    # qemu-system-x86 provides qemu-system-x86_64 on Debian/Ubuntu
                    # virtiofsd is available as a separate package on newer releases
                    packages="qemu-system-x86 virtiofsd"
                    ;;
                *)
                    packages=""
                    ;;
            esac
            ;;
        dnf|yum)
            update_cmd="$PKG_MANAGER update -y"
            install_cmd="$PKG_MANAGER install -y"
            case "$mode" in
                dev|crossdev)
                    # For dnf/yum, install cargo instead of rustup (rustup can be installed via curl if needed)
                    # Crossdev packages may not be available on all distros
                    # Note: crossdev mode not supported for dnf/yum - cross-compilation tools not packaged
                    packages="cargo gcc openssl-devel musl-gcc libstdc++-static lua-devel"
                    ;;
                sandbox)
                    # newuidmap/newgidmap are shipped by shadow-utils on Fedora/RHEL
                    packages="shadow-utils"
                    ;;
                qemu)
                    # qemu-system-x86_64 and virtiofs daemon
                    packages="qemu-system-x86_64 qemu-virtiofsd"
                    ;;
                *)
                    packages=""
                    ;;
            esac
            ;;
        zypper)
            update_cmd="zypper refresh"
            install_cmd="zypper install -y"
            case "$mode" in
                dev|crossdev)
                    packages="rustup gcc openssl-devel musl-gcc lua-devel"
                    # Crossdev packages may not be available
                    ;;
                sandbox)
                    # shadow provides newuidmap/newgidmap on openSUSE
                    packages="shadow"
                    ;;
                qemu)
                    # QEMU system emulator and virtiofs daemon on openSUSE
                    packages="qemu-x86 qemu-virtiofsd"
                    ;;
                *)
                    packages=""
                    ;;
            esac
            ;;
        pacman)
            update_cmd="pacman -Sy"
            install_cmd="pacman -S --noconfirm"
            case "$mode" in
                dev|crossdev)
                    packages="rustup base-devel openssl musl lua"
                    # Crossdev packages: aarch64-linux-gnu-gcc, riscv64-linux-gnu-gcc, loongarch64-linux-gnu-gcc (from AUR)
                    ;;
                sandbox)
                    # shadow provides newuidmap/newgidmap on Arch
                    packages="shadow"
                    ;;
                qemu)
                    # Arch packages: qemu-desktop (includes qemu-system-x86_64) and virtiofsd (if packaged separately)
                    # Users may need to adjust package names on derivatives.
                    packages="qemu-desktop virtiofsd"
                    ;;
                *)
                    packages=""
                    ;;
            esac
            ;;
        apk)
            update_cmd="apk update"
            install_cmd="apk add"
            case "$mode" in
                dev|crossdev)
                    packages="rustup build-base openssl-dev musl-dev lua-dev"
                    # Crossdev packages: cross-compile tools may be in community repos
                    ;;
                sandbox)
                    # shadow-uidmap provides newuidmap/newgidmap on Alpine
                    packages="shadow-uidmap"
                    ;;
                *)
                    packages=""
                    ;;
            esac
            ;;
        brew)
            update_cmd="brew update"
            install_cmd="brew install"
            case "$mode" in
                dev|crossdev)
                    packages="rustup lua openssl pkg-config"
                    ;;
                sandbox)
                    # No sandbox packages needed on macOS
                    packages=""
                    ;;
                qemu)
                    # QEMU optional
                    packages="qemu"
                    ;;
                *)
                    packages=""
                    ;;
            esac
            ;;
        choco)
            update_cmd="choco update -y"
            install_cmd="choco install -y"
            case "$mode" in
                dev|crossdev)
                    packages="rustup lua openssl git wget"
                    ;;
                sandbox)
                    packages=""
                    ;;
                qemu)
                    packages="qemu"
                    ;;
                *)
                    packages=""
                    ;;
            esac
            ;;
        scoop)
            update_cmd="scoop update"
            install_cmd="scoop install"
            case "$mode" in
                dev|crossdev)
                    packages="rustup lua openssl git wget"
                    ;;
                sandbox)
                    packages=""
                    ;;
                qemu)
                    packages="qemu"
                    ;;
                *)
                    packages=""
                    ;;
            esac
            ;;
        *)
            echo "Unsupported package manager: $PKG_MANAGER"
            exit 1
            ;;
    esac
    packages="$packages $common_packages"
}

# Install packages using detected package manager
install_os_packages() {
    # Determine if we need sudo
    local SUDO
    if [[ $(id -u) -eq 0 ]]; then
        SUDO=""
    else
        SUDO="sudo"
    fi

    # Run update command
    if [[ -n "$update_cmd" ]]; then
        echo "Updating package lists..."
        $SUDO $update_cmd || echo "Warning: Package update failed, continuing..."
    fi

    # Install packages
    if [[ -n "$packages" ]]; then
        echo "Installing packages: $packages"
        $SUDO $install_cmd $packages || {
            echo "Error: Package installation failed"
            exit 1
        }
    fi
}

# Install Rust toolchain (common across distros)
install_rust_toolchain() {
    local mode="$1"
    local current_arch="$2"

    echo "Installing Rust toolchain..."

    # For all other distros, try to use rustup if available
    if has_cmd rustup; then
        echo "Using rustup installation"
        rustup default stable
        if [[ "$mode" == "dev" ]]; then
            local rust_target=$(get_rust_target "$current_arch")
            rustup target add "$rust_target"
        else
            rustup target add "$RUST_TARGET_X86_64"
            rustup target add "$RUST_TARGET_AARCH64"
            rustup target add "$RUST_TARGET_RISCV64"
            rustup target add "$RUST_TARGET_LOONGARCH64"
        fi
    else
        echo "rustup not found, using system cargo if available"
        if ! has_cmd cargo; then
            echo "Warning: Neither rustup nor cargo found. Rust toolchain may be missing."
        fi
    fi
}

install_packages() {
    local mode="${1:-dev}"
    detect_os
    detect_package_manager
    echo "Detected OS: $OS_ID $OS_VERSION"
    echo "Detected package manager: $PKG_MANAGER"

    local current_arch=$(arch)
    echo "Detected architecture: $current_arch"

    echo "Installing dependencies ($mode mode)..."

    # Get package manager configuration
    get_package_manager_config "$mode"

    # Install packages
    install_os_packages
}

# Clone required repositories (without building elf-loader dependencies)
clone_repos() {
    clone_or_update_repo "https://gitee.com/wu_fengguang/rpm-rs"
    clone_or_update_repo "https://gitee.com/wu_fengguang/resolvo"
    clone_or_update_repo "https://gitee.com/wu_fengguang/elf-loader"
    clone_or_update_repo "https://gitee.com/wu_fengguang/krun"
    (
        cd krun
        clone_or_update_repo "https://gitee.com/wu_fengguang/libkrun"
    )

    [[ "$mode" = "crossdev" ]] && {
        clone_or_update_repo "https://github.com/tpoechtrager/osxcross.git"
        (
            cd osxcross/tarballs
            wget https://github.com/joseluisq/macosx-sdks/releases/download/26.1/sha256sum.txt
            wget https://github.com/joseluisq/macosx-sdks/releases/download/26.1/MacOSX26.1.sdk.tar.xz
            sha256sum -c sha256sum.txt
        )
    }
}

# Unified dependency installer
install_depends() {
    install_packages "$@"

    # Install Rust toolchain for dev/crossdev modes
    install_rust_toolchain "$mode" "$current_arch"

    clone_repos

    # leave this to developers to run on-demand
    # cd elf-loader/src && make $mode-depends

    echo "Installation complete!"
}

# Install development dependencies (current arch only)
dev_depends() {
    install_depends dev
}

# Install cross-development dependencies (all arch cross-compilers)
crossdev_depends() {
    install_depends crossdev
}

# Clean build artifacts
clean() {
    echo "Cleaning build artifacts..."
    cargo clean
}

# Clean everything including distribution files
clean_all() {
    clean
    echo "Cleaning distribution files..."
    rm -rf "$OUTPUT_DIR"
}

# Get Rust target for architecture
get_rust_target() {
    local arch="$1"
    case "$arch" in
        x86_64)
            echo "$RUST_TARGET_X86_64"
            ;;
        aarch64)
            echo "$RUST_TARGET_AARCH64"
            ;;
        riscv64)
            echo "$RUST_TARGET_RISCV64"
            ;;
        loongarch64)
            echo "$RUST_TARGET_LOONGARCH64"
            ;;
        *)
            echo "Unknown architecture: $arch" >&2
            exit 1
            ;;
    esac
}

# Export linker variable for architecture
export_linker_var() {
    local arch="$1"
    local cross_compiler=$(get_cross_compiler "$arch")
    if [[ -n "$cross_compiler" ]]; then
        local rust_target=$(get_rust_target "$arch")
        local target_var=$(echo "${rust_target//-/_}" | tr '[:lower:]' '[:upper:]')
        export "CARGO_TARGET_${target_var}_LINKER=$cross_compiler"
    fi
}

# Get Rust flags for architecture
get_rustflags() {
    local arch="$1"
    local common_opts=
    case "$arch" in
        x86_64|aarch64|riscv64|loongarch64)
            # Valid architecture
            ;;
        *)
            echo "Unknown architecture: $arch" >&2
            exit 1
            ;;
    esac

    local cross_compiler=$(get_cross_compiler "$arch")
    if [[ -z "$cross_compiler" ]]; then
        # Native compilation or x86_64: no cross-compiler linker needed
        echo "$common_opts"
        return
    fi

    local cross_opts="$common_opts -C linker=$cross_compiler -C link-arg=-lgcc -C link-arg=-lc"
    case "$arch" in
        riscv64|loongarch64)
            cross_opts="$cross_opts -C link-arg=-lm"
            ;;
    esac
    echo "$cross_opts"
}

# Build static binary for a specific architecture with mode (debug/release)
# Usage: build_static <arch> <mode>
build_static() {
    local arch=$(get_arch "$1")
    local mode="$2"
    local rust_target=$(get_rust_target "$arch")
    local rustflags=$(get_rustflags "$arch")
    local cargo_features="${EPKG_CARGO_FEATURES:-}"

    echo "Building $arch binary ($mode)..."

    # Export environment variables directly
    export LUA_LIB_NAME=lua
    export LUA_LIB="$PROJECT_ROOT/target/lua-musl-$arch"
    export LUA_LINK=static
    export LUA_NO_PKG_CONFIG=1

    # Export linker variable for this architecture
    export_linker_var "$arch"

    # Set C compiler for mlua-sys build
    export CC=$(get_c_compiler "$arch" "musl")
    export CFLAGS="-D_FILE_OFFSET_BITS=64 -U_LARGEFILE64_SOURCE"
    # Set target-specific CFLAGS for cc crate (hyphens to underscores)
    local target_var="${rust_target//-/_}"
    export "CFLAGS_${target_var}"="$CFLAGS"
    export "CC_${target_var}"="$CC"

    if [[ -n "$rustflags" ]]; then
        export RUSTFLAGS="$rustflags"
    fi

    # If we're building with libkrun support, ensure libkrunfw is available.
    if [[ "$cargo_features" == *"libkrun"* ]]; then
        install_libkrunfw "$arch"
    fi

    # Build the binary (optionally with extra Cargo features)
    local cargo_feature_args=()
    if [[ -n "$cargo_features" ]]; then
        cargo_feature_args=(--features "$cargo_features")
    fi

    if [[ "$mode" == "release" ]]; then
        cargo build --release --target "$rust_target" --ignore-rust-version "${cargo_feature_args[@]}"
        local build_dir="release"
    else
        cargo build --target "$rust_target" --ignore-rust-version "${cargo_feature_args[@]}"
        local build_dir="debug"
    fi

    # Deploy only for release mode
    if [[ "$mode" == "release" ]]; then
        mkdir -p "$OUTPUT_DIR"
        # Copy binary, handling "Text file busy" error
        safe_cp "target/$rust_target/$build_dir/$BINARY_NAME" "$OUTPUT_DIR/$BINARY_NAME-$arch"
        echo "Generating checksum for $arch binary..."
        pushd "$OUTPUT_DIR" >/dev/null
        sha256sum "$BINARY_NAME-$arch" > "$BINARY_NAME-$arch.sha256"
        echo "$arch release completed: $PROJECT_ROOT/$OUTPUT_DIR/$BINARY_NAME-$arch"
        popd >/dev/null
    fi

    # Install to dev environment if this is the native architecture
    if is_native_arch "$arch"; then
        # The static/dynamic executable file sizes are similar:
        #
        # wfg /c/epkg% ll target/debug/epkg
        # -rwxr-xr-x 2 wfg wfg 156M 2026-02-23 15:54 target/debug/epkg
        # wfg /c/epkg% ll target/x86_64-unknown-linux-musl/debug/epkg
        # -rwxrwxr-x 2 wfg wfg 152M 2026-02-23 17:12 target/x86_64-unknown-linux-musl/debug/epkg
        # wfg /c/epkg% ldd target/x86_64-unknown-linux-musl/debug/epkg
        #         statically linked
        # wfg /c/epkg% ldd target/debug/epkg
        #         linux-vdso.so.1 (0x00007f0b433a9000)
        #         /lib/$LIB/liblsp.so => /lib/lib/x86_64-linux-gnu/liblsp.so (0x00007f0b41000000)
        #         libgcc_s.so.1 => /lib/x86_64-linux-gnu/libgcc_s.so.1 (0x00007f0b43348000)
        #         libm.so.6 => /lib/x86_64-linux-gnu/libm.so.6 (0x00007f0b43258000)
        #         libc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x00007f0b4120b000)
        #         /lib64/ld-linux-x86-64.so.2 (0x00007f0b433ab000)
        #         libdl.so.2 => /lib/x86_64-linux-gnu/libdl.so.2 (0x00007f0b43253000)
        #
        # Also copy to target/$mode/ dir for easy access.
        mkdir -p "target/$mode"
        cp -vfs "$PROJECT_ROOT/target/$rust_target/$build_dir/$BINARY_NAME" target/$mode/epkg
        install_to_dev_env "$PROJECT_ROOT/target/$rust_target/$build_dir/$BINARY_NAME"
    fi
}


# Setup glibc Lua static linking
setup_glibc_lua() {
    local arch="${HOST_ARCH:-$(arch)}"
    local lua_lib_dir="$PROJECT_ROOT/target/lua-glibc-$arch"

    # Build Lua library if it doesn't exist
    if [[ ! -f "$lua_lib_dir/liblua.a" ]]; then
        echo "Lua static library not found at $lua_lib_dir/liblua.a"
        echo "Building Lua library for $arch (glibc)..."
        build_lua_lib "$arch" "glibc"
    fi

    # Export environment variables for static Lua linking
    export LUA_LIB_NAME=lua
    export LUA_LIB="$lua_lib_dir"
    export LUA_LINK=static
    export LUA_NO_PKG_CONFIG=1
}

# Build development binary
build() {
    echo "Building debug binary..."

    # Detect OS to adjust Lua linking
    detect_os
    if [[ "$OS_FAMILY" == "linux" ]]; then
        # Set up static Lua linking for glibc (Linux only)
        setup_glibc_lua
    else
        # On macOS/Windows, use dynamic linking via pkg-config
        export LUA_LINK=dynamic
        unset LUA_NO_PKG_CONFIG 2>/dev/null || true
    fi

    cargo build --ignore-rust-version

    echo "Development build completed. Binary is in $PROJECT_ROOT/target/debug/$BINARY_NAME"

    install_to_dev_env "$PROJECT_ROOT/target/debug/$BINARY_NAME"
}

# Build release binary
build_release() {
    echo "Building release binary..."

    # Detect OS to adjust Lua linking
    detect_os
    if [[ "$OS_FAMILY" == "linux" ]]; then
        # Set up static Lua linking for glibc (Linux only)
        setup_glibc_lua
    else
        # On macOS/Windows, use dynamic linking via pkg-config
        export LUA_LINK=dynamic
        unset LUA_NO_PKG_CONFIG 2>/dev/null || true
    fi

    cargo build --release --ignore-rust-version

    echo "Release build completed. Binary is in $PROJECT_ROOT/target/release/$BINARY_NAME"

    install_to_dev_env "$PROJECT_ROOT/target/release/$BINARY_NAME"
}

# Cross-compilation to macOS
cross-macos() {
    local arch="${1:-aarch64}"
    local target=""
    case "$arch" in
        x86_64) target="$RUST_TARGET_X86_64_DARWIN" ;;
        aarch64) target="$RUST_TARGET_AARCH64_DARWIN" ;;
        *) echo "Unsupported architecture for macOS: $arch"; exit 1 ;;
    esac

    echo "Building for macOS ($arch)..."
    # Install Rust target if needed
    if has_cmd rustup; then
        rustup target add "$target"
    fi

    # Setup cross-compilation environment
    setup_cross_env "$target"

    # Lua dynamic linking
    export LUA_LINK=dynamic
    unset LUA_NO_PKG_CONFIG 2>/dev/null || true

    cargo build --release --target "$target" --ignore-rust-version

    echo "Cross-compilation to macOS completed. Binary is in target/$target/release/$BINARY_NAME"
}

# Cross-compilation to Windows
cross-windows() {
    local arch="${1:-x86_64}"
    local target=""
    case "$arch" in
        x86_64) target="$RUST_TARGET_X86_64_WINDOWS" ;;
        aarch64) target="$RUST_TARGET_AARCH64_WINDOWS" ;;
        *) echo "Unsupported architecture for Windows: $arch"; exit 1 ;;
    esac

    echo "Building for Windows ($arch)..."
    if has_cmd rustup; then
        rustup target add "$target"
    fi

    setup_cross_env "$target"

    export LUA_LINK=dynamic
    unset LUA_NO_PKG_CONFIG 2>/dev/null || true

    cargo build --release --target "$target" --ignore-rust-version

    echo "Cross-compilation to Windows completed. Binary is in target/$target/release/$BINARY_NAME"
}

# Run tests (module-level unit tests)
run_tests() {
    RUSTFLAGS="-A dead_code -A unused_imports -A unused_variables" cargo test
}

HOST_ARCH=$(detect_native_arch)

is_native_arch() {
    local arch="$1"
    [[ "$arch" == "$HOST_ARCH" ]]
}

get_cross_compiler() {
    local arch="$1"
    if [[ "$arch" == "x86_64" ]] || is_native_arch "$arch"; then
        # No cross-compiler needed for x86_64 or native builds
        echo ""
    else
        echo "$arch-linux-gnu-gcc"
    fi
}

get_c_compiler() {
    local arch="$1"
    local lib_type="${2:-musl}"
    case "$lib_type" in
        musl)
            case "$arch" in
                x86_64)
                    echo "musl-gcc"
                    ;;
                aarch64|riscv64|loongarch64)
                    local cross_compiler=$(get_cross_compiler "$arch")
                    if [[ -z "$cross_compiler" ]]; then
                        echo "gcc"
                    else
                        echo "$cross_compiler"
                    fi
                    ;;
                *)
                    echo "Unknown architecture: $arch" >&2
                    exit 1
                    ;;
            esac
            ;;
        glibc)
            echo "gcc"
            ;;
        *)
            echo "Unknown library type: $lib_type (must be 'musl' or 'glibc')" >&2
            exit 1
            ;;
    esac
}

# Set/get architecture - either from argument or auto-detect
get_arch() {
    local provided_arch="$1"

    if [[ -z "$provided_arch" ]]; then
        # Auto-detect current architecture
        local arch=$(detect_native_arch)
        echo "Auto-detected architecture: $arch" >&2
        echo "$arch"
    else
        echo "$provided_arch"
    fi
}

# Detect cross-compilation toolchains
detect_osxcross() {
    local osxcross_dir=""
    for dir in "/opt/osxcross" "$HOME/osxcross" "$HOME/.osxcross" "/c/rust/osxcross"; do
        # Check for universal compiler first, then architecture-specific compilers
        # Try target/bin first (where osxcross installs after building)
        if [[ -d "$dir/target/bin" ]]; then
            # Check for any OSXCross compiler (o64-clang, *-apple-darwin*-clang)
            local found=false
            if [[ -f "$dir/target/bin/o64-clang" ]]; then
                found=true
            elif compgen -G "$dir/target/bin/*-apple-darwin*-clang" >/dev/null; then
                found=true
            fi
            if $found; then
                osxcross_dir="$dir/target"
                break
            fi
        fi
        # Fallback to bin (older installations or symlinks)
        if [[ -d "$dir/bin" ]]; then
            local found=false
            if [[ -f "$dir/bin/o64-clang" ]]; then
                found=true
            elif compgen -G "$dir/bin/*-apple-darwin*-clang" >/dev/null; then
                found=true
            fi
            if $found; then
                osxcross_dir="$dir"
                break
            fi
        fi
    done
    if [[ -n "$osxcross_dir" ]]; then
        echo "$osxcross_dir"
        return 0
    fi

    # Check if osxcross directory exists but not built
    for dir in "/opt/osxcross" "$HOME/osxcross" "$HOME/.osxcross" "/c/rust/osxcross"; do
        if [[ -d "$dir" && -f "$dir/build.sh" ]]; then
            # Check for SDK tarball
            local sdk_tarball=""
            if [[ -f "$dir/tarballs/MacOSX26.1.sdk.tar.xz" ]]; then
                sdk_tarball="$dir/tarballs/MacOSX26.1.sdk.tar.xz"
            elif [[ -f "$dir/MacOSX26.1.sdk.tar.xz" ]]; then
                sdk_tarball="$dir/MacOSX26.1.sdk.tar.xz"
            fi
            if [[ -n "$sdk_tarball" ]]; then
                echo "Warning: osxcross found at $dir but not built. SDK tarball: $sdk_tarball" >&2
                echo "Run 'cd $dir && ./build.sh' to build osxcross." >&2
            fi
        fi
    done
    return 1
}

detect_mingw() {
    if has_cmd x86_64-w64-mingw32-gcc; then
        echo "x86_64-w64-mingw32"
        return 0
    fi
    return 1
}

# Get Rust target for architecture and OS
get_rust_target_for_platform() {
    local arch="$1"
    local os="$2"  # linux, darwin, windows
    case "$os" in
        linux)
            case "$arch" in
                x86_64) echo "$RUST_TARGET_X86_64" ;;
                aarch64) echo "$RUST_TARGET_AARCH64" ;;
                riscv64) echo "$RUST_TARGET_RISCV64" ;;
                loongarch64) echo "$RUST_TARGET_LOONGARCH64" ;;
                *) echo "" ;;
            esac
            ;;
        darwin)
            case "$arch" in
                x86_64) echo "$RUST_TARGET_X86_64_DARWIN" ;;
                aarch64) echo "$RUST_TARGET_AARCH64_DARWIN" ;;
                *) echo "" ;;
            esac
            ;;
        windows)
            case "$arch" in
                x86_64) echo "$RUST_TARGET_X86_64_WINDOWS" ;;
                aarch64) echo "$RUST_TARGET_AARCH64_WINDOWS" ;;
                *) echo "" ;;
            esac
            ;;
        *)
            echo ""
            ;;
    esac
}

# Setup environment for cross-compilation
setup_cross_env() {
    local target="$1"
    local arch="${target%%-*}"
    local os=""
    if [[ "$target" == *"apple-darwin"* ]]; then
        os="darwin"
    elif [[ "$target" == *"pc-windows-"* ]]; then
        os="windows"
    else
        os="linux"
    fi

    # Clear previous environment
    unset CC CFLAGS LUA_LIB LUA_LINK LUA_NO_PKG_CONFIG RUSTFLAGS
    unset CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER
    unset CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER
    unset CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER
    unset CARGO_TARGET_AARCH64_PC_WINDOWS_MSVC_LINKER
    unset CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER
    unset CARGO_TARGET_AARCH64_PC_WINDOWS_GNU_LINKER

    # Common for all targets
    export LUA_LINK=dynamic
    export LUA_NO_PKG_CONFIG=1
    export PKG_CONFIG_ALLOW_CROSS=1

    case "$os" in
        darwin)
            local osxcross_dir=$(detect_osxcross)
            if [[ -n "$osxcross_dir" ]]; then
                export PATH="$osxcross_dir/bin:$PATH"
                # Find the actual compiler binary (may have version suffix like aarch64-apple-darwin25.1-clang)
                local cc_name=""
                # Try architecture-specific compiler first
                local pattern="$arch-apple-darwin*-clang"
                local match=$(compgen -G "$osxcross_dir/bin/$pattern" 2>/dev/null | head -1)
                if [[ -n "$match" && -f "$match" ]]; then
                    cc_name="$(basename "$match")"
                elif [[ "$arch" == "x86_64" && -f "$osxcross_dir/bin/o64-clang" ]]; then
                    cc_name="o64-clang"
                else
                    cc_name="$arch-apple-darwin-clang"
                fi
                export CC="$cc_name"
                # Set target-specific linker
                local target_var=$(echo "${target//-/_}" | tr '[:lower:]' '[:upper:]')
                export "CARGO_TARGET_${target_var}_LINKER=$cc_name"
                # SDK path for osxcross
                local sdk_path=""
                if [[ -L "$osxcross_dir/SDK/MacOSX.sdk" ]]; then
                    sdk_path="$osxcross_dir/SDK/MacOSX.sdk"
                else
                    # Find the first MacOSX*.sdk directory
                    local sdk_dir
                    for sdk_dir in "$osxcross_dir/SDK"/MacOSX*.sdk; do
                        if [[ -d "$sdk_dir" ]]; then
                            sdk_path="$sdk_dir"
                            break
                        fi
                    done
                fi
                if [[ -n "$sdk_path" ]]; then
                    export SDK_PATH="$sdk_path"
                    export LIBRARY_PATH="$sdk_path/usr/lib"
                else
                    echo "Warning: Could not find macOS SDK in $osxcross_dir/SDK"
                fi
            else
                echo "Warning: osxcross not found, using system clang (may not work)"
                export CC="clang"
            fi
            ;;
        windows)
            local mingw_prefix=$(detect_mingw)
            if [[ -n "$mingw_prefix" ]]; then
                export CC="${mingw_prefix}-gcc"
                local target_var=$(echo "${target//-/_}" | tr '[:lower:]' '[:upper:]')
                export "CARGO_TARGET_${target_var}_LINKER=${mingw_prefix}-gcc"
            else
                echo "Warning: mingw-w64 not found, using default (may not work)"
            fi
            ;;
        linux)
            # Already handled by build_static
            ;;
    esac
}

cmd="${1:-build}"
# Main dispatcher
case $cmd in
    lua|build_lua_lib)
        build_lua_lib "$2"
        ;;
    static-debug|static)  # default in Makefile
        build_static "$2" debug
        ;;
    static-libkrun)
        # Build static debug binary with libkrun integrated. Additional
        # features can be supplied via EPKG_CARGO_FEATURES if needed.
        arch=$(get_arch "$2")
        if [[ -z "$EPKG_CARGO_FEATURES" ]]; then
            EPKG_CARGO_FEATURES="libkrun"
        fi
        build_static "$arch" debug
        ;;
    static-release)
        build_static "$2" release
        ;;
    build)
        build
        ;;
    release)  # not used in Makefile
        build_release
        ;;
    dev-depends)
        dev_depends
        ;;
    crossdev-depends)
        crossdev_depends
        ;;
    dev-pkgs)
        install_packages dev
        ;;
    crossdev-pkgs)
        install_packages crossdev
        ;;
    qemu-pkgs)
        # Install VMM (QEMU + virtiofsd) host dependencies for --sandbox=vm
        install_packages qemu
        ;;
    sandbox-pkgs)
        # Install sandbox-related host dependencies (user namespaces, uid/gid mapping tools)
        # They are standard utils that are normally already installed, so no callers for this
        install_packages sandbox
        ;;
    clone-repos)
        clone_repos
        ;;
    cross-macos)
        cross-macos "$2"
        ;;
    cross-windows)
        cross-windows "$2"
        ;;
    test)
        run_tests
        ;;
    clean)
        clean
        ;;
    clean_all)
        clean_all
        ;;
    *)
        echo "Usage: $0 [command] [options...]"
        echo ""
        echo "Commands:"
        echo "  build                                Build development binary (default)"
        echo "  lua [<arch>]                         Build Lua library for architecture (auto-detects if not specified)"
        echo "  release                              Build release binary (dynamic linking)"
        echo "  static [<arch>]                      Build static debug binary (auto-detects arch if not specified)"
        echo "  static-debug [<arch>]                Build static debug binary"
        echo "  static-release [<arch>]              Build static release binary"
        echo "  static-libkrun [<arch>]              Build static debug binary with --features libkrun and bundled libkrunfw (x86_64 only)"
        echo "  dev-depends                          Install development dependencies (current arch only)"
        echo "  crossdev-depends                     Install cross-development dependencies (all arch cross-compilers)"
        echo "  clone-repos                          Clone required repositories (rpm-rs, resolvo, elf-loader)"
        echo "  cross-macos [<arch>]                 Cross-compile to macOS (aarch64 default, or x86_64)"
        echo "  cross-windows [<arch>]               Cross-compile to Windows (x86_64 or aarch64)"
        echo "  qemu-pkgs                            Install qemu dependency packages"
        echo "  sandbox-pkgs                         Install sandbox dependency packages"
        echo "  test                                 Run module-level unit tests"
        echo "  clean                                Clean build artifacts"
        echo "  clean_all                            Clean all artifacts and distribution files"
        echo ""
        echo "Supported architectures: x86_64, aarch64, riscv64, loongarch64"
        exit 1
        ;;
esac
