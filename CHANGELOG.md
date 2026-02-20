# EPKG Changelog

---

## [v0.2.2] – 2026-02-20

### Added

#### Package Format & Integration
- **AUR (Arch User Repository) support** – Build and install from AUR via PKGBUILD fetch and `makepkg` in env.
- **RPM trigger system** – Package/File triggers parsing, .hook generation and execution with instance arguments.
- **Debian trigger support** – dpkg-trigger compatible package/file triggers mapped to .hook and `postinst triggered` execution.
- **Arch Linux ALPM hooks** – Load `usr/share/libalpm/hooks/*.hook` and `etc/pacman.d/hooks/`.
- **Systemd hooks integration** – In systemd-less env, auto install applets `systemd-sysusers` / `systemd-tmpfiles` and create hooks on `usr/lib/sysusers.d/*.conf` and `usr/lib/tmpfiles.d/*.conf`
- **Unified hook system** – Based on ALPM hooks, with general parsing, filtering and execution.
- **Enhanced Conda support** – Improved link handling and requirement parsing.
- **RPM sources parsing** – Auto load `$env_root/etc/yum.repos.d/*.repo` repo configs.
- **DEB sources parsing** – Auto load `$env_root/etc/apt/sources.list.d/*.list` and deb822 `*.sources` configs.

#### Core Features & Management
- **Service management** – `epkg service` subcommand to parse systemd .service files and run (start/stop/status) in env.
- **Multi-user environment sharing** – Normal users list/run root public envs; reuse root download cache via symlinks.
- **Mirror system** – Cleanup discovery data pipeline, simplified format for `sources/mirrors.json`, blacklist support.
- **Package cache** – RwLock-based concurrent access; filelist caching with pkgkey indexes.
- **Transaction batching** – Batch-first hook execution and file linking/unlinking.
- **File deduplication** – Hardlink deduplication in store, among packages with the same name.
- **Package linking types** – (hardlink, symlink, reflink, move) with proper fallbacks.
- **Environment SSL certificates** – Populate env `etc/ssl/certs` from host for HTTPS inside environments.
- **Mtree filelist enhancements** – Handle spaces in filename and `link=` values; error messages with line numbers.
- **Cross-platform builds** – Enhanced static builds and cross-compilation (Makefile and `make.sh` overhaul, `static-aarch64` on musl).
- **Developer installation** – Support `make dev-depends` on openEuler and more distros, migrating `apt-get` + `.venv` => unified epkg `.eenv` for `scripts/mirror/`

#### Dependency Resolution
- **World concept** – Add `world.json` (Alpine-inspired) to track user-requested packages -- the original intention like python `requirements.txt`.
- **No-install packages** – Add `systemd`, `udev`, `dbus`, `grub`, `dracut`, `pam`, `cron` etc. to world no-install list by default.
- **Weighted circular dependency resolution** – Depth-based ordering and cycle breaking.
- **Debian/Conda virtual packages** – Generic creation for resolver and skipping before install.

#### CLI & User Experience
- **New subcommands** – `epkg self` (manage epkg installation, replaces `init`/`deinit`), `epkg busybox` (run built-in command implementations with 80+ applets), `epkg service` (service management: start/stop/status/reload/restart).
- **List command enhancements** – `epkg list`: depth column, package size, totals, dynamic padding; `epkg env list` Root column, @order; `epkg busybox --list`.
- **Search improvements** – `--ignore-case`, glob matching, deduplicated output.
- **Environment discovery** – Search upward for `.eenv` from CWD or command path; `--root` for env create; Run-in-env checks for `/etc/epkg/env.yaml`.
- **CLI error handling** – `arg_required_else_help(true)`, full command context in errors, centralized help, usage examples.
- **Download** – Parallel processing, progress tracking, hash validation, conditional filelists download, proper refresh checks.

#### New Applets (80+)

**Coreutils (63 commands)**: `[` (test alias), `base64`, `basename`, `cat`, `chgrp`, `chmod`, `chown`, `chroot`, `cksum`, `comm`, `cp`, `cut`, `dirname`, `echo`, `env`, `false`, `head`, `hostid`,
 `install`, `kill`, `killall`, `link`, `ln`, `logname`, `ls`, `md5sum`, `mkdir`, `mkfifo`, `mv`, `nice`, `nl`, `nohup`, `nproc`, `od`, `pidof`, `pkill`, `printenv`, `pwd`, `readlink`, `realpath`,
 `rm`, `rmdir`, `sha256sum`, `sleep`, `sort`, `stat`, `sync`, `tac`, `tail`, `test`, `touch`, `tr`, `true`, `truncate`, `tty`, `uniq`, `unlink`, `usleep`, `wc`, `which`, `whoami`, `xargs`, `yes`.

**Package management commands (4)**: `dpkg-query`, `dpkg-trigger`, `rpm`, `rpmlua` (embedded Lua interpreter for RPM scriptlets).

**System administration (11)**: `addgroup`, `adduser`, `delgroup`, `deluser`, `groupadd`, `groupdel`, `systemd-sysusers`, `systemd-tmpfiles`, `useradd`, `userdel`, `usermod`.

**Text processing utilities (4)**: `egrep`, `fgrep`, `grep`, `sed`.

**Archive utilities (2)**: `tar`, `zcat`.

**Network utilities (1)**: `wget`.


#### Testing & Development
- **Sandbox development** – `sandbox-epkg.sh` script for AI-aided development.
- **BusyBox test suite integration** – Wrapper scripts to run 400+ busybox compatibility tests. ALL PASSED.
- **Lua test suite** – 50 `lua` test scripts, 50+ `rpmlua` compatibility tests.
- **End-to-end test suite** – Docker-based covering random install/remove/upgrade, export/import, history restore, multi-user.
- **RPM conformance testing** – Query compatibility against system RPM.
- **Improved solver tests** – Make YAML configurations straight with `GenerationCommand`.
- **Development tools** – `account-sh-commands.py` for accounting shell command in real world scripts.

#### Documentation
- `README.md` shortened to overview and quick start; `README.zh.md` added.
- New documentation structure with en/zh index, user guide (getting started, environments, package operations, advanced, developer quick start), reference (commands, paths, repositories), design notes, and `x2epkg`.
- New `CHANGELOG.md`, replacing `release_notes`.

### Changed
- **Installer / Gitee** – Owner changed to `wu_fengguang`; update checks use Gitee releases API and asset URLs.
- **Naming and paths** – Config dir `channel/` → `sources/`; env registration priority → path-order; unpack dir `.cache/epkg/` → `.epkg/store/`.
- **Self management** – Base env renamed to `self`; `epkg self install/upgrade/remove` replaces `init`/`deinit`.
- **`epkg-rc.sh`** – Improved command parsing for options-before-command and help-flag handling.
- **`installed-packages.json`** – Format changed from hash to array in `generations/current/`; backward compatibility retained.
- **Output formatting** – `epkg env list` and service status formatting refined; tried and removed comfy-table.
- **Environment create** – Reflink option added and channel example in help.
- **Environment export/import** – More simple and flexible export/import file format.
- **CLI error handling** – Consolidated into `handle_clap_error_with_cmdline()`; better help detection.

### Fixed
- **Build** – Lua tarball download; musl compatibility shim; Cargo scanning ignores `.eenv`.
- **Lua** – `posix.access()` error message format matching RPM; skip popen-dependent tests when `io.popen` unavailable.
- **RPM** – Query output order, `-qp` support for unpacked package files.
- **`ln`, `rm`** – Dangling symlink handling (use `symlink_metadata`).
- **`rm`, `cp`** – `-R` as alias for `-r` (recursive).
- **Busybox** – Help exit code 0; clean help without extra prefixes.
- **Package listing** – Empty scope headers suppressed; correct size display for `--available`.
- **File conflicts / upgrades** – Risk checking; prevent file removal during upgrades; batch-wide file union.
- **Cross-device rename** – Fallback to copy.
- **Permissions** – Normal user access to root public envs; skip non-readable directories in walk.
- **SIGPIPE/EPIPE** – Setup properly in main and child processes.
- **Download** – PID reuse prevention; PID file verification via `/proc`; finalize by full file size; correct `.etag.json` path in cache.
- **Circular dependencies** – All-0 depth fixed in some RPM installs.
- **AUR** – Binary package handling, reverse dependency updates; prevent AUR matching affecting non-AUR packages.
- **Debian** – Virtual package support; install `dpkg` by default.
- **Tests** – Lock poisoning and clap `root` argument; solver test recovery.
- **History** – Deadlock in `rollback_history()`.
- **`epkg-rc.sh`** – Command parsing fixes (options-before-command, help); shell exit after uninstall.
- **Run** – Empty PATH handling; fallback for containers without `/run/user/` (e.g. XDG_RUNTIME_DIR).
- **Symlink resolution** – `.eenv` discovery, applet symlinks, relative path commands.
- **`systemd_tmpfiles`** – Symlink line parsing when age field omitted in `tmpfiles.d` configs.
- **Environment import** – Package installation when importing.
- **Init** – Alpine compatibility for glibc binaries; skip API calls for local dev binary.

### Removed
- `PackageManager` struct – Replaced by top-level functions.
- Optimized path trigger evaluation – Superseded by unified hook system.

### Design / Architecture
- **`models.rs`** – `LinkType` enum, `EnvConfig` link field, `register_priority` → `register_path_order`; `InstalledPackageInfo` in `Arc`, `installed_packages` as `RwLock<Map>` with O(1) lookup; `EnvExport` / `ExportFile`; `Package`, `EpkgCommand` extended.
- **Package cache** – RwLock-based with multiple indexes (`pkgkey2package`, `pkgline2package`, …).
- **Download subsystem split** – Parallel, resumable downloads with mirror support, chunking, integrity validation.
- **Mirror management split** – Geographic optimization, performance tracking, load balancing.
- **Transaction system** – 3-level (RPM-inspired) for atomic package operations.
- **Plan module** – `InstallationPlan` and `PackageOperation` with flags.
- **Hooks** – Unified batch-first execution, parsing `.hook` files.
- **AUR module** – PKGBUILD download, `makepkg` builds, dependency resolution.
- **Userdb** – Standalone parsing of `/etc/passwd`, `/etc/group`, `/etc/shadow` for static binaries.
- **Expose** – `ebin` wrappers for direct binary execution without activation.
- **Service** – Systemd service management inside environments.
- **XDesktop** – Desktop integration (symlink `.desktop` files, icons, MIME, DBus, autostart).
- **Shebang** – Utilities for conda linking and package exposure.
- **POSIX** – Rust implementations of POSIX functions for Lua bindings.
- **Design notes** – New/expanded: build-system, download-chunks, download-integrity, environment, epkg-list, epkg-suid, normal-user-install, repodata, etc.

### Security
- **SUID bit** – Disabled on epkg binary.
- **PID verification** – Process existence via `/proc` for download cache.
- **File permissions** – Symlink and directory permission handling.

### Statistics
- **433 files changed, 79792 insertions(+), 30542 deletions(-)**

---

## [v0.2.1] – 2025-11-24

### Added

#### Dependency Resolution
- **Constraint-aware resolution** – SAT solver using `resolvo` crate.
- **RPM "with" operator support** – for multiple constraints on the same package.
- **Conditional dependency parsing** – with fixed "if" operator handling.
- **Architecture and capability parsing** – for RPM and Arch Linux library aliases.
- **Version comparison fixes** – for Debian, RPM, and APK algorithms.
- **Wildcard/pattern matching** – (`VersionEqualStar`, `VersionNotEqualStar`).

#### Core Functionality
- **Centralized `InitPlan` struct** – for initialization logic.
- **Conda ELF binary execution** – run directly (no elf-loader/namespace isolation).

#### Testing Infrastructure
- **Batch package dependency testing** – `test_depends.py` with random package selection and configurable --os/--batch
- **Data-driven test framework** – for dependency solver (YAML/JSON definitions).
- **141 solver test files** – covering basic, conflict, error, installif, pinning, provides, upgrade scenarios.
- **Ported APK test suite** – with Python validation script.

#### Installer
- **Light installer** – `bin/epkg-installer.sh` now fetches the latest release from Gitee.

### Changed
- **CLI improvements** – Added `--assume-no` global flag, `--no-install-essentials` install option, `--assume-installed` package list, `--full` upgrade flag.
- **Conda repository support** – Noarch repos and improved error logging.
- **Local package / URL support** – Install `.rpm`, `.deb`, `.apk`, `.epkg`, `.conda`, `.pkg.tar.*`, `.tar.bz2` directly.
- **Version comparison algorithms** – for Debian, RPM, APK.
- **Dependency parsing improvements** – operator order, conditional handling.
- **Provider resolution** – with constraint checking.
- **Enhanced handling** – for Conda, Arch, Alpine, Debian, RPM formats.
- **Various improvements** – to installation, removal, upgrade, environment, and mirror modules.

### Fixed
- **Solver panics** – when package obsoleted during resolution.
- **RPM repository indexing** – last package not saved fixed.
- **"Text file busy" errors** – during installation.
- **Checksum file path resolution** – use `resolve_mirror_path()`.
- **`epkg info`** – support for `pkgkey` format (`pkgname__version__arch`).

### Statistics
- **189 files changed, 18521 insertions(+), 1709 deletions(-)**

---

## [v0.2.0] – 2025-09-14

### Added

- **Version output** – includes build date and commit hash.
- **Gitee release integration** – download versioned epkg/elf-loader via Gitee API.
- **Global parallel downloading** – restructured repo metadata processing with global merge and deduplication.
- **Conda channel configuration** – support `$conda_arch` and `$conda_repofile` variables.
- **Environment export/import** – `epkg env export` and `epkg env create --import` for env sharing.

### Changed
- **Base environment detection** – now checks for `etc/epkg` directory instead of `etc/epkg/env.yaml`.
- **Repository processing** – eliminated nested parallelism; centralized parallel execution.

### Fixed
- **Base environment detection** – fixed "Base environment not found" error during `epkg init`.

### Removed
- **Old conda channel configuration** – (`channel/anaconda.yaml`).

### Statistics
- **16 files changed, 689 insertions(+), 254 deletions(-)**

---

## [v0.1.0] – 2025-08-15

### Added

#### Global epkg commands
- `epkg init` – Install epkg package manager (automatically run by installer).
- `epkg deinit` – Uninstall epkg.
- `epkg repo list` – List all repositories.
- `epkg hash` – Compute hash of a directory.
- `epkg build` – Build packages from source (in development).
- `epkg convert` – Convert rpm/deb/apk/… to epkg format.
- `epkg unpack` – Extract an epkg package.
- `epkg run <pkg> -- [args]` – Run a program provided by a package.
- `epkg help` – Show help.
- `epkg --version` – Show version.

#### Package management
- `epkg install <pkg>` – Install a package.
- `epkg remove <pkg>` – Remove a package.
- `epkg list` – List installed packages.
- `epkg search <keyword>` – Search available packages.
- `epkg info <pkg>` – Show detailed package information.
- `epkg update` – Update package indexes.
- `epkg upgrade` – Upgrade all installed packages.

#### Environment management
- `epkg env list` – List all environments.
- `epkg env create <env>` – Create a new environment.
- `epkg env remove <env>` – Remove an environment.
- `epkg env register <env>` – Register an environment.
- `epkg env unregister <env>` – Unregister an environment.
- `epkg env activate <env>` – Activate an environment.
- `epkg env deactivate` – Deactivate the current environment.
- `epkg env path` – Show PATH for current environment.
- `epkg env config` – View or configure current environment.
- `epkg history` – View environment history.

### Statistics
- **Initial release.**
