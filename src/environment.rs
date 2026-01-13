use std::fs;
use std::env;
use std::path::Path;
use std::path::PathBuf;
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;
use color_eyre::Result;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use serde_json;
use serde_yaml;
use nix::unistd::chown;
use glob;
use crate::models::*;
use crate::dirs::*;
use crate::repo::sync_channel_metadata;
use crate::utils::{self, force_symlink};
use crate::deinit::force_remove_dir_all;
use crate::deb_triggers::ensure_triggers_dir;
use crate::plan::prepare_installation_plan;
use std::sync::Arc;
use crate::install::execute_installation_plan;
use crate::history::record_history;
use crate::path::update_path;
use crate::io;
use log::warn;

// epkg stores persistent PATH registration metadata inside each environment's
// `etc/epkg/env.yaml`. The `register_to_path` flag combined with
// `register_priority` drives how PATH is constructed:
//
// PATH layout:
//   registered prepend entries (priority >= 0)
//   + original PATH
//   + registered append entries (priority < 0)
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
// - `epkg env register <name> [--priority N]`
// - If `--priority` is omitted the first free multiple of 10 (>= 10) is chosen
// - `N >= 0` participates in the prepend side, `N < 0` in the append side
//
// Example registrations:
//   epkg env register openeuler2409
//   epkg env register debian12 --priority 18
//
// Example activations:
//   epkg env activate project-dev                  # Activate project environment
//   epkg env activate test-env --pure              # Activate in pure mode (no inherited paths)
//   epkg env deactivate                            # Return to default environment

// Helper function to handle environment variable changes
// Note: PATH is handled by update_path() instead of push_env_var(), since PATH could be changed by
// interleaved (de)activate/(un)register calls.
fn push_env_var(script: &mut String, key: &str, new_value: Option<String>, original_value: Option<String>) {
    // Set new value (print to stdout)
    if let Some(v) = &new_value {
        println!("export {}={}", key, v);
    }

    // Prepare restore command (store in script)
    match original_value {
        Some(v) => script.push_str(&format!("export {}={}\n", key, v)),
        None => script.push_str(&format!("unset {}\n", key)),
    }
}

fn next_prepend_priority() -> Result<i32> {
    let registered = registered_env_configs();
    let used: HashSet<i32> = registered.into_iter()
        .filter(|cfg| cfg.register_priority >= 0)
        .map(|cfg| cfg.register_priority)
        .collect();

    let mut priority = 10;
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

    // Walk environments based on shared_store setting
    walk_environments(|env_path, owner| {
        let name = env_path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();

        if name != SELF_ENV && !name.starts_with('.') {
            // An environment is considered "public" when:
            // - shared_store is enabled (environments live under /opt/epkg/envs)
            // - the environment directory does NOT have private-only (700) permissions
            //
            // Private environments are explicitly created with mode 0o700
            // in create_environment_dirs().
            let is_public = if !shared_store {
                // In non-shared-store mode all envs are private; avoid extra fs ops.
                false
            } else {
                let mode = fs::metadata(env_path)?
                    .permissions()
                    .mode() & 0o777;
                let is_private = mode == 0o700;
                !is_private
            };

            // Decide ownership: any env under the personal store (owner == None)
            // or with owner == current user is considered "mine".
            let is_mine = match owner {
                None => true,
                Some(o) => o == current_user.as_str(),
            };

            // For environments owned by another user in shared_store mode,
            // prefix with "$owner/" to match the directory layout.
            let env_display_name = if is_mine {
                name.clone()
            } else {
                format!("{}/{}", owner.unwrap(), name)
            };

            if is_mine {
                my_envs.push((env_display_name, is_public));
            } else {
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
    let registered_envs: Vec<String> = get_registered_env_names()?;

    // Get active environments list once and convert to HashSet for O(1) lookups
    let active_list: Vec<String> = env::var("EPKG_ACTIVE_ENV")
        .ok()
        .map(|active| active.split(':').map(String::from).collect())
        .unwrap_or_default();

    // Print table header (no separate Owner column; owner is encoded in env name when needed)
    println!("{:<20}  {:<10}  {:<20}", "Environment", "Type", "Status");
    println!("{}", "-".repeat(55));

    // Print each environment with its status
    for (env, is_public) in all_envs {
        let mut status = Vec::new();

        // Check if environment is in active list - O(1) lookup
        if active_list.contains(&env) {
            status.push("activated");
        }

        if registered_envs.contains(&env) {
            status.push("registered");
        }

        let env_type = if is_public { "public" } else { "private" };
        println!("{:<20}  {:<10}  {:<20}",
            env,
            env_type,
            status.join(",")
        );
    }

    Ok(())
}

fn setup_resolv_conf(env_root: &Path) -> Result<()> {
    // Create /etc directory if it doesn't exist
    fs::create_dir_all(env_root.join("etc"))?;

    let resolv_conf_path = env_root.join("etc/resolv.conf");
    let host_resolv_conf = Path::new("/etc/resolv.conf");

    // Skip on 'docker -v /etc/resolv.conf:/etc/resolv.conf:ro' and installing to /
    if resolv_conf_path.exists() {
        return Ok(());
    }

    // Check if /etc/resolv.conf exists on host before trying to copy
    if host_resolv_conf.exists() {
        fs::copy(host_resolv_conf, &resolv_conf_path)
            .with_context(|| format!("Failed to copy /etc/resolv.conf to {}", resolv_conf_path.display()))?;
    } else {
        // If /etc/resolv.conf doesn't exist on host, create a default one
        warn!("/etc/resolv.conf does not exist on host. Creating default resolv.conf");
        let default_resolv_conf = "nameserver 8.8.8.8\nnameserver 223.6.6.6\nnameserver 8.8.4.4\nnameserver 1.1.1.1\n";
        fs::write(&resolv_conf_path, default_resolv_conf)
            .with_context(|| format!("Failed to create default resolv.conf at {}", resolv_conf_path.display()))?;
    }

    Ok(())
}

fn create_environment_dirs_early(env_root: &Path) -> Result<()> {
    let generations_root = env_root.join("generations");
    let gen_1_dir = generations_root.join("1");

    // Create basic directories
    fs::create_dir_all(&gen_1_dir)?;
    fs::create_dir_all(env_root.join("root"))?;
    fs::create_dir_all(env_root.join("ebin"))?;     // for script interpreters,
                                                    // won't go to PATH
    fs::create_dir_all(env_root.join("usr/ebin"))?;
    fs::create_dir_all(env_root.join("usr/sbin"))?;
    fs::create_dir_all(env_root.join("usr/bin"))?;
    fs::create_dir_all(env_root.join("usr/lib"))?;
    fs::create_dir_all(env_root.join("usr/local/bin"))?;
    fs::create_dir_all(env_root.join("var"))?;
    fs::create_dir_all(env_root.join("opt/epkg"))?;
    fs::create_dir_all(env_root.join("etc/epkg"))?;

    // Create symlinks in generation 1
    force_symlink("usr/sbin", env_root.join("sbin"))?;
    force_symlink("usr/bin", env_root.join("bin"))?;
    force_symlink("usr/lib", env_root.join("lib"))?;

    // Create "current" symlink in generations directory pointing to generation 1
    force_symlink("1", generations_root.join("current"))?;

    setup_resolv_conf(env_root)?;

    Ok(())
}

fn create_environment_dirs(env_root: &Path, pkg_format: &PackageFormat, env_config: &EnvConfig) -> Result<()> {
    // Create different lib64 symlinks based on package format
    match pkg_format {
        PackageFormat::Pacman => {
            // For Pacman format:
            // /usr/lib64 -> lib
            // /lib64 -> usr/lib
            fs::create_dir_all(env_root.join("usr"))?;
            force_symlink("lib", env_root.join("usr/lib64"))?;
            force_symlink("usr/lib", env_root.join("lib64"))?;
        },
        _ => {
            // Default behavior for other formats
            fs::create_dir_all(env_root.join("usr/lib64"))?;
            fs::create_dir_all(env_root.join("usr/lib32"))?;
            force_symlink("usr/lib64", env_root.join("lib64"))?;
            force_symlink("usr/lib32", env_root.join("lib32"))?;
        }
    }

    // Create symlinks for applets in usr/local/bin/
    create_applet_symlinks(env_root, pkg_format)?;

    // Set owner and permissions if environment is private (public = false)
    if !env_config.public {
        // Get current user's UID and GID (effective, handles suid)
        let uid = nix::unistd::geteuid();
        let gid = nix::unistd::getegid();

        // Set owner to current user
        chown(env_root, Some(uid), Some(gid))
            .wrap_err_with(|| format!("Failed to set owner for {}", env_root.display()))?;

        // Set mode to 700 (rwx------)
        utils::set_permissions_from_mode(env_root, 0o700)
            .wrap_err_with(|| format!("Failed to set permissions for {}", env_root.display()))?;
    }

    Ok(())
}

// These symlinks must be created and available before running scriptlets.
// If the distro provides the commands, they'll overwrite symlink to our implementation.
fn create_applet_symlink(env_root: &Path, target: &Path, name: &str) -> Result<()> {
    let symlink_path = env_root.join(format!("usr/bin/{}", name));
    force_symlink(target, &symlink_path)
        .with_context(|| format!("Failed to create {} symlink in {}", name, symlink_path.display()))?;
    Ok(())
}

fn create_applet_symlinks(env_root: &Path, pkg_format: &PackageFormat) -> Result<()> {
    let epkg_exe = std::env::current_exe()
        .with_context(|| "Failed to get current executable path")?;

    // Create a symlink from systemctl to /usr/bin/true to prevent blocking on systemctl daemon-reload
    let systemctl_path = env_root.join("usr/bin/systemctl");
    if !systemctl_path.exists() {
        force_symlink("/usr/bin/true", &systemctl_path)
            .with_context(|| format!("Failed to create systemctl symlink in {}", systemctl_path.display()))?;
    }

    // Create symlinks for applets
    create_applet_symlink(env_root, &epkg_exe, "systemd-sysusers")?;
    create_applet_symlink(env_root, &epkg_exe, "systemd-tmpfiles")?;
    create_applet_symlink(env_root, &epkg_exe, "rpmlua")?;

    // Debian-specific setup
    if pkg_format == &PackageFormat::Deb {
        create_applet_symlink(env_root, &epkg_exe, "dpkg-trigger")?;
        create_applet_symlink(env_root, &epkg_exe, "dpkg-query")?;

        // Ensure triggers directory exists
        ensure_triggers_dir(env_root)?;
    }

    Ok(())
}

fn create_default_world_json(env_root: &Path, pkg_format: &PackageFormat) -> Result<()> {
    let mut world = std::collections::HashMap::new();

    // Set default no-install packages for Pacman/Rpm/Deb formats
    match pkg_format {
        PackageFormat::Pacman | PackageFormat::Rpm | PackageFormat::Deb => {
            let mut no_install_packages = vec!["systemd", "dbus"];

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
    let world_path = env_root.join("generations/1/world.json");
    let world_json = serde_json::to_string_pretty(&world)?;
    fs::write(&world_path, world_json)?;

    Ok(())
}

/// Install packages and create metadata files for the environment
fn import_packages_and_create_metadata(env_root: &Path) -> Result<()> {
    let gen_1_dir = env_root.join("generations/1");
    let installed_packages_path = gen_1_dir.join("installed-packages.json");

    // Read packages to install from JSON if importing
    let packages_to_import = if let Some(_) = &config().env.import_file {
        io::read_json_file::<HashMap<String, InstalledPackageInfo>>(&installed_packages_path)?
    } else {
        HashMap::new()
    };

    // Install packages if any
    if !packages_to_import.is_empty() {
        sync_channel_metadata()?;
        let packages_map: InstalledPackagesMap = packages_to_import.into_iter().map(|(k, v)| (k, Arc::new(v))).collect();
        let plan = prepare_installation_plan(&packages_map, None)?;
        execute_installation_plan(plan)?;
    } else {
        // Create metadata files
        fs::write(installed_packages_path, "{\n}")?;

        // Record the environment creation in command history
        record_history(&gen_1_dir, None)?;
    }

    Ok(())
}

/// Initialize env_config and channel_configs
fn initialize_environment_config(env_name: &str, env_root: &Path, env_base: &Path) -> Result<(EnvConfig, PackageFormat)> {
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

    Ok((env_config, pkg_format))
}

/// Setup and validate environment paths, create symlinks if needed
fn setup_environment_paths(env_base: &PathBuf) -> Result<PathBuf> {
    let env_root = if let Some(path) = &config().env.env_path {
        PathBuf::from(path)
    } else {
        env_base.clone()
    };

    let env_channel_yaml = env_root.join("etc/epkg/channel.yaml");
    if env_channel_yaml.exists() {
        return Err(eyre::eyre!("Environment already exists at path: '{}'", env_root.display()));
    }

    // If env_path is specified, we need to create a symlink from env_base to env_root
    if config().env.env_path.is_some() {
        // Check if env_base already exists as a directory (not a symlink)
        if env_base.exists() && !env_base.is_symlink() {
            return Err(eyre::eyre!("Environment base path '{}' already exists as a directory. Cannot create symlink.", env_base.display()));
        }
        // Ensure parent directory of env_base exists
        if let Some(parent) = env_base.parent() {
            fs::create_dir_all(parent)?;
        }
        force_symlink(&env_root, &env_base)
            .with_context(|| format!("Failed to create symlink from {} to {}", env_base.display(), env_root.display()))?;
    }

    Ok(env_root)
}

pub fn create_environment(env_name: &str) -> Result<()> {
    let env_base = dirs().user_envs.join(env_name);
    let env_root = setup_environment_paths(&env_base)?;

    println!("Creating environment '{}' in {}", env_name, env_root.display());

    // Create basic directories early (before we need channel configs)
    create_environment_dirs_early(&env_root)?;

    // Initialize environment config and get package format
    let (env_config, pkg_format) = initialize_environment_config(env_name, &env_root, &env_base)?;
    create_environment_dirs(&env_root, &pkg_format, &env_config)?;

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
        let file_path = env_root.join(&export_file.path);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory for {}", file_path.display()))?;
        }

        // Write the file
        fs::write(&file_path, &export_file.data)
            .with_context(|| format!("Failed to write file {}", file_path.display()))?;
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
    let dest_channel_path = env_root.join("etc/epkg/channel.yaml");
    fs::create_dir_all(dest_channel_path.parent().unwrap())?;
    fs::write(&dest_channel_path, &channel_content)?;

    Ok(())
}

/// Copy additional repo configurations to etc/epkg/repos.d/
fn copy_repo_configs(sources_path: &Path, env_root: &Path, distro_name: &str) -> Result<()> {
    for repo in &config().env.repos {
        let src_repo_yaml_path = sources_path.join(format!("{}-{}.yaml", distro_name, repo));

        // Copy repo config file
        let repos_dir = env_root.join("etc/epkg/repos.d");
        fs::create_dir_all(&repos_dir)?;
        let dest_repo_path = repos_dir.join(format!("{}.yaml", repo));
        fs::copy(&src_repo_yaml_path, &dest_repo_path)?;
    }

    Ok(())
}


/// Copy channel configuration from source to target environment
/// Handles finding the source channel YAML, reading it, optionally updating version,
/// and saving it to etc/epkg/channel.yaml in the target environment.
/// Also copies additional repo configurations to etc/epkg/repos.d/
fn copy_channel_configs(env_root: &Path) -> Result<()> {
    let sources_path = get_epkg_src_path().join("sources");
    let (distro_name, distro_version) = parse_channel_option();

    copy_main_channel_config(&sources_path, env_root, &distro_name, distro_version.as_deref())?;
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


pub fn remove_environment(name: &str) -> Result<()> {
    // Validate environment name
    if name == SELF_ENV || name == MAIN_ENV {
        return Err(eyre::eyre!("Environment cannot be removed: '{}'", name));
    }

    // Check if environment exists
    let env_path = get_env_root(name.to_string())?;
    if !env_path.exists() {
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
    if name == SELF_ENV {
        return Err(eyre::eyre!("Environment 'self' cannot be activated"));
    }

    // Check if environment exists
    if !get_env_root(name.to_string())?.exists() {
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

    // Initialize deactivate script
    let mut script = String::new();

    // Handle session path
    let session_path = original_session_path.unwrap_or_else(|| {
        let path = format!("/tmp/deactivate-{}-{:08x}", std::process::id(), StdRng::from_entropy().gen::<u32>());
        println!("export EPKG_SESSION_PATH=\"{}\"", path);
        script.push_str(&format!("unset EPKG_SESSION_PATH\n"));
        path
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
    push_env_var(&mut script, "EPKG_ACTIVE_ENV", Some(new_active_envs.clone()), original_active_envs);
    std::env::set_var("EPKG_ACTIVE_ENV", new_active_envs);

    // Export env_vars from config
    for (key, value) in &env_config.env_vars {
        let original_value = env::var(key).ok();
        push_env_var(&mut script, key, Some(value.clone()), original_value);
    }

    // Update PATH
    update_path()?;

    // Action 2: Create deactivate shell script
    let deactivate_script = format!("{}-{}.sh", session_path, name);
    fs::write(&deactivate_script, script)?;

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

    let deactivate_script = format!("{}-{}.sh", session_path, deactivated_env);
    let script = fs::read_to_string(&deactivate_script)
        .with_context(|| format!("Failed to read deactivate script: {}", deactivate_script))?;
    println!("{}", script);

    if let Err(e) = fs::remove_file(&deactivate_script) {
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
    if name == SELF_ENV {
        return Err(eyre::eyre!("Environment 'self' cannot be registered"));
    }

    if env_config.register_to_path {
        println!("# Environment '{}' is already registered.", name);
        return Ok(());
    }

    // Get priority from options or auto-detect
    let priority = if let Some(priority) = config().env.priority {
        priority
    } else {
        next_prepend_priority()?
    };

    println!("# Registering environment '{}' with priority {}", name, priority);

    // Update and save environment config
    env_config.register_to_path = true;
    env_config.register_priority = priority;
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
        println!("# Environment '{}' is not registered.", name);
        return Ok(());
    }

    // Update and save environment config
    env_config.register_to_path = false;
    env_config.register_priority = 0;
    io::serialize_env_config(env_config)?;

    update_path()?;
    println!("# Environment '{}' has been unregistered.", name);
    Ok(())
}

/// Get list of registered environment names from env.yaml metadata
///
/// This function scans both private (~/.epkg/envs) and current user's
/// public (/opt/epkg/envs/$USER) environment directories. Each
/// `etc/epkg/env.yaml` is parsed for the `register_to_path` flag and the
/// environment name is included when the flag is true.
pub fn get_registered_env_names() -> Result<Vec<String>> {
    let mut result: Vec<String> = registered_env_configs()
        .into_iter()
        .map(|cfg| cfg.name)
        .collect();
    result.sort();
    result.dedup();
    Ok(result)
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
        fs::write(&output_path, yaml_output)?;
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

    // SELF_ENV.public = (always) true
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
    env_config.register_priority = 0;

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

    // Validate that we're only setting top-level fields
    if parts.len() != 1 {
        return Err(eyre::eyre!("Can only set top-level configuration keys"));
    }

    match parts[0] {
        "name" => config.name = value.to_string(),
        "env_base" => config.env_base = value.to_string(),
        "env_root" => config.env_root = value.to_string(),
        "public" => config.public = value.parse()?,
        "register_to_path" => config.register_to_path = value.parse()?,
        "register_priority" => config.register_priority = value.parse()?,
        _ => return Err(eyre::eyre!("Unknown configuration key: {}", parts[0]))
    }

    // Save the updated config
    io::serialize_env_config(config)?;

    Ok(())
}

pub fn registered_env_configs() -> Vec<EnvConfig> {
    let mut configs = Vec::new();
    let shared_store = config().init.shared_store;
    let current_user = get_username().unwrap_or_default();

    // Collect own environments
    collect_registered_envs_from_dir(&dirs().user_envs, &mut configs);

    // In shared_store mode: also collect other users' public registered envs
    if shared_store {
        let allusers_envs_base = dirs().opt_epkg.join("envs");
        if let Ok(entries) = fs::read_dir(&allusers_envs_base) {
            for entry in entries.flatten() {
                let owner_path = entry.path();
                if !owner_path.is_dir() {
                    continue;
                }

                let owner = owner_path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();

                // Skip own environments (already collected)
                if owner == current_user {
                    continue;
                }

                // Walk this owner's environments and collect public registered ones
                if let Ok(env_entries) = fs::read_dir(&owner_path) {
                    for env_entry in env_entries.flatten() {
                        let env_path = env_entry.path();
                        if !env_path.is_dir() {
                            continue;
                        }

                        let env_name = env_path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or_default();

                        if env_name == SELF_ENV || env_name.starts_with('.') {
                            continue;
                        }

                        // Check if environment is public (not 0o700)
                        let is_public = match fs::metadata(&env_path) {
                            Ok(metadata) => {
                                let mode = metadata.permissions().mode() & 0o777;
                                mode != 0o700
                            }
                            Err(_) => false,
                        };

                        if !is_public {
                            continue;
                        }

                        // Check if registered
                        let config_path = env_path.join("etc/epkg/env.yaml");
                        if !config_path.exists() {
                            continue;
                        }

                        if let Ok(contents) = fs::read_to_string(&config_path) {
                            if let Ok(mut config) = serde_yaml::from_str::<EnvConfig>(&contents) {
                                if config.register_to_path {
                                    // Set name to owner/env_name format
                                    if config.name.is_empty() {
                                        config.name = format!("{}/{}", owner, env_name);
                                    } else {
                                        config.name = format!("{}/{}", owner, config.name);
                                    }
                                    configs.push(config);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    configs
}

fn collect_registered_envs_from_dir(dir: &Path, configs: &mut Vec<EnvConfig>) {
    if !dir.exists() {
        return;
    }

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            warn!("Failed to read environments under {}: {}", dir.display(), err);
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                warn!("Failed to read entry in {}: {}", dir.display(), err);
                continue;
            }
        };

        match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => {},
            _ => continue,
        }

        let env_name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };

        if env_name == SELF_ENV || env_name.starts_with('.') {
            continue;
        }

        let config_path = entry.path().join("etc/epkg/env.yaml");
        if !config_path.exists() {
            continue;
        }

        let contents = match fs::read_to_string(&config_path) {
            Ok(contents) => contents,
            Err(err) => {
                warn!("Failed to read {}: {}", config_path.display(), err);
                continue;
            }
        };

        let mut config: EnvConfig = match serde_yaml::from_str(&contents) {
            Ok(cfg) => cfg,
            Err(err) => {
                warn!("Failed to parse {}: {}", config_path.display(), err);
                continue;
            }
        };

        if config.name.is_empty() {
            config.name = env_name.clone();
        }

        if config.register_to_path {
            configs.push(config);
        }
    }
}

