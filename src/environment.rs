use std::fs;
use std::env;
use std::path::Path;
use std::path::PathBuf;
use std::collections::{HashSet, HashMap};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;
use std::time::{SystemTime, UNIX_EPOCH};
use color_eyre::Result;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use serde_json;
use serde_yaml;
#[cfg(unix)]
use nix::errno::Errno;
#[cfg(unix)]
use nix::unistd::chown;
use glob;
use crate::models::*;
use crate::dirs::*;
use crate::repo::sync_channel_metadata;
use crate::utils::{force_symlink_dir_for_virtiofs, force_symlink_file_for_native};
use crate::utils::force_symlink_file_for_virtiofs;
use crate::deinit::force_remove_dir_all;
#[cfg(unix)]
use crate::deb_triggers::ensure_triggers_dir;
use crate::plan::prepare_installation_plan;
use crate::install::execute_installation_plan;
use crate::history::record_history;
use crate::path::update_path;
use crate::shell_emit;
use crate::io;
use crate::lfs;
use log::warn;

// epkg stores persistent PATH registration metadata inside each environment's
// `etc/epkg/env.yaml`. The `register_to_path` flag combined with
// `register_path_order` drives how PATH is constructed:
//
// PATH layout:
//   registered prepend entries (path-order >= 0)
//   + original PATH
//   + registered append entries (path-order < 0)
//
// Register/Unregister:
//   * `epkg env register` / `epkg env unregister` toggle env.yaml values
//   * Affects all shell sessions
//
// Activate/Deactivate:
//   * Session-only PATH updates stacked on top of registered envs
//   * Compatible with pure/stack modes
//
// Environment Registration Rules:
// - `epkg env register <name> [--path-order N]`
// - If `--path-order` is omitted the first free multiple of 10 (>= 100) is chosen
//   (100, 110, 120, ...) so earlier registrations get earlier PATH positions by default.
// - `N >= 0` participates in the prepend side, `N < 0` in the append side
//
// Example registrations:
//   epkg env register openeuler2409
//   epkg env register debian12 --path-order 18
//
// Example activations:
//   epkg env activate project-dev                  # Activate project environment
//   epkg env activate test-env --pure              # Activate in pure mode (no inherited paths)
//   epkg env deactivate                            # Return to default environment

// Helper function to handle environment variable changes
// Note: PATH is handled by update_path() instead of push_env_var(), since PATH could be changed by
// interleaved (de)activate/(un)register calls.
fn push_env_var(
    script: &mut String,
    key: &str,
    new_value: Option<String>,
    original_value: Option<String>,
    kind: shell_emit::ShellKind,
) {
    if let Some(v) = &new_value {
        println!("{}", shell_emit::emit_export(key, v, kind));
    }

    match kind {
        shell_emit::ShellKind::Bash => match original_value {
            Some(v) => script.push_str(&format!(
                "export {}=\"{}\"\n",
                key,
                v.replace('\\', "\\\\").replace('"', "\\\"")
            )),
            None => script.push_str(&format!("unset {}\n", key)),
        },
        shell_emit::ShellKind::PowerShell => match original_value {
            Some(v) => script.push_str(&format!(
                "$env:{} = '{}'\n",
                key,
                shell_emit::ps_escape_single_quoted(&v)
            )),
            None => script.push_str(&format!(
                "Remove-Item \"Env:{}\" -ErrorAction SilentlyContinue\n",
                key
            )),
        },
    }
}

fn next_prepend_path_order() -> Result<i32> {
    let shared_store = config().init.shared_store;
    let registered = registered_env_configs(shared_store);
    let used: HashSet<i32> = registered.into_iter()
        .filter(|cfg| cfg.register_path_order >= 0)
        .map(|cfg| cfg.register_path_order)
        .collect();

    let mut priority = 100;
    while used.contains(&priority) {
        priority += 10;
    }

    Ok(priority)
}

/// Get list of all environment names except 'self'
///
/// This function lists all environment directories in both private and public
/// locations, excluding the special 'self' environment.
///
/// Returns a Vec of (env_name, is_public) tuples.
pub fn get_all_env_names() -> Result<Vec<(String, bool)>> {
    let mut my_envs = Vec::new();
    let mut other_envs = Vec::new();
    let current_user = get_username()?;
    let shared_store = config().init.shared_store;
    let user_envs = dirs_ref().user_envs.clone();

    // Walk environments (private and public)
    walk_environments(shared_store, &user_envs, |env_path, owner| {
        let name = env_path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();

        if name != SELF_ENV && !name.starts_with('.') {
            // An environment is considered "public" when:
            // - It lives under /opt/epkg/envs (owner is Some)
            // - The environment directory does NOT have private-only (700) permissions
            //
            // Private environments are explicitly created with mode 0o700
            // in create_environment_dirs().
            let is_public = if owner.is_none() {
                // Environments from private store (owner=None) are always private
                false
            } else {
                // Environments from shared store (owner=Some) may be public or private
                // Use symlink_metadata to avoid following symlinks in env context
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = lfs::symlink_metadata(env_path)?
                        .permissions()
                        .mode() & 0o777;
                    let is_private = mode == 0o700;
                    !is_private
                }
                #[cfg(not(unix))]
                {
                    // On Windows, all environments are considered public
                    true
                }
            };

            // Decide ownership: any env under the personal store (owner == None)
            // or with owner == current user is considered "mine".
            let is_mine = match owner {
                None => true,
                Some(o) => o == current_user.as_str(),
            };

            // For environments owned by another user (owner is Some),
            // prefix with "$owner/" to match the directory layout.
            let env_display_name = if is_mine {
                name.clone()
            } else {
                format!("{}/{}", owner.unwrap(), name)
            };

            if is_mine {
                my_envs.push((env_display_name, is_public));
            } else if is_public {
                other_envs.push((env_display_name, is_public));
            }
        }
        Ok(())
    })?;

    // Sort by name within each group and then return "mine" first, others second.
    my_envs.sort_by(|a, b| a.0.cmp(&b.0));
    other_envs.sort_by(|a, b| a.0.cmp(&b.0));

    my_envs.extend(other_envs.into_iter());
    Ok(my_envs)
}

pub fn list_environments() -> Result<()> {
    // Get all environments except self
    let all_envs = get_all_env_names()?;

    // Get active environments list once and convert to Vec for order lookup
    let active_list: Vec<String> = env::var("EPKG_ACTIVE_ENV")
        .ok()
        .map(|active| active.split(':').map(String::from).collect())
        .unwrap_or_default();

    // Get registered environment configs to retrieve register_path_order
    let shared_store = config().init.shared_store;
    let registered_configs = registered_env_configs(shared_store);
    let registered_map: HashMap<String, i32> = registered_configs
        .into_iter()
        .map(|cfg| (cfg.name, cfg.register_path_order))
        .collect();

    // Print table header with columns: Type, Status, Environment, Channel, Root
    println!("{:<10}  {:<25}  {:<30}  {:<20}  {}",
             "Type", "Status", "Environment", "Channel", "Root");
    println!("{}", "-".repeat(130));

    // Print each environment with its status
    for (env, is_public) in all_envs {
        let mut status_parts = Vec::new();

        // Check if environment is in active list and get its order (1-indexed)
        if let Some(pos) = active_list.iter().position(|e| e == &env) {
            let order = pos + 1;
            status_parts.push(format!("activated@{}", order));
        }

        // Check if environment is registered and get its order
        if let Some(&order) = registered_map.get(&env) {
            status_parts.push(format!("registered@{}", order));
        }

        let env_type = if is_public { "public" } else { "private" };
        let status = status_parts.join(",");

        // Get environment root path and channel config
        let (env_root, channel) = match get_env_root(env.clone()) {
            Ok(root) => {
                let root_str = root.display().to_string();
                // Try to get channel config for this environment
                match crate::io::deserialize_channel_config_from_root(&root) {
                    Ok(configs) => {
                        if let Some(cc) = configs.first() {
                            (root_str, cc.channel.clone())
                        } else {
                            (root_str, String::new())
                        }
                    }
                    Err(_) => (root_str, String::new()),
                }
            }
            Err(_) => ("N/A".to_string(), String::new()),
        };

        println!("{:<10}  {:<25}  {:<30}  {:<20}  {}",
            env_type,
            status,
            env,
            channel,
            env_root
        );
    }

    Ok(())
}

fn setup_resolv_conf(env_root: &Path) -> Result<()> {
    // Create /etc directory if it doesn't exist
    lfs::create_dir_all(env_root.join("etc"))?;

    let resolv_conf_path = crate::dirs::path_join(env_root, &["etc", "resolv.conf"]);

    // Skip on 'docker -v /etc/resolv.conf:/etc/resolv.conf:ro' and installing to /
    if lfs::exists_in_env(&resolv_conf_path) {
        return Ok(());
    }

    #[cfg(windows)]
    {
        // Windows doesn't use /etc/resolv.conf. Add a placeholder so Linux-oriented tooling
        // inside env sees a predictable file without forcing Unix-only code paths.
        let windows_stub = "# Managed by epkg on Windows\n# DNS resolution is provided by Windows networking.\n";
        lfs::write(&resolv_conf_path, windows_stub)?;
    }

    #[cfg(not(windows))]
    {
        let host_resolv_conf = Path::new("/etc/resolv.conf");
        if lfs::exists_on_host(host_resolv_conf) {
            lfs::copy(host_resolv_conf, &resolv_conf_path)?;
        } else {
            // If /etc/resolv.conf doesn't exist on host, create a default one
            warn!("/etc/resolv.conf does not exist on host. Creating default resolv.conf");
            let default_resolv_conf = "nameserver 8.8.8.8\nnameserver 223.6.6.6\nnameserver 8.8.4.4\nnameserver 1.1.1.1\n";
            lfs::write(&resolv_conf_path, default_resolv_conf)?;
        }
    }

    Ok(())
}

fn setup_hosts(env_root: &Path) -> Result<()> {
    // Create /etc directory if it doesn't exist
    lfs::create_dir_all(env_root.join("etc"))?;

    let hosts_path = crate::dirs::path_join(env_root, &["etc", "hosts"]);

    // Skip if already exists (e.g., mounted or installed by package)
    if lfs::exists_in_env(&hosts_path) {
        return Ok(());
    }

    #[cfg(windows)]
    {
        // Windows doesn't use /etc/hosts for DNS. Add a placeholder.
        let windows_stub = "# Managed by epkg on Windows\n127.0.0.1 localhost\n::1 localhost\n";
        lfs::write(&hosts_path, windows_stub)?;
    }

    #[cfg(not(windows))]
    {
        let host_hosts = Path::new("/etc/hosts");
        if lfs::exists_on_host(host_hosts) {
            lfs::copy(host_hosts, &hosts_path)?;
        } else {
            // If /etc/hosts doesn't exist on host, create a default one
            let default_hosts = "127.0.0.1 localhost\n::1 localhost\n";
            lfs::write(&hosts_path, default_hosts)?;
        }
    }

    Ok(())
}

fn create_environment_dirs_early(env_root: &Path) -> Result<()> {
    let generations_root = env_root.join("generations");
    let gen_1_dir = generations_root.join("1");

    // Create env_root directory first and enable case sensitivity on Windows
    // This must be done before creating any subdirectories
    lfs::create_dir_all_with_case_sensitivity(env_root)?;

    // Create basic directories
    lfs::create_dir_all(&gen_1_dir)?;
    lfs::create_dir_all(env_root.join("root"))?;
    lfs::create_dir_all(env_root.join("ebin"))?;     // for script interpreters,
                                                    // won't go to PATH
    lfs::create_dir_all(env_root.join("ebin"))?;
    // usr/sbin creation is delayed to create_environment_dirs() (may be symlink on Fedora)
    lfs::create_dir_all(crate::dirs::path_join(env_root, &["usr", "bin"]))?;
    lfs::create_dir_all(crate::dirs::path_join(env_root, &["usr", "lib"]))?;
    lfs::create_dir_all(crate::dirs::path_join(env_root, &["usr", "share"]))?;
    lfs::create_dir_all(crate::dirs::path_join(env_root, &["usr", "include"]))?;
    lfs::create_dir_all(crate::dirs::path_join(env_root, &["usr", "local", "bin"]))?;
    // Create usr/bin and usr/sbin for usr-merge symlinks (needed for Windows junctions)
    lfs::create_dir_all(crate::dirs::path_join(env_root, &["usr", "bin"]))?;
    lfs::create_dir_all(crate::dirs::path_join(env_root, &["usr", "sbin"]))?;
    lfs::create_dir_all(env_root.join("var"))?;
    lfs::create_dir_all(crate::dirs::path_join(env_root, &["opt", "epkg"]))?;
    lfs::create_dir_all(env_root_etc_epkg(env_root))?;

    // Create symlinks in generation 1 (usr-merge layout)
    // This allows brew packages (which have bin/, share/, include/ at root)
    // to work correctly with Linux namespace isolation that mounts env_root/usr -> /usr
    force_symlink_dir_for_virtiofs("usr/sbin", env_root.join("sbin"))?;
    force_symlink_dir_for_virtiofs("usr/bin", env_root.join("bin"))?;
    // On Windows, skip lib symlink because:
    // 1. Conda packages use Lib/ (capital L) for Python standard library
    // 2. Windows is case-insensitive: Lib == lib
    // 3. If lib -> usr/lib exists, conda's Lib/ files would resolve to usr/lib/
    //    breaking Python's expected Lib/ directory structure
    #[cfg(not(windows))]
    force_symlink_dir_for_virtiofs("usr/lib", env_root.join("lib"))?;
    force_symlink_dir_for_virtiofs("usr/share", env_root.join("share"))?;
    force_symlink_dir_for_virtiofs("usr/include", env_root.join("include"))?;
    // NOTE: usr/libexec, Frameworks, opt symlinks for brew packages are created in
    // create_environment_dirs() only when pkg_format is Brew.
    // See the Brew-specific block in create_environment_dirs() for details.

    // Create "current" symlink in generations directory pointing to generation 1
    force_symlink_dir_for_virtiofs("1", generations_root.join("current"))?;

    setup_resolv_conf(env_root)?;
    setup_hosts(env_root)?;

    Ok(())
}

// ============================================================================
// epkg Binary and Applet Symlink Architecture
// ============================================================================
//
// This section documents the dual-binary mechanism and symlink setup for epkg
// and its busybox-style applets across different platforms.
//
// ## Self Environment (epkg Installation)
//
// The `self` environment stores epkg's own binaries. Layout:
// ```
// $ENVS/self/usr/bin/
// ├── epkg[.exe]          # Native host binary (Linux/Windows/macOS)
// ├── elf-loader          # Linux only: dynamic linker for glibc packages
// └── epkg-linux-$arch    # Windows/macOS only: Linux ELF for VM execution
// ```
//
// Setup by: `init.rs::setup_common_binaries()` during `epkg self install`
//
// ## Other Environments (User Package Environments)
//
// Each user environment has symlinks to the appropriate epkg binary:
// ```
// $ENVS/<env>/usr/bin/
// ├── epkg[.exe]  -> $ENVS/self/usr/bin/<appropriate-epkg-binary>
// ├── rpm         -> ../epkg[.exe]  (applet symlink)
// ├── dpkg        -> ../epkg[.exe]
// └── ...
// ```
//
// Setup by:
// - `environment.rs::create_epkg_symlink()` - main epkg symlink
// - `busybox/mod.rs::create_all_applet_symlinks()` - applet symlinks
//
// ## Dual-Binary Mechanism
//
// On Windows/macOS hosts, two binaries may exist:
// 1. Native binary (epkg.exe/epkg): handles Conda/Brew/msys2 packages natively
// 2. Linux ELF binary (epkg-linux-$arch): runs inside VM for Linux packages
//
// Binary selection by package format:
// | Host       | Package Format | Binary Used           | Execution Context |
// |------------|---------------|----------------------|-------------------|
// | Linux      | All           | epkg                  | Native            |
// | Windows    | Conda/msys2   | epkg.exe              | Native            |
// | Windows    | deb/rpm/apk   | epkg-linux-$arch      | VM (libkrun)      |
// | macOS      | Conda/Brew    | epkg                  | Native            |
// | macOS      | deb/rpm/apk   | epkg-linux-$arch      | VM (libkrun)      |
//
// ## Symlink Naming Convention
//
// - Native packages on Windows: use `.exe` suffix (epkg.exe, rpm.exe)
// - Linux packages (any host): NO `.exe` suffix (epkg, rpm)
//   - These run in Linux VM where Windows extensions don't exist
//   - Applets follow same convention via `is_windows_target()` in busybox/mod.rs
//
// ## When Symlinks Are Used
//
// 1. Scriptlet execution: maintainer scripts call `rpm`, `dpkg`, etc.
//    - Linux hosts: native execution
//    - Windows/macOS with Linux packages: executed inside VM via libkrun
//
// 2. User commands: `epkg run`, `epkg install`, etc.
//    - Always use native epkg binary on host
//    - May internally use VM for package operations
//
// 3. Applet invocation: symlink names like `rpm`, `dpkg` invoke epkg
//    - epkg detects invoked name and routes to appropriate applet handler
// ============================================================================

/// Determine the symlink name for epkg binary based on package format.
/// - Linux-format packages (deb/rpm/apk/arch-linux): "epkg" (no .exe, runs in VM)
/// - Native packages on Windows: "epkg.exe"
/// - Native packages on Unix: "epkg"
fn epkg_symlink_name(pkg_format: &PackageFormat) -> &'static str {
    // Linux-format packages run in VM where .exe suffix doesn't exist
    let is_linux_target = matches!(pkg_format,
        PackageFormat::Deb | PackageFormat::Rpm | PackageFormat::Apk
    );
    let is_arch_linux = *pkg_format == PackageFormat::Pacman &&
        crate::models::channel_config().distro != "msys2";

    if is_linux_target || is_arch_linux {
        return "epkg";  // No .exe suffix for Linux VM execution
    }

    // Native packages: use platform-specific name
    crate::dirs::EPKG_USR_BIN_NAME
}

/// Ensure \$env_root/usr/bin/epkg symlink exists and points to appropriate epkg binary.
///
/// See the architecture documentation above for the dual-binary mechanism.
///
/// Key behaviors:
/// - Uses absolute path for the symlink target to work correctly when accessed from host
/// - Always creates or overwrites the symlink (`force_symlink_to_*`) without checking existing target
/// - Returns Ok(()) even if self environment not found (no-op)
/// - Logs debug messages for symlink creation
pub fn create_epkg_symlink(env_root: &Path, pkg_format: &PackageFormat) -> Result<()> {
    #[cfg(target_os = "linux")]
    let _ = pkg_format;

    // Try to find appropriate epkg binary in self environment
    if let Some(self_env_root) = find_env_root(SELF_ENV) {
        // Skip if this IS the self environment - the binary is already installed by setup_common_binaries()
        // Creating a symlink here would either be redundant or create a self-referencing symlink
        // Use canonicalize for comparison to handle Windows \\?\ prefix differences
        let env_root_normalized = std::fs::canonicalize(env_root).unwrap_or_else(|_| env_root.to_path_buf());
        let self_env_root_normalized = std::fs::canonicalize(&self_env_root).unwrap_or(self_env_root.clone());
        if env_root_normalized == self_env_root_normalized {
            log::debug!("Skipping epkg symlink creation in self environment (binary already installed)");
            return Ok(());
        }

        // Determine symlink name: "epkg" for Linux packages, "epkg[.exe]" for native
        let symlink_name = epkg_symlink_name(pkg_format);
        let epkg_symlink = crate::dirs::path_join(env_root, &["usr", "bin", symlink_name]);

        // On Windows/macOS with Linux-format packages, use epkg-linux-$arch
        #[cfg(not(target_os = "linux"))]
        {
            let needs_vm = matches!(pkg_format,
                PackageFormat::Deb | PackageFormat::Rpm | PackageFormat::Apk
            );
            let is_arch_linux = *pkg_format == PackageFormat::Pacman &&
                crate::models::channel_config().distro != "msys2";

            if needs_vm || is_arch_linux {
                let arch = &crate::config().common.arch;
                let self_epkg_linux = crate::dirs::path_join(&self_env_root, &["usr", "bin", &format!("epkg-linux-{}", arch)]);
                if lfs::exists_in_env(&self_epkg_linux) {
                    log::debug!("Creating epkg symlink {} -> {} (Linux VM)", epkg_symlink.display(), self_epkg_linux.display());
                    force_symlink_file_for_native(&self_epkg_linux, &epkg_symlink)
                        .with_context(|| format!("Failed to create epkg symlink in {}", epkg_symlink.display()))?;
                    return Ok(());
                } else {
                    log::debug!("epkg-linux-{} not found in self env, skipping epkg symlink", arch);
                    return Ok(());
                }
            }
        }

        // Default: use native epkg binary
        let self_epkg = crate::dirs::path_join(&self_env_root, &["usr", "bin", crate::dirs::EPKG_USR_BIN_NAME]);
        if lfs::exists_in_env(&self_epkg) {
            log::debug!("Creating epkg symlink {} -> {} (native)", epkg_symlink.display(), self_epkg.display());
            force_symlink_file_for_native(&self_epkg, &epkg_symlink)
                .with_context(|| format!("Failed to create epkg symlink in {}", epkg_symlink.display()))?;
        }
    }
    Ok(())
}

fn create_environment_dirs(env_root: &Path, pkg_format: &PackageFormat, env_config: &EnvConfig, channel_config: &ChannelConfig) -> Result<()> {
    // Create different lib64 symlinks based on package format
    match pkg_format {
        PackageFormat::Pacman => {
            // For Pacman format:
            // /usr/lib64 -> lib
            // /lib64 -> usr/lib
            lfs::create_dir_all(env_root.join("usr"))?;
            force_symlink_dir_for_virtiofs("lib", crate::dirs::path_join(env_root, &["usr", "lib64"]))?;
            force_symlink_dir_for_virtiofs("usr/lib", env_root.join("lib64"))?;
        },
        _ => {
            // Default behavior for other formats
            lfs::create_dir_all(crate::dirs::path_join(env_root, &["usr", "lib64"]))?;
            force_symlink_dir_for_virtiofs("usr/lib64", env_root.join("lib64"))?;

            if lfs::exists_on_host("/usr/lib32") {
                lfs::create_dir_all(crate::dirs::path_join(env_root, &["usr", "lib32"]))?;
                force_symlink_dir_for_virtiofs("usr/lib32", env_root.join("lib32"))?;
            }
        }
    }

    // Create usr/libexec symlink for Brew packages (macOS only)
    // Brew packages like 'go' have symlinks like bin/go -> ../libexec/bin/go
    // This expects usr/libexec to point to ../libexec (top-level libexec dir)
    //
    // Also create Frameworks and opt symlinks for macOS brew packages
    // e.g., bin/python3.12 -> ../Frameworks/Python.framework/...
    if *pkg_format == PackageFormat::Brew {
        #[cfg(target_os = "macos")]
        {
            force_symlink_dir_for_virtiofs("../libexec", crate::dirs::path_join(env_root, &["usr", "libexec"]))?;
            force_symlink_dir_for_virtiofs("../Frameworks", crate::dirs::path_join(env_root, &["usr", "Frameworks"]))?;
            force_symlink_dir_for_virtiofs("../opt", crate::dirs::path_join(env_root, &["usr", "opt"]))?;
        }
        // On Linux, do nothing - packages will create usr/libexec as a real directory if needed
    }

    // Fedora: usr/sbin is a symlink to bin (unified /usr/bin and /usr/sbin)
    if channel_config.distro == "fedora" {
        force_symlink_dir_for_virtiofs("bin", crate::dirs::path_join(env_root, &["usr", "sbin"]))?;
    } else {
        lfs::create_dir_all(crate::dirs::path_join(env_root, &["usr", "sbin"]))?;
    }

    // Debian-specific layout (triggers directory for dpkg-trigger compatibility)
    #[cfg(unix)]
    if pkg_format == &PackageFormat::Deb {
        ensure_triggers_dir(env_root)?;
    }

    // Ensure usr/bin/epkg exists, pointing to appropriate epkg binary
    create_epkg_symlink(env_root, pkg_format)?;

    // Create symlinks for applets in usr/local/bin/
    create_applet_symlinks(env_root, pkg_format)?;

    // Set owner and permissions if environment is private (public = false)
    #[cfg(unix)]
    if !env_config.public {
        // Get current user's UID and GID (effective, handles suid)
        let uid = nix::unistd::geteuid();
        let gid = nix::unistd::getegid();

        // Set owner to current user (best-effort: user namespaces / some tmpfs setups return EINVAL)
        match chown(env_root, Some(uid), Some(gid)) {
            Ok(()) => {}
            Err(e) if e == Errno::EINVAL || e == Errno::EPERM || e == Errno::ENOTSUP => {
                log::debug!(
                    "Could not chown private env at {} ({}); continuing",
                    env_root.display(),
                    e
                );
            }
            Err(e) => {
                return Err(e).wrap_err_with(|| format!("Failed to set owner for {}", env_root.display()));
            }
        }

        // Set mode to 700 (rwx------)
        crate::utils::set_permissions_from_mode(env_root, 0o700)
            .wrap_err_with(|| format!("Failed to set permissions for {}", env_root.display()))?;
    }

    #[cfg(windows)]
    if !env_config.public {
        // Keep behavior explicit on Windows until ACL-based private env enforcement is added.
        log::debug!("private environment requested for '{}'; ACL hardening is not implemented on Windows yet", env_root.display());
    }

    Ok(())
}

// These symlinks must be created and available before running scriptlets.
// If the distro provides the commands, they'll overwrite symlink to our implementation.
fn create_applet_symlinks(env_root: &Path, pkg_format: &PackageFormat) -> Result<()> {
    // Create a symlink from systemctl to /usr/bin/true to prevent blocking on systemctl daemon-reload
    let systemctl_path = crate::dirs::path_join(env_root, &["usr", "bin", "systemctl"]);
    if !lfs::exists_in_env(&systemctl_path) {
        force_symlink_file_for_virtiofs("/usr/bin/true", &systemctl_path)
            .with_context(|| format!("Failed to create systemctl symlink in {}", systemctl_path.display()))?;
    }

    // Automatically discover all applets and create links.
    // On Windows, file symlinks use symlink_file_for_virtiofs (hardlink/copy when needed).
    crate::busybox::create_all_applet_symlinks(env_root, pkg_format)?;

    Ok(())
}

fn create_default_world_json(env_root: &Path, pkg_format: &PackageFormat) -> Result<()> {
    let mut world = std::collections::HashMap::new();

    // Set default no-install packages for Pacman/Rpm/Deb formats
    match pkg_format {
        PackageFormat::Pacman | PackageFormat::Rpm | PackageFormat::Deb => {
            let mut no_install_packages = vec!["systemd", "systemd-udev", "udev", "dbus",
                "grubby", "grub2", "dracut", "kpartx",
                "pam", "kbd", "kmod",
                "cron", "cronie", "crontabs",
            ];

            // Add format-specific packages
            match pkg_format {
                PackageFormat::Pacman => no_install_packages.push("pacman"),
                PackageFormat::Rpm => no_install_packages.push("dnf"),
                PackageFormat::Deb => no_install_packages.push("apt"),
                _ => {}
            }

            world.insert("no-install".to_string(), no_install_packages.join(" "));
        }
        _ => {}
    }

    // Write world.json
    let world_path = crate::dirs::path_join(env_root, &["generations", "1", "world.json"]);
    let world_json = serde_json::to_string_pretty(&world)?;
    lfs::write(&world_path, world_json)?;

    Ok(())
}

/// Install packages and create metadata files for the environment
fn import_packages_and_create_metadata(env_root: &Path) -> Result<()> {
    let gen_1_dir = crate::dirs::path_join(env_root, &["generations", "1"]);
    let installed_packages_path = gen_1_dir.join("installed-packages.json");

    // Read packages to install from JSON if importing (supports both object and array format)
    let packages_to_import = if config().env.import_file.is_some() {
        io::read_installed_packages_from_path(&installed_packages_path)?
    } else {
        InstalledPackagesMap::new()
    };

    // Install packages if any
    if !packages_to_import.is_empty() {
        sync_channel_metadata()?;
        let plan = prepare_installation_plan(&packages_to_import, None)?;
        execute_installation_plan(plan)?;
    } else {
        // Create metadata files
        lfs::write(installed_packages_path, "{\n}")?;

        // Record the environment creation in command history
        record_history(&gen_1_dir, None)?;
    }

    Ok(())
}

/// Initialize env_config and channel_configs
fn initialize_environment_config(env_name: &str, env_root: &Path, env_base: &Path) -> Result<(EnvConfig, PackageFormat, ChannelConfig)> {
    // Initialize environment config and create channel config files
    let mut env_config = if let Some(import_file) = &config().env.import_file {
        import_environment_from_file(env_root, import_file)?
    } else {
        copy_channel_configs(env_root)?;
        EnvConfig::default()
    };

    // Override config values by command line options
    override_env_config(&mut env_config, env_name, env_base, env_root);

    // Save environment config
    io::serialize_env_config(env_config.clone())?;

    let channel_configs = io::deserialize_channel_config_from_root(&env_root.to_path_buf())?;
    let pkg_format = channel_configs[0].format.clone();
    let channel_config = channel_configs[0].clone();

    Ok((env_config, pkg_format, channel_config))
}

/// Setup and validate environment paths, create symlinks if needed
fn setup_environment_paths(env_base: &PathBuf) -> Result<PathBuf> {
    let env_root = if !config().common.env_root.is_empty() {
        PathBuf::from(&config().common.env_root)
    } else {
        env_base.clone()
    };

    let env_channel_yaml = env_root_channel_yaml(&env_root);
    if lfs::exists_on_host(&env_channel_yaml) {
        return Err(eyre::eyre!("Environment already exists at path: '{}'", env_root.display()));
    }

    // If env_root is specified, we need to create a symlink from env_base to env_root
    if !config().common.env_root.is_empty() {
        // Check if env_base already exists as a directory (not a symlink/junction)
        // Use exists_no_follow to check if path exists, then check if it's NOT a symlink/junction
        if lfs::exists_no_follow(&env_base) && !lfs::is_symlink_or_junction(&env_base) {
            return Err(eyre::eyre!("Environment base path '{}' already exists as a directory. Cannot create symlink.", env_base.display()));
        }
        // Ensure parent directory of env_base exists
        if let Some(parent) = env_base.parent() {
            lfs::create_dir_all(parent)?;
        }
        force_symlink_dir_for_virtiofs(&env_root, &env_base)
            .with_context(|| format!("Failed to create symlink from {} to {}", env_base.display(), env_root.display()))?;
    }

    Ok(env_root)
}

pub fn create_environment(env_name: &str) -> Result<()> {
    let env_base = dirs().user_envs.join(env_name);
    let env_root = setup_environment_paths(&env_base)?;

    println!("Creating environment '{}' in {}", env_name, env_root.display());

    // Warn if auto-generated name (starts with "__")
    if env_name.starts_with("__") {
        println!("# Note: environment name '{}' was auto-generated from path '{}'", env_name, env_root.display());
    }

    // Create basic directories early (before we need channel configs)
    create_environment_dirs_early(&env_root)?;

    // Initialize environment config and get package format
    let (env_config, pkg_format, channel_config) = initialize_environment_config(env_name, &env_root, &env_base)?;
    create_environment_dirs(&env_root, &pkg_format, &env_config, &channel_config)?;

    // Create world.json with default no-install packages
    create_default_world_json(&env_root, &pkg_format)?;

    // Install packages and create metadata files
    import_packages_and_create_metadata(&env_root)?;

    Ok(())
}

/*
 * Import environment configuration from a YAML file.
 *
 * The file contains a single YAML document with EnvImport structure.
 * Channel configs are stored in the 'files' field as ImportFile entries
 * with paths like "etc/epkg/channel.yaml" or "etc/epkg/repos.d/debian-ceph.yaml".
 */
fn import_environment_from_file(env_root: &Path, import_file: &str) -> Result<EnvConfig> {
    // Parse the file as EnvExport
    let env_export: EnvExport = io::read_yaml_file(Path::new(import_file))?;

    // Save all files to the environment
    for export_file in &env_export.files {
        // Create parent directories if needed
        let file_path = env_root.join(lfs::host_path_from_manifest_rel_path(
            export_file.path.trim_start_matches('/'),
        ));
        if let Some(parent) = file_path.parent() {
            lfs::create_dir_all(parent)?;
        }

        // Write the file
        lfs::write(&file_path, &export_file.data)?;
    }

    Ok(env_export.env)
}

/// Copy main channel configuration YAML file
fn copy_main_channel_config(sources_path: &Path, env_root: &Path, distro_name: &str, distro_version: Option<&str>) -> Result<()> {
    let src_channel_yaml_path = sources_path.join(format!("{}.yaml", distro_name));

    // Read and optionally modify main channel config
    let mut channel_content = fs::read_to_string(&src_channel_yaml_path)?;
    if let Some(version) = distro_version {
        channel_content = update_version_in_contents(&channel_content, version);
    }

    // Save main channel config
    let dest_channel_path = env_root_channel_yaml(env_root);
    lfs::create_dir_all(dest_channel_path.parent().unwrap())?;
    lfs::write(&dest_channel_path, &channel_content)?;

    Ok(())
}

/// Copy additional repo configurations to etc/epkg/repos.d/
fn copy_repo_configs(sources_path: &Path, env_root: &Path, distro_name: &str) -> Result<()> {
    for repo in &config().env.repos {
        let src_repo_yaml_path = sources_path.join(format!("{}-{}.yaml", distro_name, repo));

        // Copy repo config file
        let repos_dir = env_root_repos_d(env_root);
        lfs::create_dir_all(&repos_dir)?;
        let dest_repo_path = repos_dir.join(format!("{}.yaml", repo));
        lfs::copy(&src_repo_yaml_path, &dest_repo_path)?;
    }

    Ok(())
}


/// Copy channel configuration from source to target environment
/// Handles finding the source channel YAML, reading it, optionally updating version,
/// and saving it to etc/epkg/channel.yaml in the target environment.
/// Also copies additional repo configurations to etc/epkg/repos.d/
fn copy_channel_configs(env_root: &Path) -> Result<()> {
    let sources_path = crate::dirs::path_join(get_epkg_src_path().as_path(), &["assets", "repos"]);
    let (distro_name, distro_version) = parse_channel_option();

    // On Windows, the source path may not exist if running from a standalone binary.
    // Use embedded channel YAML for msys2 (pacman); otherwise default to Conda.
    if !sources_path.exists() {
        #[cfg(windows)]
        {
            if distro_name == "msys2" {
                create_default_msys2_channel_config(env_root)?;
                return Ok(());
            }
            create_default_conda_channel_config(env_root)?;
            return Ok(());
        }
        #[cfg(not(windows))]
        {
            return Err(eyre::eyre!(
                "Channel configs source path does not exist: {}",
                sources_path.display()
            ));
        }
    }

    copy_main_channel_config(&sources_path, env_root, &distro_name, distro_version.as_deref())?;
    copy_repo_configs(&sources_path, env_root, &distro_name)?;

    Ok(())
}

/// Create MSYS2 channel configuration from embedded assets (standalone Windows binary).
#[cfg(windows)]
fn create_default_msys2_channel_config(env_root: &Path) -> Result<()> {
    let channel_content = include_str!("../assets/repos/msys2.yaml");

    let dest_channel_path = env_root_channel_yaml(env_root);
    lfs::create_dir_all(dest_channel_path.parent().unwrap())?;
    lfs::write(&dest_channel_path, channel_content)?;

    println!("Created MSYS2 (pacman) channel configuration");
    Ok(())
}

/// Create a default Conda channel configuration for Windows
#[cfg(windows)]
fn create_default_conda_channel_config(env_root: &Path) -> Result<()> {
    let channel_content = r#"format: conda
distro: conda
distro_dirs:
- anaconda
versions:
- "latest"
repos:
  main:
  free:
index_url: $mirror/pkgs/$repo/$conda_arch/$conda_repofile
amend_index_urls:
  noarch: $mirror/pkgs/$repo/noarch/$conda_repofile
"#;

    let dest_channel_path = env_root_channel_yaml(env_root);
    lfs::create_dir_all(dest_channel_path.parent().unwrap())?;
    lfs::write(&dest_channel_path, channel_content)?;

    println!("Created default Conda channel configuration");
    Ok(())
}

/// Parse channel string into distro name and version components
fn parse_channel_option() -> (String, Option<String>) {
    // Initialize channel from command line option or default
    let channel = config().env.channel.clone().unwrap_or(DEFAULT_CHANNEL.to_string());

    if let Some((name, version)) = channel.split_once(io::CHANNEL_SEPARATOR) {
        (name.to_string(), Some(version.to_string()))
    } else {
        (channel.clone(), None)
    }
}

/// Update version line in YAML contents
/// If a version line exists, replace it; otherwise append a new version line
fn update_version_in_contents(contents: &str, version: &str) -> String {
    let lines: Vec<&str> = contents.lines().collect();
    let mut has_version_line = false;
    let mut new_lines = Vec::new();

    for line in lines {
        if line.trim().starts_with("version:") {
            new_lines.push(format!("version: {}", version));
            has_version_line = true;
        } else {
            new_lines.push(line.to_string());
        }
    }

    if !has_version_line {
        new_lines.push(format!("version: {}", version));
    }

    new_lines.join("\n")
}


pub fn remove_environment(name: &str) -> Result<()> {
    // Validate environment name
    // 'self' environment contains package manager files; 'main' is the default private environment
    if name == SELF_ENV || name == MAIN_ENV {
        return Err(eyre::eyre!("Environment cannot be removed: '{}'", name));
    }

    // Resolve env path without loading config (config may be missing for non-existent env)
    let env_path = get_env_base_path(name);
    if !lfs::exists_on_host(&env_path) {
        return Err(eyre::eyre!("Environment does not exist: '{}'", name));
    }

    // Check if environment is active and handle stacked environments
    if let Ok(active_envs) = env::var("EPKG_ACTIVE_ENV") {
        let env_stack: Vec<&str> = active_envs.split(':').collect();

        if let Some(pos) = env_stack.iter().position(|&x| x == name) {
            if pos == 0 {
                // If it's the first environment, we can remove it
                deactivate_environment()?;
            } else {
                // If it's in the middle of the stack, return error
                return Err(eyre::eyre!(
                    "Cannot remove environment '{}' as it is in the middle of active environment stack. \
                    Please deactivate environments in reverse order: {}",
                    name,
                    env_stack[..=pos].join(" -> ")
                ));
            }
        }
    }

    // Unregister if registered
    unregister_environment(name)?;

    force_remove_dir_all(&env_path)
        .with_context(|| format!("Failed to remove environment directory '{}'", env_path.display()))?;

    println!("# Environment '{}' has been removed.", name);
    Ok(())
}

pub fn activate_environment(name: &str) -> Result<()> {
    // Validate environment name
    // 'self' environment is special: contains only package manager files (epkg, elf-loader)
    // and is not used for regular package installations, thus cannot be activated
    if name == SELF_ENV {
        return Err(eyre::eyre!("Environment 'self' cannot be activated"));
    }

    // Check if environment exists
    if !lfs::exists_on_host(&get_env_root(name.to_string())?) {
        return Err(eyre::eyre!("Environment not exist: '{}'", name));
    }

    // Get current environment states
    let original_active_envs = env::var("EPKG_ACTIVE_ENV").ok();
    let original_session_path = env::var("EPKG_SESSION_PATH").ok();

    // Check if environment is already active
    if let Some(active_envs) = &original_active_envs {
        if active_envs.split(':').any(|env| env == name) {
            return Err(eyre::eyre!("Environment '{}' is already active", name));
        }
        // Check if pure mode is incompatible with stack mode
        if config().env.pure && config().env.stack {
            return Err(eyre::eyre!("Cannot use pure mode with stack mode"));
        }
        // Check if non-stack mode is incompatible with existing active environments
        if !config().env.stack && !active_envs.is_empty() {
            return Err(eyre::eyre!("Cannot activate environment in non-stack mode when other environments are active. Please deactivate them first."));
        }
    }

    // Get environment config for env_vars
    let env_config = env_config();

    let shell_kind = shell_emit::detect();

    // Initialize deactivate script
    let mut script = String::new();

    // Handle session path
    let session_path = original_session_path.unwrap_or_else(|| {
        let seed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64 ^ (std::process::id() as u64);
        let mut rng = StdRng::seed_from_u64(seed);
        let path = std::env::temp_dir().join(format!(
            "deactivate-{}-{:08x}",
            std::process::id(),
            rng.random::<u32>()
        ));
        let path_str = path.to_string_lossy().into_owned();
        println!(
            "{}",
            shell_emit::emit_export("EPKG_SESSION_PATH", &path_str, shell_kind)
        );
        match shell_kind {
            shell_emit::ShellKind::Bash => script.push_str("unset EPKG_SESSION_PATH\n"),
            shell_emit::ShellKind::PowerShell => {
                script.push_str(
                    "Remove-Item \"Env:EPKG_SESSION_PATH\" -ErrorAction SilentlyContinue\n",
                );
            }
        }
        path_str
    });

    // Prepare new active envs
    let name_with_pure_mark = if config().env.pure {
        format!("{}{}", name, PURE_ENV_SUFFIX.to_string())
    } else {
        name.to_string()
    };
    let new_active_envs = if config().env.stack {
        match &original_active_envs {
            Some(envs) => format!("{}:{}", name_with_pure_mark, envs),
            None => name_with_pure_mark.to_string(),
        }
    } else {
        name_with_pure_mark.to_string()
    };

    // Action 1: Show export commands for shell eval
    println!("# Activate environment '{}'{}", name, if config().env.pure { " in pure mode" } else { "" });
    push_env_var(
        &mut script,
        "EPKG_ACTIVE_ENV",
        Some(new_active_envs.clone()),
        original_active_envs,
        shell_kind,
    );
    std::env::set_var("EPKG_ACTIVE_ENV", new_active_envs);

    // Export env_vars from config
    for (key, value) in &env_config.env_vars {
        let original_value = env::var(key).ok();
        push_env_var(
            &mut script,
            key,
            Some(value.clone()),
            original_value,
            shell_kind,
        );
    }

    update_path()?;

    // Action 2: Create deactivate script for shell eval
    let deactivate_script = format!(
        "{}-{}.{}",
        session_path,
        name,
        shell_emit::deactivate_script_extension(shell_kind)
    );
    lfs::write(&deactivate_script, script)?;

    Ok(())
}

pub fn deactivate_environment() -> Result<()> {
    let active_env = match env::var("EPKG_ACTIVE_ENV") {
        Ok(env) => env,
        Err(_) => {
            eprintln!("Warning: No environment is currently active");
            return Ok(());
        }
    };
    let session_path = match env::var("EPKG_SESSION_PATH") {
        Ok(path) => path,
        Err(_) => {
            eprintln!("Warning: EPKG_SESSION_PATH not set");
            return Ok(());
        }
    };

    let mut active_envs: Vec<String> = active_env.split(':').map(String::from).collect();

    if active_envs.is_empty() {
        return Err(eyre::eyre!("No environment is currently active"));
    }

    // Remove the last activated environment
    let deactivated_env = active_envs.pop().unwrap();

    // Remove pure mode suffix from the environment name for script filename lookup
    // The deactivate script is created without the '!' suffix
    let deactivated_env_name = deactivated_env.trim_end_matches(PURE_ENV_SUFFIX);
    let deactivate_script = format!(
        "{}-{}.{}",
        session_path,
        deactivated_env_name,
        shell_emit::deactivate_script_extension(shell_emit::detect())
    );
    let script = fs::read_to_string(&deactivate_script)
        .with_context(|| format!("Failed to read deactivate script: {}", deactivate_script))?;
    println!("{}", script);

    if let Err(e) = lfs::remove_file(&deactivate_script) {
        eprintln!("Warning: Could not remove deactivate script: {}", e);
    }

    if active_envs.is_empty() {
        // println!("unset EPKG_ACTIVE_ENV");
        env::remove_var("EPKG_ACTIVE_ENV");
    } else {
        // println!("export EPKG_ACTIVE_ENV={}", active_envs.join(":"));
        env::set_var("EPKG_ACTIVE_ENV", active_envs.join(":"));
    }

    // Update environment variables EPKG_ACTIVE_ENV and PATH
    // For eval by caller shell.
    println!("# Deactivate environment '{}'", deactivated_env);
    update_path()?;
    Ok(())
}

pub fn register_environment_for(name: &str, mut env_config: EnvConfig) -> Result<()> {
    // Validate environment name
    // 'self' environment is for package manager files only, not for regular packages
    if name == SELF_ENV {
        return Err(eyre::eyre!("Environment 'self' cannot be registered"));
    }

    if env_config.register_to_path {
        println!("# Environment '{}' is already registered.", name);
        return Ok(());
    }

    // Get path order from options or auto-detect
    let path_order = if let Some(order) = config().env.path_order {
        order
    } else {
        next_prepend_path_order()?
    };

    println!("# Registering environment '{}' with PATH order {}", name, path_order);

    // Update and save environment config
    env_config.register_to_path = true;
    env_config.register_path_order = path_order;
    io::serialize_env_config(env_config)?;

    update_path()?;
    Ok(())
}

pub fn register_environment(name: &str) -> Result<()> {
    let env_config = io::deserialize_env_config_for(name.to_string())?;
    register_environment_for(name, env_config)
}

pub fn unregister_environment(name: &str) -> Result<()> {
    let mut env_config = io::deserialize_env_config_for(name.to_string())?;

    if !env_config.register_to_path {
        // Only show message when explicitly called via "epkg env unregister"
        // When called from "epkg env remove", skip the message since user
        // is removing the environment, not specifically unregistering
        if config().subcommand == EpkgCommand::EnvUnregister {
            println!("# Environment '{}' is not registered.", name);
        }
        return Ok(());
    }

    // Update and save environment config
    env_config.register_to_path = false;
    env_config.register_path_order = 0;
    io::serialize_env_config(env_config)?;

    update_path()?;
    println!("# Environment '{}' has been unregistered.", name);
    Ok(())
}

pub fn export_environment(output: Option<String>) -> Result<()> {
    // Prepare environment export container
    let mut env_export = EnvExport {
        env: env_config().clone(),
        ..EnvExport::default()
    };

    // Get installed packages and world files
    let env_root = PathBuf::from(&env_export.env.env_root);

    // Add channel configs
    collect_files_for_export(&mut env_export.files, &env_root, "etc/epkg/channel.yaml")?;
    collect_files_for_export(&mut env_export.files, &env_root, "etc/epkg/repos.d/*.yaml")?;

    // Add generation-specific files
    collect_files_for_export(&mut env_export.files, &env_root, &format!("generations/current/world.json"))?;
    collect_files_for_export(&mut env_export.files, &env_root, &format!("generations/current/installed-packages.json"))?;

    // Serialize env_export
    let yaml_output = serde_yaml::to_string(&env_export)?;

    // Write to file or stdout
    if let Some(output_path) = output {
        lfs::write(&output_path, yaml_output)?;
        println!("Environment configuration exported to {}", output_path);
    } else {
        println!("{}", yaml_output);
    }

    Ok(())
}

/// Apply command line option overrides to environment config
fn override_env_config(env_config: &mut EnvConfig, name: &str, env_base: &Path, env_root: &Path) {
    env_config.name = name.to_string();
    env_config.env_base = env_base.to_string_lossy().to_string();
    env_config.env_root = env_root.to_string_lossy().to_string();
    // Note: env_config.public controls visibility/permissions, not location
    // Location is determined by InitOptions.shared_store (handled via dirs().user_envs)

    // SELF_ENV.public = (always) true - self environment contains only package manager files
    // This simplifies setting and works better in case $HOME is accessible to others,
    // so other users can still manually access it.
    if name == SELF_ENV {
        env_config.public = true;
    } else if name == MAIN_ENV {
        // 'main' is always private
        env_config.public = false;
    } else {
        // Other normal envs: decided by '--public' option on 'epkg env create'
        env_config.public = config().env.public;
    }

    env_config.register_to_path = false;
    env_config.register_path_order = 0;

    // Set link type from CLI option if provided
    if let Some(link_type) = config().env.link {
        env_config.link = link_type;
    }
}

/// Helper function to collect files matching a glob pattern or specific file for export
fn collect_files_for_export(files: &mut Vec<ExportFile>, base_dir: &Path, pattern: &str) -> Result<()> {
    use glob::glob;

    let full_pattern = base_dir.join(pattern);
    let pattern_str = full_pattern.to_string_lossy();

    for entry in glob(&pattern_str)
        .with_context(|| format!("Failed to parse glob pattern: {}", pattern_str))?
    {
        match entry {
            Ok(path) => {
                if let Ok(contents) = fs::read_to_string(&path) {
                    if let Ok(relative_path) = path.strip_prefix(base_dir) {
                        let export_path = relative_path.display().to_string();
                        files.push(ExportFile {
                            path: export_path,
                            data: contents,
                        });
                    }
                }
            }
            Err(e) => {
                eprintln!("Warning: glob error for {}: {}", pattern_str, e);
            }
        }
    }

    Ok(())
}


/// Get environment configuration value
pub fn get_environment_config(name: &str) -> Result<()> {
    let config = env_config();

    // Split name by dots to handle nested fields
    let parts: Vec<&str> = name.split('.').collect();
    let mut current = serde_yaml::to_value(&config)?;

    for part in parts {
        current = current.get(part)
            .ok_or_else(|| eyre::eyre!("Configuration key not found: {}", name))?
            .clone();
    }

    println!("{:?}", current);
    Ok(())
}

/// Set environment configuration value
pub fn set_environment_config(name: &str, value: &str) -> Result<()> {
    let config = env_config(); // load from file
    let mut config = config.clone();
    // Split name by dots to handle nested fields
    let parts: Vec<&str> = name.split('.').collect();

    match parts.as_slice() {
        // Top-level scalar fields
        ["name"] => config.name = value.to_string(),
        ["env_base"] => config.env_base = value.to_string(),
        ["env_root"] => config.env_root = value.to_string(),
        ["public"] => config.public = value.parse()?,
        ["register_to_path"] => config.register_to_path = value.parse()?,
        ["register_path_order"] => config.register_path_order = value.parse()?,

        // Environment variables: env_vars.FOO or legacy env_var.FOO from design notes
        ["env_vars", key] | ["env_var", key] => {
            config.env_vars.insert((*key).to_string(), value.to_string());
        }

        // Sandbox options: sandbox.isolate_mode
        ["sandbox", "isolate_mode"] => {
            config.sandbox.isolate_mode = Some(value.parse()?);
        }

        // Unknown or unsupported keys
        [top, ..] => {
            return Err(eyre::eyre!("Unknown or unsupported configuration key: {}", top));
        }
        [] => {
            return Err(eyre::eyre!("Configuration key cannot be empty"));
        }
    }

    // Save the updated config
    io::serialize_env_config(config)?;

    Ok(())
}

/// Get list of registered environment configs from env.yaml files.
///
/// **Note**: This function takes `shared_store` as parameter to avoid calling `config()`
/// which would cause deadlock during config initialization.
pub fn registered_env_configs(shared_store: bool) -> Vec<EnvConfig> {
    use std::fs;

    let mut configs = Vec::new();
    let current_user = get_username().unwrap_or_default();

    // Compute user_envs without relying on dirs_ref() which may not be initialized yet
    let user_envs = match compute_user_envs(shared_store) {
        Ok(path) => path,
        Err(_) => return configs,
    };

    // Walk all environments (private and public based on shared_store setting)
    let _ = walk_environments(shared_store, &user_envs, |env_path, owner| {
        let env_name = match env_path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name,
            None => return Ok(()),
        };

        // Skip special environments
        if env_name == SELF_ENV || env_name.starts_with('.') {
            return Ok(());
        }

        // In private store mode, skip other users' environments
        if !shared_store && owner.is_some() {
            return Ok(());
        }

        // Check if environment is public (same logic as get_all_env_names)
        let is_public = if owner.is_none() {
            // Environments from private store (owner=None) are always private
            false
        } else {
            // Environments from shared store (owner=Some) may be public or private
            match lfs::symlink_metadata(env_path) {
                Ok(metadata) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let mode = metadata.permissions().mode() & 0o777;
                        let is_private = mode == 0o700;
                        !is_private
                    }
                    #[cfg(not(unix))]
                    {
                        // Windows: all environments considered public
                        let _ = metadata;
                        true
                    }
                }
                Err(_) => false,
            }
        };

        // For environments owned by other users, only include public ones
        // Current user's environments are always included (both public and private)
        if let Some(owner_name) = owner {
            if owner_name != current_user && !is_public {
                return Ok(());
            }
        }

        // Check if environment has a config file
        let config_path = env_root_env_yaml(env_path);
        if !lfs::exists_in_env(&config_path) {
            return Ok(());
        }

        // Read and parse config
        let contents = match fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(err) => {
                warn!("Failed to read {}: {}", config_path.display(), err);
                return Ok(());
            }
        };

        let mut config: EnvConfig = match serde_yaml::from_str(&contents) {
            Ok(cfg) => cfg,
            Err(err) => {
                warn!("Failed to parse {}: {}", config_path.display(), err);
                return Ok(());
            }
        };

        // Only include if registered to PATH
        if !config.register_to_path {
            return Ok(());
        }

        // Set appropriate name
        if config.name.is_empty() {
            config.name = env_name.to_string();
        }

        // If environment has an owner (shared store), prefix with owner/
        if let Some(owner_name) = owner {
            config.name = format!("{}/{}", owner_name, config.name);
        }

        configs.push(config);
        Ok(())
    });

    configs
}


/// Find which registered environment contains a given command
/// Returns the environment name and root path if found, None otherwise
/// Searches environments in order of registration path-order (lower number first: earlier in PATH)
///
/// **Note**: This function takes `shared_store` as parameter to avoid calling `config()`
/// which would cause deadlock during config initialization.
pub fn find_command_in_registered_envs(cmd_name: &str, shared_store: bool) -> Result<Option<(String, PathBuf)>> {
    // Get registered environment configs with PATH orders
    let mut configs = registered_env_configs(shared_store);

    // Sort by registration order (lower number = earlier in PATH = checked first)
    // For equal order, sort by name for deterministic results
    configs.sort_by(|a, b| {
        a.register_path_order.cmp(&b.register_path_order)
            .then_with(|| a.name.cmp(&b.name))
    });

    // Common binary directories to check in each environment
    let bin_dirs = ["usr/bin", "bin", "usr/local/bin", "usr/sbin", "sbin"];

    #[cfg(windows)]
    let command_candidates = {
        let has_extension = Path::new(cmd_name).extension().is_some();
        let mut candidates = vec![cmd_name.to_string()];
        if !has_extension {
            candidates.push(format!("{}.exe", cmd_name));
            candidates.push(format!("{}.bat", cmd_name));
            candidates.push(format!("{}.cmd", cmd_name));
        }
        candidates
    };

    #[cfg(not(windows))]
    let command_candidates = vec![cmd_name.to_string()];

    for env_cfg in configs {
        // Use env_root directly from EnvConfig instead of calling get_env_root()
        // which would cause deadlock by calling config() during initialization
        let env_root = PathBuf::from(&env_cfg.env_root);
        for bin_dir in &bin_dirs {
            #[cfg(windows)]
            let bin_path = PathBuf::from(bin_dir.replace('/', "\\"));
            #[cfg(not(windows))]
            let bin_path = Path::new(bin_dir);

            for candidate in &command_candidates {
                let cmd_path = env_root.join(&bin_path).join(candidate);
                if !lfs::exists_in_env(&cmd_path) {
                    continue;
                }
                // Check if executable (Unix only)
                #[cfg(unix)]
                {
                    if let Ok(metadata) = lfs::symlink_metadata(&cmd_path) {
                        let permissions = metadata.permissions();
                        if permissions.mode() & 0o111 != 0 {
                            log::debug!("found command '{}' at path '{}'", cmd_name, cmd_path.display());
                            return Ok(Some((env_cfg.name.clone(), env_root)));
                        }
                    }
                }
                #[cfg(not(unix))]
                {
                    // On non-Unix, just check existence
                    return Ok(Some((env_cfg.name.clone(), env_root)));
                }
            }
        }
    }

    Ok(None)
}

