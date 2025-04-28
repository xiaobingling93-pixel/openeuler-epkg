use std::fs;
use std::os::unix::fs::symlink;
use std::env;
use anyhow::Result;
use crate::paths::instance;
use crate::models::*;

impl PackageManager {

    pub fn check_init(&self) -> Result<()> {
        let paths = &*instance;

        if !paths.epkg_envs_root.join("main").exists() {
            self.init()?;
        }

        Ok(())
    }

    pub fn init(&self) -> Result<()> {
        let paths = &*instance;

        // Check if already initialized
        if paths.epkg_envs_root.join("main").exists() {
            eprintln!("epkg was already initialized for user {}", env::var("USER")?);
            return Ok(());
        }

        // Create necessary directories
        fs::create_dir_all(&paths.epkg_config_dir.join("registered-envs"))?;

        // Create main environment
        self.create_environment("main")?;
        self.register_environment("main")?;

        eprintln!("Warning: For changes to take effect, close and re-open your current shell.");
        Ok(())
    }

    pub fn list_environments(&self) -> Result<()> {
        let paths = &*instance;

        // Get all environments except common
        let all_envs: Vec<String> = fs::read_dir(&paths.epkg_envs_root)?
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

        // Get registered environments
        let registered_envs: Vec<String> = fs::read_dir(&paths.epkg_config_dir.join("registered-envs"))?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                entry.file_name().into_string().ok()
            })
            .collect();

        // Get active environment
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
        let paths = &*instance;

        // Validate environment name
        if name == "common" {
            return Err(anyhow::anyhow!("Environment 'common' cannot be created"));
        }

        // Check if environment already exists
        if paths.epkg_envs_root.join(name).exists() {
            return Err(anyhow::anyhow!("Environment '{}' already exists", name));
        }

        // Create environment directory structure
        let env_root = paths.epkg_envs_root.join(name);
        let profile_1 = env_root.join("profile-1");

        // Create directories
        fs::create_dir_all(profile_1.join("usr/ebin"))?;
        fs::create_dir_all(profile_1.join("usr/bin"))?;
        fs::create_dir_all(profile_1.join("usr/sbin"))?;
        fs::create_dir_all(profile_1.join("usr/lib"))?;
        fs::create_dir_all(profile_1.join("usr/lib64"))?;
        fs::create_dir_all(profile_1.join("etc"))?;
        fs::create_dir_all(profile_1.join("var"))?;

        // Create symlinks
        symlink("usr/bin", profile_1.join("bin"))?;
        symlink("usr/sbin", profile_1.join("sbin"))?;
        symlink("usr/lib", profile_1.join("lib"))?;
        symlink("usr/lib64", profile_1.join("lib64"))?;

        // Link profile-current to profile-1
        symlink("profile-1", env_root.join("profile-current"))?;

        // Copy resolv.conf
        fs::copy("/etc/resolv.conf", profile_1.join("etc/resolv.conf"))?;

        // Initialize channel
        let channel = self.options.channel.clone().unwrap_or("openEuler-24.03-LTS".to_string());
        let channel_yaml = paths.epkg_cache.join("epkg-manager/channel")
            .join(format!("{}-channel.yaml", channel));

        if !channel_yaml.exists() {
            return Err(anyhow::anyhow!("Channel '{}' not found", channel));
        }

        let env_channel_yaml = env_root.join("profile-current/etc/epkg/channel.yaml");
        fs::create_dir_all(env_channel_yaml.parent().unwrap())?;
        fs::copy(channel_yaml, env_channel_yaml)?;

        // Create empty installed-packages.json
        let installed_packages = env_root.join("profile-current/installed-packages.json");
        fs::write(installed_packages, "{\n}")?;

        println!("Environment '{}' has been created.", name);
        Ok(())
    }

    pub fn remove_environment(&self, name: &str) -> Result<()> {
        let paths = &*instance;

        // Validate environment name
        if name == "common" || name == "main" {
            return Err(anyhow::anyhow!("Environment '{}' cannot be removed", name));
        }

        // Check if environment exists
        let env_path = paths.epkg_envs_root.join(name);
        if !env_path.exists() {
            return Err(anyhow::anyhow!("Environment '{}' does not exist", name));
        }

        // Unregister if registered
        if paths.epkg_config_dir.join("registered-envs").join(name).exists() {
            self.unregister_environment(name)?;
        }

        // Deactivate if this is the active environment
        if let Ok(active_env) = env::var("EPKG_ACTIVE_ENV") {
            if active_env == name {
                self.deactivate_environment()?;
            }
        }

        // Rename to hide environment
        let hidden_path = paths.epkg_envs_root.join(format!(".{}", name));
        fs::rename(env_path, hidden_path)?;

        println!("Environment '{}' has been removed.", name);
        Ok(())
    }

    pub fn activate_environment(&self, name: &str) -> Result<()> {
        let paths = &*instance;

        // Validate environment name
        if name == "common" {
            return Err(anyhow::anyhow!("Environment 'common' cannot be activated"));
        }

        // Check if environment exists
        if !paths.epkg_envs_root.join(name).exists() {
            return Err(anyhow::anyhow!("Environment '{}' does not exist", name));
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
        let paths = &*instance;

        // Validate environment name
        if name == "common" {
            return Err(anyhow::anyhow!("Environment 'common' cannot be registered"));
        }

        // Check if environment exists
        let env_path = paths.epkg_envs_root.join(name);
        if !env_path.exists() {
            return Err(anyhow::anyhow!("Environment '{}' does not exist", name));
        }

        // Check if environment is already registered
        let registered_path = paths.epkg_config_dir.join("registered-envs").join(name);
        if registered_path.exists() {
            // If it's a symlink, check if it points to the correct location
            if registered_path.is_symlink() {
                if let Ok(target) = fs::read_link(&registered_path) {
                    if target == env_path {
                        return Err(anyhow::anyhow!("Environment '{}' is already registered", name));
                    }
                }
            }
            // Remove existing symlink or file
            fs::remove_file(&registered_path)?;
        }

        // Ensure registered-envs directory exists
        fs::create_dir_all(registered_path.parent().unwrap())?;

        // Create symlink
        symlink(&env_path, &registered_path)?;

        println!("Environment '{}' has been registered.", name);
        Ok(())
    }

    pub fn unregister_environment(&self, name: &str) -> Result<()> {
        let paths = &*instance;

        // Validate environment name
        if name == "common" {
            return Err(anyhow::anyhow!("Environment 'common' cannot be unregistered"));
        }

        // Check if environment exists and is registered
        let registered_path = paths.epkg_config_dir.join("registered-envs").join(name);
        if !registered_path.exists() {
            return Err(anyhow::anyhow!("Environment '{}' is not registered", name));
        }

        // Remove symlink
        fs::remove_file(registered_path)?;

        println!("Environment '{}' has been unregistered.", name);
        Ok(())
    }

}
