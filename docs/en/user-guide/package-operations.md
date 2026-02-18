# Package operations

This page describes install, remove, update, upgrade, list, search, and info. All apply to the selected environment (default: main, or use `-e ENV` / `--root DIR`).

## Dependency resolution

epkg uses a SAT solver (resolvo) to resolve dependencies. When you install a package, epkg:

1. Fetches repository metadata (if needed, via `update`)
2. Resolves all dependencies recursively
3. Handles conflicts and obsoletes
4. Considers recommends/suggests based on flags (`--no-install-recommends`, `--install-suggests`)
5. Presents a plan showing what will be installed/upgraded/removed

The **DEPTH** column in install plans shows dependency depth (0 = direct dependency, higher = transitive). This helps you understand the dependency tree.

## Install

```bash
epkg install [OPTIONS] [PACKAGE_SPEC]...
```

Common options:

- **-y, --assume-yes** — Answer "yes" to prompts (useful for scripts)
- **--no-install-recommends** — Don't install recommended packages
- **--no-install-essentials** — Don't install essential packages automatically
- **--install-suggests** — Also install suggested packages
- **-m, --ignore-missing** — Continue if some packages are missing
- **--dry-run** — Show what would be done without making changes

Example:

```bash
epkg -e alpine install bash jq
```

Example output (summary):

```
Packages to be freshly installed:
DEPTH       SIZE  PACKAGE
0       469.7 KB  bash__5.3.3-r1__x86_64
0       147.9 KB  jq__1.8.1-r0__x86_64
1       182.8 KB  oniguruma__6.9.10-r0__x86_64
2       403.6 KB  musl__1.2.5-r21__x86_64
...
Packages to be exposed:
- jq__1.8.1-r0__x86_64
- bash__5.3.3-r1__x86_64

0 upgraded, 19 newly installed, 0 to remove, 2 to expose, 0 to unexpose.
Need to get 4.6 MB archives.
After this operation, 11.0 MB of additional disk space will be used.
```

Then download and install proceed; at the end you may see scriptlet messages and “Exposed package commands to …”.

## Remove

```bash
epkg remove [OPTIONS] [PACKAGE_SPEC]...
```

Example:

```bash
epkg -e alpine remove htop
```

Example output:

```
Packages to remove:
- htop__3.4.1-r1__x86_64
- libncursesw__6.5_p20251123-r0__x86_64
Do you want to continue? [Y/n] y
```

## Update (refresh metadata)

```bash
epkg update
```

Downloads refreshed repository metadata for the env’s channel. Required before seeing new packages or running upgrade.

## Upgrade

```bash
epkg upgrade [PACKAGE_SPEC]...
```

Upgrades all installed packages (or the specified ones) to the latest versions available in the channel.

## List

```bash
epkg list [--installed|--available|--upgradable|--all] [PKGNAME_GLOB]
```

- **--installed** (default) — Installed packages in the env.
- **--available** — Available from the channel (not necessarily installed).
- **--upgradable** — Installed and have a newer version.
- **--all** — All (installed + available).
- **PKGNAME_GLOB** — Optional glob to filter by name.

Example:

```bash
epkg -e alpine list
```

Example output:

```bash
Exposed/Installed/Available
| Upgradable
|/  Depth      Size  Name                                  Version                         Arch      Repo                Description
===-======-=========-=====================================-===============================-=========-===================-============================================================
E       0  520.1 KB  coreutils                             9.8-r1                          x86_64    main                The basic file, shell and text manipulation utilities
E       0  127.9 KB  htop                                  3.4.1-r1                        x86_64    main                Interactive process viewer
E       0  147.9 KB  jq                                    1.8.1-r0                        x86_64    main                A lightweight and flexible command-line JSON processor
I       1   14.0 KB  acl-libs                              2.3.2-r1                        x86_64    main                Access control list utilities (libraries)
I       1   16.5 KB  coreutils-env                         9.8-r1                          x86_64    main                The basic file, shell and text manipulation utilities
I       1   14.5 KB  coreutils-fmt                         9.8-r1                          x86_64    main                The basic file, shell and text manipulation utilities
I       1   14.4 KB  coreutils-sha512sum                   9.8-r1                          x86_64    main                The basic file, shell and text manipulation utilities
I       1    7.8 KB  libattr                               2.5.2-r2                        x86_64    main                utilities for managing filesystem extended attributes (libraries)
I       1  155.0 KB  libncursesw                           6.5_p20251123-r0                x86_64    main                Console display library (libncursesw)
I       1  182.8 KB  oniguruma                             6.9.10-r0                       x86_64    main                a regular expressions library
I       1    5.1 KB  utmps-libs                            0.1.3.1-r0                      x86_64    main                A secure utmp/wtmp implementation (libraries)
I       1    1.5 KB  yash-binsh                            2.60-r0                         x86_64    main                yash as /bin/sh
I       2    1.9 MB  libcrypto3                            3.5.5-r0                        x86_64    main                Crypto library from openssl
I       2  403.6 KB  musl                                  1.2.5-r21                       x86_64    main                the musl c library (libc) implementation
I       2   21.3 KB  ncurses-terminfo-base                 6.5_p20251123-r0                x86_64    main                Descriptions of common terminals
I       2   76.3 KB  skalibs-libs                          2.14.4.0-r0                     x86_64    main                Set of general-purpose C programming libraries for skarnet.org software. (libraries)
I       2  159.7 KB  yash                                  2.60-r0                         x86_64    main                Yet another shell
I       3  492.9 KB  busybox                               1.37.0-r30                      x86_64    main                Size optimized toolbox of many common UNIX utilities
Total: 18 packages, 4.2 MB, 9.6 MB if installed
```

## Search

```bash
epkg search [PATTERN]
```

Searches package names and descriptions (and optionally file names). Output is a list of matching packages with short descriptions.

Example:

```bash
epkg -e debian search htop
```

## Info

```bash
epkg info [PACKAGE]
```

Shows detailed information for a package (name, version, summary, homepage, arch, maintainer, requires, suggests, size, location, status, etc.). If PACKAGE is omitted, lists info for all installed packages (or in context of the selected env).

Example:

```bash
epkg -e alpine info jq
```

Example-style output:

```
pkgname: jq
version: 1.8.1-r0
summary: A lightweight and flexible command-line JSON processor
arch: x86_64
...
status: Installed
```

(Exact fields depend on the channel format.)

## Tips and best practices

### Update metadata regularly

Run `epkg update` periodically to get the latest package versions:

```bash
epkg update
epkg update -e myenv  # Update specific env
```

### Use --dry-run before major changes

Preview what will happen:

```bash
epkg install --dry-run large-package
epkg upgrade --dry-run
```

### Check upgradable packages

See what can be upgraded:

```bash
epkg list --upgradable
```

### Install from local/remote package files

If you have `.rpm` files:

```bash
epkg install package.rpm https://.../package2.rpm
```
