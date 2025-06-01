
use serde_json;
use serde_yaml;
use std::fs;
use std::env;

use std::path::PathBuf;
use std::collections::HashMap;
use color_eyre::eyre::{self, bail, Result, WrapErr};
use crate::dirs::*;
use crate::models::*;
use log;

pub fn load_package_json(file_path: &str) -> Result<Package> {
    let contents = fs::read_to_string(&file_path)
        .with_context(|| format!("Failed to read file: {}", file_path))?;

    let package: Package = serde_json::from_str(&contents)
        .with_context(|| format!("Failed to parse JSON from file: {}", file_path))?;

    Ok(package)
}

// Function to parse a pkgline into a PackageSpec
fn parse_package_line(pkgline: &str, reponame: &str) -> Result<PackageSpec> {
    let parts: Vec<&str> = pkgline.split("__").collect();
    if parts.len() != 4 {
        bail!("Invalid package line format: {}", pkgline);
    }

    let spec = PackageSpec {
        repo: reponame.to_string(),
        hash: parts[0].to_string(),
        name: parts[1].to_string(),
        version: parts[2].to_string(),
        release: parts[3].to_string(),
    };
    Ok(spec)
}

/// Load channel/mirrors.yaml
#[allow(dead_code)]
pub fn load_mirrors() -> Result<HashMap<String, Mirror>> {
    let file_path = get_epkg_manager_path()?.join("channel/mirrors.yaml");
    let contents = fs::read_to_string(&file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

    serde_yaml::from_str(&contents)
        .with_context(|| format!("Failed to parse YAML from file: {}", file_path.display()))
}

impl PackageManager {

    /// Load environment configuration from in-memory hash or on-disk file
    pub fn get_env_config(&mut self, env_name: String) -> Result<&EnvConfig> {
        if self.envs_config.contains_key(&env_name) {
            return Ok(&self.envs_config[&env_name]);
        }

        let config_path = get_env_config_path(&env_name);

        // Read the file contents
        let contents = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read file: {}", config_path.display()))?;

        // Deserialize the YAML into EnvConfig
        let env_config: EnvConfig = serde_yaml::from_str(&contents)
            .with_context(|| format!("Failed to parse YAML from file: {}", config_path.display()))?;

        self.envs_config.insert(env_name.clone(), env_config);

        Ok(&self.envs_config[&env_name])
    }

    /// On-demand load channel/mirrors.yaml to self.mirrors
    #[allow(dead_code)]
    pub fn get_mirrors(&mut self) -> Result<&HashMap<String, Mirror>> {
        if self.mirrors.is_empty() {
            self.mirrors = load_mirrors()?;
        }
        Ok(&self.mirrors)
    }

    fn set_channel_config_defaults(&mut self, cc: &mut ChannelConfig) -> Result<()> {
        if cc.arch.is_empty() {
            cc.arch = config().common.arch.clone();
        }

        if cc.channel.is_empty() {
            cc.channel = format!("{}:{}", cc.distro, cc.version);
        } else if cc.distro.is_empty() || cc.version.is_empty() {
            let parts: Vec<&str> = cc.channel.split(':').collect();
            if parts.len() == 2 {
                cc.distro = parts[0].to_string();
                cc.version = parts[1].to_string();
            }
        }
        if cc.version.is_empty() {
            if let Some(v0) = cc.versions.first() {
                let mut parts = v0.split_whitespace();
                if let Some(version) = parts.next() {
                    cc.version = version.to_string();
                } else {
                    bail!("malformed version string: {}", v0);
                }
            } else {
                bail!("channel has no versions");
            }
        }
        Ok(())
    }

    fn expand_channel_config_urls(&mut self, cc: &mut ChannelConfig) -> Result<()> {
        // First pass: set default URLs from channel config
        for (_, repo_config) in &mut cc.repos {
            if repo_config.index_url.is_none() {
                repo_config.index_url = Some(cc.index_url.clone());
            }
            if repo_config.index_url_updates.is_none() {
                repo_config.index_url_updates = cc.index_url_updates.clone();
            }
            if repo_config.index_url_security.is_none() {
                repo_config.index_url_security = cc.index_url_security.clone();
            }
        }

        // Second pass: interpolate URLs
        let mut interpolated_urls = Vec::new();
        for (repo_name, repo_config) in &cc.repos {
            if let Some(url) = &repo_config.index_url {
                interpolated_urls.push((repo_name.clone(), "index_url", self.interpolate_index_url(cc, repo_name, url)?));
            }
            if let Some(url) = &repo_config.index_url_updates {
                interpolated_urls.push((repo_name.clone(), "index_url_updates", self.interpolate_index_url(cc, repo_name, url)?));
            }
            if let Some(url) = &repo_config.index_url_security {
                interpolated_urls.push((repo_name.clone(), "index_url_security", self.interpolate_index_url(cc, repo_name, url)?));
            }
        }

        // Third pass: update the URLs
        for (repo_name, url_type, interpolated_url) in interpolated_urls {
            if let Some(repo_config) = cc.repos.get_mut(&repo_name) {
                match url_type {
                    "index_url" => repo_config.index_url = Some(interpolated_url),
                    "index_url_updates" => repo_config.index_url_updates = Some(interpolated_url),
                    "index_url_security" => repo_config.index_url_security = Some(interpolated_url),
                    _ => unreachable!(),
                }
            }
        }

        Ok(())
    }

    pub fn get_channel_config(&mut self, env_name: String) -> Result<&ChannelConfig> {
        if self.channels_config.contains_key(&env_name) {
            return Ok(&self.channels_config[&env_name]);
        }

        let env_root = self.get_env_root(env_name.clone())?;
        let file_path = env_root.join("etc/epkg/channel.yaml");
        let contents = fs::read_to_string(&file_path)
            .with_context(|| format!("Failed to read file: {}", file_path.display()))?;
        let mut channel_config: ChannelConfig = serde_yaml::from_str(&contents)
            .with_context(|| format!("Failed to parse YAML from file: {}", file_path.display()))?;

        // Load and merge additional configs from repos.d
        let repos_dir = env_root.join("etc/epkg/repos.d");
        if repos_dir.exists() {
            for entry in fs::read_dir(repos_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("yaml") {
                    let contents = fs::read_to_string(&path)
                        .with_context(|| format!("Failed to read file: {}", path.display()))?;
                    let repo_config: ChannelConfig = serde_yaml::from_str(&contents)
                        .with_context(|| format!("Failed to parse YAML from file: {}", path.display()))?;
                    self.merge_channel_configs(&mut channel_config, repo_config)?;
                }
            }
        }
        self.set_channel_config_defaults(&mut channel_config)?;
        self.expand_channel_config_urls(&mut channel_config)?;
        log::trace!("channel_config {:#?}", channel_config);
        self.channels_config.insert(env_name.clone(), channel_config);
        Ok(&self.channels_config[&env_name])
    }

    fn merge_channel_configs(&self, base: &mut ChannelConfig, additional: ChannelConfig) -> Result<()> {
        // Merge repos
        for (repo_name, mut repo_config) in additional.repos {
            if repo_config.index_url.is_none() {
                repo_config.index_url = Some(additional.index_url.clone());
            }
            if repo_config.index_url_updates.is_none() {
               if !additional.index_url_updates.is_none() {
                repo_config.index_url_updates = additional.index_url_updates.clone();
               } else {
                repo_config.index_url_updates = Some("".to_string());
               }
            }
            if repo_config.index_url_security.is_none() {
               if !additional.index_url_security.is_none() {
                repo_config.index_url_security = additional.index_url_security.clone();
               } else {
                repo_config.index_url_security = Some("".to_string());
               }
            }
            base.repos.insert(repo_name, repo_config);
        }

        // Merge other fields if they're not set in base
        if base.arch.is_empty() {
            base.arch = additional.arch;
        }
        if base.channel.is_empty() {
            base.channel = additional.channel;
        }
        if base.distro.is_empty() {
            base.distro = additional.distro;
        }
        if base.version.is_empty() {
            base.version = additional.version;
        }

        Ok(())
    }

    pub fn read_installed_packages(&mut self, env: &str, generation_id: u32) -> Result<HashMap<String, InstalledPackageInfo>> {
        let generations_root = self.get_generations_root(env)?;
        let file_path = generations_root.join(generation_id.to_string()).join("installed-packages.json");

        let contents = fs::read_to_string(&file_path)
            .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

        let packages: HashMap<String, InstalledPackageInfo> = serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse JSON from file: {}", file_path.display()))?;

        Ok(packages)
    }

    pub fn load_installed_packages(&mut self) -> Result<()> {
        let generation_id = self.get_current_generation_id()?;
        self.installed_packages = self.read_installed_packages(&config().common.env, generation_id)?;
        Ok(())
    }

    pub fn save_installed_packages(&mut self, new_generation: &PathBuf) -> Result<()> {
        // Construct the file path
        let file_path = new_generation.join("installed-packages.json");

        // Serialize the installed packages to JSON
        let json = serde_json::to_string_pretty(&self.installed_packages)?;

        // Write the JSON to the file
        fs::write(&file_path, json)?;

        if config().common.verbose {
            println!("Installed packages saved to: {}", file_path.display());
        }

        Ok(())
    }

    /// Save environment configuration to file
    pub fn save_env_config(&mut self, env_name: &str) -> Result<()> {
        let env_config = self.envs_config.get(env_name)
            .ok_or_else(|| eyre::eyre!("Environment config not found: {}", env_name))?;

        let config_path = get_env_config_path(env_name);

        // Serialize the EnvConfig to YAML
        let yaml = serde_yaml::to_string(env_config)
            .with_context(|| format!("Failed to serialize environment config to YAML"))?;

        // Write the YAML to the file
        fs::write(&config_path, yaml)
            .with_context(|| format!("Failed to write environment config to file: {}", config_path.display()))?;

        Ok(())
    }

    /// Edit environment configuration file
    pub fn edit_environment_config(&self) -> Result<()> {
        let env_name = &config().common.env;
        let config_path = get_env_config_path(env_name);

        // Open editor
        let editor = env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
        let status = std::process::Command::new(editor)
            .arg(&config_path)
            .status()?;

        if !status.success() {
            return Err(eyre::eyre!("Editor exited with non-zero status"));
        }

        Ok(())
    }

}
