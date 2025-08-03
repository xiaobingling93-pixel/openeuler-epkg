use serde_json;
use serde_yaml;
use log;
use std::fs;
use std::env;
use std::path::PathBuf;
use std::collections::HashMap;
use color_eyre::eyre::{self, Result, WrapErr};
use crate::dirs::*;
use crate::models::{self, *};

/// Deserialize environment configuration from disk
pub fn deserialize_env_config() -> Result<EnvConfig> {
    let env_name = config().common.env.clone();
    deserialize_env_config_for(env_name)
}

pub fn deserialize_env_config_for(env_name: String) -> Result<EnvConfig> {
    let config_path = crate::dirs::find_env_config_path(&env_name)
        .ok_or_else(|| eyre::eyre!("Environment config not found for: {}", env_name))?;
    let (env_config, _): (EnvConfig, _) = read_yaml_file(&config_path)?;
    Ok(env_config)
}

/// Get environment configuration (simplified API)
#[allow(dead_code)]
pub fn get_env_config() -> Result<EnvConfig> {
    Ok(env_config().clone())
}

pub fn set_channel_config_defaults(cc: &mut ChannelConfig) -> Result<()> {
    // Set default architecture if missing
    if cc.arch.is_empty() {
        cc.arch = config().common.arch.clone();
    }

    // Handle the data dependencies between channel, distro, and version
    resolve_channel_distro_version(cc)?;

    Ok(())
}

fn process_channel_config(mut channel_config: ChannelConfig) -> Result<ChannelConfig> {
    set_channel_config_defaults(&mut channel_config)?;
    expand_channel_config_urls(&mut channel_config)?;

    // Sort distro_dirs by length once during deserialization
    channel_config.distro_dirs.sort_by(|a, b| a.len().cmp(&b.len()));

    // If distro_dirs contains the distro, it's a distro config, move it to the end so that
    // resolve_mirror_path() will use the distro name as local_subdir
    if channel_config.distro_dirs.contains(&channel_config.distro) {
        // Remove the distro from its current position
        channel_config.distro_dirs.retain(|d| d != &channel_config.distro);
        // Add it to the end
        channel_config.distro_dirs.push(channel_config.distro.clone());
    }

    Ok(channel_config)
}

pub fn read_yaml_file<T>(file_path: &std::path::Path) -> Result<(T, String)>
where
    T: serde::de::DeserializeOwned,
{
    let contents = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;
    let config: T = serde_yaml::from_str(&contents)
        .with_context(|| format!("Failed to parse YAML from file: {}", file_path.display()))?;
    Ok((config, contents))
}

pub fn load_and_process_channel_config(file_path: &std::path::Path, channel_configs: &mut Vec<ChannelConfig>, record_file_info: bool) -> Result<()> {
    let (mut channel_config, contents): (ChannelConfig, String) = read_yaml_file(file_path)?;

    if record_file_info {
        // Set the original file name
        let file_name = file_path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| "unknown")
            .to_string();

        channel_config.file_data = format!("file_name: {}\n{}", file_name, contents);
        channel_config.file_name = Some(file_name);
    }

    let processed_config = process_channel_config(channel_config)?;
    channel_configs.push(processed_config);
    Ok(())
}

fn resolve_channel_distro_version(cc: &mut ChannelConfig) -> Result<()> {
    // Step 1: If channel is provided, try to extract distro and version from it
    if !cc.channel.is_empty() {
        let parts: Vec<&str> = cc.channel.split(':').collect();
        if parts.len() == 2 {
            if cc.distro.is_empty() {
                cc.distro = parts[0].to_string();
            }
            if cc.version.is_empty() {
                cc.version = parts[1].to_string();
            }
        }
    }

    // Step 2: If version is still empty, fall back to versions list
    if cc.version.is_empty() {
        let version_from_list = cc.versions.first()
            .ok_or_else(|| eyre::eyre!("channel has no versions"))?;

        let version = version_from_list.split_whitespace().next()
            .ok_or_else(|| eyre::eyre!("malformed version string: {}", version_from_list))?;

        cc.version = version.to_string();
    }

    // Step 3: If channel is empty, construct it from distro:version
    if cc.channel.is_empty() {
        if !cc.distro.is_empty() && !cc.version.is_empty() {
            cc.channel = format!("{}:{}", cc.distro, cc.version);
        }
    }

    // Step 4: Set default app_version from app_versions if empty
    if cc.app_version.is_empty() {
        if let Some(app_version_from_list) = cc.app_versions.first() {
            let app_version = app_version_from_list.split_whitespace().next()
                .ok_or_else(|| eyre::eyre!("malformed app_version string: {}", app_version_from_list))?;
            cc.app_version = app_version.to_string();
        }
    }

    // Step 5: Validate that all required fields are now set
    if cc.channel.is_empty() {
        return Err(eyre::eyre!("channel name could not be determined"));
    }
    if cc.distro.is_empty() {
        return Err(eyre::eyre!("distro name could not be determined"));
    }
    if cc.version.is_empty() {
        return Err(eyre::eyre!("version could not be determined"));
    }

    Ok(())
}

pub fn expand_channel_config_urls(cc: &mut ChannelConfig) -> Result<()> {
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
            interpolated_urls.push((repo_name.clone(), "index_url", interpolate_index_url(cc, repo_name, url)?));
        }
        if let Some(url) = &repo_config.index_url_updates {
            interpolated_urls.push((repo_name.clone(), "index_url_updates", interpolate_index_url(cc, repo_name, url)?));
        }
        if let Some(url) = &repo_config.index_url_security {
            interpolated_urls.push((repo_name.clone(), "index_url_security", interpolate_index_url(cc, repo_name, url)?));
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

/// Deserialize channel configuration from disk
pub fn deserialize_channel_config() -> Result<Vec<ChannelConfig>> {
    let env_config = models::env_config();
    let env_root = PathBuf::from(&env_config.env_root);

    let mut channel_configs = Vec::new();

    // Load main channel config
    let file_path = env_root.join("etc/epkg/channel.yaml");
    load_and_process_channel_config(&file_path, &mut channel_configs, false)?;

    // Load additional configs from repos.d
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
                load_and_process_channel_config(&path, &mut channel_configs, false)?;
            }
        }
    }

    log::trace!("channel_configs {:#?}", channel_configs);

    Ok(channel_configs)
}

// Replace variables in the index_url string with actual values
// Examples:
// input:  $mirror/debian/dists/$VERSION/Release
// output: https://mirrors.huaweicloud.com///debian/dists/TRIXIE/contrib/Release
//
// Variables:
// - $mirror: the top priority mirror that supports the distribution
// - $VERSION: the upper case version string
// - $version_integer: the version string with non-numeric characters stripped
// - $version: the version string
// - $repo: the repository name
// - $arch: the architecture name
// - $app_version: the app_version string
pub fn interpolate_index_url(config: &ChannelConfig, repo_name: &str, index_url: &str) -> Result<String> {
    // Keep $mirror placeholder for later resolution in download functions
    let mut url = index_url.to_string();

    // Strip non-numeric characters from config.version for $version_integer
    let version_integer: String = config.version.chars().filter(|c| c.is_ascii_digit()).collect();
    url = url.replace("$version_integer", &version_integer);

    // Replace other variables but keep $mirror for download-time resolution
    url = url.replace("$VERSION", &config.version.to_uppercase());
    url = url.replace("$version", &config.version);
    url = url.replace("$repo", repo_name);
    url = url.replace("$arch", &config.arch);
    url = url.replace("$app_version", &config.app_version);

    Ok(url)
}

/// Save environment configuration to file
pub fn serialize_env_config(env_config: EnvConfig) -> Result<()> {
    let config_path = get_env_config_path(&env_config);

    // Serialize the EnvConfig to YAML
    let yaml = serde_yaml::to_string(&env_config)
        .with_context(|| format!("Failed to serialize environment config to YAML"))?;

    // Ensure the parent directory exists before writing the file
    if let Some(parent_dir) = config_path.parent() {
        fs::create_dir_all(parent_dir)
            .with_context(|| format!("Failed to create directory for environment config: {}", parent_dir.display()))?;
    }

    // Write the YAML to the file
    fs::write(&config_path, yaml)
        .with_context(|| format!("Failed to write environment config to file: {}", config_path.display()))?;

    Ok(())
}

impl PackageManager {

    pub fn read_installed_packages(&mut self, env: &str, generation_id: u32) -> Result<HashMap<String, InstalledPackageInfo>> {
        let generations_root = get_generations_root(env)?;
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

    /// Edit environment configuration file
    pub fn edit_environment_config(&self) -> Result<()> {
        let env_config = crate::models::env_config();
        let config_path = get_env_config_path(&env_config);

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
