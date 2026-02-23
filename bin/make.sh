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
BINARY_NAME=epkg

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
    if [[ -f /etc/os-release ]]; then
        OS_ID=$(grep -E '^ID=' /etc/os-release | cut -d= -f2 | tr -d '"')
        OS_VERSION=$(grep -E '^VERSION_ID=' /etc/os-release | cut -d= -f2 | tr -d '"')
    else
        OS_ID="unknown"
        OS_VERSION="unknown"
    fi
}

has_cmd()
{
    command -v "$1" >/dev/null
}

# Detect package manager
detect_package_manager() {
    # Detect available package manager
    if   has_cmd apt;       then PKG_MANAGER="apt"
    elif has_cmd dnf;       then PKG_MANAGER="dnf"
    elif has_cmd yum;       then PKG_MANAGER="yum"
    elif has_cmd zypper;    then PKG_MANAGER="zypper"
    elif has_cmd pacman;    then PKG_MANAGER="pacman"
    elif has_cmd apk;       then PKG_MANAGER="apk"
    else
        PKG_MANAGER="unknown"
        echo "Warning: Could not detect package manager"
        exit 1
    fi
    echo "Detected package manager: $PKG_MANAGER"
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
    [ -f "$tarball" ] || wget -q "https://www.lua.org/ftp/lua-$LUA_VERSION.tar.gz" -O "$tarball"
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
    local common_packages="git wget"
    packages=""
    update_cmd=""
    install_cmd=""

    case "$PKG_MANAGER" in
        apt)
            update_cmd="apt-get update"
            install_cmd="apt-get install -y"
            packages="rustup build-essential libssl-dev musl-tools liblua5.4-dev"
            if [[ "$mode" == "crossdev" ]]; then
                packages="$packages gcc-aarch64-linux-gnu gcc-riscv64-linux-gnu gcc-loongarch64-linux-gnu"
            fi
            ;;
        dnf|yum)
            update_cmd="$PKG_MANAGER update -y"
            install_cmd="$PKG_MANAGER install -y"
            # For dnf/yum, install cargo instead of rustup (rustup can be installed via curl if needed)
            packages="cargo gcc openssl-devel musl-gcc libstdc++-static lua-devel"
            # Crossdev packages may not be available on all distros
            # Note: crossdev mode not supported for dnf/yum - cross-compilation tools not packaged
            ;;
        zypper)
            update_cmd="zypper refresh"
            install_cmd="zypper install -y"
            packages="rustup gcc openssl-devel musl-gcc lua-devel"
            # Crossdev packages may not be available
            ;;
        pacman)
            update_cmd="pacman -Sy"
            install_cmd="pacman -S --noconfirm"
            packages="rustup base-devel openssl musl lua"
            # Crossdev packages: aarch64-linux-gnu-gcc, riscv64-linux-gnu-gcc, loongarch64-linux-gnu-gcc (from AUR)
            ;;
        apk)
            update_cmd="apk update"
            install_cmd="apk add"
            packages="rustup build-base openssl-dev musl-dev lua-dev"
            # Crossdev packages: cross-compile tools may be in community repos
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

    # Install Rust toolchain
    install_rust_toolchain "$mode" "$current_arch"
}

# Clone required repositories (without building elf-loader dependencies)
clone_repos() {
    clone_or_update_repo "https://gitee.com/wu_fengguang/rpm-rs"
    clone_or_update_repo "https://gitee.com/wu_fengguang/resolvo"
    clone_or_update_repo "https://gitee.com/wu_fengguang/elf-loader"
}

# Unified dependency installer
install_depends() {
    install_packages "$@"
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

    # Build the binary
    if [[ "$mode" == "release" ]]; then
        cargo build --release --target "$rust_target" --ignore-rust-version
        local build_dir="release"
    else
        cargo build --target "$rust_target" --ignore-rust-version
        local build_dir="debug"
    fi

    # Deploy only for release mode
    if [[ "$mode" == "release" ]]; then
        mkdir -p "$OUTPUT_DIR"
        # Copy binary, handling "Text file busy" error
        safe_cp "target/$rust_target/$build_dir/$BINARY_NAME" "$OUTPUT_DIR/$BINARY_NAME-$arch"
        echo "Generating checksum for $arch binary..."
        cd "$OUTPUT_DIR"
        sha256sum "$BINARY_NAME-$arch" > "$BINARY_NAME-$arch.sha256"
        echo "$arch release completed: $PROJECT_ROOT/$OUTPUT_DIR/$BINARY_NAME-$arch"
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

    # Set up static Lua linking for glibc
    setup_glibc_lua

    cargo build --ignore-rust-version

    echo "Development build completed. Binary is in $PROJECT_ROOT/target/debug/$BINARY_NAME"

    install_to_dev_env "$PROJECT_ROOT/target/debug/$BINARY_NAME"
}

# Build release binary
build_release() {
    echo "Building release binary..."

    # Set up static Lua linking for glibc
    setup_glibc_lua

    cargo build --release --ignore-rust-version

    echo "Release build completed. Binary is in $PROJECT_ROOT/target/release/$BINARY_NAME"

    install_to_dev_env "$PROJECT_ROOT/target/release/$BINARY_NAME"
}

# Run tests (module-level unit tests)
run_tests() {
    RUSTFLAGS="-A dead_code -A unused_imports -A unused_variables" cargo test
}

HOST_ARCH=$(arch)

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
        local arch=$(arch)
        echo "Auto-detected architecture: $arch" >&2
        echo "$arch"
    else
        echo "$provided_arch"
    fi
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
    clone-repos)
        clone_repos
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
        echo "  dev-depends                          Install development dependencies (current arch only)"
        echo "  crossdev-depends                     Install cross-development dependencies (all arch cross-compilers)"
        echo "  clone-repos                          Clone required repositories (rpm-rs, resolvo, elf-loader)"
        echo "  test                                 Run module-level unit tests"
        echo "  clean                                Clean build artifacts"
        echo "  clean_all                            Clean all artifacts and distribution files"
        echo ""
        echo "Supported architectures: x86_64, aarch64, riscv64, loongarch64"
        exit 1
        ;;
esac
