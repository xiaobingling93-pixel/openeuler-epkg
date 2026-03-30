# Makefile for building and deploying static binaries for multiple architectures

# Variables
OUTPUT_DIR := dist

# FEATURES variable for cargo features:
#   - unset or "auto": auto-enable libkrun for supported platforms (default)
#   - ""            : disable all features
#   - "libkrun"     : explicitly enable libkrun
#   - "..."         : custom features (comma-separated)
FEATURES ?= auto
export FEATURES

# Detect Makefile location and adjust paths
MAKEFILE_DIR := $(dir $(lastword $(MAKEFILE_LIST)))
ifeq ($(shell basename $(CURDIR)),src)
	# Running from src/ directory
	SRC_DIR := .
	PROJECT_ROOT := ..
else
	# Running from project root
	SRC_DIR := src
	PROJECT_ROOT := .
endif

# Detect host architecture and OS
HOST_ARCH := $(shell uname -m | sed -e 's/amd64/x86_64/' -e 's/arm64/aarch64/')
UNAME_S := $(shell uname -s)

# Default target is
# - development build (fast)
# - static build (necessary for running applets inside various env)
# - libkrun auto-enabled for supported platforms (see make.sh)
# Use FEATURES=xxx to override, e.g., FEATURES="" to disable libkrun

# Lua is only needed on Linux for RPM scriptlets
ifeq ($(UNAME_S),Linux)
static: $(PROJECT_ROOT)/target/lua-musl-$(HOST_ARCH)/liblua.a
	@$(PROJECT_ROOT)/bin/make.sh static-debug $(HOST_ARCH)
else
static:
	@$(PROJECT_ROOT)/bin/make.sh static-debug $(HOST_ARCH)
endif

# Static build with libkrun integrated (Cargo --features libkrun) and
# sandbox-kernel unpacked into the self env so the libkrun backend can run
# without extra manual steps on the host.
# Note: libkrun is auto-enabled for supported platforms, this target is
# kept for explicit usage documentation and appending extra features.
ifeq ($(UNAME_S),Linux)
static-libkrun: $(PROJECT_ROOT)/target/lua-musl-$(HOST_ARCH)/liblua.a
	@$(PROJECT_ROOT)/bin/make.sh static-libkrun $(HOST_ARCH)
else
static-libkrun:
	@$(PROJECT_ROOT)/bin/make.sh static-libkrun $(HOST_ARCH)
endif

# Release build target
# Note: libkrun auto-enabled for supported platforms (see make.sh)
ifeq ($(UNAME_S),Linux)
release: $(PROJECT_ROOT)/target/lua-musl-$(HOST_ARCH)/liblua.a
	@$(PROJECT_ROOT)/bin/make.sh static-release $(HOST_ARCH)
else
release:
	@$(PROJECT_ROOT)/bin/make.sh static-release $(HOST_ARCH)
endif

# Development build with dynamic linking, only useful for run in local host rootfs 
build:
	@$(PROJECT_ROOT)/bin/make.sh build

# Install development dependencies (current arch only)
dev-depends:
	@$(PROJECT_ROOT)/bin/make.sh dev-depends

# Install release dependencies (all arch cross-compilers)
crossdev-depends:
	@$(PROJECT_ROOT)/bin/make.sh crossdev-depends

# Clone required repositories (rpm-rs, resolvo, elf-loader)
clone-repos:
	@$(PROJECT_ROOT)/bin/make.sh clone-repos

# Build release binaries for all architectures (sequentially to avoid Cargo lock conflicts)
release-all:
	mkdir -p "$(OUTPUT_DIR)"
	$(MAKE) release-x86_64
	$(MAKE) release-aarch64
	$(MAKE) release-riscv64
	$(MAKE) release-loongarch64
	# Cross build macOS (asset names: epkg-macos-<arch>)
	$(MAKE) cross-macos-release ARCH=x86_64
	$(MAKE) cross-macos-release ARCH=aarch64
	# Cross build Windows (asset names: epkg-windows-<arch>.exe)
	# Note: only x86_64 supported; aarch64 requires mingw-w64 libraries not available in Debian
	$(MAKE) cross-windows-release

# Build release binary for a specific architecture
# Note: libkrun auto-enabled for supported platforms (see make.sh)
define build_release
release-$(1): $(PROJECT_ROOT)/target/lua-musl-$(1)/liblua.a
	@$(PROJECT_ROOT)/bin/make.sh static-release $(1)
endef

$(eval $(call build_release,x86_64))
$(eval $(call build_release,aarch64))
$(eval $(call build_release,riscv64))
$(eval $(call build_release,loongarch64))


# Build Lua library for a specific architecture
define build_lua_lib
$(PROJECT_ROOT)/target/lua-musl-$(1)/liblua.a:
	@$(PROJECT_ROOT)/bin/make.sh build_lua_lib $(1)
endef

# Define build targets for each architecture
$(eval $(call build_lua_lib,x86_64))
$(eval $(call build_lua_lib,aarch64))
$(eval $(call build_lua_lib,riscv64))
$(eval $(call build_lua_lib,loongarch64))

# Cross-compilation to macOS (default aarch64, debug mode)
cross-macos:
	@$(PROJECT_ROOT)/bin/make.sh cross-macos $(ARCH) debug

# Cross-compilation to macOS (release mode)
cross-macos-release:
	@$(PROJECT_ROOT)/bin/make.sh cross-macos $(ARCH) release

# Cross-compilation to Windows (x86_64 only; aarch64 not supported, debug mode)
# Note: Windows cross-compilation requires two build steps:
#   1. `make` - builds the Linux binary which generates init/init for the guest
#   2. `make cross-windows` - cross-compiles the Windows binary with embedded init
#
# Build and deployment chain:
#   make:
#     target/x86_64-unknown-linux-musl/debug/epkg (build output)
#       -> ~/.epkg/envs/self/usr/bin/epkg-linux-x86_64 (self environment)
#   make cross-windows:
#     target/x86_64-pc-windows-gnu/debug/epkg.exe (build output for Windows host)
#       -> ~/.epkg/envs/alpine/usr/bin/epkg (alpine environment, hardlinked)
#       -> ~/.epkg/envs/alpine/usr/bin/init (hardlink to epkg)
#       -> ~/.epkg/envs/alpine/usr/bin/vm-daemon -> epkg (symlink)
#
# Hardlink preservation:
#   - make.sh deploy uses 'cat > file' to overwrite in-place, preserving hardlinks
#   - 'epkg self install --force' should also preserve hardlinks across all envs
#   - This ensures all hardlinked copies are updated atomically
#
# The init applet in the Windows binary is the Linux guest init process,
# embedded via libkrun's embedded_init feature to run inside the Windows VM.
cross-windows:
	@$(PROJECT_ROOT)/bin/make.sh cross-windows x86_64 debug

# Cross-compilation to Windows (x86_64 only, release mode)
cross-windows-release:
	@$(PROJECT_ROOT)/bin/make.sh cross-windows x86_64 release

# Run tests (module-level unit tests)
test:
	@$(PROJECT_ROOT)/bin/make.sh test

# Clean build artifacts only
clean:
	@$(PROJECT_ROOT)/bin/make.sh clean

# Clean build artifacts and distribution files
clean-all:
	@$(PROJECT_ROOT)/bin/make.sh clean_all

.PHONY: dev-depends crossdev-depends clone-repos build static release release-all release-x86_64 release-aarch64 release-riscv64 release-loongarch64 cross-macos cross-macos-release cross-windows cross-windows-release test clean clean-all
