use glob;
use serde_json;
use serde_yaml;
use std::fs;
use std::env;
use std::path::Path;
use dirs::home_dir;
use anyhow::{Context, Result, bail};
use crate::dirs;
use crate::models::*;

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

    Ok(PackageSpec {
        repo: reponame.to_string(),
        hash: parts[0].to_string(),
        name: parts[1].to_string(),
        version: parts[2].to_string(),
        release: parts[3].to_string(),
    })
}

impl PackageManager {

    pub fn get_env_config(&mut self, env_name: String) -> Result<&EnvConfig> {
        if self.env_config.contains_key(&env_name) {
            return Ok(&self.env_config[&env_name]);
        }

        let env_path = format!("{}/envs/{}.yaml",
            self.dirs.home_config.display(),
            env_name
        );

        // Read the file contents
        let contents = fs::read_to_string(&env_path)
            .with_context(|| format!("Failed to read file: {}", env_path))?;

        // Deserialize the YAML into EnvConfig
        let env_config: EnvConfig = serde_yaml::from_str(&contents)
            .with_context(|| format!("Failed to parse YAML from file: {}", env_path))?;

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
        let channel_config = self.get_channel_config(self.options.env.clone())?;
        let file_glob: String = format!("{}/channel/{}/*/{}/repodata/index.json",
            self.dirs.epkg_cache.display(),
            channel_config.name,
            self.options.arch,
        );
        for entry in glob::glob(&file_glob).expect("Failed to read glob pattern") {
            match entry {
                Ok(path) => {
                    let path_str = path.to_str().with_context(|| format!("Invalid UTF-8 in path: {:?}", path))?;
                    // Call the global function to load repodata
                    let mut repodata = load_repodata_index(path_str)
                        .with_context(|| format!("Failed to load repodata from {}", path.display()))?;
                    let provide_path = path.parent().unwrap().join("provide2pkgnames.txt");
                    repodata.decode_provide_hashmap(provide_path.to_str().unwrap())?;
                    let essential_path = path.parent().unwrap().join("essential_pkgnames.txt");
                    repodata.decode_essential_hashset(essential_path.to_str().unwrap())?;
                    self.repos_data.push(repodata);
                },
                Err(e) => println!("{:?}", e),
            }
        }

        Ok(())
    }

    pub fn load_store_paths(&mut self) -> Result<()> {
        if self.repos_data.is_empty() {
            self.load_repodata()?;
        }
        for repodata in &self.repos_data {
            self.provide2pkgnames.extend(repodata.provide2pkgnames.clone());
            self.essential_pkgnames.extend(repodata.essential_pkgnames.clone());

            for entry in &repodata.store_paths {
                let file_path = format!(
                    "{}/{}",
                    repodata.dir,
                    entry.filename.strip_suffix(".zst").unwrap()
                                                       .splitn(3, '-')
                                                       .take(2)
                                                       .collect::<Vec<_>>()
                                                       .join("-")
                );
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
        Ok(())
    }

    pub fn load_installed_packages(&mut self) -> Result<()> {
        let env_root = self.get_default_env_root()?;
        let file_path = env_root.join("installed-packages.json");

        let contents = fs::read_to_string(&file_path)
            .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

        self.installed_packages = serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse JSON from file: {}", file_path.display()))?;

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

        if self.options.verbose {
            println!("Installed packages saved to: {}", file_path.display());
        }

        Ok(())
    }

    pub fn save_env_config(&mut self, env_name: &str) -> Result<()> {
        let env_config = self.env_config.get(env_name)
            .ok_or_else(|| anyhow::anyhow!("Environment config not found: {}", env_name))?;

        let env_path = format!("{}/envs/{}.yaml",
            self.dirs.home_config.display(),
            env_name
        );

        // Serialize the EnvConfig to YAML
        let yaml = serde_yaml::to_string(env_config)
            .with_context(|| format!("Failed to serialize environment config to YAML"))?;

        // Write the YAML to the file
        fs::write(&env_path, yaml)
            .with_context(|| format!("Failed to write environment config to file: {}", env_path))?;

        Ok(())
    }

}
