use std::path::Path;
use std::fs;
use std::env;
use glob;
use serde_json;
use serde_yaml;
use dirs::home_dir;
use anyhow::{Context, Result, bail};
use crate::models::*;

pub fn load_package_json(file_path: &str) -> Result<Package> {
    let contents = fs::read_to_string(&file_path)
        .with_context(|| format!("Failed to read file: {}", file_path))?;

    let package: Package = serde_json::from_str(&contents)
        .with_context(|| format!("Failed to parse JSON from file: {}", file_path))?;

    Ok(package)
}

fn load_repodata_index(file_path: &str) -> Result<Repodata> {
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

    pub fn load_env_config(&mut self) -> Result<()> {
        let file_path = format!("{}/.epkg/envs/{}/profile-current/etc/epkg/channel.yaml",
            env::var("HOME")?,
            self.options.env,
        );

        // Read the file contents
        let contents = fs::read_to_string(&file_path)
            .with_context(|| format!("Failed to read file: {}", file_path))?;

        // Deserialize the YAML into the Config struct
        self.env_config = serde_yaml::from_str(&contents)
            .with_context(|| format!("Failed to parse YAML from file: {}", file_path))?;

        Ok(())
    }

    // load repodata/index.json and store to repodata
    pub fn load_repodata(&mut self) -> Result<()> {
        if self.env_config.channel.name.is_empty() {
            self.load_env_config()?;
        }

        let file_glob: String = format!("{}/.cache/epkg/channel/{}/*/{}/repodata/index.json",
            env::var("HOME")?,
            self.env_config.channel.name,
            self.options.arch,
        );

        for entry in glob::glob(&file_glob).expect("Failed to read glob pattern") {
            match entry {
                Ok(path) => {
                    let path_str = path.to_str().with_context(|| format!("Invalid UTF-8 in path: {:?}", path))?;
                    // Call the global function to load repodata
                    let repodata = load_repodata_index(path_str)
                        .with_context(|| format!("Failed to load repodata from {}", path.display()))?;
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
            for entry in &repodata.store_paths {
                let file_path = format!(
                    "{}/{}",
                    repodata.dir,
                    entry.filename.strip_suffix(".zst").unwrap()
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

        let file_path: String = format!("{}/.epkg/envs/{}/profile-current/installed-packages.json",
            env::var("HOME")?,
            self.options.env,
        );

        let contents = fs::read_to_string(&file_path)
            .with_context(|| format!("Failed to read file: {}", file_path))?;

        self.installed_packages = serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse JSON from file: {}", file_path))?;

        Ok(())
    }

    pub fn save_installed_packages(&self) -> Result<()> {
        // Get the home directory
        let home = home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;

        // Construct the file path
        let file_path = home
            .join(".epkg")
            .join("envs")
            .join(self.options.env.clone())
            .join("profile-current")
            .join("installed-packages.json");

        // Serialize the installed packages to JSON
        let json = serde_json::to_string_pretty(&self.installed_packages)?;

        // Write the JSON to the file
        fs::write(&file_path, json)?;

        if self.options.verbose {
            println!("Installed packages saved to: {}", file_path.display());
        }

        Ok(())
    }
}
