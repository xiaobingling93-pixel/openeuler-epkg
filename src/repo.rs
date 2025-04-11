use std::fs;
use std::path::Path;
use std::path::PathBuf;
use anyhow::Ok;
use anyhow::Result;
use anyhow::{anyhow, Context};
use crate::models::*;
use crate::store::*;
use crate::paths;
use crate::download::*;
use crate::io::load_repodata_index;

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

fn download_repodata(base_url: &str, repodata_path: &PathBuf) -> Result<()> {
    let index_url = format!("{}/index.json", base_url);
    download_urls(vec![index_url], repodata_path.to_str().unwrap(), 1, 6, None).unwrap();
    let index_file_path = repodata_path.join("index.json");
    let repo_data = load_repodata_index(index_file_path.to_str().unwrap())
                      .with_context(|| format!("Failed to load repodata from {}", repodata_path.display()))?;
    let all_filenames: Vec<&str> = repo_data.store_paths.iter()
                    .map(|zst| zst.filename.as_str())
                    .chain(
                        repo_data.pkg_infos.iter()
                            .map(|zst| zst.filename.as_str())
                    )
                    .collect();
    for filename in all_filenames {
        let zst_url = format!("{}/{}", base_url, filename);
        download_urls(vec![zst_url], repodata_path.to_str().unwrap(), 1, 6, None).unwrap();
    }

    Ok(())
}

fn unzst_all_repodatas(repodata_path: &PathBuf) -> Result<()> {
    let index_file_path = repodata_path.join("index.json");
    let repo_data = load_repodata_index(index_file_path.to_str().unwrap())
                      .with_context(|| format!("Failed to load repodata from {}", repodata_path.display()))?;
    for store_path in &repo_data.store_paths {
        let store_path_zst = repodata_path.join(&store_path.filename);
        unzst(store_path_zst.to_str().unwrap(), repodata_path.join("store-paths").to_str().unwrap()).unwrap();
    }
    for pkg_info in &repo_data.pkg_infos {
        let pkg_info_zst = repodata_path.join(&pkg_info.filename);
        untar_zst(pkg_info_zst.to_str().unwrap(), repodata_path.parent().unwrap().to_str().unwrap(), false).unwrap();
    }

    Ok(())
}

pub fn cache_repo_name(repo_name: &str, repo_url: &str) -> Result<()> {
    let local_cache_path = match repo_url.find("/channel/") {
        Some(idx) => paths::instance.epkg_channel_cache_dir.join(&repo_url[idx + 9..]),
        None => return Err(anyhow!("Invalid repo URL format: no /channel/ found")),
    };
    // [TODO] should check index.json pkg-info-xxx.zst store-paths-xxx.zst all valid
    // Check if index.json already exists
    if local_cache_path.join("repodata/index.json").exists() {
        return Ok(());
    }
    println!("Caching repodata {} from {}", repo_name, repo_url);

    // clean old metadata files and re-init metadata dir
    if local_cache_path.exists() {
        fs::remove_dir_all(&local_cache_path).unwrap();
    }
    fs::create_dir_all(&local_cache_path.join("repodata")).unwrap();

    // sync repo from local & http
    let repodata_path = local_cache_path.join("repodata");
    match repo_url {
        url if url.starts_with('/') => {
            let src_path = Path::new(url).join("repodata");
            for src_entry in fs::read_dir(src_path)? {
                let entry = src_entry?;
                let src_path = entry.path();
                let dst_path = repodata_path.join(entry.file_name());
                fs::copy(src_path, dst_path).unwrap();
            }
        },
        url if url.starts_with("http") => {
            let repo_url = format!("{}/repodata", url);
            download_repodata( repo_url.as_str(), &repodata_path).unwrap();
        },
        _ => return Err(anyhow!("Unsupported repo URL scheme")),
    }
    unzst_all_repodatas(&repodata_path).unwrap();

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