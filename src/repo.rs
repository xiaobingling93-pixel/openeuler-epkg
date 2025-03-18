use std::fs;
use std::path::Path;
use anyhow::Result;
use anyhow::anyhow;
use crate::models::*;
use crate::store::*;
use crate::paths;
use crate::download::*;

impl PackageManager {
    pub fn cache_repo(&mut self) -> Result<()> {
        if self.env_config.channel.name.is_empty() {
            self.load_env_config().unwrap();
        }

        let arch = std::env::consts::ARCH;
        if arch == "unknown" || arch == "riscv64" || arch == "loongarch64" {
            return Err(anyhow!("Unsupported system architecture: {}", arch));
        }
        let repos: Vec<_> = self.env_config.repos.keys().cloned().collect();
        for repo_name in repos {
            let repo_url = format!("{}/{}/{}/", self.env_config.channel.baseurl, &repo_name, arch);
            self.cache_repo_name(&repo_name, &repo_url).unwrap();
        }
        Ok(())
    }
}

pub fn cache_repo_name(repo_name: &str, repo_url: &str) -> Result<()> {
    let local_cache_path = match repo_url.find("/channel/") {
        Some(idx) => paths::instance.epkg_channel_cache_dir.join(&repo_url[idx + 9..]),
        None => return Err(anyhow!("Invalid repo URL format: no /channel/ found")),
    };

    // Check if store-paths.zst already exists
    if local_cache_path.join("repodata/store-paths.zst").exists() {
        return Ok(());
    }
    println!("Caching repodata {} from {}", repo_name, repo_url);

    // clean old metadata files and re-init metadata dir
    if local_cache_path.exists() {
        fs::remove_dir_all(&local_cache_path).unwrap();
    }
    fs::create_dir_all(&local_cache_path.join("repodata")).unwrap();

    // sync repo from local & http
    let files = ["store-paths.zst", "pkg-info.zst", "index.json"];
    let repodata_path = local_cache_path.join("repodata");
    match repo_url {
        url if url.starts_with('/') => {
            let src_path = Path::new(url).join("repodata");
            for file in &files {
                fs::copy(src_path.join(file), repodata_path.join(file)).unwrap();
            }
        },
        url if url.starts_with("http") => {
            for file in &files {
                let url = format!("{}/repodata/{}", url, file);
                download_urls(vec![url], repodata_path.to_str().unwrap(), 1, 6, None).unwrap();
            }
        },
        _ => return Err(anyhow!("Unsupported repo URL scheme")),
    }

    // cached medatata file should be decompressed
    untar_zst(repodata_path.join("pkg-info.zst").to_str().unwrap(), local_cache_path.to_str().unwrap(), false).unwrap();
    unzst(repodata_path.join("store-paths.zst").to_str().unwrap(), repodata_path.join("store-paths").to_str().unwrap()).unwrap();

    println!("Cache repodata succeed: {}", repo_name);
    Ok(())
}

pub fn list_repos() -> Result<()> {
    let manager_channel_dir = paths::instance.epkg_mananger_cache_dir.join("channel");
    if !manager_channel_dir.exists() {
        return Ok(());
    }

    println!("{}", "-".repeat(100));
    println!("{:<30} | {:<15} | {}", "channel", "repo", "url");
    println!("{}", "-".repeat(100));

    for entry in fs::read_dir(&manager_channel_dir).unwrap() {
        let path = entry?.path();
        if !path.is_file() || path.extension().unwrap_or_default() != "yaml" {
            continue;
        }

        let channel_config: EnvConfig = serde_yaml::from_str(
            &fs::read_to_string(&path)?
        )?;

        for repo_name in channel_config.repos.keys() {
            println!("{:<30} | {:<15} | {}", 
                channel_config.channel.name,
                repo_name,
                channel_config.channel.baseurl
            );
        }
    }

    println!("{}", "-".repeat(100));
    Ok(())
}