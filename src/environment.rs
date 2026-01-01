use std::fs;
use std::env;
use std::path::Path;
use std::path::PathBuf;
use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;
use color_eyre::Result;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use serde_json;
use serde_yaml;
use nix::unistd::chown;
use crate::models::*;
use crate::dirs::*;
use crate::models::PACKAGE_CACHE;
use crate::utils::force_symlink;
use crate::deinit::force_remove_dir_all;
use crate::deb_triggers::ensure_triggers_dir;
use crate::plan::prepare_installation_plan;
use crate::install::execute_installation_plan;
use crate::history::record_history;
use crate::path::update_path;
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
            // in create_environment_directories().
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

fn create_environment_directories(env_root: &Path, format: &PackageFormat, env_config: &EnvConfig) -> Result<()> {
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

    // Create symlinks in generation 1
    force_symlink("usr/sbin", env_root.join("sbin"))?;
    force_symlink("usr/bin", env_root.join("bin"))?;
    force_symlink("usr/lib", env_root.join("lib"))?;

    // Create different lib64 symlinks based on package format
    match format {
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

    // Create "current" symlink in generations directory pointing to generation 1
    force_symlink("1", generations_root.join("current"))?;

    setup_resolv_conf(env_root)?;

    // Create a symlink from systemctl to /usr/bin/true to prevent blocking on systemctl daemon-reload
    let systemctl_path = env_root.join("usr/local/bin/systemctl");
    if !systemctl_path.exists() {
        force_symlink("/usr/bin/true", &systemctl_path)
            .with_context(|| format!("Failed to create systemctl symlink in {}", systemctl_path.display()))?;
    }

    // Debian-specific setup
    if format == &PackageFormat::Deb {
        // Create symlink from dpkg-trigger to epkg executable
        let dpkg_trigger_path = env_root.join("usr/local/bin/dpkg-trigger");
        let dpkg_query_path = env_root.join("usr/local/bin/dpkg-query");
        let epkg_exe = std::env::current_exe()
            .with_context(|| "Failed to get current executable path")?;
        force_symlink(&epkg_exe, &dpkg_trigger_path)
            .with_context(|| format!("Failed to create dpkg-trigger symlink in {}", dpkg_trigger_path.display()))?;
        force_symlink(&epkg_exe, &dpkg_query_path)
            .with_context(|| format!("Failed to create dpkg-trigger symlink in {}", dpkg_query_path.display()))?;

        // Ensure triggers directory exists
        ensure_triggers_dir(env_root)?;
    }

    // Set owner and permissions if environment is private (public = false)
    if !env_config.public {
        // Get current user's UID and GID (effective, handles suid)
        let uid = nix::unistd::geteuid();
        let gid = nix::unistd::getegid();

        // Set owner to current user
        chown(env_root, Some(uid), Some(gid))
            .wrap_err_with(|| format!("Failed to set owner for {}", env_root.display()))?;

        // Set mode to 700 (rwx------)
        let perms = fs::Permissions::from_mode(0o700);
        fs::set_permissions(env_root, perms)
            .wrap_err_with(|| format!("Failed to set permissions for {}", env_root.display()))?;
    }

    Ok(())
}

fn create_default_world_json(gen_1_dir: &Path, format: &PackageFormat) -> Result<()> {
    let mut world = std::collections::HashMap::new();

    // Set default no-install packages for Pacman/Rpm/Deb formats
    match format {
        PackageFormat::Pacman | PackageFormat::Rpm | PackageFormat::Deb => {
            let mut no_install_packages = vec!["systemd", "dbus"];

            // Add format-specific packages
            match format {
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
    let world_path = gen_1_dir.join("world.json");
    let world_json = serde_json::to_string_pretty(&world)?;
    fs::write(&world_path, world_json)?;

    Ok(())
}

pub fn create_environment(name: &str) -> Result<()> {
    let env_base = dirs().user_envs.join(name);

    let env_root = if let Some(path) = &config().env.env_path {
        PathBuf::from(path)
    } else {
        env_base.clone()
    };

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

    let env_channel_yaml = env_root.join("etc/epkg/channel.yaml");
    if env_channel_yaml.exists() {
        return Err(eyre::eyre!("Environment already exists at path: '{}'", env_root.display()));
    }
    println!("Creating environment '{}' in {}", name, env_root.display());
    fs::create_dir_all(env_root.join("etc/epkg"))?;

    // Initialize channel and environment config
    let (mut env_export, channel_configs) = if let Some(config_path) = &config().env.import_file {
        import_environment_from_file(config_path)?
    } else {
        create_default_environment_config(name, &env_root)?
    };

    let mut env_config = env_export.env;

    // Override config values with command line options
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
    env_config.link = config().env.link;

    // Get packages before saving config (since env_config will be moved)
    let packages_to_install = std::mem::take(&mut env_export.packages);

    // Save environment config
    crate::io::serialize_env_config(env_config.clone())?;

    // Save channel configs
    save_channel_configs(&env_root, &channel_configs)?;

    // Use the channel config that was just created and saved to env_channel_yaml
    let format = channel_configs[0].format.clone();
    create_environment_directories(&env_root, &format, &env_config)?;

    // Create world.json with default no-install packages
    let generations_root = env_root.join("generations");
    let gen_1_dir = generations_root.join("1");
    create_default_world_json(&gen_1_dir, &format)?;

    // Install packages if any
    if !packages_to_install.is_empty() {
        // Clear installed_packages since this is a new environment with no packages installed yet
        PACKAGE_CACHE.installed_packages.write().unwrap().clear();
        let plan = prepare_installation_plan(&packages_to_install)?;
        execute_installation_plan(plan)?;
    } else {
        // Create metadata files
        let generations_root = env_root.join("generations");
        let gen_1_dir = generations_root.join("1");
        let installed_packages = gen_1_dir.join("installed-packages.json");
        fs::write(installed_packages, "{\n}")?;

        // Record the environment creation in command history
        record_history(&gen_1_dir, None)?;
    }

    Ok(())
}

/*
 * Import environment configuration from a YAML file that may contain multiple documents.
 *
 * File Format:
 * The file can contain multiple YAML documents separated by "---" delimiters:
 *
 * # Environment configuration (first document)
 * name: myenv
 * env_base: /path/to/env
 * env_root: /path/to/env
 * public: false
 * register_to_path: false
 * register_priority: 0
 * env_vars: {}
 * packages: {}
 * world: {}
 *
 * ---
 * # Main channel configuration (second document, optional)
 * format: deb
 * distro: debian
 * version: trixie
 * arch: x86_64
 * channel: debian
 * repos:
 *   main:
 *     enabled: true
 *     index_url: https://deb.debian.org/debian
 * index_url: https://deb.debian.org/debian
 *
 * ---
 * file_name: repo_1.yaml
 * # Additional channel config from repos.d (optional)
 * format: deb
 * distro: debian
 * repos:
 *   contrib:
 *     enabled: true
 *     index_url: https://deb.debian.org/debian
 * index_url: https://deb.debian.org/debian
 *
 * ---
 * file_name: repo_2.yaml
 * # Another additional channel config (optional)
 * format: deb
 * distro: debian
 * repos:
 *   non-free:
 *     enabled: true
 *     index_url: https://deb.debian.org/debian
 * index_url: https://deb.debian.org/debian
 *
 * Parsing Logic:
 * 1. First document is always parsed as EnvImport (EnvConfig plus packages/world)
 * 2. Subsequent documents are parsed as ChannelConfig
 * 3. Documents with "file_name:" field are from repos.d and are skipped
 * 4. If no ChannelConfig documents found, try parsing entire content as single ChannelConfig
 *
 * Returns: (EnvImport, Vec<ChannelConfig>) tuple
 */
fn import_environment_from_file(config_path: &str) -> Result<(EnvImport, Vec<ChannelConfig>)> {
    let config_contents = fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file: {}", config_path))?;

    // Parse channel configs
    let mut channel_configs = Vec::new();
    let mut env_import: Option<EnvImport> = None;

    // Split the content by "---" to handle multiple YAML documents
    let documents: Vec<&str> = config_contents.split("---").collect();

    for (i, doc) in documents.iter().enumerate() {
        let doc = doc.trim();
        if doc.is_empty() {
            continue;
        }

        // Parse first document as EnvImport
        if i == 0 {
            env_import = Some(serde_yaml::from_str(doc)
                .with_context(|| format!("Failed to parse env import from file: {}", config_path))?);
            continue;
        }

        // Try to parse as ChannelConfig
        if let Ok(mut channel_config) = serde_yaml::from_str::<ChannelConfig>(doc) {
            // Store original document data
            channel_config.file_data = doc.to_string();
            channel_configs.push(channel_config);
        }
    }

    let env_import = env_import.ok_or_else(|| eyre::eyre!("No environment config found in file: {}", config_path))?;
    Ok((env_import, channel_configs))
}

fn create_default_environment_config(name: &str, env_root: &Path) -> Result<(EnvImport, Vec<ChannelConfig>)> {
    // Initialize channel from command line option or default
    let channel = config().env.channel.clone().unwrap_or(DEFAULT_CHANNEL.to_string());

    // Split channel into channel_name and version_name
    let (channel_name, version_name) = if let Some(colon_pos) = channel.find(':') {
        let (name, version) = channel.split_at(colon_pos);
        (name.to_string(), Some(version[1..].to_string()))
    } else {
        (channel.clone(), None)
    };

    // If creating self environment, use env_root directly
    // Otherwise, try to find the self environment's epkg_src path
    let epkg_src = if name == SELF_ENV {
        env_root.join("usr/src/epkg")
    } else {
        get_epkg_src_path()
    };
    let mut channel_configs = Vec::new();

    // Load main channel config from the built-in sources directory
    let channel_path = epkg_src.join("sources");
    let mut src_channel_yaml_path = channel_path.join(format!("{}.yaml", channel));
    if !src_channel_yaml_path.exists() {
        src_channel_yaml_path = channel_path.join(format!("{}.yaml", channel_name));
    }
    if !src_channel_yaml_path.exists() {
        return Err(eyre::eyre!("Channel not found: '{}'", channel));
    }

    // Load and process main channel config with record_file_info=true
    crate::io::load_and_process_channel_config(&src_channel_yaml_path, &mut channel_configs, true)?;

    // Apply version override if specified
    if let Some(version) = version_name {
        if let Some(main_config) = channel_configs.first_mut() {
            main_config.version = version.clone();
            // Update the file_data with the new version
            let contents = update_version_in_contents(&main_config.file_data, &version);
            main_config.file_data = contents;
        }
    }

    // Load additional repo configs
    for repo in &config().env.repos {
        let mut src_repo_yaml_path = channel_path.join(format!("{}-{}.yaml", channel_name, repo));
        if !src_repo_yaml_path.exists() {
            // Try without channel prefix
            src_repo_yaml_path = channel_path.join(format!("{}.yaml", repo));
        }
        if !src_repo_yaml_path.exists() {
            return Err(eyre::eyre!("Repo config not found in {} or {}-{}.yaml", src_repo_yaml_path.display(), channel_name, repo));
        }
        crate::io::load_and_process_channel_config(&src_repo_yaml_path, &mut channel_configs, true)?;
    }

    Ok((EnvImport::default(), channel_configs))
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

fn save_channel_configs(env_root: &Path, channel_configs: &[ChannelConfig]) -> Result<()> {
    if channel_configs.is_empty() {
        return Err(eyre::eyre!("No channel configs to save"));
    }

    // Save main channel config
    let env_channel_yaml = env_root.join("etc/epkg/channel.yaml");
    fs::write(&env_channel_yaml, &channel_configs[0].file_data)?;

    // Save additional channel configs to repos.d
    if channel_configs.len() > 1 {
        let repos_dir = env_root.join("etc/epkg/repos.d");
        fs::create_dir_all(&repos_dir)?;

        for config in channel_configs.iter().skip(1) {
            if config.file_data.starts_with("file_name: ") {
                // Extract filename from first line and strip it from file_data
                if let Some((first_line, file_data)) = config.file_data.split_once('\n') {
                    let filename = first_line[11..].trim().to_string();
                    let file_path = repos_dir.join(&filename);
                    fs::write(&file_path, &file_data)?;
                };
            }
        }
    }

    Ok(())
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
    crate::io::serialize_env_config(env_config)?;

    update_path()?;
    Ok(())
}

pub fn register_environment(name: &str) -> Result<()> {
    let env_config = crate::io::deserialize_env_config_for(name.to_string())?;
    register_environment_for(name, env_config)
}

pub fn unregister_environment(name: &str) -> Result<()> {
    let mut env_config = crate::io::deserialize_env_config_for(name.to_string())?;

    if !env_config.register_to_path {
        println!("# Environment '{}' is not registered.", name);
        return Ok(());
    }

    // Update and save environment config
    env_config.register_to_path = false;
    env_config.register_priority = 0;
    crate::io::serialize_env_config(env_config)?;

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

pub fn export_environment(name: &str, output: Option<String>) -> Result<()> {
    // Prepare environment export container
    let mut env_export = EnvExport {
        env: env_config().clone(),
        ..EnvExport::default()
    };
    let generations_root = get_generations_root(name)?;

    // Get installed packages
    let current_gen = fs::read_link(generations_root.join("current"))?;
    let installed_packages_path = generations_root.join(current_gen.clone()).join("installed-packages.json");

    if installed_packages_path.exists() {
        env_export.packages = crate::io::read_json_file(&installed_packages_path)?;
    } else {
        warn!("No installed packages found for environment '{}' at {}", name, installed_packages_path.display());
        return Err(eyre::eyre!("No installed packages found for environment '{}' at {}", name, installed_packages_path.display()));
    }

    // Load world.json content
    let world_path = generations_root.join(current_gen).join("world.json");
    env_export.world = crate::io::read_json_file(&world_path)
        .with_context(|| format!("Failed to read world.json from {}", world_path.display()))?;

    // Serialize env_export
    let combined_yaml = serde_yaml::to_string(&env_export)?;
    let combined_yaml = build_combined_yaml(combined_yaml, &env_export.env)?;

    // Write to file or stdout
    if let Some(output_path) = output {
        fs::write(&output_path, combined_yaml)?;
        println!("Environment configuration exported to {}", output_path);
    } else {
        println!("{}", combined_yaml);
    }

    Ok(())
}

// Its output will be used by import_environment_from_file(), see its comment for the content format.
fn build_combined_yaml(mut combined_yaml: String, env_config: &EnvConfig) -> Result<String> {
    let env_root = PathBuf::from(&env_config.env_root);
    let channel_file = env_root.join("etc/epkg/channel.yaml");
    if channel_file.exists() {
        let contents = fs::read_to_string(channel_file)?;
        // Add main channel config with separator
        combined_yaml.push_str(&format!("\n---\n{}", contents));
    }

    // Add additional channel configs from repos.d
    let repos_dir = env_root.join("etc/epkg/repos.d");
    if repos_dir.exists() {
        for entry in fs::read_dir(repos_dir)? {
            let entry = entry?;
            let path = entry.path();
            // Skip dot files
            if let Some(file_name) = path.file_name() {
                if file_name.to_string_lossy().starts_with('.') {
                    continue;
                }
            }
            if path.extension().and_then(|s| s.to_str()) == Some("yaml") {
                let file_name = path.file_name().unwrap().to_string_lossy().to_string();
                let contents = fs::read_to_string(&path)?;

                combined_yaml.push_str(&format!("\n---\nfile_name: {}\n{}", file_name, contents));
            }
        }
    }

    Ok(combined_yaml)
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
    crate::io::serialize_env_config(config)?;

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
