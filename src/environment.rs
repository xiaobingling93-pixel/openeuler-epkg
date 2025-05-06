use std::fs;
use std::os::unix::fs::symlink;
use std::env;
use anyhow::Result;
use crate::models::*;
use std::path::Path;

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

impl PackageManager {

    /// Get list of all environment names except 'common'
    ///
    /// This function lists all environment directories in the epkg_envs_root
    /// directory, excluding the special 'common' environment.
    ///
    /// Returns a Vec of environment names.
    pub fn get_all_env_names(&self) -> Result<Vec<String>> {
        // Note: This method still uses private_envs directly since we need to
        // list all available environments from the directory, not access a specific one
        // Note: In the future, this could be refactored to not depend on the directory structure
        let all_envs: Vec<String> = fs::read_dir(&self.dirs.private_envs)?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let name = entry.file_name().into_string().ok()?;
                if name != "common" {
                    Some(name)
                } else {
                    None
                }
            })
            .collect();

        Ok(all_envs)
    }

    pub fn list_environments(&self) -> Result<()> {

        // Get all environments except common
        let all_envs = self.get_all_env_names()?;
        let registered_envs: Vec<String> = self.get_registered_env_names()?;
        let active_env = env::var("EPKG_ACTIVE_ENV").ok();

        // Print table header
        println!("{:<15}  {:>20}", "Environment", "Status");
        println!("{}", "-".repeat(35));

        // Print each environment with its status
        for env in all_envs {
            let mut status = Vec::new();
            if Some(&env) == active_env.as_ref() {
                status.push("activated");
            }
            if registered_envs.contains(&env) {
                status.push("registered");
            }
            println!("{:<15}  {:>20}", env, status.join(","));
        }

        Ok(())
    }

    pub fn create_environment(&self, name: &str) -> Result<()> {
        // Create environment directory structure
        let env_root = self.get_env_root(name.to_string())?;

        // Create generations directory with first generation
        let generations_root = self.get_generations_root(name)?;
        let gen_1_dir = generations_root.join("1");

        if gen_1_dir.join("installed-packages.json").exists() {
            return Err(anyhow::anyhow!("Environment already exists: '{}'", name));
        }

        // Create directories for generation 1
        fs::create_dir_all(&generations_root)?;
        fs::create_dir_all(env_root.join("usr/ebin"))?;
        fs::create_dir_all(env_root.join("usr/bin"))?;
        fs::create_dir_all(env_root.join("usr/sbin"))?;
        fs::create_dir_all(env_root.join("usr/lib"))?;
        fs::create_dir_all(env_root.join("usr/lib64"))?;
        fs::create_dir_all(env_root.join("etc/epkg"))?;
        fs::create_dir_all(env_root.join("var"))?;

        // Create symlinks in generation 1
        symlink("usr/bin", env_root.join("ebin"))?;
        symlink("usr/bin", env_root.join("bin"))?;
        symlink("usr/sbin", env_root.join("sbin"))?;
        symlink("usr/lib", env_root.join("lib"))?;
        symlink("usr/lib64", env_root.join("lib64"))?;

        // Create "current" symlink in generations directory pointing to generation 1
        symlink("1", generations_root.join("current"))?;

        // Initialize channel
        let channel = self.options.channel.clone().unwrap_or("openeuler:24.03-lts".to_string());
        let common_env_root = self.get_env_root("common".to_string())?;
        let src_channel_yaml = common_env_root.join("opt/epkg-manager/channel").join(format!("{}.yaml", channel));
        let env_channel_yaml = env_root.join("etc/epkg/channel.yaml");

        if !src_channel_yaml.exists() {
            return Err(anyhow::anyhow!("Channel not found: '{}'", channel));
        }

        fs::copy(src_channel_yaml, env_channel_yaml)?;
        fs::copy("/etc/resolv.conf", env_root.join("etc/resolv.conf"))?;

        // Create metadata files
        let installed_packages = gen_1_dir.join("installed-packages.json");
        fs::write(installed_packages, "{\n}")?;

        // Create command.json to record the creation command
        let command_json = gen_1_dir.join("command.json");
        fs::write(command_json, "{\n  \"command\": \"epkg env create\",\n  \"timestamp\": \"0\"\n}")?;

        println!("Environment '{}' has been created in {}", name, env_root.display());
        Ok(())
    }

    pub fn remove_environment(&self, name: &str) -> Result<()> {
        // Validate environment name
        if name == "common" || name == "main" {
            return Err(anyhow::anyhow!("Environment cannot be removed: '{}'", name));
        }

        // Check if environment exists
        let env_path = self.get_env_root(name.to_string())?;
        if !env_path.exists() {
            return Err(anyhow::anyhow!("Environment does not exist: '{}'", name));
        }

        // Unregister if registered
        self.unregister_environment(name)?;

        // Deactivate if this is the active environment
        if let Ok(active_env) = env::var("EPKG_ACTIVE_ENV") {
            if active_env == name {
                self.deactivate_environment()?;
            }
        }

        // Rename to hide environment
        let hidden_path = self.dirs.private_envs.join(format!(".{}", name));
        fs::rename(env_path, hidden_path)?;

        println!("Environment '{}' has been removed.", name);
        Ok(())
    }

    pub fn activate_environment(&self, name: &str) -> Result<()> {
        // Validate environment name
        if name == "common" {
            return Err(anyhow::anyhow!("Environment 'common' cannot be activated"));
        }

        // Check if environment exists
        if !self.get_env_root(name.to_string())?.exists() {
            return Err(anyhow::anyhow!("Environment not exist: '{}'", name));
        }

        // Update environment variables EPKG_ACTIVE_ENV and PATH
        // For eval by caller shell.
        println!("# Activate environment '{}'{}", name, if self.options.pure { " in pure mode" } else { "" });
        println!("export EPKG_ACTIVE_ENV={}", name);

        env::set_var("EPKG_ACTIVE_ENV", name);
        self.update_path(self.options.pure)?;

        Ok(())
    }

    pub fn deactivate_environment(&self) -> Result<()> {
        if let Ok(active_env) = env::var("EPKG_ACTIVE_ENV") {
            // Update environment variables EPKG_ACTIVE_ENV and PATH
            // For eval by caller shell.
            println!("# Deactivate environment '{}'", active_env);
            println!("unset EPKG_ACTIVE_ENV");
            env::remove_var("EPKG_ACTIVE_ENV");
            self.update_path(false)?;
        }
        Ok(())
    }

    pub fn register_environment(&self, name: &str) -> Result<()> {
        // Validate environment name
        if name == "common" {
            return Err(anyhow::anyhow!("Environment 'common' cannot be registered"));
        }

        // Check if environment exists
        let env_path = self.dirs.private_envs.join(name);
        if !env_path.exists() {
            return Err(anyhow::anyhow!("Environment does not exist: '{}'", name));
        }

        // Create path.d directories if they don't exist
        let prepend_dir = self.dirs.home_config.join("path.d/prepend");
        let append_dir = self.dirs.home_config.join("path.d/append");

        // Get priority from options or auto-detect
        let priority = if let Some(priority) = self.options.priority {
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
        let ebin_path = env_path.join("usr/ebin");
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

        println!("Environment '{}' has been registered with priority {}.", name, priority);
        Ok(())
    }

    pub fn unregister_environment(&self, name: &str) -> Result<()> {
        // Validate environment name
        if name == "common" {
            return Err(anyhow::anyhow!("Environment 'common' cannot be unregistered"));
        }

        // Remove symlinks from both prepend and append directories
        let glob_pattern = self.dirs.home_config.join(format!("path.d/{{prepend,append}}/*-{}*", name));
        for path in glob::glob(glob_pattern.to_str().unwrap())? {
            if let Ok(path) = path {
                fs::remove_file(path)?;
            }
        }

        println!("Environment '{}' has been unregistered.", name);
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
        process_dir(&self.dirs.home_config.join("path.d/prepend"))?;
        process_dir(&self.dirs.home_config.join("path.d/append"))?;

        // Convert HashSet to sorted Vec
        let mut result: Vec<String> = env_names.into_iter().collect();
        result.sort();
        Ok(result)
    }

    /// Switch environment to a specific generation or rollback N generations
    pub fn switch_environment(&self, name: &str, generation: &str) -> Result<()> {
        // Validate environment name
        if name == "common" {
            return Err(anyhow::anyhow!("Environment 'common' cannot be switched"));
        }

        // Check if environment exists
        let env_root = self.get_env_root(name.to_string())?;
        if !env_root.exists() {
            return Err(anyhow::anyhow!("Environment does not exist: '{}'", name));
        }

        // Determine target generation
        let generations_root = self.get_generations_root(name)?;
        let current_symlink = generations_root.join("current");

        let target_generation = if generation.starts_with("-") {
            // Rollback N generations
            let rollback_count: i32 = generation[1..].parse()
                .map_err(|_| anyhow::anyhow!("Invalid rollback count: '{}'", generation))?;

            if rollback_count <= 0 {
                return Err(anyhow::anyhow!("Rollback count must be positive: '{}'", rollback_count));
            }

            // Read current generation
            let current_gen = fs::read_link(&current_symlink)?
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("Invalid current generation symlink"))?
                .to_string_lossy()
                .parse::<i32>()?;

            let target_gen = current_gen - rollback_count;
            if target_gen <= 0 {
                return Err(anyhow::anyhow!("Cannot rollback beyond generation 1"));
            }

            target_gen.to_string()
        } else {
            // Direct generation specification
            generation.to_string()
        };

        // Verify target generation exists
        let target_gen_dir = generations_root.join(&target_generation);
        if !target_gen_dir.exists() {
            return Err(anyhow::anyhow!("Generation {} does not exist", target_generation));
        }

        // Update current symlink
        if current_symlink.exists() {
            fs::remove_file(&current_symlink)?;
        }
        symlink(&target_generation, &current_symlink)?;

        println!("Environment '{}' switched to generation {}", name, target_generation);
        Ok(())
    }

    /// List available generations for an environment
    pub fn list_generations(&self, name: &str) -> Result<()> {
        // Validate environment name
        if name == "common" {
            return Err(anyhow::anyhow!("Environment 'common' has no generations"));
        }

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
        let current_gen = fs::read_link(generations_root.join("current"))
            .ok()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str().map(|s| s.to_string()));

        // List all generations
        let mut generations: Vec<String> = fs::read_dir(&generations_root)?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let name = entry.file_name().into_string().ok()?;
                if name.chars().all(|c| c.is_digit(10)) {
                    Some(name)
                } else {
                    None
                }
            })
            .collect();

        // Sort numerically
        generations.sort_by(|a, b| {
            a.parse::<i32>().unwrap_or(0).cmp(&b.parse::<i32>().unwrap_or(0))
        });

        // Print table header
        println!("{:<12}  {:<10}  {}", "Generation", "Status", "Command");
        println!("{}", "-".repeat(60));

        // Print each generation with its status
        for gen in generations {
            let gen_dir = generations_root.join(&gen);
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

            let status = if Some(&gen) == current_gen.as_ref() {
                "current"
            } else {
                ""
            };

            println!("{:<12}  {:<10}  {}", gen, status, command);
        }

        Ok(())
    }
}
