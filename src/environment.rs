use std::fs;
use std::os::unix::fs::symlink;
use std::env;
use std::path::Path;
use std::path::PathBuf;
use std::collections::HashSet;
use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;
use color_eyre::Result;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use serde_json;
use serde_yaml;
use crate::models::*;
use crate::dirs::*;
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

impl PackageManager {

    fn next_prepend_priority(&self) -> Result<i32> {
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

    /// Get list of all environment names except 'base'
    ///
    /// This function lists all environment directories in both private and public
    /// locations, excluding the special 'base' environment.
    ///
    /// Returns a Vec of (env_name, is_public, owner) tuples.
    pub fn get_all_env_names(&self) -> Result<Vec<(String, bool, String)>> {
        let mut all_envs = Vec::new();
        let current_user = env::var("USER").unwrap_or_default();

        // Get private environments
        if let Ok(entries) = fs::read_dir(&dirs().private_envs) {
            for entry in entries {
                if let Ok(entry) = entry {
                    let name = entry.file_name().into_string().unwrap_or_default();
                    if name != BASE_ENV  && !name.starts_with('.') {
                        all_envs.push((name, false, current_user.clone()));
                    }
                }
            }
        }

        // Get public environments
        let public_envs_parent = dirs().public_envs.parent().unwrap_or_else(||Path::new("."));
        if let Ok(entries) = fs::read_dir(public_envs_parent) {
            for entry in entries {
                if let Ok(entry) = entry {
                    if let Ok(owner_entries) = fs::read_dir(entry.path()) {
                        let owner = entry.file_name().into_string().unwrap_or_default();
                        for owner_entry in owner_entries {
                            if let Ok(owner_entry) = owner_entry {
                                let name = owner_entry.file_name().into_string().unwrap_or_default();
                                if name != BASE_ENV {
                                    all_envs.push((name, true, owner.clone()));
                                }
                            }
                        }
                    }
                }
            }
        }

        // Sort by name
        all_envs.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(all_envs)
    }

    pub fn list_environments(&self) -> Result<()> {
        // Get all environments except base
        let all_envs = self.get_all_env_names()?;
        let registered_envs: Vec<String> = self.get_registered_env_names()?;

        // Get active environments list once and convert to HashSet for O(1) lookups
        let active_list: Vec<String> = env::var("EPKG_ACTIVE_ENV")
            .ok()
            .map(|active| active.split(':').map(String::from).collect())
            .unwrap_or_default();

        // Print table header
        println!("{:<15}  {:<10}  {:<10}  {:<20}", "Environment", "Type", "Owner", "Status");
        println!("{}", "-".repeat(55));

        // Print each environment with its status
        for (env, is_public, owner) in all_envs {
            let mut status = Vec::new();

            // Check if environment is in active list - O(1) lookup
            if active_list.contains(&env) {
                status.push("activated");
            }

            if registered_envs.contains(&env) {
                status.push("registered");
            }

            let env_type = if is_public { "public" } else { "private" };
            println!("{:<15}  {:<10}  {:<10}  {:<20}",
                env,
                env_type,
                owner,
                status.join(",")
            );
        }

        Ok(())
    }

    fn create_environment_directories(&self, env_root: &Path, format: &PackageFormat) -> Result<()> {
        let generations_root = env_root.join("generations");
        let gen_1_dir = generations_root.join("1");

        // Create base directories
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
        symlink("usr/sbin", env_root.join("sbin"))?;
        symlink("usr/bin", env_root.join("bin"))?;
        symlink("usr/lib", env_root.join("lib"))?;

        // Create different lib64 symlinks based on package format
        match format {
            PackageFormat::Pacman => {
                // For Pacman format:
                // /usr/lib64 -> lib
                // /lib64 -> usr/lib
                fs::create_dir_all(env_root.join("usr"))?;
                symlink("lib", env_root.join("usr/lib64"))?;
                symlink("usr/lib", env_root.join("lib64"))?;
            },
            _ => {
                // Default behavior for other formats
                fs::create_dir_all(env_root.join("usr/lib64"))?;
                fs::create_dir_all(env_root.join("usr/lib32"))?;
                symlink("usr/lib64", env_root.join("lib64"))?;
                symlink("usr/lib32", env_root.join("lib32"))?;
            }
        }

        // Create "current" symlink in generations directory pointing to generation 1
        symlink("1", generations_root.join("current"))?;

        fs::copy("/etc/resolv.conf", env_root.join("etc/resolv.conf"))?;

        // Create a symlink from systemctl to /usr/bin/true to prevent blocking on systemctl daemon-reload
        let systemctl_path = env_root.join("usr/local/bin/systemctl");
        if !systemctl_path.exists() {
            symlink("/usr/bin/true", &systemctl_path)
                .with_context(|| format!("Failed to create systemctl symlink in {}", systemctl_path.display()))?;
        }

        Ok(())
    }

    pub fn new_env_base(&self, name: &str) -> PathBuf {
        if config().env.public && name != MAIN_ENV {
            dirs().public_envs.join(name)
        } else {
            dirs().private_envs.join(name)
        }
    }

    pub fn create_environment(&mut self, name: &str) -> Result<()> {
        let env_base = self.new_env_base(name);

        let env_root = if let Some(path) = &config().env.env_path {
            PathBuf::from(path)
        } else {
            env_base.clone()
        };

        let env_channel_yaml = env_root.join("etc/epkg/channel.yaml");
        if env_channel_yaml.exists() {
            return Err(eyre::eyre!("Environment already exists at path: '{}'", env_base.display()));
        }
        println!("Creating environment '{}' in {}", name, env_root.display());
        fs::create_dir_all(env_root.join("etc/epkg"))?;

        // Initialize channel and environment config
        let (mut env_config, channel_configs) = if let Some(config_path) = &config().env.import_file {
            self.import_environment_from_file(config_path)?
        } else {
            self.create_default_environment_config(name)?
        };

        // Override config values with command line options
        env_config.name = name.to_string();
        env_config.env_base = env_base.to_string_lossy().to_string();
        env_config.env_root = env_root.to_string_lossy().to_string();
        env_config.public = config().env.public;
        env_config.register_to_path = false;
        env_config.register_priority = 0;

        // 'main' is the default environment for many actions, so enforce it be private,
        // to avoid careless daily installs be exposed to others.
        if env_config.public && name == MAIN_ENV {
            println!("Notice: environment 'main' is always created private");
            env_config.public = false;
        }

        // Get packages before saving config (since env_config will be moved)
        let packages_to_install = std::mem::take(&mut env_config.packages);

        // Save environment config
        crate::io::serialize_env_config(env_config)?;

        // Save channel configs
        self.save_channel_configs(&env_root, &channel_configs)?;

        // Use the channel config that was just created and saved to env_channel_yaml
        let format = channel_configs[0].format.clone();
        self.create_environment_directories(&env_root, &format)?;

        // Install packages if any
        if !packages_to_install.is_empty() {
            let plan = self.prepare_installation_plan(&packages_to_install)?;
            self.execute_installation_plan(plan)?;
        } else {
            // Create metadata files
            let generations_root = env_root.join("generations");
            let gen_1_dir = generations_root.join("1");
            let installed_packages = gen_1_dir.join("installed-packages.json");
            fs::write(installed_packages, "{\n}")?;

            // Record the environment creation in command history
            self.record_history(&gen_1_dir, None)?;
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
     * 1. First document is always parsed as EnvConfig
     * 2. Subsequent documents are parsed as ChannelConfig
     * 3. Documents with "file_name:" field are from repos.d and are skipped
     * 4. If no ChannelConfig documents found, try parsing entire content as single ChannelConfig
     *
     * Returns: (EnvConfig, Vec<ChannelConfig>) tuple
     */
    fn import_environment_from_file(&self, config_path: &str) -> Result<(EnvConfig, Vec<ChannelConfig>)> {
        let config_contents = fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read config file: {}", config_path))?;

        // Parse channel configs
        let mut channel_configs = Vec::new();
        let mut env_config: Option<EnvConfig> = None;

        // Split the content by "---" to handle multiple YAML documents
        let documents: Vec<&str> = config_contents.split("---").collect();

        for (i, doc) in documents.iter().enumerate() {
            let doc = doc.trim();
            if doc.is_empty() {
                continue;
            }

            // Parse first document as EnvConfig
            if i == 0 {
                env_config = Some(serde_yaml::from_str(doc)
                    .with_context(|| format!("Failed to parse env config from file: {}", config_path))?);
                continue;
            }

            // Try to parse as ChannelConfig
            if let Ok(mut channel_config) = serde_yaml::from_str::<ChannelConfig>(doc) {
                // Store original document data
                channel_config.file_data = doc.to_string();
                channel_configs.push(channel_config);
            }
        }

        let env_config = env_config.ok_or_else(|| eyre::eyre!("No environment config found in file: {}", config_path))?;
        Ok((env_config, channel_configs))
    }

    fn create_default_environment_config(&self, _name: &str) -> Result<(EnvConfig, Vec<ChannelConfig>)> {
        // Initialize channel from command line option or default
        let channel = config().env.channel.clone().unwrap_or(DEFAULT_CHANNEL.to_string());

        // Split channel into channel_name and version_name
        let (channel_name, version_name) = if let Some(colon_pos) = channel.find(':') {
            let (name, version) = channel.split_at(colon_pos);
            (name.to_string(), Some(version[1..].to_string()))
        } else {
            (channel.clone(), None)
        };

        let epkg_src = get_epkg_src_path()?;
        let mut channel_configs = Vec::new();

        // Load main channel config
        let channel_path = epkg_src.join("channel");
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
                let contents = self.update_version_in_contents(&main_config.file_data, &version);
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

        Ok((EnvConfig::default(), channel_configs))
    }

    /// Update version line in YAML contents
    /// If a version line exists, replace it; otherwise append a new version line
    fn update_version_in_contents(&self, contents: &str, version: &str) -> String {
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

    fn save_channel_configs(&self, env_root: &Path, channel_configs: &[ChannelConfig]) -> Result<()> {
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

    pub fn remove_environment(&mut self, name: &str) -> Result<()> {
        // Validate environment name
        if name == BASE_ENV || name == MAIN_ENV {
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
                    self.deactivate_environment()?;
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
        self.unregister_environment(name)?;

        fs::remove_dir_all(&env_path)
            .with_context(|| format!("Failed to remove environment directory '{}'", env_path.display()))?;

        println!("# Environment '{}' has been removed.", name);
        Ok(())
    }

    pub fn activate_environment(&mut self, name: &str) -> Result<()> {
        // Validate environment name
        if name == BASE_ENV {
            return Err(eyre::eyre!("Environment 'base' cannot be activated"));
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
        let env_config = crate::models::env_config();

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
        self.update_path()?;

        // Action 2: Create deactivate shell script
        let deactivate_script = format!("{}-{}.sh", session_path, name);
        fs::write(&deactivate_script, script)?;

        Ok(())
    }

    pub fn deactivate_environment(&mut self) -> Result<()> {
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
        self.update_path()?;
        Ok(())
    }

    pub fn register_environment_for(&mut self, name: &str, mut env_config: EnvConfig) -> Result<()> {
        // Validate environment name
        if name == BASE_ENV {
            return Err(eyre::eyre!("Environment 'base' cannot be registered"));
        }

        if env_config.register_to_path {
            println!("# Environment '{}' is already registered.", name);
            return Ok(());
        }

        // Get priority from options or auto-detect
        let priority = if let Some(priority) = config().env.priority {
            priority
        } else {
            self.next_prepend_priority()?
        };

        println!("# Registering environment '{}' with priority {}", name, priority);

        // Update and save environment config
        env_config.register_to_path = true;
        env_config.register_priority = priority;
        crate::io::serialize_env_config(env_config)?;

        self.update_path()?;
        Ok(())
    }

    pub fn register_environment(&mut self, name: &str) -> Result<()> {
        let env_config = crate::io::deserialize_env_config_for(name.to_string())?;
        self.register_environment_for(name, env_config)
    }

    pub fn unregister_environment(&mut self, name: &str) -> Result<()> {
        let mut env_config = crate::io::deserialize_env_config_for(name.to_string())?;

        if !env_config.register_to_path {
            println!("# Environment '{}' is not registered.", name);
            return Ok(());
        }

        // Update and save environment config
        env_config.register_to_path = false;
        env_config.register_priority = 0;
        crate::io::serialize_env_config(env_config)?;

        self.update_path()?;
        println!("# Environment '{}' has been unregistered.", name);
        Ok(())
    }

    /// Get list of registered environment names from env.yaml metadata
    ///
    /// This function scans both private (~/.epkg/envs) and current user's
    /// public (/opt/epkg/envs/$USER) environment directories. Each
    /// `etc/epkg/env.yaml` is parsed for the `register_to_path` flag and the
    /// environment name is included when the flag is true.
    pub fn get_registered_env_names(&self) -> Result<Vec<String>> {
        let mut result: Vec<String> = registered_env_configs()
            .into_iter()
            .map(|cfg| cfg.name)
            .collect();
        result.sort();
        result.dedup();
        Ok(result)
    }

    pub fn export_environment(&mut self, name: &str, output: Option<String>) -> Result<()> {
        // Get environment config and channel config first
        let mut env_config = crate::models::env_config().clone();
        let generations_root = get_generations_root(name)?;

        // Get installed packages
        let current_gen = fs::read_link(generations_root.join("current"))?;
        let installed_packages_path = generations_root.join(current_gen).join("installed-packages.json");

        if installed_packages_path.exists() {
            let contents = fs::read_to_string(&installed_packages_path)?;
            env_config.packages = serde_json::from_str(&contents)?;
        } else {
            warn!("No installed packages found for environment '{}' at {}", name, installed_packages_path.display());
            return Err(eyre::eyre!("No installed packages found for environment '{}' at {}", name, installed_packages_path.display()));
        }

        let mut combined_yaml = serde_yaml::to_string(&env_config)?;
        combined_yaml = self.build_combined_yaml(combined_yaml, &env_config)?;

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
    fn build_combined_yaml(&self, mut combined_yaml: String, env_config: &EnvConfig) -> Result<String> {
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
    pub fn get_environment_config(&mut self, name: &str) -> Result<()> {
        let config = crate::models::env_config();

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
    pub fn set_environment_config(&mut self, name: &str, value: &str) -> Result<()> {
        let config = crate::models::env_config(); // load from file
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
}

pub(crate) fn registered_env_configs() -> Vec<EnvConfig> {
    let mut configs = Vec::new();
    collect_registered_envs_from_dir(&dirs().private_envs, &mut configs);
    collect_registered_envs_from_dir(&dirs().public_envs, &mut configs);
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

        if env_name == BASE_ENV || env_name.starts_with('.') {
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
