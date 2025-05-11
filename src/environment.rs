use std::fs;
use std::os::unix::fs::symlink;
use std::env;
use std::path::Path;
use std::path::PathBuf;
use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;
use anyhow::{Result, Context};
use serde_json;
use serde_yaml;
use crate::models::*;
use crate::dirs::*;
use log::warn;

// epkg PATH management
//
// $HOME/.epkg/config/path.d directory structure:
//
// The path.d directory implements a priority-based PATH management system with two subdirectories:
// - prepend/: Contains symlinks to paths that should be added to the beginning of PATH
// - append/: Contains symlinks to paths that should be added to the end of PATH
//
// Each symlink follows the naming convention: PRIORITY-NAME
// - PRIORITY: A numeric value that determines the order of paths
// - NAME: Env name for the path entry
//
// Example structure:
// .epkg/config/path.d/
// ├── prepend/
// │   ├── 10-main -> ~/.epkg/envs/main/usr/ebin
// │   └── 20-debian12 -> ~/.epkg/envs/debian12/usr/ebin
// └── append/
//     └── 10-archlinux -> ~/.epkg/envs/archlinux/usr/ebin
//
// Paths are processed in numeric order (ascending) within each directory.
// The final PATH is constructed as:
//   prepend_paths + original_PATH + append_paths
//
// This design allows for:
// 1. Flexible path ordering through numeric priorities
// 2. Clear separation of prepend vs append paths
// 3. Easy addition/removal of paths through symlinks
// 4. System-wide and user-specific path management
// 5. Maintainable and human-readable path configuration
//
// Environment Management Operations:
//
// 1. Register/Unregister (Persistent Configuration):
//    - Purpose: Manage installed commands for daily usage
//    - Effect: Creates/removes symlinks in path.d directory
//    - Persistence: Changes are saved to disk and persist across shell sessions
//    - Usage: For making installed commands available system-wide
//    - Example: Registering development tools for daily use
//
// 2. Activate/Deactivate (Session-based):
//    - Purpose: Manage project-specific development environments
//    - Effect: Updates PATH for current shell session only
//    - Persistence: Changes only affect current terminal/shell login
//    - Usage: For project development, testing, or temporary environment switching
//    - Example: Activating a specific project's development environment
//
// Key Differences:
// - Register/Unregister:
//   * Changes filesystem (creates/removes symlinks in $HOME/.epkg/config/path.d)
//   * Affects all shell sessions
//   * Used for permanent environment setup
//   * Requires root/admin privileges for system-wide changes
//
// - Activate/Deactivate:
//   * Changes only environment variables
//   * Affects only current shell session
//   * Used for temporary environment switching
//   * Can be done by any user for their own sessions
//
// Environment Registration Rules:
// The 'epkg env register' command manages path registration with the following rules:
// - Command format: epkg env register <name> [--priority N]
// - If --priority is specified:
//   * N >= 0 entries will be created under path.d/prepend/
//   * N <  0 entries will be created under path.d/append/
// - If --priority is omitted:
//   * The path will be registered under path.d/prepend/ with the first unused priority in 10, 20, 30, ...
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
        println!("; export {}={}", key, v);
    }

    // Prepare restore command (store in script)
    match original_value {
        Some(v) => script.push_str(&format!("; export {}={}\n", key, v)),
        None => script.push_str(&format!("; unset {}\n", key)),
    }
}

impl PackageManager {

    /// Get list of all environment names except 'common'
    ///
    /// This function lists all environment directories in both private and public
    /// locations, excluding the special 'common' environment.
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
                    if name != "common" {
                        all_envs.push((name, false, current_user.clone()));
                    }
                }
            }
        }

        // Get public environments
        if let Ok(entries) = fs::read_dir(&dirs().public_envs) {
            for entry in entries {
                if let Ok(entry) = entry {
                    if let Ok(owner_entries) = fs::read_dir(entry.path()) {
                        let owner = entry.file_name().into_string().unwrap_or_default();
                        for owner_entry in owner_entries {
                            if let Ok(owner_entry) = owner_entry {
                                let name = owner_entry.file_name().into_string().unwrap_or_default();
                                if name != "common" {
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
        // Get all environments except common
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

    fn create_environment_directories(&mut self, env_root: &Path) -> Result<()> {
        let generations_root = env_root.join("generations");
        let gen_1_dir = generations_root.join("1");

        // Create base directories
        fs::create_dir_all(&gen_1_dir)?;
        fs::create_dir_all(env_root.join("ebin"))?;     // for script interpreters,
                                                        // won't go to PATH
        fs::create_dir_all(env_root.join("usr/ebin"))?;
        fs::create_dir_all(env_root.join("usr/sbin"))?;
        fs::create_dir_all(env_root.join("usr/bin"))?;
        fs::create_dir_all(env_root.join("usr/lib"))?;
        fs::create_dir_all(env_root.join("usr/lib64"))?;
        fs::create_dir_all(env_root.join("etc/epkg"))?;
        fs::create_dir_all(env_root.join("var"))?;

        // Create symlinks in generation 1
        symlink("usr/sbin", env_root.join("sbin"))?;
        symlink("usr/bin", env_root.join("bin"))?;
        symlink("usr/lib", env_root.join("lib"))?;
        symlink("usr/lib64", env_root.join("lib64"))?;

        // Create "current" symlink in generations directory pointing to generation 1
        symlink("1", generations_root.join("current"))?;

        fs::copy("/etc/resolv.conf", env_root.join("etc/resolv.conf"))?;

        Ok(())
    }

    pub fn new_env_base(&self, name: &str) -> PathBuf {
        if config().env.public {
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
            return Err(anyhow::anyhow!("Environment already exists at path: '{}'", env_base.display()));
        }

        self.create_environment_directories(&env_root)?;

        // Initialize channel and environment config
        let mut env_config = if let Some(config_path) = &config().env.import_file {
            let config_contents = fs::read_to_string(config_path)
                .with_context(|| format!("Failed to read config file: {}", config_path))?;

            // Parse configs separately
            let env_config: EnvConfig = serde_yaml::from_str(&config_contents)
                .with_context(|| format!("Failed to parse env config from file: {}", config_path))?;
            let channel_config: ChannelConfig = serde_yaml::from_str(&config_contents)
                .with_context(|| format!("Failed to parse channel config from file: {}", config_path))?;

            // Save channel config
            let channel_yaml = serde_yaml::to_string(&channel_config)?;
            fs::write(&env_channel_yaml, channel_yaml)?;

            // Store channel config
            self.channel_config.insert(name.to_string(), channel_config);

            env_config
        } else {
            // Initialize channel from command line option or default
            let channel = config().env.channel.clone().unwrap_or(DEFAULT_CHANNEL.to_string());
            let common_env_root = find_env_root("common")
                .ok_or_else(|| anyhow::anyhow!("Common environment not found"))?;
            let src_channel_yaml = common_env_root.join("opt/epkg-manager/channel").join(format!("{}.yaml", channel));

            if !src_channel_yaml.exists() {
                return Err(anyhow::anyhow!("Channel not found: '{}'", channel));
            }

            fs::copy(src_channel_yaml, &env_channel_yaml)?;

            EnvConfig::default()
        };

        // Override config values with command line options
        env_config.name = name.to_string();
        env_config.env_base = env_base.to_string_lossy().to_string();
        env_config.env_root = env_root.to_string_lossy().to_string();
        env_config.public = config().env.public;
        env_config.register_to_path = false;
        env_config.register_priority = 0;

        // Store environment config
        self.env_config.insert(name.to_string(), env_config.clone());

        // Save environment config
        let env_config_path = get_env_config_path(name);
        fs::create_dir_all(env_config_path.parent().unwrap())?;
        let yaml = serde_yaml::to_string(&env_config)?;
        fs::write(env_config_path, yaml)?;

        // Install packages if any
        if !env_config.installed_packages.is_empty() {
            self.install_pkglines(env_config.installed_packages)?;
        } else {
            // Create metadata files
            let generations_root = env_root.join("generations");
            let gen_1_dir = generations_root.join("1");
            let installed_packages = gen_1_dir.join("installed-packages.json");
            fs::write(installed_packages, "{\n}")?;

            // Record the environment creation in command history
            self.record_history("create", Vec::new(), Vec::new())?;
        }

        println!("Environment '{}' has been created in {}", name, env_root.display());
        Ok(())
    }

    pub fn remove_environment(&mut self, name: &str) -> Result<()> {
        // Validate environment name
        if name == "common" || name == "main" {
            return Err(anyhow::anyhow!("Environment cannot be removed: '{}'", name));
        }

        // Check if environment exists
        let env_path = self.get_env_root(name.to_string())?;
        if !env_path.exists() {
            return Err(anyhow::anyhow!("Environment does not exist: '{}'", name));
        }

        // Check if environment is active and handle stacked environments
        if let Ok(active_envs) = env::var("EPKG_ACTIVE_ENV") {
            let env_stack: Vec<&str> = active_envs.split(':').collect();

            if let Some(pos) = env_stack.iter().position(|&x| x == name) {
                if pos == 0 {
                    // If it's the first environment, we can remove it
                    let new_stack = env_stack[1..].join(":");
                    env::set_var("EPKG_ACTIVE_ENV", &new_stack);
                    self.deactivate_environment()?;
                } else {
                    // If it's in the middle of the stack, return error
                    return Err(anyhow::anyhow!(
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

        // Rename to hide environment
        let hidden_path = dirs().private_envs.join(format!(".{}", name));
        fs::rename(env_path, hidden_path)?;

        println!("# Environment '{}' has been removed.", name);
        Ok(())
    }

    pub fn activate_environment(&mut self, name: &str) -> Result<()> {
        // Validate environment name
        if name == "common" {
            return Err(anyhow::anyhow!("Environment 'common' cannot be activated"));
        }

        // Check if environment exists
        if !self.get_env_root(name.to_string())?.exists() {
            return Err(anyhow::anyhow!("Environment not exist: '{}'", name));
        }

        // Get current environment states
        let original_active_envs = env::var("EPKG_ACTIVE_ENV").ok();
        let original_session_path = env::var("EPKG_SESSION_PATH").ok();

        // Check if environment is already active
        if let Some(active_envs) = &original_active_envs {
            if active_envs.split(':').any(|env| env == name) {
                return Err(anyhow::anyhow!("Environment '{}' is already active", name));
            }
            // Check if pure mode is incompatible with stack mode
            if config().env.pure && config().env.stack {
                return Err(anyhow::anyhow!("Cannot use pure mode with stack mode"));
            }
            // Check if non-stack mode is incompatible with existing active environments
            if !config().env.stack && !active_envs.is_empty() {
                return Err(anyhow::anyhow!("Cannot activate environment in non-stack mode when other environments are active. Please deactivate them first."));
            }
        }

        // Get environment config for env_vars
        let env_config = self.get_env_config(name.to_string())?;

        // Initialize deactivate script
        let mut script = String::new();

        // Handle session path
        let session_path = original_session_path.unwrap_or_else(|| {
            let path = format!("/tmp/deactivate-{}-{:08x}", std::process::id(), StdRng::from_entropy().gen::<u32>());
            println!("; export EPKG_SESSION_PATH=\"{}\"", path);
            script.push_str(&format!("; unset EPKG_SESSION_PATH\n"));
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
            return Err(anyhow::anyhow!("No environment is currently active"));
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

    pub fn register_environment(&mut self, name: &str) -> Result<()> {
        // Validate environment name
        if name == "common" {
            return Err(anyhow::anyhow!("Environment 'common' cannot be registered"));
        }

        let env_config = match self.get_env_config(name.to_string()) {
            Ok(config) => config,
            Err(_) => return Err(anyhow::anyhow!("Environment '{}' does not exist", name))
        };

        if env_config.register_to_path {
            println!("# Environment '{}' is already registered.", name);
            return Ok(());
        }

        // Create path.d directories if they don't exist
        let prepend_dir = dirs().home_config.join("path.d/prepend");
        let append_dir = dirs().home_config.join("path.d/append");

        // Get priority from options or auto-detect
        let priority = if let Some(priority) = config().env.priority {
            priority
        } else {
            // Auto-detect first available priority
            let mut priority = 10;
            loop {
                let symlink_path = prepend_dir.join(format!("{}-{}", priority, name));
                if !symlink_path.exists() {
                    break priority;
                }
                priority += 10;
            }
        };

        // Create symlink in appropriate directory
        let ebin_path = Path::new(&env_config.env_root).join("usr/ebin");
        let symlink_path = if priority >= 0 {
            fs::create_dir_all(&prepend_dir)?;
            prepend_dir.join(format!("{}-{}", priority, name))
        } else {
            fs::create_dir_all(&append_dir)?;
            append_dir.join(format!("{}-{}", -priority, name))
        };

        // Remove existing symlink if it exists
        if symlink_path.exists() {
            fs::remove_file(&symlink_path)?;
        }

        // Create new symlink
        symlink(&ebin_path, &symlink_path)?;

        // Update and save environment config
        let mut env_config = env_config.clone();
        env_config.register_to_path = true;
        env_config.register_priority = priority;
        self.env_config.insert(name.to_string(), env_config.clone());
        self.save_env_config(&name)?;

        self.update_path()?;
        println!("# Environment '{}' has been registered with priority {}.", name, priority);
        Ok(())
    }

    pub fn unregister_environment(&mut self, name: &str) -> Result<()> {
        // Check if already unregistered
        let env_config = self.get_env_config(name.to_string())?;
        if !env_config.register_to_path {
            println!("# Environment '{}' is not registered.", name);
            return Ok(());
        }

        // Remove symlinks from both prepend and append directories
        let prepend_pattern = dirs().home_config.join(format!("path.d/prepend/*-{}", name));
        let append_pattern = dirs().home_config.join(format!("path.d/append/*-{}", name));

        for pattern in &[prepend_pattern, append_pattern] {
            for path in glob::glob(pattern.to_str().unwrap())? {
                if let Ok(path) = path {
                    if path.exists() {
                        fs::remove_file(&path)?;
                    }
                }
            }
        }

        // Update and save environment config
        let mut env_config = env_config.clone();
        env_config.register_to_path = false;
        env_config.register_priority = 0;
        self.env_config.insert(name.to_string(), env_config.clone());
        self.save_env_config(&name)?;

        self.update_path()?;
        println!("# Environment '{}' has been unregistered.", name);
        Ok(())
    }

    /// Get list of registered environment names from path.d directory structure
    ///
    /// This function parses the path.d directory structure to extract environment names
    /// from symlinks in both prepend/ and append/ directories. The names are extracted
    /// from symlink names that follow the pattern PRIORITY-NAME.
    ///
    /// Example structure:
    /// .epkg/config/path.d/
    /// ├── prepend/
    /// │   ├── 10-main -> ~/.epkg/envs/main/usr/ebin
    /// │   └── 20-debian12 -> ~/.epkg/envs/debian12/usr/ebin
    /// └── append/
    ///     └── 10-archlinux -> ~/.epkg/envs/archlinux/usr/ebin
    ///
    /// Returns a Vec of unique environment names found in both directories.
    pub fn get_registered_env_names(&self) -> Result<Vec<String>> {
        let mut env_names = std::collections::HashSet::new();

        // Helper function to extract env name from symlink name
        let extract_env_name = |name: &str| -> Option<String> {
            // Split on first hyphen to separate priority from name
            name.split_once('-')
                .map(|(_priority, name)| name.to_string())
        };

        // Helper function to process a directory
        let mut process_dir = |dir: &std::path::Path| -> Result<()> {
            if dir.exists() {
                for entry in fs::read_dir(dir)? {
                    let entry = entry?;
                    let name = entry.file_name()
                        .into_string()
                        .map_err(|_| anyhow::anyhow!("Invalid UTF-8 in filename"))?;
                    if let Some(env_name) = extract_env_name(&name) {
                        env_names.insert(env_name);
                    }
                }
            }
            Ok(())
        };

        // Process both directories
        process_dir(&dirs().home_config.join("path.d/prepend"))?;
        process_dir(&dirs().home_config.join("path.d/append"))?;

        // Convert HashSet to sorted Vec
        let mut result: Vec<String> = env_names.into_iter().collect();
        result.sort();
        Ok(result)
    }

    #[allow(dead_code)]
    pub fn list_generations(&mut self, name: &str) -> Result<()> {
        // Check if environment exists
        let env_root = self.get_env_root(name.to_string())?;
        if !env_root.exists() {
            return Err(anyhow::anyhow!("Environment does not exist: '{}'", name));
        }

        // Get generations directory
        let generations_root = self.get_generations_root(name)?;
        if !generations_root.exists() {
            return Err(anyhow::anyhow!("No generations found for environment: '{}'", name));
        }

        // Get current generation
        let current_id = self.get_current_generation_id()?;

        // List all generations
        let mut generations: Vec<u32> = fs::read_dir(&generations_root)?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let name = entry.file_name().into_string().ok()?;
                if name.chars().all(|c| c.is_digit(10)) {
                    name.parse::<u32>().ok()
                } else {
                    None
                }
            })
            .collect();

        // Sort numerically
        generations.sort();

        // Print table header
        println!("{:<12}  {:<10}  {}", "Generation", "Status", "Command");
        println!("{}", "-".repeat(60));

        // Print each generation with its status
        for gen in generations {
            let gen_dir = generations_root.join(gen.to_string());
            let command_file = gen_dir.join("command.json");

            // Try to read command from command.json
            let command = if command_file.exists() {
                match fs::read_to_string(&command_file) {
                    Ok(content) => {
                        // Simple extraction, in real code you'd use serde
                        content.lines()
                            .find(|line| line.contains("\"command\""))
                            .unwrap_or("")
                            .trim()
                            .replace("\"command\":", "")
                            .replace("\"", "")
                            .trim()
                            .to_string()
                    },
                    Err(_) => "unknown".to_string()
                }
            } else {
                "unknown".to_string()
            };

            let status = if gen == current_id {
                "current"
            } else {
                ""
            };

            println!("{:<12}  {:<10}  {}", gen, status, command);
        }

        Ok(())
    }

    pub fn export_environment(&mut self, name: &str, output: Option<String>) -> Result<()> {
        // Get environment config and channel config first
        let env_config = self.get_env_config(name.to_string())?.clone();
        let generations_root = self.get_generations_root(name)?;

        // Get installed packages
        let current_gen = fs::read_link(generations_root.join("current"))?;
        let installed_packages_path = current_gen.join("installed-packages.json");
        let mut env_config = env_config;

        if installed_packages_path.exists() {
            let contents = fs::read_to_string(&installed_packages_path)?;
            env_config.installed_packages = serde_json::from_str(&contents)?;
        } else {
            warn!("No installed packages found for environment '{}' at {}", name, installed_packages_path.display());
            return Err(anyhow::anyhow!("No installed packages found for environment '{}' at {}", name, installed_packages_path.display()));
        }

        // Serialize each config separately
        let channel_config = self.get_channel_config(name.to_string())?;
        let channel_yaml = serde_yaml::to_string(&channel_config)?;
        let env_yaml = serde_yaml::to_string(&env_config)?;

        // Skip leading "---" if present
        let channel_yaml = if channel_yaml.starts_with("---\n") {
            &channel_yaml[4..]
        } else {
            &channel_yaml
        };

        // Combine into single YAML document
        let combined_yaml = format!("{}\n{}\n",
            env_yaml, channel_yaml);

        // Write to file or stdout
        if let Some(output_path) = output {
            fs::write(&output_path, combined_yaml)?;
            println!("Environment configuration exported to {}", output_path);
        } else {
            println!("{}", combined_yaml);
        }

        Ok(())
    }

    /// Get environment configuration value
    pub fn get_environment_config(&mut self, name: &str) -> Result<()> {
        let env_name = config().common.env.clone();
        let config = self.get_env_config(env_name)?;

        // Split name by dots to handle nested fields
        let parts: Vec<&str> = name.split('.').collect();
        let mut current = serde_yaml::to_value(&config)?;

        for part in parts {
            current = current.get(part)
                .ok_or_else(|| anyhow::anyhow!("Configuration key not found: {}", name))?
                .clone();
        }

        println!("{:?}", current);
        Ok(())
    }

    /// Set environment configuration value
    pub fn set_environment_config(&mut self, name: &str, value: &str) -> Result<()> {
        let env_name = config().common.env.clone();
        let _config = self.get_env_config(env_name.clone())?; // load from file

        // Split name by dots to handle nested fields
        let parts: Vec<&str> = name.split('.').collect();

        // Validate that we're only setting top-level fields
        if parts.len() != 1 {
            return Err(anyhow::anyhow!("Can only set top-level configuration keys"));
        }

        // Get a mutable reference to the config
        let config = self.env_config.get_mut(&env_name)
            .ok_or_else(|| anyhow::anyhow!("Environment not found: {}", env_name))?;

        // Set the value directly on config
        match parts[0] {
            "name" => config.name = value.to_string(),
            "env_base" => config.env_base = value.to_string(),
            "env_root" => config.env_root = value.to_string(),
            "public" => config.public = value.parse()?,
            "register_to_path" => config.register_to_path = value.parse()?,
            "register_priority" => config.register_priority = value.parse()?,
            _ => return Err(anyhow::anyhow!("Unknown configuration key: {}", parts[0]))
        }

        // Save the updated config
        // self.env_config.insert(env_name.clone(), config.clone());
        self.save_env_config(&env_name)?;

        Ok(())
    }
}
