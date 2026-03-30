# Developer quick start

This guide gets you building epkg from source and running your first commands in a development setup.

**Supported platforms:** Linux (x86_64, aarch64, riscv64, loongarch64), macOS (x86_64, arm64), and Windows (x86_64, arm64).

## 1. Install build dependencies

### Linux (Debian/Ubuntu, Fedora, openSUSE, Arch, Alpine)
```bash
git clone https://atomgit.com/openeuler/epkg
cd epkg
make dev-depends
```

### macOS (with Homebrew)
```bash
git clone https://atomgit.com/openeuler/epkg
cd epkg
make dev-depends
```
The script will install Rust, Lua, OpenSSL, and other dependencies via Homebrew.

### Windows (with Chocolatey or Scoop)
```bash
git clone https://atomgit.com/openeuler/epkg
cd epkg
make dev-depends
```
The script will install Rust, Lua, OpenSSL, and other dependencies via Chocolatey (preferred) or Scoop.

> **Note:** On non‑Linux platforms, the `epkg run` and `epkg service` commands are not available due to missing Linux kernel namespaces. Package management (install, remove, upgrade, etc.) works normally.

## 2. Build and install epkg

### Linux (default)
```bash
make
target/debug/epkg self install
```

### macOS / Windows
```bash
make
```
The binary is built with dynamic linking (Lua, OpenSSL). On macOS you can install it with:
```bash
target/debug/epkg self install
```
On Windows, you may need to adjust permissions or install manually.

Then start a new shell (or `source ~/.bashrc` / restart your terminal) so PATH is updated.

### Cross‑compilation from Linux

You can build epkg for macOS (Apple Silicon) or Windows from a Linux host:

**Prerequisites:**
- **macOS (aarch64‑apple‑darwin):** Install [osxcross](https://github.com/tpoechtrager/osxcross) and place the macOS SDK in `/c/rust/osxcross` (default location used by the build script). The `make cross-macos` command will attempt to detect osxcross and guide you through installation if missing.
- **Windows (x86_64‑pc‑windows‑msvc):** Install `mingw‑w64` package:
  ```bash
  sudo apt install mingw-w64  # Debian/Ubuntu
  ```
  The `make cross-windows` command will detect mingw‑w64 and guide you through installation if missing.

**Build:**
```bash
# macOS (Apple Silicon)
make cross-macos aarch64

# Windows (x86_64, default arch)
make cross-windows
```
The resulting binaries are in `target/<triple>/release/epkg`.

**Note:** On WSL2, you can run the Windows .exe directly without needing Wine or a separate Windows machine.

**Note:** Cross‑compilation uses the same codebase with conditional compilation for platform‑specific features. Linux‑only applets (e.g., mount, umount, modprobe, vm‑daemon) are automatically disabled on non‑Linux targets.

## 3. Development loop

After `self install`, the epkg binary lives at `~/.epkg/envs/self/usr/bin/epkg`. Subsequent `make` can copy the built binary there so you can run:

```bash
make
epkg --version
```

Use `make [static]` then `epkg ...` for a fast edit–test cycle without reinstalling.

## 4. Try a channel

Create an environment and install a package:

```bash
export os=alpine   # or openeuler, fedora, debian, ubuntu, archlinux, conda
epkg env create $os -c $os
epkg -e $os install bash
epkg -e $os run bash
```

## VM Mode and `embedded_init` Configuration

When running packages in VM mode (`--isolate=vm`), epkg uses an embedded init binary to bootstrap the environment. The `embedded_init` option controls this behavior:

| Setting | Purpose | Use Case |
|---------|---------|----------|
| `embedded_init=true` | DEBUG ONLY | Use embedded init binary for bootstrap, avoiding virtiofs/NTFS noise during troubleshooting |
| `embedded_init=false` | PRODUCTION | Use `/usr/bin/init` directly from the rootfs for normal operation |

**When to use each mode:**

- **DEBUG (`embedded_init=true`)**: Use during development or when debugging VM startup issues. The embedded init provides a controlled environment that bypasses potential virtiofs/NTFS filesystem issues.

- **PRODUCTION (`embedded_init=false`)**: Use for normal package execution. This directly executes the init binary from the rootfs, which is the standard production behavior.

**Configuration:**
```bash
# Debug mode (embedded init)
epkg run -e alpine --isolate=vm --embedded-init=true ls /

# Production mode (direct init from rootfs)
epkg run -e alpine --isolate=vm --embedded-init=false ls /
```

## Testing

- **Unit tests** — In-tree `.rs` tests: `cargo test` (or use the project’s test runner).
- **tests/solver** — Solver tests.
- **tests/lua** — Lua tests.
- **tests/busybox** — BusyBox applets tests and upstream test suite integration.
- **tests/e2e** — End-to-end scripts live under `tests/e2e/cases/` (e.g. bare-rootfs, export-import, history-restore, public-multi-user, env-register-activate, install-remove-upgrade). Run from `tests/e2e/` (see [tests/e2e/README.md](../../../tests/e2e/README.md)).

## See also

- [Getting started](getting-started.md) — User installation and first steps
- [Command reference](../reference/commands.md) — All commands and options
