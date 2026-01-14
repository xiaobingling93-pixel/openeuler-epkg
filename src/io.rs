use serde_json;
use serde_yaml;
use log;
use std::fs;
use std::env;
use std::path::PathBuf;
use std::collections::{HashMap, BTreeMap};
use std::sync::Arc;
use color_eyre::eyre::{self, Result, WrapErr};
use crate::dirs::*;
use crate::models::{self, *};
use crate::models::PACKAGE_CACHE;
use crate::history::get_current_generation_id;

pub const CHANNEL_SEPARATOR: char = '-';

/// Deserialize environment configuration from disk
#[allow(dead_code)] // quiet warning in cargo test calls
pub fn deserialize_env_config() -> Result<EnvConfig> {
    let env_name = config().common.env.clone();
    deserialize_env_config_for(env_name)
}

pub fn deserialize_env_config_for(env_name: String) -> Result<EnvConfig> {
    let config_path = crate::dirs::get_env_config_path(&env_name);

    // In tests, we often don't have a real on-disk environment; fall back to a
    // minimal default EnvConfig instead of failing hard when env.yaml is missing.
    #[cfg(test)]
    {
        if !config_path.exists() {
            let mut cfg = EnvConfig::default();
            cfg.name = env_name;
            // env_root/env_base can be left empty for solver tests, since they don't touch disk.
            return Ok(cfg);
        }
    }

    let env_config = read_yaml_file(&config_path)?;
    Ok(env_config)
}

/// Get environment configuration (simplified API)
#[allow(dead_code)]
pub fn get_env_config() -> Result<EnvConfig> {
    Ok(env_config().clone())
}

pub fn set_channel_config_defaults(cc: &mut ChannelConfig, main_config: Option<&ChannelConfig>) -> Result<()> {
    // Set default architecture if missing
    if cc.arch.is_empty() {
        cc.arch = config().common.arch.clone();
    }

    // Handle the data dependencies between channel, distro, and version
    resolve_channel_distro_version(cc, main_config)?;

    Ok(())
}

fn process_channel_config(mut channel_config: ChannelConfig, main_config: Option<&ChannelConfig>) -> Result<ChannelConfig> {
    set_channel_config_defaults(&mut channel_config, main_config)?;
    merge_channel_defaults_into_repos(&mut channel_config);
    interpolate_channel_urls(&mut channel_config);

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

pub fn read_yaml_file<T>(file_path: &std::path::Path) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let contents = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;
    let config: T = serde_yaml::from_str(&contents)
        .with_context(|| format!("Failed to parse YAML from file: {}", file_path.display()))?;
    Ok(config)
}

pub fn read_json_file<T>(file_path: &std::path::Path) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let contents = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;
    let value: T = serde_json::from_str(&contents)
        .with_context(|| format!("Failed to parse JSON from file: {}", file_path.display()))?;
    Ok(value)
}

pub fn load_and_process_channel_config(file_path: &std::path::Path, channel_configs: &mut Vec<ChannelConfig>, main_config: Option<&ChannelConfig>) -> Result<()> {
    let mut channel_config: ChannelConfig = read_yaml_file(file_path)?;

    let file_path_str = file_path.to_string_lossy().to_string();
    channel_config.file_path = file_path_str.clone();

    let processed_config = process_channel_config(channel_config, main_config)?;
    channel_configs.push(processed_config);
    Ok(())
}

fn resolve_channel_distro_version(cc: &mut ChannelConfig, main_config: Option<&ChannelConfig>) -> Result<()> {
    // Step 1: If channel is provided, try to extract distro and version from it
    if !cc.channel.is_empty() {
        if let Some((distro_part, version_part)) = cc.channel.split_once(CHANNEL_SEPARATOR) {
            if cc.distro.is_empty() {
                cc.distro = distro_part.to_string();
            }
            if cc.version.is_empty() {
                cc.version = version_part.to_string();
            }
        }
    }

    // Step 1.5: If distro is still empty and main config is provided, use main config's distro
    if cc.distro.is_empty() {
        if let Some(main_cfg) = main_config {
            cc.distro = main_cfg.distro.clone();
        }
    }

    // Step 2: If version is still empty, fall back to versions list
    if cc.version.is_empty() {
        let version_from_list = if let Some(main_cfg) = main_config {
            // When given main config, first search cc.versions for the matching one with main.version,
            // alias shall also be matched; then fall back to select cc.versions.first()
            cc.versions.iter()
                .find(|v| v.split_whitespace().any(|alias| alias == main_cfg.version))
                .or_else(|| cc.versions.first())
        } else {
            cc.versions.first()
        }.ok_or_else(|| eyre::eyre!("channel has no versions"))?;

        let version = version_from_list.split_whitespace().next()
            .ok_or_else(|| eyre::eyre!("malformed version string: {}", version_from_list))?;

        cc.version = version.to_string();
    }

    // Step 3: If channel is empty, construct it from distro:version
    if cc.channel.is_empty() {
        if !cc.distro.is_empty() && !cc.version.is_empty() {
            cc.channel = format!("{}{}{}", cc.distro, CHANNEL_SEPARATOR, cc.version);
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

    // Step 5: Warn about mismatches with main config if provided
    if let Some(main_cfg) = main_config {
        if cc.distro != main_cfg.distro {
            eprintln!("Extra repo config '{}' distro '{}' does not match main config distro '{}'", cc.file_path, cc.distro, main_cfg.distro);
        }
        if cc.version != main_cfg.version {
            eprintln!("Extra repo config '{}' version '{}' does not match main config version '{}'", cc.file_path, cc.version, main_cfg.version);
        }
    }

    // Step 6: Validate that all required fields are now set
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

/// Merge channel-level default URLs into repo configs where missing
pub fn merge_channel_defaults_into_repos(cc: &mut ChannelConfig) {
    for (_, repo_config) in &mut cc.repos {
        if repo_config.components.is_empty() {
            repo_config.components = cc.components.clone();
        }
        if repo_config.index_url.is_none() {
            repo_config.index_url = Some(cc.index_url.clone());
        }
        for (key, url) in &cc.amend_index_urls {
            if !repo_config.amend_index_urls.contains_key(key) {
                repo_config.amend_index_urls.insert(key.clone(), url.clone());
            }
        }
    }
}

/// Interpolate URL variables in channel configs with actual values
pub fn interpolate_channel_urls(cc: &mut ChannelConfig) {
    // Extract needed config values to avoid borrowing conflicts
    let config_version = cc.version.clone();
    let config_arch = cc.arch.clone();
    let config_app_version = cc.app_version.clone();

    let repo_names: Vec<String> = cc.repos.keys().cloned().collect();
    for repo_name in repo_names {
        if let Some(repo_config) = cc.repos.get_mut(&repo_name) {
            if let Some(url) = &repo_config.index_url {
                let interpolated_url = interpolate_index_url(
                    url, &config_version, &config_arch, &config_app_version, &repo_name
                );
                repo_config.index_url = Some(interpolated_url);
            }

            let mut interpolated_amend_urls = HashMap::new();
            for (key, url) in &repo_config.amend_index_urls {
                let interpolated_url = interpolate_index_url(
                    url, &config_version, &config_arch, &config_app_version, &repo_name
                );
                interpolated_amend_urls.insert(key.clone(), interpolated_url);
            }
            repo_config.amend_index_urls = interpolated_amend_urls;
        }
    }
}

/// Deserialize channel configuration from disk
#[allow(dead_code)] // quiet warning in cargo test calls
pub fn deserialize_channel_config() -> Result<Vec<ChannelConfig>> {
    let env_config = models::env_config();
    let env_root = PathBuf::from(&env_config.env_root);
    deserialize_channel_config_from_root(&env_root)
}

/// Deserialize channel configuration from a specific environment root
pub fn deserialize_channel_config_from_root(env_root: &PathBuf) -> Result<Vec<ChannelConfig>> {
    let mut channel_configs = Vec::new();

    // Load main channel config
    let file_path = env_root.join("etc/epkg/channel.yaml");
    load_and_process_channel_config(&file_path, &mut channel_configs, None)?;

    // Load additional configs from repos.d
    // Ideally the latter should all use the same cc.distro and cc.version as main config,
    // for now we allow users to mix for flexibility, and just emit warning on mismatch.
    let repos_dir = env_root.join("etc/epkg/repos.d");
    if repos_dir.exists() {
        // First collect all repo config paths
        let mut repo_paths = Vec::new();
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
                repo_paths.push(path);
            }
        }

        // Now get the main config and process repo configs
        let main_config = channel_configs.first().cloned();
        for path in repo_paths {
            load_and_process_channel_config(&path, &mut channel_configs, main_config.as_ref())?;
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
// - $version: the distro version string
// - $repo: the repository name
// - $arch: the architecture name
// - $app_version: the app_version string
// - $conda_arch: the conda-specific architecture name
// - $conda_repofile: the conda repodata file name based on repository
pub fn interpolate_index_url(
    index_url: &str,
    version: &str,
    arch: &str,
    app_version: &str,
    repo_name: &str,
) -> String {
    // Keep $mirror placeholder for later resolution in download functions
    let mut url = index_url.to_string();

    // Strip non-numeric characters from version for $version_integer
    let version_integer: String = version.chars().filter(|c| c.is_ascii_digit()).collect();
    url = url.replace("$version_integer", &version_integer);

    // Replace other variables but keep $mirror for download-time resolution
    url = url.replace("$VERSION", &version.to_uppercase());
    url = url.replace("$version", version);
    url = url.replace("$repo", repo_name);
    url = url.replace("$arch", arch);
    url = url.replace("$app_version", app_version);

    // Replace $conda_arch with conda-specific architecture name
    let conda_arch = map_to_conda_arch(arch);
    url = url.replace("$conda_arch", &conda_arch);

    // Replace $conda_repofile with conda-specific repodata file name
    let conda_repofile = map_to_conda_repofile(repo_name);
    url = url.replace("$conda_repofile", &conda_repofile);

    url
}

/// Map standard architecture names to conda-specific architecture names
///
/// Conda architecture names follow the pattern: {os}-{arch}
/// Examples:
/// - linux-64      (for x86_64 on Linux)
/// - linux-aarch64 (for aarch64 on Linux)
/// - osx-arm64     (for aarch64 on macOS)
/// - win-64        (for x86_64 on Windows)
fn map_to_conda_arch(arch: &str) -> String {
    // Detect the operating system
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else if cfg!(target_os = "windows") {
        "win"
    } else {
        "linux" // Default to linux for unknown OS
    };

    // Map architecture names to conda format
    let conda_arch = match arch {
        "x86_64" | "amd64" => "64",
        "aarch64" | "arm64" => "aarch64",
        "armv6l" => "armv6l",
        "armv7l" => "armv7l",
        "ppc64le" => "ppc64le",
        "i686" | "i386" => "32",
        _ => "64", // Default to 64-bit for unknown architectures
    };

    // Special handling for macOS ARM64
    if os == "osx" && (arch == "aarch64" || arch == "arm64") {
        "arm64".to_string()
    } else {
        format!("{}-{}", os, conda_arch)
    }
}

/// Map repository names to conda-specific repodata file names
///
/// Conda repositories use different repodata file formats:
/// - 'main' and 'conda-forge' use 'current_repodata.json.gz'
/// - Other repositories use 'repodata.json.bz2'
fn map_to_conda_repofile(repo_name: &str) -> String {
    match repo_name {
        "main" | "conda-forge" => "current_repodata.json.gz".to_string(),
        _ => "repodata.json.bz2".to_string(),
    }
}

/// Save environment configuration to file
pub fn serialize_env_config(env_config: EnvConfig) -> Result<()> {
    let config_path = get_env_config_path(&env_config.name);

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

pub fn read_installed_packages(env: &str, generation_id: u32) -> Result<InstalledPackagesMap> {
    let generations_root = get_generations_root(env)?;
    let file_path = generations_root.join(generation_id.to_string()).join("installed-packages.json");

    // If the installed-packages file doesn't exist (common in tests or very
    // new environments), treat it as an empty set of installed packages.
    if !file_path.exists() {
        return Ok(HashMap::new());
    }

    let packages_raw: HashMap<String, InstalledPackageInfo> = read_json_file(&file_path)?;
    let packages: InstalledPackagesMap = packages_raw.into_iter().map(|(k, v)| (k, Arc::new(v))).collect();
    Ok(packages)
}

pub fn load_installed_packages() -> Result<()> {
    // If installed_packages is already populated (e.g., in test mode), skip loading
    // This preserves test-set installed packages and avoids overwriting them
    if !PACKAGE_CACHE.installed_packages.read().unwrap().is_empty() {
        return Ok(());
    }
    let generation_id = get_current_generation_id()?;
    let packages = read_installed_packages(&config().common.env, generation_id)?;
    let mut installed = PACKAGE_CACHE.installed_packages.write().unwrap();
    for (k, v) in packages {
        installed.insert(k, v);
    }
    Ok(())
}

pub fn save_installed_packages(new_generation: &PathBuf) -> Result<()> {
    // Construct the file path
    let file_path = new_generation.join("installed-packages.json");

    // Convert HashMap to BTreeMap to ensure keys are sorted, dereferencing Arc for serialization
    let sorted_packages: BTreeMap<_, _> = PACKAGE_CACHE.installed_packages.read().unwrap().iter().map(|(k, v)| (k.clone(), (**v).clone())).collect();

    // Serialize the installed packages to JSON (keys will be in sorted order)
    let json = serde_json::to_string_pretty(&sorted_packages)?;

    // Write the JSON to the file
    fs::write(&file_path, json)?;

    if config().common.verbose {
        println!("Installed packages saved to: {}", file_path.display());
    }

    Ok(())
}

pub fn read_world(env: &str, generation_id: u32) -> Result<HashMap<String, String>> {
    let generations_root = get_generations_root(env)?;
    let file_path = generations_root.join(generation_id.to_string()).join("world.json");

    // If file doesn't exist, return empty map
    if !file_path.exists() {
        return Ok(HashMap::new());
    }

    let world: HashMap<String, String> = read_json_file(&file_path)?;
    Ok(world)
}

pub fn load_world() -> Result<()> {
    let generation_id = get_current_generation_id()?;
    let world = read_world(&config().common.env, generation_id)?;
    let mut cache_world = PACKAGE_CACHE.world.write().unwrap();
    cache_world.clear();
    for (k, v) in world {
        cache_world.insert(k, v);
    }
    Ok(())
}

pub fn save_world(new_generation: &PathBuf) -> Result<()> {
    // Construct the file path
    let file_path = new_generation.join("world.json");

    // Convert HashMap to BTreeMap to ensure keys are sorted
    let world = PACKAGE_CACHE.world.read().unwrap();
    let sorted_world: BTreeMap<_, _> = world.iter().collect();

    // Serialize the world to JSON (keys will be in sorted order)
    let json = serde_json::to_string_pretty(&sorted_world)?;

    // Write the JSON to the file
    fs::write(&file_path, json)?;

    if config().common.verbose {
        println!("World saved to: {}", file_path.display());
    }

    Ok(())
}

/// Edit environment configuration file
pub fn edit_environment_config() -> Result<()> {
    let env_config = crate::models::env_config();
    let config_path = get_env_config_path(&env_config.name);

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
