# Command reference

This page summarizes the output of `epkg help`. For subcommand details, run `epkg <command> -h` (e.g. `epkg install -h`, `epkg env create -h`).

## Usage

```
epkg [OPTIONS] <COMMAND>
```

## Commands (grouped)

### Self management

| Command | Description |
|---------|-------------|
| `self install [--store private\|shared\|auto]` | Install or upgrade epkg itself (user or shared). |
| `self upgrade` | Upgrade epkg installation. |
| `self remove` | Remove epkg installation (deinitialize). |

### Package operations

| Command | Description |
|---------|-------------|
| `install [PACKAGE_SPEC]...` | Install packages. |
| `update` | Update package metadata for the selected env. |
| `upgrade [PACKAGE_SPEC]...` | Upgrade packages. |
| `remove [PACKAGE_SPEC]...` | Uninstall packages. |

### Environment management

| Command | Description |
|---------|-------------|
| `env create [-c\|--channel CHANNEL] [-P\|--public] [-i\|--import FILE] <ENV_NAME\|--root ENV_ROOT>` | Create a new environment. |
| `env remove \| register \| unregister \| activate \| export` | Remove, register, unregister, activate, or export env; argument: `<ENV_NAME\|--root ENV_ROOT>`. |
| `env deactivate` | Deactivate current session env. |
| `env path` | Print PATH with registered (and active) envs. |
| `env config <edit\|get\|set>` | Edit or get/set per-env config. |

### History and rollback

| Command | Description |
|---------|-------------|
| `history` | Show environment transaction history. |
| `restore <GEN_ID\|-N>` | Restore environment to a generation. |

### Garbage collection

| Command | Description |
|---------|-------------|
| `gc` | Clean up unused cache and store files. |

### Info and query

| Command | Description |
|---------|-------------|
| `list [--installed\|--available\|--upgradable\|--all] [PKGNAME_GLOB]` | List packages. |
| `info [PACKAGE]` | Show package information. |
| `search [PATTERN]` | Search packages and files. |
| `repo list` | List repositories (channels). |

### Running commands

| Command | Description |
|---------|-------------|
| `run <COMMAND> [--] [ARGS...]` | Run command in environment namespace. |
| `service <start\|stop\|restart\|status\|reload> [SERVICE]` | Service management. |
| `busybox <COMMAND> [ARGS...]` | Run built-in command implementations. |

### Package utilities

| Command | Description |
|---------|-------------|
| `hash [DIR]` | Compute content hash (e.g. for store). |
| `unpack <FILE.epkg> [DIR]` | Unpack epkg file into store or DIR. |
| `convert [OPTIONS] <PACKAGE_FILE>...` | Convert rpm/deb/apk/... to epkg format. |

### Build

| Command | Description |
|---------|-------------|
| `build` | Build package from source (development). |

### Help

| Command | Description |
|---------|-------------|
| `help` | Print help (this summary). |

## Global options

| Option | Description |
|--------|-------------|
| `--config <FILE>` | Configuration file. |
| `-e, --env <ENV_NAME>` | Select environment by name or owner/name. |
| `-r, --root <DIR>` | Select environment by root directory. |
| `--arch <ARCH>` | Override CPU architecture. |
| `--dry-run` | Simulated run, no changes. |
| `--download-only` | Download packages only, do not install. |
| `-q, --quiet` | Suppress output. |
| `-v, --verbose` | Verbose / debug. |
| `-y, --assume-yes` | Answer yes to prompts. |
| `--assume-no` | Answer no to prompts. |
| `-m, --ignore-missing` | Ignore missing packages. |
| `--metadata-expire <SECONDS>` | Metadata cache (0=never, -1=always). |
| `--proxy <URL>` | HTTP proxy. |
| `--retry <N>` | Download retries. |
| `--parallel-download <N>` | Parallel download threads. |
| `--parallel-processing <BOOL>` | Parallel metadata updates (true/false). |
| `-h, --help` | Help. |
| `-V, --version` | Version. |

## PATHS (from help)

**User private installation** (data-flow order):

- `$HOME/.bashrc` — sources epkg RC (e.g. `$HOME/.epkg/envs/self/usr/src/epkg/assets/shell/epkg.sh`).
- `$HOME/.cache/downloads/` — Downloaded package files
- `$HOME/.cache/channels/` — Repository metadata cache
- `$HOME/.epkg/store/` — Content-addressed package store
- `$HOME/.epkg/envs/$env_name/` — Environment root directories
- `$HOME/.epkg/envs/$env_name/etc/epkg/` — per-env config
- `$HOME/.epkg/envs/self/usr/bin/epkg` — epkg binary
- `$HOME/.epkg/envs/self/usr/src/epkg/` — epkg source and RC scripts

**Root global installation:**

- `$HOME/.bashrc` (or `/etc/bash.bashrc` for system-wide)
- `/opt/epkg/cache/downloads/` — Shared download cache
- `/opt/epkg/cache/channels/` — Shared metadata cache
- `/opt/epkg/store/` — Shared package store
- `/opt/epkg/envs/root/$env_name/` — Root user's environments
- `/opt/epkg/envs/$owner/$env_name/` — Other users' public environments (if shared mode)

See [Paths and layout](paths.md) for detailed explanation of cache vs store and directory structure.
