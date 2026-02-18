# Paths and layout

This page describes where epkg stores its data and how the directory layout looks for user (private) vs root (shared) installation.

## User private installation

When epkg is installed for a single user (e.g. `epkg self install` as non-root, or installer script in user mode), the following paths are used (in data-flow order, as in `epkg help`):

| Purpose | Path |
|---------|------|
| Shell integration | `$HOME/.bashrc` (or `.zshrc`) sources `$HOME/.epkg/envs/self/usr/src/epkg/lib/epkg-rc.sh` |
| Download cache | `$HOME/.cache/downloads/` |
| Channel metadata cache | `$HOME/.cache/channels/` |
| AUR build cache | `$HOME/.cache/aur_builds/` (if used) |
| Store (package content) | `$HOME/.epkg/store/` |
| Environments | `$HOME/.epkg/envs/<env_name>/` |
| Per-env epkg config | `$HOME/.epkg/envs/<env_name>/etc/epkg/` |
| epkg binary | `$HOME/.epkg/envs/self/usr/bin/epkg` |
| epkg source (RC script, etc.) | `$HOME/.epkg/envs/self/usr/src/epkg/` |

Within an environment root (e.g. `$HOME/.epkg/envs/main/`):

- **usr/** — Installed package files (bin, lib, share, etc.); **usr/ebin/** holds symlinks (or wrappers) for exposed commands so they appear on PATH when the env is registered.
- **etc/** — Environment-specific config (e.g. `etc/epkg/`).
- **var/** — Variable data if needed by packages.

The **self** environment is special: it contains the epkg binary and source used by the shell wrapper; it is not used for general package installs.

## Root global installation

When epkg is installed system-wide (e.g. root runs the installer or `epkg self install` with shared store), typical paths are:

| Purpose | Path |
|---------|------|
| Shell integration | `$HOME/.bashrc` (root’s or per-user); system-wide: `/etc/bash.bashrc` etc. |
| Download cache | `/opt/epkg/cache/downloads/` |
| Channel metadata cache | `/opt/epkg/cache/channels/` |
| Store | `/opt/epkg/store/` |
| Environments | `/opt/epkg/envs/root/<env_name>/` |

So each user still has their own `$HOME/.bashrc` (and optionally `$HOME/.epkg/` for overrides), but the store and env roots live under `/opt/epkg/`. In this mode, **public** environments (created with `-P`) are visible to other users as `owner/envname` and can be used read-only with `-e owner/envname`.

## Cache vs store

- **Cache** — Downloaded raw data: package files (e.g. .rpm, .deb, .apk), channel metadata (Release, repodata, APKINDEX, etc.). Can be re-downloaded; safe to clear with `epkg gc` or manually if you accept re-downloads.
- **Store** — Content-addressed package content (unpacked and hashed). Each store entry is referenced by environments via links. Removing store content that is still referenced can break envs; `epkg gc` only removes unreferenced store data.

## Understanding cache vs store

The distinction between cache and store is important:

- **Cache** (`~/.cache/epkg/` or `/opt/epkg/cache/`) — Temporary data that can be regenerated:
  - Downloaded package files (`.rpm`, `.deb`, `.apk`, etc.)
  - Repository metadata (Release files, repodata, APKINDEX, etc.)
  - AUR build artifacts
  - Safe to delete; will be re-downloaded when needed

- **Store** (`~/.epkg/store/` or `/opt/epkg/store/`) — Content-addressed package content:
  - Unpacked and hashed package files
  - Referenced by environments via links (hardlinks, symlinks, etc.)
  - **Do not delete manually** — Use `epkg gc` to safely remove unreferenced entries
  - Each store entry is identified by a hash derived from package content

When you install a package:
1. Package file is downloaded to **cache**
2. Package is unpacked and stored in **store** (content-addressed)
3. Environment links to store entries (not copies)
4. Binaries are exposed in `env/usr/ebin/`

This design enables:
- **Deduplication** — Same package content shared across environments
- **Efficiency** — No duplicate storage for identical files
- **Safety** — Store entries remain until no longer referenced

## References

- [README](../../../README.md) — High-level overview and installation layout.
- [design-notes/epkg-layout.md](../../design-notes/epkg-layout.md) — Historical layout notes and uninstall effects.
- [Garbage collection](../user-guide/advanced.md#garbage-collection) — How to clean up unused files safely.
