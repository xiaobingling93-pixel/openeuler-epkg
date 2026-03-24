# End-to-End Tests for epkg

This directory contains end-to-end tests for the package manager. Tests run **inside a microVM** via `epkg run --isolate=vm` (see `vm.sh`), not Docker.

## Structure

- `cases/` — One shell script per test (e.g. `cases/bash-sh.sh`, `cases/bare-rootfs.sh`).
- `host-vars.sh` — Host-side paths and `E2E_*` defaults (VM memory, VMM, harness env name).
- `vars.sh` — Guest-side variables (e.g. `ALL_OS` for multi-distro tests).
- `lib.sh` — Shared shell helpers (expects **bash** for `local` in functions).
- `vm.sh` — Runs `epkg -e bare-alpine-e2e run --isolate=vm` with explicit mounts and `env KEY=VALUE ... /bin/bash entry.sh` (no temporary launch script).
- `entry.sh` — Guest entry: optional wipe of `/opt/epkg/envs`, `self install` if needed, then the selected test script.
- `test-one.sh` — Run a single test (`-d` / `-dd` / `-ddd` for debug).
- `test-all.sh` — Run all tests in `cases/` except install-remove-upgrade and build-from-source (same exclusions as before).
- `test-iur.sh` — Install/remove/upgrade matrix.
- `e2e-combo.sh` — Thin wrapper to run one test with `E2E_VMM`, `E2E_OS`, etc. in the environment.

## Prerequisites

- Linux host with working KVM/QEMU user networking, virtiofsd, and a static `epkg` at `target/<musl-triple>/debug/epkg` (`make static` / `make`).
- Harness environment `bare-alpine-e2e` is created automatically by `vm.sh` (Alpine + `bash` + `busybox-static`).
- First guest `self install` needs outbound HTTPS (release metadata). Host `~/.cache/epkg/downloads` is mounted at guest `/opt/epkg/cache/downloads` by default.

## Environment variables (combinations)

| Variable | Purpose |
|----------|---------|
| `E2E_BARE_ENV` | Harness env name (default `bare-alpine-e2e`). |
| `E2E_VMM` | VMM preference, e.g. `qemu` or `libkrun,qemu` |
| `E2E_VM_MEMORY` | VM RAM (default `16G`). |
| `E2E_BARE_CHROOT` | Chroot root used by `cases/bare-rootfs.sh` (default `/tmp/epkg-bare-chroot`). |
| `E2E_OS` | In `cases/bash-sh.sh`, restrict OS list without extra args. |
| `E2E_DOWNLOAD_CACHE` | Host path bound to guest download cache. |
| `E2E_LOG_DIR` | Host directory bound read-write to guest `/var/log/epkg-e2e` (default `~/.cache/epkg/e2e-logs`). |
| `E2E_RESOLV_CONF` | Host file to mount as guest `/etc/resolv.conf` (overrides the default QEMU + fallback list built by `vm.sh`). |

Single OS example:

```bash
./test-one.sh cases/bash-sh.sh debian
```

VM + memory example:

```bash
E2E_VMM=qemu E2E_VM_MEMORY=8G ./e2e-combo.sh cases/sandbox-run.sh
```

## Usage

```bash
./test-all.sh
./test-one.sh cases/env-register-activate.sh
./test-iur.sh
```

Alpine-focused VM runs (sets `E2E_OS=alpine` and runs a few cases):

```bash
./run-alpine-vm.sh
```

## Notes

- The harness uses tmpfs `/opt/epkg` and root; epkg runs in global install mode inside the guest.
- `vm.sh` passes `-u root` to `epkg run`, and VM command requests now propagate user selection end-to-end (`--user` works in vm-daemon/cmdline paths).
- Guest `RUST_LOG` / `RUST_BACKTRACE` / `INTERACTIVE` are set from host via `env` in `vm.sh`; use `test-one.sh -dd` for `RUST_LOG=debug` and `RUST_BACKTRACE=1` in guest.
- Debug pause in `lib.sh` now prompts only when stdin is a TTY; non-interactive runs print a skip message instead of blocking.
- The VM launcher sets **`E2E_BACKEND=vm`** in the guest (see `vm.sh`). epkg uses this for nested-microVM behavior (no extra namespaces, `ld-linux` for `epkg run`, offline `self install` assets, etc.).
- `cases/bare-rootfs.sh` runs only when `E2E_BACKEND=vm`: it builds a tmpfs chroot with busybox + epkg, runs `epkg self install`, then `epkg env create … --root /`, install, and `epkg run` checks.

## VM reuse (host)

After a command finishes, the guest can wait for another connection for a configurable idle period (`--vm-keep-timeout SECS`). Long-running commands (e.g. `bash`) keep the VM busy until they exit; only then does the idle timer apply.

```bash
epkg -e myenv run --isolate=vm --vmm=qemu --vm-keep-timeout 120 bash
# another terminal, within the idle window after bash exits (or after shorter follow-ups):
epkg -e myenv run --isolate=vm --vmm=qemu --reuse --vm-keep-timeout 60 -- sh -c 'echo hi'
```

Omit `--vm-keep-timeout` on the first command for a one-shot VM (no reuse session).
