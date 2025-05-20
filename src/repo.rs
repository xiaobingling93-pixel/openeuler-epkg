use std::collections::{HashMap, HashSet};
use std::fs;
use std::fs::File;
use std::path::Path;
use std::path::PathBuf;
use std::io::{BufRead, BufReader, BufWriter, Write};
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre;
use crate::models::*;
use crate::store::*;
use crate::download::*;
use crate::parse_requires::*;
use crate::io::{load_repodata_index, load_package_json};
use crate::utils::copy_all;
use crate::dirs::find_env_root;

impl Repodata {
    pub fn save_package_provides(&mut self, path: &str) -> Result<()> {
        let target_path = Path::new(path);
        if target_path.exists() {
            fs::remove_file(&target_path)?;
        }
        let file = File::create(target_path)?;
        let mut writer = BufWriter::new(file);

        for (key, values) in self.provide2pkgnames.iter() {
            let line = format!("{}: {}", key, values.join(" "));
            writeln!(writer, "{}", line)?;
        }

        writer.flush()?;
        Ok(())
    }

    pub fn load_package_provides(&mut self, file_path: &str) -> Result<()> {
        let file = File::open(file_path)?;
        let reader = BufReader::new(file);
        let mut map: HashMap<String, Vec<String>> = HashMap::new();

        for (line_num, line_result) in reader.lines().enumerate() {
            let line = line_result.context(format!("Failed to read line {} from {}", line_num + 1, file_path))?;
            if let Some((key, values)) = line.split_once(": ") {
                let values: Vec<String> = values.split(" ").map(|s| s.to_string()).collect();
                map.insert(key.to_string(), values);
            }
        }
        self.provide2pkgnames = map;

        Ok(())
    }

    pub fn save_essential_packages(&mut self, path: &str) -> Result<()> {
        let target_path = Path::new(path);
        if target_path.exists() {
            fs::remove_file(&target_path)?;
        }
        let file = File::create(target_path)?;
        let mut writer = BufWriter::new(file);

        for item in self.essential_pkgnames.iter() {
            writeln!(writer, "{}", item)?;
        }

        writer.flush()?;
        Ok(())
    }

    pub fn load_essential_packages(&mut self, file_path: &str) -> Result<()> {
        let file = File::open(file_path)?;
        let reader = BufReader::new(file);
        let mut hashset: HashSet<String> = HashSet::new();

        for line in reader.lines() {
            let line = line?;
            hashset.insert(line);
        }
        self.essential_pkgnames = hashset;

        Ok(())
    }

    pub fn generate_repo_metadata(&mut self) -> Result<()> {
        let pkg_info_dir = Path::new(&self.dir).parent().unwrap().join("pkg-info");
        for entry in &self.store_paths {
            let file_path = format!(
                "{}/{}",
                self.dir,
                entry.filename.strip_suffix(".zst").unwrap()
                                                   .splitn(3, '-')
                                                   .take(2)
                                                   .collect::<Vec<_>>()
                                                   .join("-")
            );
            let contents = fs::read_to_string(&file_path)
                .with_context(|| format!("Failed to load store-paths from {}", file_path))?;
            for pkgline in contents.lines() {
                let file_path: String = format!("{}/{}/{}.json",
                    pkg_info_dir.display(),
                    &pkgline[0..2],
                    pkgline,
                );
                let pkg_json = load_package_json(&file_path)?;
                let format = match pkg_json.origin_url {
                    Some(ref url) => {
                        get_package_format(url)
                    },
                    None => {
                        Some("rpm".to_string())
                    }
                };
                for provide in &pkg_json.provides {
                    let and_deps = match parse_requires(&format.clone().unwrap().as_str(), provide) {
                        std::result::Result::Ok(deps) => deps,
                        Err(e) => {
                            println!("Failed to parse requirement '{}': {}", provide, e);
                            continue;
                        }
                    };
                    if let Some(pkgnames) = self.provide2pkgnames.get_mut(and_deps[0][0].capability.as_str()) {
                        pkgnames.push(pkg_json.name.clone());
                    } else {
                        self.provide2pkgnames.insert(and_deps[0][0].capability.clone(), vec![pkg_json.name.clone()]);
                    }
                }
                if matches!(pkg_json.priority.as_deref(), Some("essential")) {
                    self.essential_pkgnames.insert(pkg_json.name.clone());
                }
            }
        }

        Ok(())
    }
}

impl PackageManager {

    pub fn cache_channel_repositories(&mut self) -> Result<()> {
        let channel_config = self.get_channel_config(config().common.env.clone())?;

        for (repo_name, repo_config) in &channel_config.repos {
            // Skip disabled repos
            if !repo_config.enabled {
                continue;
            }

            // Use configured URL or construct default URL
            let repo_url = match &repo_config.url {
                Some(url) => url.clone(),
                None => format!(
                    "{}/{}/{}/",
                    channel_config.channel.baseurl.clone().ok_or_else(|| eyre::eyre!("baseurl not configured"))?,
                    &repo_name,
                    config().common.arch
                )
            };

            cache_single_repository(&channel_config.channel.name, &repo_name, &repo_url)?;
        }
        Ok(())
    }

}

fn download_repodata(base_url: &str, repodata_path: &PathBuf) -> Result<()> {
    let index_url = format!("{}/index.json", base_url);
    download_urls(vec![index_url], repodata_path.to_str().unwrap(), 1, 6, None)?;
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
        download_urls(vec![zst_url], repodata_path.to_str().unwrap(), 1, 6, None)?;
    }

    Ok(())
}

fn unzst_all_repodatas(repodata_path: &PathBuf) -> Result<Repodata> {
    let index_file_path = repodata_path.join("index.json");
    let repo_data = load_repodata_index(index_file_path.to_str().unwrap())
                      .with_context(|| format!("Failed to load repodata from {}", repodata_path.display()))?;
    for store_path in &repo_data.store_paths {
        let store_path_zst = repodata_path.join(&store_path.filename);
        unzst(store_path_zst.to_str().unwrap(), repodata_path.join("store-paths").to_str().unwrap())?;
    }
    for pkg_info in &repo_data.pkg_infos {
        let pkg_info_zst = repodata_path.join(&pkg_info.filename);
        untar_zst(pkg_info_zst.to_str().unwrap(), repodata_path.parent().unwrap().to_str().unwrap(), false)?;
    }

    Ok(repo_data)
}

pub fn cache_single_repository(channel_name: &str, repo_name: &str, repo_url: &str) -> Result<()> {
    let local_cache_path = dirs().epkg_channel_cache.join(channel_name).join(repo_name).join(&config().common.arch);
    let repodata_path = local_cache_path.join("repodata");
    // [TODO] should check index.json pkg-info-xxx.zst store-paths-xxx.zst all valid
    // Check if index.json already exists
    if repodata_path.join("provide2pkgnames.yaml").exists() &&
       repodata_path.join("essential_pkgnames.txt").exists() {
        return Ok(());
    }
    println!("Caching repodata {} from {}", repo_name, repo_url);

    // clean old metadata files and re-init metadata dir
    if local_cache_path.exists() {
        fs::remove_dir_all(&local_cache_path)?;
    }

    // sync repo from local & http
    match repo_url {
        url if url.starts_with('/') => {
            let src_path = Path::new(url).join("repodata");
            copy_all(src_path, repodata_path.clone())?;
        },
        url if url.starts_with("http") => {
            let repo_url = format!("{}/repodata", url);
            download_repodata(repo_url.as_str(), &repodata_path).unwrap();
        },
        _ => return Err(eyre::eyre!("Unsupported repo URL scheme")),
    }
    let mut repodata = unzst_all_repodatas(&repodata_path)?;
    repodata.generate_repo_metadata()?;
    repodata.save_package_provides(repodata_path.join("provide2pkgnames.yaml").to_str().unwrap())?;
    repodata.save_essential_packages(repodata_path.join("essential_pkgnames.txt").to_str().unwrap())?;

    println!("Cache repodata succeed: {}", repo_name);
    Ok(())
}

pub fn list_repos() -> Result<()> {
    let common_env_root = find_env_root("common")
                .ok_or_else(|| eyre::eyre!("Common environment not found"))?;
    let manager_channel_dir = common_env_root.join("opt/epkg-manager/channel");
    if !manager_channel_dir.exists() {
        return Ok(());
    }

    println!("{}", "-".repeat(100));
    println!("{:<30} | {:<15} | {}", "channel", "repo", "url");
    println!("{}", "-".repeat(100));

    for entry in fs::read_dir(&manager_channel_dir)? {
        let path = entry?.path();
        if !path.is_file() || path.extension().unwrap_or_default() != "yaml" {
            continue;
        }

        let channel_config: ChannelConfig = serde_yaml::from_str(
            &fs::read_to_string(&path)?
        )?;

        for (repo_name, repo_config) in &channel_config.repos {
            // Skip disabled repos
            if !repo_config.enabled {
                continue;
            }

            // Use configured URL or construct default URL
            let repo_url = match &repo_config.url {
                Some(url) => url.clone(),
                None => format!(
                    "{}/{}/{}/",
                    channel_config.channel.baseurl.clone().ok_or_else(|| eyre::eyre!("baseurl not configured"))?,
                    &repo_name,
                    config().common.arch
                )
            };

            println!("{:<30} | {:<15} | {}",
                channel_config.channel.name,
                repo_name,
                repo_url
            );
        }
    }

    println!("{}", "-".repeat(100));
    Ok(())
}

