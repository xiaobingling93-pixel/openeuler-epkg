#!/bin/bash
set -e

# =============================================================================
# Build Policy
# =============================================================================
#
# Static Linking (Default):
#   - All meaningful/default/actively-used builds are statically linked
#   - This applies to both debug and release/deploy builds
#   - Static binaries are self-contained and portable across environments
#
# Dynamic Linking (Legacy):
#   - Only retained for potential corner case usage
#   - NOT recommended for production or deployment
#   - Commands: `dynamic-build`, `dynamic-release`
#
# libkrun Feature Auto-Enable Matrix:
#   Platform     | Architecture      | libkrun
#   -------------|-------------------|--------
#   Linux        | x86_64/aarch64/riscv64 | enabled (static linked)
#   Linux        | loongarch64       | disabled (not supported)
#   macOS        | all architectures | enabled (static linked)
#   Windows      | all architectures | disabled (not supported)
#
# User Override:
#   - FEATURES="auto"   : auto-enable libkrun for supported platforms (default)
#   - FEATURES=""       : disable all features (no libkrun)
#   - FEATURES="libkrun": explicitly enable libkrun
#   - FEATURES="..."    : custom features (comma-separated)
#
# =============================================================================

# Variables
LUA_VERSION=5.4.7
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT_DIR=dist

# Setup PATH to include cargo if available
# This ensures 'cargo' and 'rustup' commands work even if not in shell PATH
if [[ -d "$HOME/.cargo/bin" ]]; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi
# Also check rustup's default toolchain location
RUSTUP_CARGO_DIR="${RUSTUP_HOME:-$HOME/.rustup}/toolchains/stable-$(uname -m | sed 's/amd64/x86_64/;s/arm64/aarch64/')-apple-darwin/bin"
if [[ -d "$RUSTUP_CARGO_DIR" ]]; then
    export PATH="$RUSTUP_CARGO_DIR:$PATH"
fi
# MSYS2/MinGW64: add mingw64/bin to PATH for cargo, gcc, etc.
if [[ -d "/mingw64/bin" ]]; then
    export PATH="/mingw64/bin:$PATH"
fi
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
            if has_cmd pacman; then
                # MSYS2 environment
                PKG_MANAGER="pacman"
            elif has_cmd choco; then
                PKG_MANAGER="choco"
            elif has_cmd scoop; then
                PKG_MANAGER="scoop"
            else
                PKG_MANAGER="unknown"
                echo "Warning: No package manager detected (pacman/choco/scoop). Some dependencies may need manual installation."
            fi
            ;;
        *)
            PKG_MANAGER="unknown"
            echo "Warning: Unknown OS family, cannot detect package manager"
            ;;
    esac
    echo "Detected package manager: $PKG_MANAGER"
}

# Install kernel for libkrun VM from local build or download.
# For local development: copies vmlinux from git/sandbox-kernel/linux-stable to ~/.epkg/envs/self/boot/
# Only works when building for host architecture.
install_kernel_for_libkrun() {
    local arch="$1"

    # Only needed for current arch
    [[ "$arch" != $(arch) ]] && return

    # Check if libkrun is supported for this architecture
    case "$arch" in
        x86_64|aarch64|riscv64)
            ;;
        loongarch64)
            echo "Warning: libkrun not available for loongarch64, VM feature won't be usable" >&2
            return 0
            ;;
        *)
            echo "Warning: libkrun not available for $arch, VM feature won't be usable" >&2
            return 0
            ;;
    esac

    # Check if kernel already exists
    local self_boot_vmlinux="${HOME}/.epkg/envs/self/boot/vmlinux"
    if [[ -f "$self_boot_vmlinux" ]]; then
        return 0
    fi

    # Try to install from local build (sandbox-kernel)
    local vmlinux="$PROJECT_ROOT/git/sandbox-kernel/linux-stable/vmlinux"
    if [[ -f "$vmlinux" ]]; then
        local self_boot_dir="${HOME}/.epkg/envs/self/boot"
        mkdir -p "$self_boot_dir"

        # Get kernel version for naming
        local version
        version=$(strings "$vmlinux" 2>/dev/null | grep -m1 "Linux version " | awk '{print $3}')
        version=$(echo "$version" | tr -cd '0-9.-')

        if [[ -n "$version" ]]; then
            local named_vmlinux="${self_boot_dir}/vmlinux-${version}-${arch}"
            cp -l "$vmlinux" "$named_vmlinux" 2>/dev/null || cp "$vmlinux" "$named_vmlinux"
            ln -sf "vmlinux-${version}-${arch}" "$self_boot_vmlinux"
            echo "Installed kernel from local build: vmlinux-${version}-${arch}"
        else
            cp -l "$vmlinux" "$self_boot_vmlinux" 2>/dev/null || cp "$vmlinux" "$self_boot_vmlinux"
            echo "Installed kernel from local build: vmlinux"
        fi
        return 0
    fi

    echo "Note: No kernel found for libkrun VM. Run 'epkg self install' to download one." >&2
    echo "      Or build one with: cd git/sandbox-kernel && ./scripts/build.sh $arch" >&2
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
        local src_rc="$PROJECT_ROOT/assets/shell/epkg.sh"
        local dst_rc="$DEV_ENV_SRC_DIR/assets/shell/epkg.sh"
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
    ar rs src/liblua.a src/musl_compat.o || echo "Failed to add musl_compat.o to liblua.a"
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
    local common_packages
    packages=""
    update_cmd=""
    install_cmd=""

    # Set common packages based on package manager
    case "$PKG_MANAGER" in
        brew)
            # macOS: tar is built-in, git/curl/jq often pre-installed or don't need sudo
            common_packages="wget jq"
            ;;
        *)
            # Linux and others
            common_packages="git wget curl jq tar"
            ;;
    esac

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
            # Check if we're on MSYS2/Windows
            if [[ "$OS_FAMILY" == "windows" ]]; then
                # MSYS2 packages: MinGW-w64 toolchain for native Windows builds
                # Note: install individual packages instead of mingw-w64-x86_64-toolchain group
                # to avoid interactive selection prompt
                common_packages="git wget curl jq tar"
                case "$mode" in
                    dev|crossdev)
                        packages="base-devel mingw-w64-x86_64-gcc mingw-w64-x86_64-make mingw-w64-x86_64-pkgconf mingw-w64-x86_64-rust mingw-w64-x86_64-openssl"
                        ;;
                    sandbox)
                        packages=""
                        ;;
                    qemu)
                        packages="mingw-w64-x86_64-qemu"
                        ;;
                    *)
                        packages=""
                        ;;
                esac
            else
                # Arch Linux
                common_packages="git wget curl jq tar"
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
            fi
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
    elif [[ "$PKG_MANAGER" == "brew" ]]; then
        # Homebrew doesn't use sudo for installs
        SUDO=""
    elif [[ "$PKG_MANAGER" == "pacman" && "$OS_FAMILY" == "windows" ]]; then
        # MSYS2 pacman doesn't use sudo
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
    mkdir -p git
    cd git || exit

    clone_or_update_repo "https://gitee.com/wu_fengguang/rpm-rs"
    clone_or_update_repo "https://gitee.com/wu_fengguang/resolvo"
    clone_or_update_repo "https://gitee.com/wu_fengguang/elf-loader"
    clone_or_update_repo "https://gitee.com/wu_fengguang/libkrun"
    clone_or_update_repo "https://gitee.com/wu_fengguang/imago"
    clone_or_update_repo "https://gitee.com/wu_fengguang/sandbox-kernel"

    if [[ "$mode" = "crossdev" ]]; then
        clone_or_update_repo "https://github.com/tpoechtrager/osxcross.git"
        (
            cd osxcross/tarballs
            wget https://github.com/joseluisq/macosx-sdks/releases/download/26.1/sha256sum.txt
            wget https://github.com/joseluisq/macosx-sdks/releases/download/26.1/MacOSX26.1.sdk.tar.xz
            sha256sum -c sha256sum.txt
        )
    else
        true
    fi
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
    # Force static CRT linkage for musl targets
    local common_opts="-C target-feature=+crt-static"
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

# Add musl compatibility shims to mlua-sys's Lua library
# This is needed because mlua-sys builds its own Lua which may reference *64 functions
# that don't exist in musl libc (musl always uses 64-bit file operations)
add_musl_compat_to_mlua() {
    local arch="$1"
    local rust_target=$(get_rust_target "$arch")
    local build_dir="$2"  # "debug" or "release"

    # Find mlua-sys output directory
    local mlua_lib_dir=$(find "$PROJECT_ROOT/target/$rust_target/$build_dir/build" -type d -name "out" -path "*mlua-sys*" 2>/dev/null | head -1)
    if [[ -z "$mlua_lib_dir" ]]; then
        return 0  # mlua-sys not built yet or not found
    fi

    local mlua_lib="$mlua_lib_dir/lib/liblua5.4.a"
    if [[ ! -f "$mlua_lib" ]]; then
        return 0  # library not found
    fi

    # Check if musl_compat.o is already in the library
    if ar t "$mlua_lib" 2>/dev/null | grep -q "musl_compat.o"; then
        return 0  # already added
    fi

    # Compile musl_compat.c for this architecture
    local compiler=$(get_c_compiler "$arch" "musl")
    local musl_compat_o="$mlua_lib_dir/musl_compat.o"

    echo "Adding musl compatibility shims to mlua-sys's Lua library..."
    $compiler -O2 -Wall -fPIC -D_FILE_OFFSET_BITS=64 -U_LARGEFILE64_SOURCE \
        -c -o "$musl_compat_o" "$PROJECT_ROOT/lib/musl_compat.c" || {
        echo "Warning: Failed to compile musl_compat.c for $arch" >&2
        return 1
    }

    # Add to the library
    ar rs "$mlua_lib" "$musl_compat_o" || {
        echo "Warning: Failed to add musl_compat.o to mlua-sys library" >&2
        return 1
    }

    echo "Added musl compatibility shims to $mlua_lib"
    return 0
}

# Check if libkrun feature should be enabled by default for a given platform
# Returns 0 (true) if libkrun should be enabled, 1 (false) otherwise
#
# Platform support matrix:
# - Linux: x86_64, aarch64, riscv64 (all enabled)
# - macOS: aarch64 only (Hypervisor.framework limitation)
# - Windows: not supported
should_enable_libkrun() {
    local arch="$1"
    local os="$2"  # linux, darwin, windows

    case "$os" in
        linux)
            case "$arch" in
                x86_64|aarch64|riscv64)
                    return 0  # libkrun supported
                    ;;
                loongarch64)
                    return 1  # libkrun not available
                    ;;
                *)
                    return 1
                    ;;
            esac
            ;;
        darwin)
            # libkrun on macOS only supports aarch64 (Hypervisor.framework limitation)
            # x86_64 macOS is not supported by libkrun
            case "$arch" in
                aarch64) return 0 ;;
                *) return 1 ;;
            esac
            ;;
        windows)
            # libkrun not supported on Windows
            return 1
            ;;
        *)
            return 1
            ;;
    esac
}

# Get default Cargo features for a given architecture and OS
# Returns feature string (e.g., "libkrun" or "")
get_default_cargo_features() {
    local arch="$1"
    local os="$2"

    if should_enable_libkrun "$arch" "$os"; then
        echo "libkrun"
    else
        echo ""
    fi
}

# Detect OS from Rust target triple
detect_os_from_target() {
    local target="$1"
    if [[ "$target" == *"apple-darwin"* ]]; then
        echo "darwin"
    elif [[ "$target" == *"pc-windows-"* ]]; then
        echo "windows"
    else
        echo "linux"
    fi
}

# Build static binary for a specific architecture with mode (debug/release)
# This is the DEFAULT and RECOMMENDED build method for all platforms
# - Produces self-contained, portable binaries
# - libkrun auto-enabled for supported platforms (see matrix above)
# Usage: build_static <arch> <mode>
build_static() {
    local arch=$(get_arch "$1")
    local mode="$2"
    local rust_target
    local rustflags
    local cargo_features=""

    # Detect host OS and determine target
    local host_os=$(uname -s)
    if [[ "$host_os" == "Darwin" ]]; then
        # On macOS, build for native macOS target
        rust_target=$(get_rust_target_for_platform "$arch" "darwin")
        rustflags=""
        echo "Building for macOS ($arch)..."
    else
        # On Linux, build for musl target (static Linux binary)
        rust_target=$(get_rust_target "$arch")
        rustflags=$(get_rustflags "$arch")
    fi

    # Auto-enable libkrun if FEATURES="auto" and platform supports it
    # Note: FEATURES="" means user explicitly wants no features
    if [[ "$FEATURES" == "auto" ]]; then
        local target_os="linux"
        [[ "$host_os" == "Darwin" ]] && target_os="darwin"
        if should_enable_libkrun "$arch" "$target_os"; then
            cargo_features="libkrun"
            echo "Auto-enabling libkrun feature for $arch $target_os"
        fi
    else
        cargo_features="${FEATURES:-}"
    fi

    # Warn if user tries to force libkrun on unsupported platform
    if [[ "$cargo_features" == *"libkrun"* ]]; then
        local target_os="linux"
        [[ "$host_os" == "Darwin" ]] && target_os="darwin"
        if ! should_enable_libkrun "$arch" "$target_os"; then
            echo "Warning: libkrun is not supported on $arch $target_os, build may fail"
        fi
    fi

    echo "Building $arch binary ($mode)..."

    # Detect host OS
    local host_os=$(uname -s)
    local is_macos=false
    [[ "$host_os" == "Darwin" ]] && is_macos=true

    # On macOS, we don't need Lua (it's only for Linux RPM scriptlets)
    # On Linux, we need Lua for RPM scriptlet support
    if [[ "$is_macos" == "false" ]]; then
        export LUA_LIB_NAME=lua
        export LUA_LIB="$PROJECT_ROOT/target/lua-musl-$arch"
        export LUA_LINK=static
        export LUA_NO_PKG_CONFIG=1
    fi

    # Export linker variable for this architecture
    export_linker_var "$arch"

    # Set C compiler for mlua-sys build
    # On macOS, use clang; on Linux, use musl-gcc
    if [[ "$is_macos" == "true" ]]; then
        export CC="clang"
        export CFLAGS=""
    else
        export CC=$(get_c_compiler "$arch" "musl")
        export CFLAGS="-D_FILE_OFFSET_BITS=64 -U_LARGEFILE64_SOURCE"
        # Set target-specific CFLAGS for cc crate (hyphens to underscores)
        local target_var="${rust_target//-/_}"
        export "CFLAGS_${target_var}"="$CFLAGS"
        export "CC_${target_var}"="$CC"
    fi

    if [[ -n "$rustflags" ]]; then
        export RUSTFLAGS="$rustflags"
    fi

    # If we're building with libkrun support, ensure kernel is available.
    # Note: On macOS, libkrun doesn't need a kernel image (it uses hypervisor framework)
    if [[ "$cargo_features" == *"libkrun"* ]]; then
        if [[ "$is_macos" == "false" ]]; then
            install_kernel_for_libkrun "$arch"
        fi
    fi

    # Build the binary (optionally with extra Cargo features)
    local cargo_feature_args=()
    if [[ -n "$cargo_features" ]]; then
        cargo_feature_args=(--features "$cargo_features")
    fi

    local build_dir
    if [[ "$mode" == "release" ]]; then
        build_dir="release"
        local cargo_args=(--release)
    else
        build_dir="debug"
        local cargo_args=()
    fi

    # For musl targets on Linux, if the compiler is not musl-gcc (e.g., using glibc cross-compiler),
    # we need to add musl compatibility shims to mlua-sys's Lua library.
    # This is because mlua-sys builds its own Lua which may reference *64 functions
    # that don't exist in musl libc.
    # We pre-build mlua-sys first, add shims, then build everything else.
    # Note: On macOS, we don't need this since we don't use Lua.
    if [[ "$is_macos" == "false" ]] && [[ "$CC" != "musl-gcc" ]]; then
        # Pre-build mlua (and mlua-sys) to generate Lua library before main build
        # We build just the lua-related deps first so we can add musl shims
        echo "Pre-building mlua for $arch to add musl compatibility shims..."
        # Build mlua and its dependencies (including mlua-sys) only
        cargo build --target "$rust_target" --ignore-rust-version "${cargo_args[@]}" --package mlua 2>&1 || true

        # Add musl compatibility shims to mlua-sys's Lua library
        add_musl_compat_to_mlua "$arch" "$build_dir"

        # Now build everything (with shims already in place)
        cargo build --target "$rust_target" --ignore-rust-version "${cargo_args[@]}" "${cargo_feature_args[@]}"
    else
        cargo build --target "$rust_target" --ignore-rust-version "${cargo_args[@]}" "${cargo_feature_args[@]}"
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

# Build development binary (LEGACY - dynamic linking)
# Prefer `static` or `static-debug` for production use
build() {
    echo "Building debug binary (dynamic linking - legacy)..."

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

    # Auto-enable libkrun for supported platforms
    # Note: FEATURES="" means user explicitly wants no features
    local cargo_features=""
    local current_arch=$(detect_native_arch)
    if [[ "$FEATURES" == "auto" ]]; then
        if should_enable_libkrun "$current_arch" "$OS_FAMILY"; then
            cargo_features="libkrun"
            echo "Auto-enabling libkrun feature for $OS_FAMILY $current_arch"
        fi
    else
        cargo_features="${FEATURES:-}"
    fi

    local cargo_feature_args=()
    if [[ -n "$cargo_features" ]]; then
        cargo_feature_args=(--features "$cargo_features")
    fi

    cargo build --ignore-rust-version "${cargo_feature_args[@]}"

    echo "Development build completed. Binary is in $PROJECT_ROOT/target/debug/$BINARY_NAME"

    install_to_dev_env "$PROJECT_ROOT/target/debug/$BINARY_NAME"
}

# Build release binary (LEGACY - dynamic linking)
# Prefer `static-release` for production use
build_release() {
    echo "Building release binary (dynamic linking - legacy)..."

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

    # Auto-enable libkrun for supported platforms
    # Note: FEATURES="" means user explicitly wants no features
    local cargo_features=""
    local current_arch=$(detect_native_arch)
    if [[ "$FEATURES" == "auto" ]]; then
        if should_enable_libkrun "$current_arch" "$OS_FAMILY"; then
            cargo_features="libkrun"
            echo "Auto-enabling libkrun feature for $OS_FAMILY $current_arch"
        fi
    else
        cargo_features="${FEATURES:-}"
    fi

    local cargo_feature_args=()
    if [[ -n "$cargo_features" ]]; then
        cargo_feature_args=(--features "$cargo_features")
    fi

    cargo build --release --ignore-rust-version "${cargo_feature_args[@]}"

    echo "Release build completed. Binary is in $PROJECT_ROOT/target/release/$BINARY_NAME"

    install_to_dev_env "$PROJECT_ROOT/target/release/$BINARY_NAME"
}

# Cross-compilation to macOS
# Note: Lua is only needed for Linux RPM scriptlets (disabled for macOS)
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

    # Auto-enable libkrun for macOS if not explicitly set
    # Note: FEATURES="" means user explicitly wants no features
    local cargo_features=""
    if [[ "$FEATURES" == "auto" ]]; then
        if should_enable_libkrun "$arch" "darwin"; then
            cargo_features="libkrun"
            echo "Auto-enabling libkrun feature for macOS $arch"
        fi
    else
        cargo_features="${FEATURES:-}"
    fi

    local cargo_feature_args=()
    if [[ -n "$cargo_features" ]]; then
        cargo_feature_args=(--features "$cargo_features")
    fi

    cargo build --release --target "$target" --ignore-rust-version "${cargo_feature_args[@]}"

    echo "Cross-compilation to macOS completed. Binary is in target/$target/release/$BINARY_NAME"
}

# Cross-compilation to Windows
# Note: Lua is only needed for Linux RPM scriptlets (disabled for Windows)
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

    # libkrun is not supported on Windows
    # Note: FEATURES="" means user explicitly wants no features
    local cargo_features=""
    if [[ "$FEATURES" == "auto" ]]; then
        # No features for Windows (libkrun not supported)
        cargo_features=""
    else
        cargo_features="${FEATURES:-}"
        if [[ "$cargo_features" == *"libkrun"* ]]; then
            echo "Warning: libkrun is not supported on Windows, ignoring libkrun feature"
            cargo_features="${cargo_features//libkrun/}"
            cargo_features="${cargo_features//,,/,}"  # Remove double commas
            cargo_features="${cargo_features#,}"      # Remove leading comma
            cargo_features="${cargo_features%,}"      # Remove trailing comma
        fi
    fi

    local cargo_feature_args=()
    if [[ -n "$cargo_features" ]]; then
        cargo_feature_args=(--features "$cargo_features")
    fi

    cargo build --release --target "$target" --ignore-rust-version "${cargo_feature_args[@]}"

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

# Setup environment for cross-compilation (macOS/Windows only, not for Linux)
setup_cross_env() {
    local target="$1"
    local arch="${target%%-*}"
    local os=""
    if [[ "$target" == *"apple-darwin"* ]]; then
        os="darwin"
    elif [[ "$target" == *"pc-windows-"* ]]; then
        os="windows"
    else
        echo "Error: setup_cross_env only supports macOS and Windows targets, got: $target" >&2
        exit 1
    fi

    # Clear previous environment
    unset CC CFLAGS LUA_LIB LUA_LIB_NAME LUA_LINK LUA_NO_PKG_CONFIG RUSTFLAGS
    unset CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER
    unset CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER
    unset CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER
    unset CARGO_TARGET_AARCH64_PC_WINDOWS_MSVC_LINKER
    unset CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER
    unset CARGO_TARGET_AARCH64_PC_WINDOWS_GNU_LINKER

    # No Lua needed for macOS/Windows (only Linux RPM scriptlets use it)
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
    esac
}

cmd="${1:-build}"

# Main dispatcher
case $cmd in
    lua|build_lua_lib)
        build_lua_lib "$2"
        ;;
    build)
        # Default: static debug build (recommended)
        build_static "$2" debug
        ;;
    release)
        # Default: static release build (recommended)
        build_static "$2" release
        ;;
    static|static-debug)
        # Explicit static debug build
        build_static "$2" debug
        ;;
    static-libkrun)
        # Build static debug binary with libkrun integrated.
        # Note: libkrun is now auto-enabled for supported platforms,
        # so this command is mainly for explicit usage documentation.
        # Additional features can be supplied via FEATURES.
        arch=$(get_arch "$2")
        # Append libkrun to FEATURES (whether or not it's empty)
        if [[ -n "$FEATURES" && "$FEATURES" != "auto" ]]; then
            FEATURES="libkrun,$FEATURES"
        else
            FEATURES="libkrun"
        fi
        build_static "$arch" debug
        ;;
    static-release)
        build_static "$2" release
        ;;
    dynamic-build)
        # Legacy: dynamic linking (not recommended)
        build
        ;;
    dynamic-release)
        # Legacy: dynamic linking (not recommended)
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
        # Install VMM (QEMU + virtiofsd) host dependencies for --isolate=vm
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
        echo "Commands (static linking - DEFAULT):"
        echo "  build [<arch>]                       Build static debug binary (default)"
        echo "  release [<arch>]                     Build static release binary"
        echo "  static [<arch>]                      (alias for 'build')"
        echo "  static-debug [<arch>]                Build static debug binary (explicit, same with 'static')"
        echo "  static-release [<arch>]              Build static release binary (explicit)"
        echo "  static-libkrun [<arch>]              Build static debug with libkrun (auto-enabled anyway in some platform/archs)"
        echo ""
        echo "Commands (cross-platform builds - Linux x86_64 host only):"
        echo "  cross-macos [<arch>]                 Cross-compile to macOS (aarch64/x86_64)"
        echo "  cross-windows [<arch>]               Cross-compile to Windows (x86_64/aarch64)"
        echo ""
        echo "Commands (dynamic linking - LEGACY):"
        echo "  dynamic-build                        Build dynamic debug binary (not recommended)"
        echo "  dynamic-release                      Build dynamic release binary (not recommended)"
        echo ""
        echo "Other commands:"
        echo "  lua [<arch>]                         Build Lua library for architecture"
        echo "  dev-depends                          Install development dependencies (current arch only)"
        echo "  crossdev-depends                     Install cross-development dependencies (all arch cross-compilers)"
        echo "  clone-repos                          Clone required repositories (rpm-rs, resolvo, elf-loader)"
        echo "  qemu-pkgs                            Install qemu dependency packages"
        echo "  sandbox-pkgs                         Install sandbox dependency packages"
        echo "  test                                 Run module-level unit tests"
        echo "  clean                                Clean build artifacts"
        echo "  clean_all                            Clean all artifacts and distribution files"
        echo ""
        echo "Build types:"
        echo "  - Native build:      build on host arch (supported on all platforms/archs)"
        echo "  - Cross-arch build:  build for different Linux arch on x86_64 Linux host"
        echo "  - Cross-platform:    build macOS/Windows binary on x86_64 Linux host"
        echo ""
        echo "Note: Cross builds require crossdev-depends, mainly supported on Debian distros"
        echo ""
        echo "Architectures: x86_64, aarch64, riscv64, loongarch64"
        echo ""
        echo "libkrun auto-enable matrix:"
        echo "  Linux x86_64/aarch64/riscv64: enabled"
        echo "  macOS (all archs):            enabled"
        echo "  Linux loongarch64:            disabled (not supported)"
        echo "  Windows:                      disabled (not supported)"
        echo ""
        echo "Set FEATURES to override default features."
        exit 1
        ;;
esac
