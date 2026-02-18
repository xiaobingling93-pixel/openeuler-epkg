# Advanced usage

This page covers running commands in an environment, service management, history/restore, garbage collection, and package utilities (convert, unpack, hash, busybox).

## Run command in environment

```bash
epkg run [OPTIONS] <COMMAND> [--] [ARGS...]
```

Runs **COMMAND** in the environment’s namespace (mount + user namespaces). The env’s `usr`, `etc`, `var` are bind-mounted so the command sees the env’s files and libraries. Use `--` to separate epkg options from the command and its arguments.

Examples:

```bash
epkg -e alpine run jq --version
# jq-1.8.1

epkg -e alpine run htop --version
# htop 3.4.1-3.4.1

epkg run gcc -- --version
# (uses activated or first registered env that has gcc)
```

If the current directory is under a project that has a `.eenv` directory, you can run a script and let epkg resolve the env from that path:

```bash
epkg run ./myscript.sh
epkg run /path/to/project/subdir/myscript.sh
```

## Service management

```bash
epkg service <start|stop|restart|status|reload> [SERVICE_NAME]
```

Manages systemd (or compatible) services installed in the environment. Start/stop/restart/status/reload apply to the given service, or to all services in the env if no name is given.

## History and restore

- **history** — Show transaction history for the environment (generation id, timestamp, action, package counts, command line).

```bash
epkg history
```

  Example output:

```
% epkg history -e alpine
_________________________________  ENVIRONMENT HISTORY  _________________________________
id  | timestamp                  | action   | packages | command line
----+----------------------------+----------+----------+---------------------------------
1   | 2026-02-16 13:05:50 +0800  | Create   |          | epkg env create alpine -c alpine
2   | 2026-02-16 13:05:58 +0800  | Install  | +6       | epkg -e alpine install /bin/sh
3   | 2026-02-16 13:45:13 +0800  | Install  | +2       | epkg -e alpine install jq
4   | 2026-02-17 13:45:50 +0800  | Install  | +9       | epkg -e alpine install coreutils
```

- **restore** — Roll back the environment to a given generation (by id or `-N` for N generations back).

```bash
epkg restore <GEN_ID|-N>
```

  After restore, a new generation is created (rollback action). Use `epkg history` again to confirm.

## Garbage collection

```bash
epkg gc
```

Removes unused files from the cache and store (e.g. package files and metadata no longer referenced by any environment). Use when you want to reclaim disk after removing envs or many packages.

## Package utilities

### hash

Compute the content hash of a directory (used for store keys and verification):

```bash
epkg hash <STORE_DIR>
```

Example:
```
% epkg hash ~/.epkg/store/rzvdceiy4gmlg6fod4fjzhjndqauh4bu__bash__5.2.37-7.oe2509__x86_64
rzvdceiy4gmlg6fod4fjzhjndqauh4bu
```

### unpack

Unpack an rpm/deb/apk/... package file into the store. Example:

```bash
% epkg unpack /home/wfg/.cache/epkg/downloads/openeuler/openEuler-25.09/everything/x86_64/Packages/selinux-policy-40.7-9.oe2509.noarch.rpm
/home/wfg/.epkg/store/qf5m4eqovu3ho7lz6vxku5c6oliz6zjj__selinux-policy__40.7-9.oe2509__noarch
```

### busybox

Run built-in implementations of common linux commands (busybox-style) without installing a full package:

```bash
epkg busybox <COMMAND> [ARGS...]
```

Example:
```bash
% epkg busybox whoami
wfg
```

Useful in minimal or container environments where you want to avoid pulling full coreutils/sed/grep etc. from a channel.

## Best practices

### Use history and restore for safety

Before major changes, note the current generation:

```bash
epkg history  # Note the latest generation ID
epkg install large-package
# If something breaks:
epkg restore <GEN_ID>  # Roll back
```

### Regular garbage collection

Periodically clean up unused files:

```bash
epkg gc  # Remove unused cache and store files
```

This is especially useful after removing environments or many packages.

### Service management

For systemd services installed via epkg:

```bash
epkg service status  # Check all services
epkg service start redis
epkg service stop  redis
```

Services run in the environment's namespace, isolated from the host.

### Project workflows with .eenv

For project-specific dependencies:

1. Create `.eenv` at project root: `epkg env create --root ./.eenv -c alpine`
2. Install dependencies: `epkg --root ./.eenv install <deps>`
3. Use in scripts: `epkg run ./script.sh` (auto-discovers `.eenv`)
4. Share with team: Export env config or document channel/versions

## Global options (recap)

Useful across commands:

- **-e, --env ENV** — Select environment by name (or `owner/name` for public envs).
- **-r, --root DIR** — Select environment by root path.
- **-y, --assume-yes** — Non-interactive; answer yes to prompts.
- **--dry-run** — Show what would be done without changing the system.
- **--download-only** — Fetch packages without installing.
- **-q, --quiet** / **-v, --verbose** — Less or more output.
- **--proxy URL** — HTTP proxy for downloads.
- **--parallel-download N** — Number of parallel download threads.

See [Command reference](../reference/commands.md) for the full list.

## See also

- [Package operations](package-operations.md) — Install, remove, update, upgrade
- [Environments](environments.md) — Environment management
