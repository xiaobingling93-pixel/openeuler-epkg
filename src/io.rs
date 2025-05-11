use glob;
use serde_json;
use serde_yaml;
use std::fs;
use std::env;
use std::path::Path;
use anyhow::{Context, Result, bail};
use crate::dirs::*;
use crate::models::*;
use std::collections::HashMap;
use log;

pub fn load_package_json(file_path: &str) -> Result<Package> {
    let contents = fs::read_to_string(&file_path)
        .with_context(|| format!("Failed to read file: {}", file_path))?;

    let package: Package = serde_json::from_str(&contents)
        .with_context(|| format!("Failed to parse JSON from file: {}", file_path))?;

    Ok(package)
}

pub fn load_repodata_index(file_path: &str) -> Result<Repodata> {
    // Read the file contents
    let contents = fs::read_to_string(&file_path)
        .with_context(|| format!("Failed to read file: {}", file_path))?;

    // Deserialize the JSON into Repodata
    let mut repodata: Repodata = serde_json::from_str(&contents)
        .with_context(|| format!("Failed to parse JSON from file: {}", file_path))?;


    let dir = Path::new(file_path).parent().unwrap();
    repodata.dir = dir.to_string_lossy().into_owned(); // Convert Path to String
    repodata.name = dir
        .parent().unwrap()
        .parent().unwrap()
        .file_name().unwrap()
        .to_string_lossy() // Convert &OsStr to String
        .into_owned();

    Ok(repodata)
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

impl PackageManager {

    /// Load environment configuration from in-memory hash or on-disk file
    pub fn get_env_config(&mut self, env_name: String) -> Result<&EnvConfig> {
        if self.env_config.contains_key(&env_name) {
            return Ok(&self.env_config[&env_name]);
        }

        let config_path = get_env_config_path(&env_name);

        // Read the file contents
        let contents = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read file: {}", config_path.display()))?;

        // Deserialize the YAML into EnvConfig
        let env_config: EnvConfig = serde_yaml::from_str(&contents)
            .with_context(|| format!("Failed to parse YAML from file: {}", config_path.display()))?;

        self.env_config.insert(env_name.clone(), env_config);

        Ok(&self.env_config[&env_name])
    }

    pub fn get_channel_config(&mut self, env_name: String) -> Result<&ChannelConfig> {
        if self.channel_config.contains_key(&env_name) {
            return Ok(&self.channel_config[&env_name]);
        }

        let env_root = self.get_env_root(env_name.clone())?;

        let file_path = env_root.join("etc/epkg/channel.yaml");

        // Read the file contents
        let contents = fs::read_to_string(&file_path)
            .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

        // Deserialize the YAML into the ChannelConfig struct
        let channel_config: ChannelConfig = serde_yaml::from_str(&contents)
            .with_context(|| format!("Failed to parse YAML from file: {}", file_path.display()))?;

        self.channel_config.insert(env_name.clone(), channel_config);

        Ok(&self.channel_config[&env_name])
    }

    // load repodata/index.json and store to repodata
    pub fn load_repodata(&mut self) -> Result<()> {
        log::trace!("Starting load_repodata");
        let channel_config = self.get_channel_config(config().common.env.clone())?;
        let file_glob: String = format!("{}/channel/{}/*/{}/repodata/index.json",
            dirs().epkg_cache.display(),
            channel_config.channel.name,
            config().common.arch,
        );
        log::debug!("Searching for repodata files with pattern: {}", file_glob);

        let mut total_repos = 0;
        let mut total_store_paths = 0;
        let mut total_pkg_infos = 0;

        for entry in glob::glob(&file_glob).expect("Failed to read glob pattern") {
            match entry {
                Ok(path) => {
                    total_repos += 1;
                    log::trace!("Found repodata file: {}", path.display());
                    let path_str = path.to_str().with_context(|| format!("Invalid UTF-8 in path: {:?}", path))?;
                    // Call the global function to load repodata
                    let mut repodata = load_repodata_index(path_str)
                        .with_context(|| format!("Failed to load repodata from {}", path.display()))?;

                    log::trace!("Loading provides for repo: {}", repodata.name);
                    let provide_path = path.parent().unwrap().join("provide2pkgnames.txt");
                    repodata.decode_provide_hashmap(provide_path.to_str().unwrap())?;

                    log::trace!("Loading essential packages for repo: {}", repodata.name);
                    let essential_path = path.parent().unwrap().join("essential_pkgnames.txt");
                    repodata.decode_essential_hashset(essential_path.to_str().unwrap())?;

                    total_store_paths += repodata.store_paths.len();
                    total_pkg_infos += repodata.pkg_infos.len();

                    log::debug!("Loaded repository: {}", repodata.name);
                    log::debug!("  Store paths: {}", repodata.store_paths.len());
                    log::debug!("  Package infos: {}", repodata.pkg_infos.len());
                    log::debug!("  Provides: {}", repodata.provide2pkgnames.len());
                    log::debug!("  Essential packages: {}", repodata.essential_pkgnames.len());

                    self.repos_data.push(repodata);
                },
                Err(e) => {
                    log::warn!("Error processing repodata entry: {:?}", e);
                    println!("{:?}", e);
                },
            }
        }

        log::debug!("Repodata loading statistics:");
        log::debug!("  Total repositories found: {}", total_repos);
        log::debug!("  Total store paths: {}", total_store_paths);
        log::debug!("  Total package infos: {}", total_pkg_infos);
        log::debug!("  Total repositories loaded: {}", self.repos_data.len());

        Ok(())
    }

    pub fn load_store_paths(&mut self) -> Result<()> {
        log::trace!("Starting load_store_paths");
        if self.repos_data.is_empty() {
            log::trace!("Repos data is empty, loading repodata first");
            self.load_repodata()?;
        }

        let mut total_provides = 0;
        let mut total_essential = 0;

        for repodata in &self.repos_data {
            log::trace!("Processing repodata for repo: {}", repodata.name);
            self.provide2pkgnames.extend(repodata.provide2pkgnames.clone());
            self.essential_pkgnames.extend(repodata.essential_pkgnames.clone());

            total_provides += repodata.provide2pkgnames.len();
            total_essential += repodata.essential_pkgnames.len();

            for entry in &repodata.store_paths {
                log::trace!("Processing store path entry: {}", entry.filename);
                let file_path = format!(
                    "{}/{}",
                    repodata.dir,
                    entry.filename.strip_suffix(".zst").unwrap()
                                                       .splitn(3, '-')
                                                       .take(2)
                                                       .collect::<Vec<_>>()
                                                       .join("-")
                );
                log::trace!("Reading store paths from file: {}", file_path);
                let contents = fs::read_to_string(&file_path)
                    .with_context(|| format!("Failed to load store-paths from {}", file_path))?;
                for pkgline in contents.lines() {
                    if let Ok(pkg_spec) = parse_package_line(pkgline, &repodata.name) {
                        self.pkgname2lines
                            .entry(pkg_spec.name.clone())
                            .or_insert_with(Vec::new)
                            .push(pkgline.to_string());
                        self.pkghash2spec.insert(pkg_spec.hash.clone(), pkg_spec);
                    }
                }
            }
        }

        log::debug!("Store paths loading statistics:");
        log::debug!("  Total repositories processed: {}", self.repos_data.len());
        log::debug!("  Total provides loaded: {}", total_provides);
        log::debug!("  Total essential packages: {}", total_essential);
        log::debug!("  Unique package names: {}", self.pkgname2lines.len());
        log::debug!("  Unique package hashes: {}", self.pkghash2spec.len());

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

    pub fn save_installed_packages(&mut self) -> Result<()> {
        // Get the generations root
        let generations_root = self.get_default_generations_root()?;

        // Construct the file path
        let file_path = generations_root.join("current").join("installed-packages.json");

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
        let env_config = self.env_config.get(env_name)
            .ok_or_else(|| anyhow::anyhow!("Environment config not found: {}", env_name))?;

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
            return Err(anyhow::anyhow!("Editor exited with non-zero status"));
        }

        Ok(())
    }

}
