use serde::{Deserialize, Serialize};
use serde_json::{self, Value};
use serde_yaml;
use log;
use std::fs;
use std::env;
use std::path::{Path, PathBuf};
use std::collections::{HashMap, BTreeMap};
use std::sync::Arc;
use color_eyre::eyre::{self, Result, WrapErr};
use crate::dirs::*;
use crate::models::{self, *};
use crate::models::PACKAGE_CACHE;
use crate::history::get_current_generation_id;
use crate::lfs;

pub const CHANNEL_SEPARATOR: char = '-';

/// Helper struct for serializing/deserializing installed packages in array format
#[derive(Debug, Serialize, Deserialize)]
struct InstalledPackageEntry {
    /// Package key (pkgname__version__arch)
    pkgkey: String,
    /// Flattened InstalledPackageInfo fields
    #[serde(flatten)]
    info: InstalledPackageInfo,
}

/// Convert JSON Value to installed packages map (supports both object and array formats)
fn installed_packages_from_value(value: Value) -> Result<HashMap<String, InstalledPackageInfo>> {
    match value {
        Value::Object(obj) => {
            // Old format: map from pkgkey to InstalledPackageInfo
            let mut map = HashMap::new();
            for (pkgkey, val) in obj {
                let info: InstalledPackageInfo = serde_json::from_value(val)
                    .with_context(|| format!("Failed to deserialize package info for key: {}", pkgkey))?;
                map.insert(pkgkey, info);
            }
            Ok(map)
        }
        Value::Array(arr) => {
            // New format: array of InstalledPackageEntry
            let mut map = HashMap::new();
            for entry_val in arr {
                let entry: InstalledPackageEntry = serde_json::from_value(entry_val)
                    .with_context(|| "Failed to deserialize installed package entry")?;
                map.insert(entry.pkgkey, entry.info);
            }
            Ok(map)
        }
        _ => {
            Err(eyre::eyre!("Invalid installed-packages.json: expected object or array"))
        }
    }
}

/// Convert installed packages map to sorted array of entries
fn installed_packages_to_array(installed: &InstalledPackagesMap) -> Vec<InstalledPackageEntry> {
    let mut entries: Vec<InstalledPackageEntry> = installed.iter()
        .map(|(pkgkey, info)| InstalledPackageEntry {
            pkgkey: pkgkey.clone(),
            info: (**info).clone(),
        })
        .collect();

    // Sort by depend_depth then pkgkey
    entries.sort_by(|a, b| {
        a.info.depend_depth.cmp(&b.info.depend_depth)
            .then_with(|| a.pkgkey.cmp(&b.pkgkey))
    });
    entries
}

/// Deserialize environment configuration from disk
#[allow(dead_code)] // quiet warning in cargo test calls
pub fn deserialize_env_config() -> Result<EnvConfig> {
    let env_name = config().common.env_name.clone();
    deserialize_env_config_for(env_name)
}

pub fn deserialize_env_config_for(env_name: String) -> Result<EnvConfig> {
    if env_name.is_empty() {
        return Err(eyre::eyre!("Environment name is empty; cannot load environment config. This may indicate a bug in initialization order."));
    }
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

    // Check if environment exists
    if !config_path.exists() {
        return Err(eyre::eyre!("Environment config file not found: '{}'", config_path.display()));
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
            // alias shall also be matched; then fall back to select cc.versions.first(),
            // then fall back to main_cfg.version
            cc.versions.iter()
                .find(|v| v.split_whitespace().any(|alias| alias == main_cfg.version))
                .or_else(|| cc.versions.first())
                .or_else(|| Some(&main_cfg.version))
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
        if repo_config.index_url.is_empty() {
            repo_config.index_url = cc.index_url.clone();
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
            if !repo_config.index_url.is_empty() {
                let interpolated_url = interpolate_index_url(
                    &repo_config.index_url, &config_version, &config_arch, &config_app_version, &repo_name
                );
                repo_config.index_url = interpolated_url;
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

/// Update system channel configs with inherited settings and proper naming
fn update_system_channel_configs(
    system_channel_configs: Vec<ChannelConfig>,
    channel_configs: &mut Vec<ChannelConfig>,
    main_config: Option<&ChannelConfig>,
) -> Result<()> {
    let mut updated_configs = system_channel_configs;
    for channel_config in &mut updated_configs {
        set_channel_config_defaults(channel_config, main_config)?;
        interpolate_channel_urls(channel_config);
    }
    channel_configs.append(&mut updated_configs);

    Ok(())
}

/// Load system repository configurations as separate ChannelConfig instances
fn load_system_repositories(channel_configs: &mut Vec<ChannelConfig>, env_root: &Path) -> Result<()> {
    // Get the main channel config to inherit common settings
    let main_config = channel_configs.first().cloned();

    // Load Deb system repositories
    if let Ok(system_channel_configs) = crate::deb_sources::load_deb_system_repos(env_root, &config().common.arch) {
        update_system_channel_configs(system_channel_configs, channel_configs, main_config.as_ref())?;
    }

    // Load RPM system repositories
    if let Ok(system_channel_configs) = crate::rpm_sources::load_rpm_system_repos(env_root) {
        update_system_channel_configs(system_channel_configs, channel_configs, main_config.as_ref())?;
    }

    Ok(())
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

    // Load system repository configurations based on format
    load_system_repositories(&mut channel_configs, env_root)?;

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
// - $releasever: the RPM release version (same as $version)
// - $basearch: the RPM base architecture (same as $arch)
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

    // Replace RPM variables
    url = url.replace("$releasever", version);
    url = url.replace("$basearch", arch);

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
        lfs::create_dir_all(parent_dir)?;
    }

    // Write the YAML to the file
    lfs::write(&config_path, yaml)?;

    Ok(())
}

/// Read installed packages from an arbitrary path (supports both object and array JSON formats).
pub fn read_installed_packages_from_path(file_path: &Path) -> Result<InstalledPackagesMap> {
    if !file_path.exists() {
        return Ok(HashMap::new());
    }
    let contents = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;
    let value: Value = serde_json::from_str(&contents)
        .with_context(|| format!("Failed to parse JSON from file: {}", file_path.display()))?;
    let packages_raw = installed_packages_from_value(value)?;
    let packages: InstalledPackagesMap =
        packages_raw.into_iter().map(|(k, v)| (k, Arc::new(v))).collect();
    Ok(packages)
}

pub fn read_installed_packages(env: &str, generation_id: u32) -> Result<InstalledPackagesMap> {
    let generations_root = get_generations_root(env)?;
    let file_path = generations_root
        .join(generation_id.to_string())
        .join("installed-packages.json");
    read_installed_packages_from_path(&file_path)
}

pub fn load_installed_packages() -> Result<()> {
    // If installed_packages is already populated (e.g., in test mode), skip loading
    // This preserves test-set installed packages and avoids overwriting them
    if !PACKAGE_CACHE.installed_packages.read().unwrap().is_empty() {
        return Ok(());
    }
    let generation_id = get_current_generation_id()?;
    let packages = read_installed_packages(&config().common.env_name, generation_id)?;
    let mut installed = PACKAGE_CACHE.installed_packages.write().unwrap();
    for (k, v) in packages {
        installed.insert(k, v);
    }
    drop(installed);
    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
    let mut pkgline2installed = PACKAGE_CACHE.pkgline2installed.write().unwrap();
    for (_, info) in installed.iter() {
        pkgline2installed.insert(info.pkgline.clone(), Arc::clone(info));
    }
    Ok(())
}

pub fn save_installed_packages(new_generation: &PathBuf) -> Result<()> {
    // Construct the file path
    let file_path = new_generation.join("installed-packages.json");

    // Collect installed packages into Vec<InstalledPackageEntry> for array format
    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
    let entries = installed_packages_to_array(&installed);

    // Serialize the array to JSON
    let json = serde_json::to_string_pretty(&entries)?;

    // Write the JSON to the file
    lfs::write(&file_path, json)?;

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
    let world = read_world(&config().common.env_name, generation_id)?;
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
    lfs::write(&file_path, json)?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::InstalledPackageInfo;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    fn test_installed_packages_conversion() {
        // Create a simple map with varying depend_depth
        let mut map = HashMap::new();
        map.insert(
            "pkg1__1.0__x86_64".to_string(),
            Arc::new(InstalledPackageInfo {
                pkgline: "hash1__pkg1__1.0__x86_64".to_string(),
                arch: "x86_64".to_string(),
                depend_depth: 2,
                ..Default::default()
            }),
        );
        map.insert(
            "pkg2__2.0__x86_64".to_string(),
            Arc::new(InstalledPackageInfo {
                pkgline: "hash2__pkg2__2.0__x86_64".to_string(),
                arch: "x86_64".to_string(),
                depend_depth: 0,
                ..Default::default()
            }),
        );
        map.insert(
            "pkg3__3.0__x86_64".to_string(),
            Arc::new(InstalledPackageInfo {
                pkgline: "hash3__pkg3__3.0__x86_64".to_string(),
                arch: "x86_64".to_string(),
                depend_depth: 1,
                ..Default::default()
            }),
        );

        // Convert to array
        let entries = installed_packages_to_array(&map);
        assert_eq!(entries.len(), 3);
        // Check sorting: depend_depth 0 first, then 1, then 2
        assert_eq!(entries[0].pkgkey, "pkg2__2.0__x86_64");
        assert_eq!(entries[1].pkgkey, "pkg3__3.0__x86_64");
        assert_eq!(entries[2].pkgkey, "pkg1__1.0__x86_64");
        // Ensure pkgkey matches pkgline2pkgkey
        use crate::package::pkgline2pkgkey;
        for entry in &entries {
            let computed = pkgline2pkgkey(&entry.info.pkgline).unwrap();
            assert_eq!(entry.pkgkey, computed);
        }

        // Simulate JSON roundtrip: convert to Value array and back
        let json = serde_json::to_value(&entries).unwrap();
        let reconstructed = installed_packages_from_value(json).unwrap();
        assert_eq!(reconstructed.len(), 3);
        for (pkgkey, info) in reconstructed {
            assert!(map.contains_key(&pkgkey));
            let original = &map[&pkgkey];
            assert_eq!(info.pkgline, original.pkgline);
            assert_eq!(info.depend_depth, original.depend_depth);
        }
    }
}
