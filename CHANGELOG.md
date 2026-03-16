# EPKG Changelog

---

## [v0.2.4] – 2026-03-12

### Added

#### VMM Kernel Management
- **sandbox-kernel repository** – New standalone repository for building VM kernels.
- **Multi-arch kernel builds** – Build x86_64, aarch64, riscv64 kernels with single command: `./scripts/build.sh ALL`.
- **Kernel naming convention** – `vmlinux-$KVER-$arch` format; multiple architectures can coexist.
- **zstd compression** – Release kernels compressed with zstd for smaller downloads (~7.3MB vs ~26MB).

#### libkrun Backend
- **libkrun module** – New `src/libkrun.rs` for libkrun microVM backend support.
- **vsock mode** – Unified control plane using vsock instead of TCP; improved reliability.
- **External kernel support** – `--kernel` option to specify custom kernel image; ELF vmlinux only.
- **Extra kernel cmdline** – `--kernel-args` for appending kernel cmdline.
- **VM ready notification** – Eliminates vsock connection delay.

#### Tool Mirror Acceleration
- **tool_wrapper module** – Mirror acceleration for pip, npm, gem, cargo, go, mvn.
- **Environment variables** – Pre-configured mirror URLs for CN region (`assets/tool/env_vars/cn/`).
- **Wrapper scripts** – Transparent wrapper scripts for development tools.

#### DPKG Compatibility
- **dpkg_db module** – Generate dpkg-compatible metadata (`/var/lib/dpkg/status`, `available`, `info/*`) for real dpkg commands.
- **Pending packages support** – Handle packages being installed during dpkg-query operations.

#### Security
- **AppArmor profile** – `assets/etc/apparmor.d/epkg` for confined execution.

#### Testing
- **dev-projects test suite** – Multi-language development tests (C, C++, Rust, Go, Python, Node.js, Ruby, Java, Lua, Lisp, Scala, Zig).
- **test-env-register-activate.sh** – Environment registration and activation tests extended.

#### Documentation (zh)
- **Architecture docs** – dpkg-database, ebin-exposure, elf-loader, kernel-config, vmlinux-kernel.
- **Troubleshooting docs** – Corresponding troubleshooting guides.
- **User guides** – ebin-exposure, elf-loader, vmlinux-kernel.

### Changed
- **applets → busybox** – Renamed applets module to busybox for clarity.
- **ebin exposure** – Extend transitively to all dependencies; limit propagation to meta-packages only.
- **Meta-package detection** – Robust `pkg_is_likely_metapkg()` and `package_has_binaries()` for better exposure handling.
- **Git repos reorganization** – Moved add-on repos to `git/` directory.

### Fixed
- **init** – Type mismatch and lifetime issues in `sha256_files_to_delete`.
- **main** – Unpack command output to stdout instead of stderr.
- **dpkg_divert** – `is_dir()` following symlinks on merged-usr systems.
- **deb_triggers** – Demote 'no hook found' warning to debug level.
- **install** – Skip exposure for packages with empty pkgline.
- **expose** – Fix ebin wrapper creation for various edge cases (broken symlinks, paths without leading slash, Node.js wrappers).
- **VM reliability** – vsock connection race, stdin thread blocking, shutdown delay, socket cleanup.
- **lfs functions** – Replaced `fs::metadata()` and `.exists()` calls with explicit `lfs::` functions for consistent symlink handling.
- **Various symlink handling** – Recursive symlink resolution, extract_tar_gz symlinks, relative symlink paths.

### Statistics
- **218 commits, 330 files changed, 12936 insertions(+), 1697 deletions(-)**

---

## [v0.2.3] – 2026-03-01

### Added

#### Sandbox System
- **Comprehensive sandbox architecture** – Three isolation levels (env/fs/vm) with unified process creation framework.
- **Filesystem sandbox (`--isolate=fs`)** – Full container isolation via pivot_root into tmpfs-based root; proc, sysfs, tmpfs, devtmpfs/devpts, mqueue; capability dropping.
- **Virtual machine sandbox (`--isolate=vm`)** – QEMU-based hardware virtualization with virtiofs shared filesystem; JSON command protocol (host vm_client ↔ guest vm_daemon); PTY and non-PTY execution modes.
- **Flexible mount specification** – Docker-like syntax `[HOST|FS_TYPE:]SANDBOX_DIR[:OPTIONS]` (bind, tmpfs, proc, remount, `@` `env_root` substitution); automatic source existence checking with `try` fallback.
- **Configuration hierarchy** – CLI `--isolate` > `~/.config/epkg/options.yaml` > `$env_root/etc/epkg/env.yaml`; mount specs additive across levels.
- **Guest init and vm_daemon** – PID 1 init: mounts, network (10.0.2.15/24), kernel cmdline parsing, command/vm_daemon fork; vm_daemon: TCP server, JSON Lines protocol, PTY/pipes, Base64 binary, auto poweroff.
- **Kernel module applets** – `insmod` (finit_module/init_module, compressed .ko); `modprobe` (modules.dep parsing, recursive deps, fs glob fallback) for VM guest `virtio_net`.
- **Build targets** – `make.sh` `qemu-pkgs` (QEMU+virtiofsd), `sandbox-pkgs` (newuidmap/newgidmap); per-mode package lists for major distros.
- **Documentation** – `docs/design-notes/sandbox-vmm.md`; user-guide sandbox sections (en/zh).

#### New Applets
- **Debian/DPKG** – `dpkg` (status/list via dpkg-query or PACKAGE_CACHE, --compare-versions), `dpkg-divert` (--listpackage, --truename, --add/--remove, diversions DB), `dpkg-statoverride` (--list, --add/--remove), `dpkg-maintscript-helper` (rm_conffile, mv_conffile, symlink_to_dir, dir_to_symlink), `deb-systemd-helper` (enable/disable/unmask/purge/update-state/was-enabled), `update-alternatives` (--install/--remove/--auto/--display/--list/--query), `dpkg-realpath` (--root).
- **System and initrd** – `uname` (-s/-n/-r/-v/-m/-a via posix_uname), `mktemp` (-d, -u, -p, --suffix, template rules), `df` (-P/-k/-m/-h/-T/-i/-B/-a, /proc/mounts, statfs), `mount`/`umount`/`mountpoint` (initrd options: -t, -o, --bind, --rbind, -f/-l/-a), `ifconfig`/`route` (legacy ioctl, IPv4, SIOCSIFADDR/SIOCADDRT).

#### Applet Enhancements
- **chmod/chown** – `--reference=RFILE` to copy mode/owner from another file; shared `extract_reference_metadata()` in applets.
- **sed** – Empty text for append/insert (`$a\`, `i\`) allowed; match GNU behavior for ensure-newline idiom.
- **mv** – Overwrite existing destination by default (POSIX/GNU); -n no-clobber; -f skips prompt when not writable.
- **od** – Added --help flag.
- **truncate** – Size parsing utilities extracted to `src/utils.rs`.
- **Applet error handling** – `try_get_matches_from` + `handle_clap_error_with_cmdline()` for consistent error prefix when invoked via symlinks.
- **busybox_subcommands()** – Cached with `OnceLock<Vec<Command>>` to avoid repeated allocations.

#### Environment & Run
- **CommonOptions.in_env_root** – Flag when config is loaded from inside env (e.g. /etc/epkg/env.yaml); used for env_config path and light_init skip.
- **env_config path inside namespace** – `get_env_config_path()` reads from /etc/epkg/env.yaml when `in_env_root`; ENV_CONFIG cache when inside env.
- **Run env detection** – Reorder `determine_environment_final()` to try /etc/epkg/env.yaml before config env_name; `apply_env_config_from_path()` for -r path and run-selected env.
- **LC_ALL=C** – Use LC_ALL instead of LANG for command env to avoid setlocale warnings (e.g. debianutils update-shells).
- **env create --root** – Write config to `$env_root/etc/epkg/env.yaml`; skip light_init for create; -e/-r equivalence for run (namespace/mounts when -r path).
- **try_light_init skip** – Skip when `in_env_root` (avoid "Environment already exists" inside chroot); skip for `EpkgCommand::EnvPath`.

#### Testing & E2E
- **test-one.sh** – Renamed from test.sh; -d/-dd/-ddd debug (RUST_LOG, sh -x); `parse_debug_flags()` in lib.sh.
- **test-iur.sh** – Install-remove-upgrade tests with predefined OS/package matrix; skipped in test-all.sh; -d/-dd support.
- **test-dev.sh** – Build-from-source across Docker images (openeuler, ubuntu, fedora, archlinux); git safe.directory, clone to writable dir.
- **test-sandbox-run.sh** – Automated env/fs sandbox CLI and config tests.
- **test-vm-sandbox.sh** – VM integration test (echo, whoami, ls, QEMU log checks); -d/-dd/-ddd.
- **test-bash-sh** – Install curl and test https://bing.com/; skip epkg list for conda; limit search --paths to one OS per format; unified diff on list mismatch; bash -c epkg list.
- **Static binary paths** – host-vars.sh and build-from-source test use `target/$RUST_TARGET/debug/epkg`; build_static_binary() and test_static_binary() in e2e.
- **common.sh** – Shared `parse_debug_flags()` for sandbox tests.

#### Scriptlets & Maintainer Scripts
- **Scriptlet timeout** – 100s timeout and kill for stuck scriptlets; warn after 10s if blocked; newuidmap/newgidmap error hints.
- **Deb scriptlet layout** – Symlink scriptlets (post_install.sh → ../deb/postinst) so debconf sees postinst path and loads templates; resolve script path with canonicalize for $0; fixes ca-certificates postinst exit 10 and missing CA bundle.
- **dpkg-maintscript-helper** – Full rm_conffile, mv_conffile, symlink_to_dir, dir_to_symlink with version checks, DPKG_ROOT, abort/purge handling.

#### Other
- **find-long-fns.py** – Script for long function analysis.
- **.gitignore** – Additional patterns.
- **BUILD_TIME** – Full timestamp and timezone in build.rs for debug builds.

### Changed
- **Makefile / make.sh** – Default `make` = static debug; `make release` = static release (static binaries); `make build` = dynamic debug; `make release-x86_64` / `release-aarch64` / `release-all` for cross-compilation; `make dev-depends` installs git/wget; avoid sudo when root; simplify detect_arch to arch; native aarch64 musl via is_native_arch() and get_cross_compiler().
- **build_static()** – pushd/popd around checksum step; mkdir -p target/$mode before cp -vfs for symlink when using --target.
- **Cargo** – Updated/downgraded crate versions for rustc 1.82 (openEuler 24.03-LTS); `cargo build --ignore-rust-version`; Cargo.lock and build fixes.
- **Logging** – `ureq_proto` capped at Warn in setup_logging() so RUST_LOG=trace is usable.
- **Repo iteration** – `repodata_indice` and `RepoIndex::repo_shards` use BTreeMap for deterministic `epkg list` output.
- **Shared store logic** – Simplified; no executable-path rules; root + envs dir existence only; debug logging.
- **E2E** – build-from-source-test skipped in test-all.sh; test-dev.sh runs it; clone to /opt/epkg/build-xxx, git safe.directory; dev-pkgs/crossdev-pkgs split.
- **docs** – Search vs list output stability note (search may vary, list sorted); package-operations en/zh.
- **environment** – curl needs e2fsprogs in openEuler (libcom_err); nested config key support (env_vars.FOO, sandbox.isolate_mode); dirs.rs home_cache, find_nearest_dot_eenv.
- **systemd_tmpfiles** – Removed namespace setup (responsibility separation).
- **main.rs** – Early logging for init applet; improved Ctrl-C handler.

### Fixed
- **init** – Skip light_init when running inside environment (try_light_init when in_env_root); avoids "Environment already exists" in chroot.
- **scriptlets** – Remove embedded Lua and namespace dependency from scriptlet execution.
- **install** – Create store_root and download_cache dirs before get_filesystem_info() so statvfs succeeds on first install (avoids fsid=0 and hardlink→symlink downgrade).
- **deb_triggers** – Resolve Unincorp trigger hooks by base name and batch preference (find_hook_for_trigger); log available hooks at DEBUG when no hook found.
- **Conda** – Honor rattler prefix_placeholder schema (flattened string + file_mode sibling); skip checksum validation when expected is empty (repodata items with hash_type but no hash).
- **Download** – Treat truncated downloads as ContentIncomplete (resumable) not corrupt; do not delete .part on incomplete; chunk offset mismatch keeps file for resume; avoid deadlock between wait loop and processing thread (do not hold manager.tasks and task.status together).
- **Mirror** – Fedora mirror selection: re-insert "fedora" in distro_dirs for matching/path resolution; eq_ignore_ascii_case → exact match for distro_dirs.
- **Expose** – Resolve script interpreter inside env with resolve_symlink_in_env before canonicalize (avoids ENOENT for sh→yash when only env usr/bin/yash exists); add yash to alternative interpreters for sh.
- **env remove** – Use get_env_base_path() instead of get_env_root() to avoid panic when env does not exist (config file missing).
- **dpkg_divert / dpkg_statoverride** – Align with upstream (path cleanup, --list [FILE], exit codes, add/remove validation, --list [GLOB] for divert).
- **rpm_verify** – Quiet unnecessary log warn.
- **Source** – Arch Linux and archlinuxcn mirror/config updates (fix 404 for gmp etc.).
- **E2E** – build-from-source and bare-rootfs static binary path; git safe.directory and clone to writable dir; conda epkg list comparison skipped.

### Removed
- **Environment SSL from host** – Reverted "populate /etc/ssl/certs from host"; each distro installs its own SSL certs (ca-certificates scriptlet fix used instead).

### Design / Architecture
- **models.rs** – IsolateMode, NamespaceStrategy, MountSpec, SandboxOptions, ProcessCreationConfig, UnifiedChildContext; ENV_CONFIG for in-env config cache.
- **Unified process creation** – fork_and_execute() → prepare_run_options_for_command, determine_process_config(), build_unified_context(), create_process_with_namespaces(); NamespaceStrategy × IsolateMode; IdMapSync pipe; child_setup_with_namespaces, child_mount_and_exec, mount_batch_specs(); Vm path via qemu::run_command_in_qemu().
- **New modules** – src/mount.rs (MountSpec, parse_mount_spec), src/namespace.rs, src/idmap.rs, src/qemu.rs, src/vm_client.rs, src/utils.rs (size parsing, resolve_symlink_in_env); applets: init.rs, vm_daemon.rs, insmod, modprobe, df, mount, umount, mountpoint, ifconfig, route, uname, deb_systemd_helper, update_alternatives, dpkg_realpath, mktemp; dpkg, dpkg_divert, dpkg_statoverride, dpkg_maintscript_helper.
- **dirs.rs** – get_env_config_path() in_env_root branch; get_env_base_path() public; home_cache, find_nearest_dot_eenv.

### Statistics
- **124 files changed, 14486 insertions(+), 2350 deletions(-)**

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
