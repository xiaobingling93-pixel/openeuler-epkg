# Developer quick start

This guide gets you building epkg from source and running your first commands in a development setup.

## 1. Install build dependencies

```bash
git clone https://atomgit.com/openeuler/epkg
cd epkg
make dev-depends
```

## 2. Build and install epkg

```bash
make
target/debug/epkg self install
```

Then start a new shell (or `source ~/.bashrc`) so PATH is updated.

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

## Testing

- **Unit tests** — In-tree `.rs` tests: `cargo test` (or use the project’s test runner).
- **tests/solver** — Solver tests.
- **tests/lua** — Lua tests.
- **tests/busybox** — External busybox tests.
- **tests/applets** — Busybox applets tests.
- **tests/e2e** — End-to-end: bare-rootfs, export-import, history-restore, public-multi-user, env-register-activate, install-remove-upgrade. Run from `tests/e2e/` (see [tests/e2e/README.md](../../../tests/e2e/README.md)).

## See also

- [Getting started](getting-started.md) — User installation and first steps
- [Command reference](../reference/commands.md) — All commands and options
