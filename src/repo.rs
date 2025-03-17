use std::fs;
use std::path::Path;
use anyhow::Result;
use anyhow::anyhow;
use crate::utils::*;
use crate::models::*;
use crate::paths;

impl PackageManager {
    pub fn cache_repo(&mut self) -> Result<()> {
        if self.env_config.channel.name.is_empty() {
            self.load_env_config()?;
        }

        let arch = get_system_arch();
        if arch == "unknown" || arch == "riscv64" || arch == "loongarch64" {
            return Err(anyhow!("Unsupported system architecture: {}", arch));
        }
        let repos: Vec<_> = self.env_config.repos.keys().cloned().collect();
        for repo_name in repos {
            let repo_url = format!("{}/{}/{}/", self.env_config.channel.baseurl, &repo_name, arch);
            self.cache_repo_index(&repo_name, &repo_url)?;
        }
        Ok(())
    }

    fn cache_repo_index(&mut self, repo_name: &str, repo_url: &str) -> Result<()> {
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
        self.create_dir(&local_cache_path.join("repodata"))?;

        // sync repo from local & http
        let files = ["store-paths.zst", "pkg-info.zst", "index.json"];
        let repodata_path = local_cache_path.join("repodata");
        match repo_url {
            url if url.starts_with('/') => {
                let src_path = Path::new(url).join("repodata");
                for file in &files {
                    fs::copy(src_path.join(file), repodata_path.join(file))?;
                }
            },
            url if url.starts_with("http") => {
                for file in &files {
                    let url = format!("{}/repodata/{}", url, file);
                    self.download_urls(vec![url], repodata_path.to_str().unwrap(), 1, 6, None)?;
                }
            },
            _ => return Err(anyhow!("Unsupported repo URL scheme")),
        }

        // cached medatata file should be decompressed
        self.untar_zst(repodata_path.join("pkg-info.zst").to_str().unwrap(), local_cache_path.to_str().unwrap(), false)?;
        self.unzst(repodata_path.join("store-paths.zst").to_str().unwrap(), repodata_path.join("store-paths").to_str().unwrap())?;

        println!("Cache repodata succeed: {}", repo_name);
        Ok(())
    }
}
