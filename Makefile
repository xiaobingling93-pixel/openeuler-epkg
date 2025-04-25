# Makefile for building and deploying static binaries for multiple architectures

# Variables
RUST_TARGET_X86_64 := x86_64-unknown-linux-musl
RUST_TARGET_AARCH64 := aarch64-unknown-linux-musl
RUST_TARGET_RISCV64 := riscv64gc-unknown-linux-musl
RUST_TARGET_LOONGARCH64 := loongarch64-unknown-linux-gnu
BINARY_NAME := epkg
OUTPUT_DIR := dist

# Detect OS and package manager
OS_ID := $(shell grep -E '^ID=' /etc/os-release | cut -d= -f2)
OS_VERSION := $(shell grep -E '^VERSION_ID=' /etc/os-release | cut -d= -f2 | tr -d '"')

# Default target (development build for local use)
build:
	@cargo build
	@echo "Development build completed. Binary is in target/debug/$(BINARY_NAME)"

# Install dependencies and set up Rust toolchain
install-depends:
	@echo "Detected OS: $(OS_ID) $(OS_VERSION)"
	@echo "Installing dependencies..."
ifeq ($(OS_ID),$(filter $(OS_ID),debian ubuntu))
	sudo apt-get update
	sudo apt-get install -y rustup build-essential libssl-dev musl-tools gcc-aarch64-linux-gnu gcc-riscv64-linux-gnu gcc-loongarch64-linux-gnu
else ifeq ($(OS_ID),fedora)
	# no rustup!
	$(error Unsupported OS: $(OS_ID))
	sudo dnf install -y gcc openssl-devel musl-gcc musl-libc-static gcc-aarch64-linux-gnu gcc-riscv64-linux-gnu
else
	$(error Unsupported OS: $(OS_ID))
endif
	@echo "Installing Rust toolchain..."
	rustup default stable
	rustup target add $(RUST_TARGET_X86_64)
	rustup target add $(RUST_TARGET_AARCH64)
	rustup target add $(RUST_TARGET_RISCV64)
	rustup target add $(RUST_TARGET_LOONGARCH64)
	@echo "Installation complete!"

# Build release binaries for all architectures
release-all: release-x86_64 release-aarch64 release-riscv64 release-loongarch64

# Build x86_64 binary
release-x86_64:
	@echo "Building x86_64 binary..."
	cargo build --release --target $(RUST_TARGET_X86_64)
	@mkdir -p $(OUTPUT_DIR)
	cp target/$(RUST_TARGET_X86_64)/release/$(BINARY_NAME) $(OUTPUT_DIR)/$(BINARY_NAME)-x86_64
	@echo "Generating checksum for x86_64 binary..."
	cd $(OUTPUT_DIR) && sha256sum $(BINARY_NAME)-x86_64 > $(BINARY_NAME)-x86_64.sha256
	@echo "x86_64 release completed: $(OUTPUT_DIR)/$(BINARY_NAME)-x86_64"

# Build aarch64 binary
release-aarch64:
	@echo "Building aarch64 binary..."
	CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc \
	RUSTFLAGS="-C linker=aarch64-linux-gnu-gcc -C link-arg=-lgcc -C link-arg=-lc" \
	cargo build --release --target $(RUST_TARGET_AARCH64)
	@mkdir -p $(OUTPUT_DIR)
	cp target/$(RUST_TARGET_AARCH64)/release/$(BINARY_NAME) $(OUTPUT_DIR)/$(BINARY_NAME)-aarch64
	@echo "Generating checksum for aarch64 binary..."
	cd $(OUTPUT_DIR) && sha256sum $(BINARY_NAME)-aarch64 > $(BINARY_NAME)-aarch64.sha256
	@echo "aarch64 release completed: $(OUTPUT_DIR)/$(BINARY_NAME)-aarch64"

# Build RISC-V binary
release-riscv64:
	@echo "Building RISC-V binary..."
	CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_MUSL_LINKER=riscv64-linux-gnu-gcc \
	RUSTFLAGS="-C linker=riscv64-linux-gnu-gcc -C link-arg=-lgcc -C link-arg=-lm -C link-arg=-lc" \
	cargo build --release --target $(RUST_TARGET_RISCV64)
	@mkdir -p $(OUTPUT_DIR)
	cp target/$(RUST_TARGET_RISCV64)/release/$(BINARY_NAME) $(OUTPUT_DIR)/$(BINARY_NAME)-riscv64
	@echo "Generating checksum for RISC-V binary..."
	cd $(OUTPUT_DIR) && sha256sum $(BINARY_NAME)-riscv64 > $(BINARY_NAME)-riscv64.sha256
	@echo "RISC-V release completed: $(OUTPUT_DIR)/$(BINARY_NAME)-riscv64"

# Build LoongArch binary
release-loongarch64:
	@echo "Building LoongArch binary..."
	CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_GNU_LINKER=loongarch64-linux-gnu-gcc \
	cargo build --release --target $(RUST_TARGET_LOONGARCH64)
	@mkdir -p $(OUTPUT_DIR)
	cp target/$(RUST_TARGET_LOONGARCH64)/release/$(BINARY_NAME) $(OUTPUT_DIR)/$(BINARY_NAME)-loongarch64
	@echo "Generating checksum for LoongArch binary..."
	cd $(OUTPUT_DIR) && sha256sum $(BINARY_NAME)-loongarch64 > $(BINARY_NAME)-loongarch64.sha256
	@echo "LoongArch release completed: $(OUTPUT_DIR)/$(BINARY_NAME)-loongarch64"

# Clean build artifacts
clean:
	cargo clean
	rm -rf $(OUTPUT_DIR)
	@echo "Cleaned build artifacts and output directory"

.PHONY: install-depends build release-all release-x86_64 release-aarch64 release-riscv64 release-loongarch64 clean
