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

/// Setup CA certificates symlink for compatibility with applications
/// that expect /etc/ssl/certs/ca-certificates.crt (Debian/Ubuntu path)
/// but the distro may use a different path (e.g., /etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem on RPM-based distros)
pub fn setup_ca_certificates_symlink(env_root: &Path) -> Result<()> {
    let certs_dir = crate::dirs::path_join(env_root, &["etc", "ssl", "certs"]);
    let ca_cert_path = certs_dir.join("ca-certificates.crt");

    // If the file already exists (as a file or symlink), nothing to do
    if lfs::exists_in_env(&ca_cert_path) {
        return Ok(());
    }

    // Create the certs directory if it doesn't exist
    lfs::create_dir_all(&certs_dir)?;

    // Try to find the CA bundle in common locations
    // Priority: check env_root first (for already-installed ca-certificates package)
    let possible_paths = [
        // RPM-based distros (openeuler, fedora, etc.)
        "etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
        "etc/pki/tls/certs/ca-bundle.crt",
        // Arch Linux
        "etc/ca-certificates/extracted/tls-ca-bundle.pem",
        // Debian/Ubuntu (already has ca-certificates.crt, but check anyway)
        "etc/ssl/certs/ca-certificates.crt",
        // SUSE
        "etc/ssl/ca-bundle.pem",
    ];

    for rel_path in &possible_paths {
        let full_path = env_root.join(rel_path);
        if lfs::exists_in_env(&full_path) {
            // Create symlink from ca-certificates.crt to the found bundle
            // Use relative path for the symlink target so it works in chroot/VM
            let target = PathBuf::from("/").join(rel_path);
            log::debug!("Creating CA cert symlink: {} -> {}",
                ca_cert_path.display(), target.display());
            lfs::symlink_file_for_native(&target, &ca_cert_path)?;
            return Ok(());
        }
    }

    // If we reach here, no CA bundle was found
    // This is not an error - the ca-certificates package may not be installed yet
    log::debug!("No CA certificate bundle found in env, skipping symlink creation");
    Ok(())
}

fn create_environment_dirs_early(env_root: &Path) -> Result<()> {
    let generations_root = env_root.join("generations");
    let gen_0_dir = generations_root.join("0");

    // Create env_root directory first and enable case sensitivity on Windows
    // This must be done before creating any subdirectories
    lfs::create_dir_all_with_case_sensitivity(env_root)?;

    // Create basic directories
    lfs::create_dir_all(&gen_0_dir)?;
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

    // Create symlinks in generation 0 (usr-merge layout)
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

    // Create "current" symlink in generations directory pointing to generation 0
    force_symlink_dir_for_virtiofs("0", generations_root.join("current"))?;

    // Create essential mount points for VM isolation (dev, proc, sys, run, tmp)
    // These are needed for kernel to mount devtmpfs, procfs, sysfs, etc.
    lfs::create_dir_all(env_root.join("dev"))?;
    lfs::create_dir_all(env_root.join("proc"))?;
    lfs::create_dir_all(env_root.join("sys"))?;
    lfs::create_dir_all(env_root.join("run"))?;
    lfs::create_dir_all(env_root.join("tmp"))?;

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
                    // For VM-mode environments, copy/hardlink the binary instead of creating a symlink.
                    // This is necessary because:
                    // 1. The VM's rootfs is the environment directory (e.g., alpine/)
                    // 2. Symlinks to paths outside the rootfs won't work in the VM guest
                    // 3. The epkg-linux binary must be accessible from within the VM
                    //
                    // We prefer hardlink for space efficiency, falling back to copy if not possible.

                    log::debug!("Creating epkg binary for VM environment: {} from {}", epkg_symlink.display(), self_epkg_linux.display());

                    // Remove existing file if present
                    if lfs::exists_no_follow(&epkg_symlink) {
                        lfs::remove_file(&epkg_symlink)?;
                    }

                    // Try hardlink first (more efficient), fall back to copy
                    let used_hardlink = lfs::hard_link(&self_epkg_linux, &epkg_symlink).is_ok();
                    if !used_hardlink {
                        log::debug!("Hardlink failed, falling back to copy");
                        lfs::copy(&self_epkg_linux, &epkg_symlink)?;
                    }

                    // Set execute permission on the epkg binary for virtiofs/Linux guest.
                    // On Windows, virtiofs uses NTFS Extended Attributes ($LXMOD) to store POSIX mode.
                    // Without this, the file gets default 644 permissions (no execute).
                    // Note: MODE must include S_IFREG (0o100000) for regular files.
                    #[cfg(windows)]
                    {
                        const S_IFREG: u32 = 0o100000; // Regular file type bit
                        const MODE_755: u32 = S_IFREG | 0o755; // 0o100755 = regular file, rwxr-xr-x
                        if let Err(e) = crate::ntfs_ea::set_posix_mode(&epkg_symlink, MODE_755, false) {
                            log::warn!("Failed to set execute permission on {}: {}", epkg_symlink.display(), e);
                        } else {
                            log::debug!("Set execute permission (100755) on {}", epkg_symlink.display());
                        }
                    }

                    // Also create init for VM - kernel cmdline specifies init=/usr/bin/init
                    // On Windows, use copy instead of symlink for virtiofs compatibility
                    let init_path = crate::dirs::path_join(env_root, &["usr", "bin", "init"]);
                    log::debug!("Creating init copy {} -> epkg (Linux VM)", init_path.display());
                    // Remove existing file if present
                    if lfs::exists_no_follow(&init_path) {
                        lfs::remove_file(&init_path)?;
                    }
                    // Copy epkg to init (hardlink or copy)
                    let epkg_source = crate::dirs::path_join(env_root, &["usr", "bin", "epkg"]);
                    {
                        // On Windows hosts, use hardlink/copy for virtiofs compatibility
                        // Symlinks don't work well in virtiofs/VM environment
                        let used_hardlink = lfs::hard_link(&epkg_source, &init_path).is_ok();
                        if !used_hardlink {
                            log::debug!("Hardlink failed, falling back to copy");
                            lfs::copy(&epkg_source, &init_path)?;
                        }

                        // Set execute permission on init for virtiofs/Linux guest
                        // On Windows, virtiofs uses NTFS Extended Attributes ($LXMOD) to store POSIX mode.
                        #[cfg(windows)]
                        {
                            const S_IFREG: u32 = 0o100000;
                            const MODE_755: u32 = S_IFREG | 0o755;
                            if let Err(e) = crate::ntfs_ea::set_posix_mode(&init_path, MODE_755, false) {
                                log::warn!("Failed to set execute permission on {}: {}", init_path.display(), e);
                            } else {
                                log::debug!("Set execute permission (100755) on {}", init_path.display());
                            }
                        }
                    }

                    return Ok(());
                } else {
                    log::debug!("epkg-linux-{} not found in self env, skipping epkg binary", arch);
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

        // Create home/linuxbrew symlinks for Fs/VM mode pivot_root
        #[cfg(target_os = "linux")]
        {
            create_homebrew_symlinks(env_root);
        }

        // Create brew-specific directories
        // 1. Caskroom/ - for macOS casks (empty on Linux)
        lfs::create_dir_all(env_root.join("Caskroom"))?;

        // 2. var/homebrew/ - Homebrew's internal state directory
        lfs::create_dir_all(env_root.join("var").join("homebrew").join("linked"))?;
        lfs::create_dir_all(env_root.join("var").join("homebrew").join("locks"))?;
        lfs::create_dir_all(env_root.join("var").join("homebrew").join("tmp"))?;

        // 3. sbin/ - Homebrew installs binaries here (like zic from glibc)
        //    Convert usr-merge symlink to real directory for brew
        let sbin_path = env_root.join("sbin");
        if lfs::symlink_metadata(&sbin_path).is_ok() {
            // Remove the usr-merge symlink
            lfs::remove_file(&sbin_path)?;
        }
        lfs::create_dir_all(&sbin_path)?;
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
    // Note: This requires LX symlink support (WSL on Windows). Skip if not available.
    let systemctl_path = crate::dirs::path_join(env_root, &["usr", "bin", "systemctl"]);
    if !lfs::exists_in_env(&systemctl_path) {
        if let Err(e) = force_symlink_file_for_virtiofs("/usr/bin/true", &systemctl_path) {
            log::debug!("Skipping systemctl symlink: {}", e);
        }
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
    let world_path = crate::dirs::path_join(env_root, &["generations", "0", "world.json"]);
    let world_json = serde_json::to_string_pretty(&world)?;
    lfs::write(&world_path, world_json)?;

    Ok(())
}

/// Install packages and create metadata files for the environment
fn import_packages_and_create_metadata(env_root: &Path) -> Result<()> {
    let gen_0_dir = crate::dirs::path_join(env_root, &["generations", "0"]);
    let installed_packages_path = gen_0_dir.join("installed-packages.json");

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

        // Setup CA certificates symlink after package installation
        if let Err(e) = setup_ca_certificates_symlink(env_root) {
            log::warn!("Failed to setup CA certificates symlink: {}", e);
        }
    } else {
        // Create metadata files
        lfs::write(installed_packages_path, "{\n}")?;

        // Record the environment creation in command history
        record_history(&gen_0_dir, None)?;
    }

    Ok(())
}


pub fn create_environment(env_name: &str) -> Result<()> {
    let env_base = dirs().user_envs.join(env_name);

    // Warn if auto-generated name (starts with "__")
    if env_name.starts_with("__") {
        println!("# Note: environment name '{}' was auto-generated", env_name);
    }

    // Step 1: Setup initial paths and copy channel configs to determine package format
    let (env_root, pkg_format) = setup_environment_paths(&env_base)?;

    println!("Creating environment '{}' in {}", env_name, env_root.display());

    // Step 2: Create basic directories early
    create_environment_dirs_early(&env_root)?;

    // Step 3: Initialize environment config
    let (env_config, channel_config) = initialize_environment_config_after_setup(env_name, &env_root, &env_base)?;
    create_environment_dirs(&env_root, &pkg_format, &env_config, &channel_config)?;

    // Step 4: Create world.json with default no-install packages
    create_default_world_json(&env_root, &pkg_format)?;

    // Step 5: Install packages and create metadata files
    import_packages_and_create_metadata(&env_root)?;

    Ok(())
}

/// Read channel config content from source file.
/// Returns the yaml content string (optionally with version updated).
fn read_channel_config_content_from_source() -> Result<(String, String)> {
    let sources_path = crate::dirs::path_join(get_epkg_src_path().as_path(), &["assets", "repos"]);
    let (distro_name, distro_version) = parse_channel_option();

    let src_yaml_path = sources_path.join(format!("{}.yaml", distro_name));
    if !src_yaml_path.exists() {
        return Err(eyre::eyre!(
            "Channel configs source path does not exist: {}",
            src_yaml_path.display()
        ));
    }

    let mut channel_content = fs::read_to_string(&src_yaml_path)?;
    if let Some(version) = distro_version {
        channel_content = update_version_in_contents(&channel_content, &version);
    }

    Ok((channel_content, distro_name))
}

/// Determine package format by reading channel config from source (without creating directories).
fn determine_package_format() -> Result<PackageFormat> {
    let (channel_content, _) = read_channel_config_content_from_source()?;
    let channel_config: ChannelConfig = serde_yaml::from_str(&channel_content)
        .wrap_err("Failed to parse channel config")?;
    Ok(channel_config.format)
}

/// Create home/linuxbrew symlinks for brew environments.
///
/// 1. In host: /home/linuxbrew/.LB -> .linuxbrew (for skip_namespace mode)
/// 2. In env_root: home/linuxbrew/.linuxbrew -> ../../../../ (for Fs/VM pivot_root)
/// 3. In env_root: home/linuxbrew/.LB -> .linuxbrew (for Fs/VM mode)
///
/// The short .LB prefix (18 chars) fits in 22-char placeholder buffer without overflow.
#[cfg(target_os = "linux")]
fn create_homebrew_symlinks(env_root: &Path) {
    let homebrew_prefix = crate::brew_pkg::prefix::preferred();
    let hb_prefix_path = Path::new(homebrew_prefix);

    // === 1. Create .LB symlink in host ===
    // For skip_namespace and Env modes where host path needs to resolve
    let host_lb = hb_prefix_path.parent().unwrap().join(".LB");
    if !host_lb.exists() {
        if let Err(e) = std::os::unix::fs::symlink(".linuxbrew", &host_lb) {
            log::warn!("Failed to create host .LB symlink: {}", e);
        } else {
            log::info!("Created host symlink {} -> .linuxbrew", host_lb.display());
        }
    }

    // === 2. Create home/linuxbrew symlinks in env_root ===
    // For Fs/VM modes where pivot_root is used
    let hb_inside_env = env_root.join(homebrew_prefix.trim_start_matches('/'));

    // Create parent directories: env_root/home/linuxbrew/
    if let Some(parent) = hb_inside_env.parent() {
        if let Err(e) = crate::lfs::create_dir_all(parent) {
            log::warn!("Failed to create HOMEBREW_PREFIX parent in env: {}", e);
            return;
        }
    }

    // Remove existing directory/symlink if present
    if hb_inside_env.exists() {
        log::debug!("Removing existing HOMEBREW_PREFIX at {}", hb_inside_env.display());
        let _ = std::fs::remove_dir_all(&hb_inside_env);
        let _ = std::fs::remove_file(&hb_inside_env);
    }

    // Create .linuxbrew -> ../../ (2 levels up to env_root)
    // Symlink resolves from its parent directory (linuxbrew/):
    //   ../../ -> home/ -> env_root
    // After pivot_root, env_root becomes /, so this points to /
    if let Err(e) = std::os::unix::fs::symlink("../../", &hb_inside_env) {
        log::warn!("Failed to create env .linuxbrew symlink: {}", e);
    } else {
        log::info!("Created env symlink {} -> ../../", hb_inside_env.display());
    }

    // Create .LB -> ../../ directly (same as .linuxbrew, reduces one symlink lookup)
    let env_lb = hb_inside_env.parent().unwrap().join(".LB");
    if env_lb.exists() {
        let _ = std::fs::remove_file(&env_lb);
    }
    if let Err(e) = std::os::unix::fs::symlink("../../", &env_lb) {
        log::warn!("Failed to create env .LB symlink: {}", e);
    } else {
        log::info!("Created env symlink {} -> ../../", env_lb.display());
    }

    // === 3. Create .ld.so symlink for interpreter rewrite ===
    // Short path: /home/linuxbrew/.ld.so (22 chars) fits in 28-byte buffer
    // This allows rewriting interpreter from /lib64/ld-linux-x86-64.so.2 (27 chars)
    // to /home/linuxbrew/.ld.so (22 chars) which fits in gcc-15's 28-byte buffer.

    // Host symlink: /home/linuxbrew/.ld.so -> .linuxbrew/lib/ld.so (for Env mode)
    let host_ldso = hb_prefix_path.parent().unwrap().join(".ld.so");
    if !host_ldso.exists() {
        if let Err(e) = std::os::unix::fs::symlink(".linuxbrew/lib/ld.so", &host_ldso) {
            log::warn!("Failed to create host .ld.so symlink: {}", e);
        } else {
            log::info!("Created host symlink {} -> .linuxbrew/lib/ld.so", host_ldso.display());
        }
    }

    // env_root symlink: home/linuxbrew/.ld.so -> ../../lib/ld.so (for Fs/VM mode)
    // After pivot_root, ../../ from /home/linuxbrew/ resolves to /
    let env_ldso = hb_inside_env.parent().unwrap().join(".ld.so");
    if env_ldso.exists() {
        let _ = std::fs::remove_file(&env_ldso);
    }
    if let Err(e) = std::os::unix::fs::symlink("../../lib/ld.so", &env_ldso) {
        log::warn!("Failed to create env .ld.so symlink: {}", e);
    } else {
        log::info!("Created env symlink {} -> ../../lib/ld.so", env_ldso.display());
    }
}

#[cfg(not(unix))]
fn try_use_homebrew_prefix() -> Result<Option<PathBuf>> {
    log::debug!("Brew package format on non-Unix platform, using regular env_root");
    Ok(None)
}

/// Try to use HOMEBREW_PREFIX as env_root for brew environments.
/// Returns Some(path) if successful, None if should fall back to regular env_root.
#[cfg(unix)]
fn try_use_homebrew_prefix() -> Result<Option<PathBuf>> {
    let homebrew_prefix = crate::brew_pkg::prefix::preferred();
    let hb_path        = Path::new(homebrew_prefix);

    if !hb_path.exists() {
        log::info!("HOMEBREW_PREFIX {} does not exist, attempting to create...", homebrew_prefix);
        try_create_homebrew_prefix(homebrew_prefix)?;
        // Check if directory was actually created (sudo may have failed)
        if !hb_path.exists() {
            log::info!("HOMEBREW_PREFIX {} was not created (sudo may have failed), using regular env_root", homebrew_prefix);
            return Ok(None);
        }
        log::info!("Successfully created HOMEBREW_PREFIX: {}", homebrew_prefix);
        return Ok(Some(hb_path.to_path_buf()));
    }

    // Directory already exists
    #[cfg(target_os = "macos")]
    {
        // On macOS, must use HOMEBREW_PREFIX
        // Check if empty and owned by current user
        if is_empty_directory_owned_by_user(hb_path)? {
            log::info!("Reusing empty HOMEBREW_PREFIX {} owned by current user", homebrew_prefix);
            return Ok(Some(hb_path.to_path_buf()));
        }
        return Err(eyre::eyre!(
            "HOMEBREW_PREFIX {} already exists and is not empty or not owned by current user. \
             On macOS, brew environments must use HOMEBREW_PREFIX as env_root. \
             Please remove the directory first: epkg env remove <name>",
            homebrew_prefix
        ));
    }

    #[cfg(not(target_os = "macos"))]
    {
        // On Linux, check if empty and owned by current user for reuse
        if is_empty_directory_owned_by_user(hb_path)? {
            log::info!("Reusing empty HOMEBREW_PREFIX {} owned by current user", homebrew_prefix);
            return Ok(Some(hb_path.to_path_buf()));
        }
        log::info!("HOMEBREW_PREFIX {} already exists (not empty or not owned by user), using regular env_root", homebrew_prefix);
        Ok(None)
    }
}

/// Check if directory is empty and owned by current user.
/// Returns true only if both conditions are met.
#[cfg(unix)]
fn is_empty_directory_owned_by_user(path: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    use nix::unistd::{Uid, Gid};

    // Check if directory exists
    if !path.exists() {
        return Ok(false);
    }

    // Check ownership - must be owned by current user
    let metadata = std::fs::metadata(path)
        .wrap_err_with(|| format!("Failed to get metadata for {}", path.display()))?;

    let current_uid = Uid::current().as_raw();
    let current_gid = Gid::current().as_raw();

    if metadata.uid() != current_uid || metadata.gid() != current_gid {
        log::debug!("HOMEBREW_PREFIX {} not owned by current user (uid={}, gid={}, expected uid={}, gid={})",
            path.display(), metadata.uid(), metadata.gid(), current_uid, current_gid);
        return Ok(false);
    }

    // Check if empty - no files or subdirectories
    let entries = std::fs::read_dir(path)
        .wrap_err_with(|| format!("Failed to read directory {}", path.display()))?;

    let count = entries.count();
    if count > 0 {
        log::debug!("HOMEBREW_PREFIX {} not empty (contains {} entries)", path.display(), count);
        return Ok(false);
    }

    Ok(true)
}

/// Try to create HOMEBREW_PREFIX directory using sudo if needed
#[cfg(unix)]
fn try_create_homebrew_prefix(path: &str) -> Result<()> {
    // First try without sudo
    match std::fs::create_dir_all(path) {
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            // Prompt user before requesting sudo
            println!("Need to create Homebrew prefix directory: {}", path);
            println!("Requesting sudo permission to create it...");
            // Try with sudo
            let output = std::process::Command::new("sudo")
                .args(&["sh", "-c", &format!("mkdir -p '{}' && chown $(id -u):$(id -g) '{}'", path, path)])
                .status()
                .wrap_err("Failed to run sudo command")?;

            if !output.success() {
                log::info!("sudo failed or was cancelled, will fallback to regular env_root");
                return Ok(());
            }
            log::info!("Successfully created HOMEBREW_PREFIX: {}", path);
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// Setup environment paths.
/// Returns (env_root, pkg_format). Creates symlink env_base -> env_root if they differ.
fn setup_environment_paths(env_base: &Path) -> Result<(PathBuf, PackageFormat)> {
    // Step 1: Determine package format from source (without creating directories)
    let pkg_format = determine_package_format()?;

    // Step 2: Determine env_root (brew may use HOMEBREW_PREFIX)
    let env_root = if !config().common.env_root.is_empty() {
        PathBuf::from(&config().common.env_root)
    } else if pkg_format == PackageFormat::Brew {
        try_use_homebrew_prefix()?.unwrap_or(env_base.to_path_buf())
    } else {
        env_base.to_path_buf()
    };

    // Step 3: Check if environment already exists
    let env_channel_yaml = env_root_channel_yaml(&env_root);
    if !config().common.force && lfs::exists_on_host(&env_channel_yaml) {
        return Err(eyre::eyre!("Environment already exists at path: '{}'", env_root.display()));
    }

    // Step 4: If env_base != env_root, create symlink env_base -> env_root FIRST
    //         (before any directory creation, to avoid leftover directories in env_base)
    if env_base != env_root {
        // Check if env_base already exists as a non-symlink/junction directory
        if lfs::exists_no_follow(&env_base) && !lfs::is_symlink_or_junction(&env_base) {
            return Err(eyre::eyre!("Environment base path '{}' already exists as a directory. Cannot create symlink.", env_base.display()));
        }
        // Remove existing symlink/junction if present (force mode or re-creation)
        if lfs::exists_no_follow(&env_base) {
            lfs::remove_file(env_base)?;
        }
        // Ensure parent directory of env_base exists
        if let Some(parent) = env_base.parent() {
            lfs::create_dir_all(parent)?;
        }
        // Create symlink env_base -> env_root
        force_symlink_dir_for_virtiofs(&env_root, env_base)
            .with_context(|| format!("Failed to create symlink from {} to {}", env_base.display(), env_root.display()))?;
    }

    // Step 5: Create environment directories and copy channel configs in env_root
    create_environment_dirs_early(&env_root)?;
    copy_channel_configs(&env_root)?;

    Ok((env_root, pkg_format))
}

/// Initialize environment config after paths are set up
fn initialize_environment_config_after_setup(
    env_name: &str,
    env_root: &Path,
    env_base: &Path
) -> Result<(EnvConfig, ChannelConfig)> {
    // Initialize environment config
    let mut env_config = if let Some(import_file) = &config().env.import_file {
        import_environment_from_file(env_root, import_file)?
    } else {
        // Channel configs already copied in setup_environment_paths
        EnvConfig::default()
    };

    // Override config values by command line options
    override_env_config(&mut env_config, env_name, env_base, env_root);

    // Save environment config
    io::serialize_env_config(env_config.clone())?;

    let channel_configs = io::deserialize_channel_config_from_root(&env_root.to_path_buf())?;
    let channel_config = channel_configs[0].clone();

    Ok((env_config, channel_config))
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
fn copy_main_channel_config(env_root: &Path, channel_content: &str) -> Result<()> {
    // Save main channel config
    let dest_channel_path = env_root_channel_yaml(env_root);
    lfs::create_dir_all(dest_channel_path.parent().unwrap())?;
    lfs::write(&dest_channel_path, channel_content)?;

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
    let (channel_content, distro_name) = read_channel_config_content_from_source()?;
    let sources_path = crate::dirs::path_join(get_epkg_src_path().as_path(), &["assets", "repos"]);

    copy_main_channel_config(env_root, &channel_content)?;
    copy_repo_configs(&sources_path, env_root, &distro_name)?;

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


/// Validate environment name and existence for removal
fn validate_environment_for_removal(name: &str) -> Result<PathBuf> {
    // 'self' environment contains package manager files; 'main' is the default private environment
    if name == SELF_ENV || name == MAIN_ENV {
        return Err(eyre::eyre!("Environment cannot be removed: '{}'", name));
    }

    let env_base = get_env_base_path(name);
    if !lfs::exists_on_host(&env_base) {
        return Err(eyre::eyre!("Environment does not exist: '{}'", name));
    }

    Ok(env_base)
}

/// Check if environment is active and handle stacked environments
/// Returns true if the environment was deactivated (was the first in stack)
fn handle_active_environment_for_removal(name: &str) -> Result<()> {
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
    Ok(())
}

/// Clean up env_root if it differs from env_base
/// For brew environments with env_root == HOMEBREW_PREFIX:
/// - Remove contents but leave empty directory for reuse
/// - Keep home/linuxbrew/.LB symlink in host for next environment
fn cleanup_env_root_if_needed(env_base: &Path, env_root: Option<&Path>) {
    if let Some(env_root) = env_root {
        if env_root != env_base {
            log::debug!("Cleaning up env_root {} (different from env_base {})", env_root.display(), env_base.display());

            #[cfg(unix)]
            {
                // Check if env_root is HOMEBREW_PREFIX
                let homebrew_prefix = crate::brew_pkg::prefix::preferred();
                let is_homebrew_prefix = env_root == Path::new(homebrew_prefix);

                if is_homebrew_prefix {
                    // For HOMEBREW_PREFIX, remove contents but leave empty directory for reuse
                    log::info!("Cleaning HOMEBREW_PREFIX {} for reuse", env_root.display());
                    remove_contents_keep_directory(env_root);
                    return;
                }
            }

            // For other env_root, try full removal
            match lfs::remove_dir_all(env_root) {
                Ok(()) => {
                    log::debug!("Successfully removed env_root {}", env_root.display());
                }
                Err(e) => {
                    log::debug!("Could not remove env_root {}: {}", env_root.display(), e);
                }
            }
        }
    }
}

/// Remove all contents of a directory but keep the directory itself empty.
/// This allows reuse of HOMEBREW_PREFIX after `epkg env remove`.
#[cfg(unix)]
fn remove_contents_keep_directory(dir: &Path) {
    if !dir.exists() || !dir.is_dir() {
        return;
    }

    // Remove all entries in the directory
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path: PathBuf = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            // Keep home/linuxbrew/.LB symlink in host (created by create_homebrew_symlinks)
            // But remove everything else including .linuxbrew contents
            if name == ".LB" {
                log::debug!("Keeping .LB symlink for reuse");
                continue;
            }

            // Use path.is_dir() which follows symlinks
            // For symlinks to directories, we want to remove the symlink not the target
            if path.is_symlink() {
                if let Err(e) = lfs::remove_file(&path) {
                    log::warn!("Failed to remove symlink {}: {}", path.display(), e);
                }
            } else if path.is_dir() {
                if let Err(e) = lfs::remove_dir_all(&path) {
                    log::warn!("Failed to remove directory {}: {}", path.display(), e);
                }
            } else {
                if let Err(e) = lfs::remove_file(&path) {
                    log::warn!("Failed to remove file {}: {}", path.display(), e);
                }
            }
        }
    }

    log::info!("Left empty HOMEBREW_PREFIX {} for reuse", dir.display());
}

pub fn remove_environment(name: &str) -> Result<()> {
    let env_base = validate_environment_for_removal(name)?;
    handle_active_environment_for_removal(name)?;

    // Get env_root BEFORE deleting env_base (env_root info is in env_base/etc/epkg/env.yaml)
    let env_root = crate::dirs::get_env_root(name.to_string()).ok();

    unregister_environment(name)?;

    // Remove env_base first (always succeeds for user-owned directory)
    force_remove_dir_all(&env_base)
        .with_context(|| format!("Failed to remove environment directory '{}'", env_base.display()))?;

    // Clean up env_root if it differs from env_base
    cleanup_env_root_if_needed(&env_base, env_root.as_deref());

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

