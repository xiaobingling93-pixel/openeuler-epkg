# Developer quick start

This guide gets you building epkg from source and running your first commands in a development setup.

## 1. Install build dependencies

**Supported platforms and host OS:**
- Linux (x86_64, aarch64, riscv64, loongarch64): Debian/Ubuntu, openEuler, Fedora, Archlinux
- macOS (x86_64, aarch64) with homebrew
- Windows (x86_64), in WSL2 Debian/Ubuntu

```bash
git clone https://atomgit.com/openeuler/epkg
cd epkg
make dev-depends
```

## 2. Build and install epkg

### Linux / macOS

```bash
make
target/debug/epkg self install
```

Then start a new shell (or `source ~/.bashrc` / restart your terminal) so PATH is updated.

### Windows

```bash
make cross-windows  # or: cross-windows-release
target/debug/epkg.exe self install
```

On WSL2, you can run the Windows .exe directly without needing Wine or a separate Windows machine.

To debug with env vars:
```
export WSLENV=EPKG_DEBUG_LIBKRUN/p:RUST_LOG/p
export EPKG_DEBUG_LIBKRUN=1
export RUST_LOG=debug
target/debug/epkg.exe ...
```

Or more flexible
```
/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe -Command "
  \$env:RUST_LOG='trace'
  \$env:LIBKRUN_WINDOWS_VERBOSE_DEBUG='1'
  C:\Users\epkg\.epkg\envs\self\usr\bin\epkg.exe run -e alpine ls /proc
  Write-Host 'Exit code:' \$LASTEXITCODE
"
```

### Release builds from Debian

We build and release epkg[.exe] from Debian Linux.

**Build:**
```bash
make crossdev-depends   # run once
make release-all
```
The resulting binaries are in `target/<triple>/release/epkg[.exe]`, linked to `dist/` and compute sha256 there.

## 3. Development loop

After `self install`, the epkg binary lives at `~/.epkg/envs/self/usr/bin/epkg`. Subsequent `make` can copy the built binary there so you can run:

```bash
make
epkg --version
```

Use `make` then `epkg ...` for a fast edit–test cycle without reinstalling.

## 4. Try a channel

Create an environment and install a package:

```bash
export os=alpine   # or openeuler, fedora, debian, ubuntu, archlinux, conda
epkg env create $os -c $os
epkg -e $os install bash
epkg -e $os run bash
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
