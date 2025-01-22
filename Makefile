# Makefile for building and deploying static binaries for multiple architectures

# Variables
RUST_TARGET_X86_64 := x86_64-unknown-linux-musl
RUST_TARGET_AARCH64 := aarch64-unknown-linux-musl
RUST_TARGET_RISCV64 := riscv64gc-unknown-linux-musl
RUST_TARGET_LOONGARCH64 := loongarch64-unknown-linux-gnu
BINARY_NAME := epkg
OUTPUT_DIR := dist
CHECKSUM_FILE := $(OUTPUT_DIR)/checksums.sha256

# Detect OS and package manager
OS_ID := $(shell grep -E '^ID=' /etc/os-release | cut -d= -f2)
OS_VERSION := $(shell grep -E '^VERSION_ID=' /etc/os-release | cut -d= -f2 | tr -d '"')

# Install dependencies and set up Rust toolchain
install:
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
build: build-x86_64 build-aarch64 build-riscv64 build-loongarch64

# Build x86_64 binary
build-x86_64:
	@echo "Building x86_64 binary..."
	cargo build --release --target $(RUST_TARGET_X86_64)
	@mkdir -p $(OUTPUT_DIR)
	cp target/$(RUST_TARGET_X86_64)/release/$(BINARY_NAME) $(OUTPUT_DIR)/$(BINARY_NAME)-x86_64
	@echo "x86_64 binary built: $(OUTPUT_DIR)/$(BINARY_NAME)-x86_64"

# Build aarch64 binary
build-aarch64:
	@echo "Building aarch64 binary..."
	CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc \
	cargo build --release --target $(RUST_TARGET_AARCH64)
	@mkdir -p $(OUTPUT_DIR)
	cp target/$(RUST_TARGET_AARCH64)/release/$(BINARY_NAME) $(OUTPUT_DIR)/$(BINARY_NAME)-aarch64
	@echo "aarch64 binary built: $(OUTPUT_DIR)/$(BINARY_NAME)-aarch64"

# Build RISC-V binary
build-riscv64:
	@echo "Building RISC-V binary..."
	CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_MUSL_LINKER=riscv64-linux-gnu-gcc \
	cargo build --release --target $(RUST_TARGET_RISCV64)
	@mkdir -p $(OUTPUT_DIR)
	cp target/$(RUST_TARGET_RISCV64)/release/$(BINARY_NAME) $(OUTPUT_DIR)/$(BINARY_NAME)-riscv64
	@echo "RISC-V binary built: $(OUTPUT_DIR)/$(BINARY_NAME)-riscv64"

# Build LoongArch binary
build-loongarch64:
	@echo "Building LoongArch binary..."
	CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_GNU_LINKER=loongarch64-linux-gnu-gcc \
	cargo build --release --target $(RUST_TARGET_LOONGARCH64)
	@mkdir -p $(OUTPUT_DIR)
	cp target/$(RUST_TARGET_LOONGARCH64)/release/$(BINARY_NAME) $(OUTPUT_DIR)/$(BINARY_NAME)-loongarch64
	@echo "LoongArch binary built: $(OUTPUT_DIR)/$(BINARY_NAME)-loongarch64"

# Generate SHA-256 checksums for all binaries
checksums:
	@echo "Generating SHA-256 checksums..."
	@rm -f $(CHECKSUM_FILE)
	@for binary in $(OUTPUT_DIR)/$(BINARY_NAME)-*; do \
		sha256sum $$binary >> $(CHECKSUM_FILE); \
	done
	@echo "Checksums saved to $(CHECKSUM_FILE)"

# Deploy binaries (build, checksum, and package)
deploy: build checksums
	@echo "Packaging binaries..."
	tar -czvf $(OUTPUT_DIR)/$(BINARY_NAME)-binaries.tar.gz -C $(OUTPUT_DIR) \
		$(BINARY_NAME)-x86_64 \
		$(BINARY_NAME)-aarch64 \
		$(BINARY_NAME)-riscv64 \
		$(BINARY_NAME)-loongarch64 \
		checksums.sha256
	@echo "Deployment package created: $(OUTPUT_DIR)/$(BINARY_NAME)-binaries.tar.gz"

# Clean build artifacts
clean:
	cargo clean
	rm -rf $(OUTPUT_DIR)
	@echo "Cleaned build artifacts and output directory"

.PHONY: install build build-x86_64 build-aarch64 build-riscv64 build-loongarch64 checksums deploy clean
