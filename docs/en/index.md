# epkg documentation

This is the documentation index for **epkg**, a lightweight multi-source package manager for Linux. For a high-level overview, use cases, and quick start, see the [README](../../README.md).

## Documentation map

### User guide

- **[Getting started](user-guide/getting-started.md)** — How to install epkg and run your first commands (create env, install package).
- **[Developer quick start](user-guide/developer-quick-start.md)** — Build from source, development loop, testing.
- **[Environments](user-guide/environments.md)** — Environment lifecycle: create, remove, register, unregister, activate, deactivate, path, config, and `--root` / `.eenv` discovery.
- **[Package operations](user-guide/package-operations.md)** — install, remove, update, upgrade, list, search, info with example output.
- **[Advanced usage](user-guide/advanced.md)** — Running commands in an env (`run`), service management, history/restore, gc, convert/unpack/hash, busybox.

### Reference

- **[Command reference](reference/commands.md)** — Full list of commands and global options (from `epkg help`).
- **[Repositories](reference/repositories.md)** — Channel list and `epkg repo list` output.
- **[Paths and layout](reference/paths.md)** — User vs root installation paths and directory layout.

### Other

- **Design notes** — [design-notes/](../design-notes/) — Layout, repodata, package format, build system, etc.
- **Package format** — [epkg-format.md](../epkg-format.md) — epkg binary package format.
- **x2epkg** — [x2epkg/](../x2epkg/) — Converting RPM/DEB and desktop integration.
