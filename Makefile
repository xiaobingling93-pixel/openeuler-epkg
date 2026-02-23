# Makefile for building and deploying static binaries for multiple architectures

# Variables
OUTPUT_DIR := dist

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

# Detect host architecture
HOST_ARCH := $(shell uname -m | sed -e 's/amd64/x86_64/' -e 's/arm64/aarch64/')

# Default target is
# - development build (fast)
# - static build (necessary for running applets inside various env)
static: $(PROJECT_ROOT)/target/lua-musl-$(HOST_ARCH)/liblua.a
	@$(PROJECT_ROOT)/bin/make.sh static-debug $(HOST_ARCH)

# Release build target
release: $(PROJECT_ROOT)/target/lua-musl-$(HOST_ARCH)/liblua.a
	@$(PROJECT_ROOT)/bin/make.sh static-release $(HOST_ARCH)

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
	$(MAKE) release-x86_64
	$(MAKE) release-aarch64
	$(MAKE) release-riscv64
	$(MAKE) release-loongarch64

# Build release binary for a specific architecture
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


# Run tests (module-level unit tests)
test:
	@$(PROJECT_ROOT)/bin/make.sh test

# Clean build artifacts only
clean:
	@$(PROJECT_ROOT)/bin/make.sh clean

# Clean build artifacts and distribution files
clean-all:
	@$(PROJECT_ROOT)/bin/make.sh clean_all

.PHONY: dev-depends crossdev-depends clone-repos build static release release-all release-x86_64 release-aarch64 release-riscv64 release-loongarch64 test clean clean-all
