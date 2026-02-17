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

# Default target (development build for local use)
build:
	@$(PROJECT_ROOT)/bin/make.sh build

# Release build target
release:
	@$(PROJECT_ROOT)/bin/make.sh release

# Static build for detected host architecture
static: static-$(HOST_ARCH)

# Install development dependencies (current arch only)
dev-depends:
	@$(PROJECT_ROOT)/bin/make.sh dev-depends

# Install release dependencies (all arch cross-compilers)
crossdev-depends:
	@$(PROJECT_ROOT)/bin/make.sh crossdev-depends


# Build static binaries for all architectures (sequentially to avoid Cargo lock conflicts)
static-all:
	$(MAKE) static-x86_64
	$(MAKE) static-aarch64
	$(MAKE) static-riscv64
	$(MAKE) static-loongarch64

# Build static binary for a specific architecture
define build_static
static-$(1): $(PROJECT_ROOT)/target/lua-musl-$(1)/liblua.a
	@$(PROJECT_ROOT)/bin/make.sh static $(1)
endef

# Define static targets for each architecture
$(eval $(call build_static,x86_64))
$(eval $(call build_static,aarch64))
$(eval $(call build_static,riscv64))
$(eval $(call build_static,loongarch64))


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

.PHONY: dev-depends crossdev-depends build release static static-all static-x86_64 static-aarch64 static-riscv64 static-loongarch64 test clean clean-all
