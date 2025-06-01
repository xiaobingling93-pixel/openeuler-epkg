use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{SystemTime, Duration};
use sha2::{Sha256, Digest};
use filetime;
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre;
use crate::models::*;

use crate::dirs;
use crate::download::download_urls;
use crate::download::DownloadTask;
use crate::download::submit_download_task;
use crate::download::DOWNLOAD_MANAGER;
use crate::mmio;

#[derive(Clone)]
#[derive(Debug)]
pub struct RepoRevise {
    pub format: PackageFormat,
    pub arch: String,
    pub channel: String,
    pub repo_name: String,
    pub repodata_name: String,
    pub index_url: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct RepoReleaseItem {
    pub format: PackageFormat,
    pub repo_name: String,
    pub repodata_name: String,
    pub need_download: bool,
    pub need_convert: bool,
    pub arch: String,
    pub url: String,
    pub package_baseurl: String,
    pub hash_type: String,
    pub hash: String,
    pub size: u64,
    pub location: String,
    pub is_packages: bool,
    pub download_path: PathBuf,
    pub output_path: PathBuf,
}

#[allow(dead_code)]
impl PackageManager {

    // Replace variables in the index_url string with actual values
    // Examples:
    // input:  $mirror/debian/dists/$VERSION/Release
    // output: https://mirrors.huaweicloud.com///debian/dists/TRIXIE/contrib/Release
    //
    // Variables:
    // - $mirror: the top priority mirror that supports the distribution
    // - $VERSION: the upper case version string
    // - $version: the version string
    // - $repo: the repository name
    // - $arch: the architecture name
    #[allow(dead_code)]
    pub fn interpolate_index_url(&mut self, config: &ChannelConfig, repo_name: &str, index_url: &str) -> Result<String> {
        let mirrors = self.get_mirrors()?;
        // Get mirrors for the distribution and filter by support
        let filtered_mirrors: Vec<&Mirror> = mirrors
            .values()
            .filter(|mirror| mirror.supports.contains(&config.distro))
            .collect();
        let mut combined_mirrors: Vec<&Mirror> = filtered_mirrors.into_iter().collect();
        combined_mirrors.extend(config.mirrors.iter());
        // Avoid borrowing self mutably and immutably at the same time
        let selected_mirror = {
            let mirrors_ref = &combined_mirrors;
            select_mirror(mirrors_ref, &config.distro, config.format.clone())?
        };

        let mut url = index_url.to_string();

        if !url.contains("$mirror") {
            // Find the first '/' after '://'
            if let Some(pos) = url.find("://") {
                let rest = &url[pos + 3..]; // Skip past '://'
                if let Some(slash_pos) = rest.find('/') {
                    let replace_pos = pos + 3 + slash_pos;
                    url.replace_range(replace_pos..replace_pos + 1, "///");
                }
            }
        } else {
            url = url.replace("$mirror", &selected_mirror);
        }

        url = url.replace("$VERSION", &config.version.to_uppercase());
        url = url.replace("$version", &config.version);
        url = url.replace("$repo", repo_name);
        url = url.replace("$arch", &config.arch);

        Ok(url)
    }

    pub fn revise_channel_metadata(&mut self) -> Result<()> {
        let channel_config = self.get_channel_config(config().common.env.clone())?;

        let all_repos = get_revise_repos(channel_config.clone())?;
        revise_repos(channel_config.format.clone(), all_repos)?;

        Ok(())
    }

}

/// Selects the highest priority mirror for a given distribution.
///
/// # Arguments
/// * `mirrors` - Map of distribution names to their available mirrors
/// * `distro` - The distribution to find a mirror for
///
/// # Returns
/// * `Result<String>` - The selected mirror URL with appropriate path formatting
///
/// # Behavior
/// * Sorts by mirror priority
/// * For top_level=true mirrors, appends "//" to the URL
/// * For other levels, appends "/$distro//" to the URL
#[allow(dead_code)]
fn select_mirror(mirrors: &Vec<&Mirror>, distro: &str, format: PackageFormat) -> Result<String> {
    if mirrors.is_empty() {
        return Err(eyre::eyre!("No supported mirrors found for distro: {}", distro));
    }

    // Sort by priority in descending order (highest priority first)
    let mut sorted_mirrors = mirrors.clone();
    sorted_mirrors.sort_by(|a, b| b.priority.cmp(&a.priority));

    // Select highest priority mirror and format URL appropriately
    let mirror = sorted_mirrors.first().unwrap();
    let url = if mirror.top_level || format == PackageFormat::Deb {
        format!("{}//", mirror.url.trim_end_matches('/'))
    } else {
        format!("{}///{}", mirror.url.trim_end_matches('/'), distro)
    };

    Ok(url)
}

pub fn get_revise_repos(config: ChannelConfig) -> Result<Vec<RepoRevise>> {
    let mut all_repos: Vec<RepoRevise> = Vec::new();

    for (repo_name, repo_config) in &config.repos {
        // Skip disabled repos
        if !repo_config.enabled {
            continue;
        }
        // Use repo-specific index_url if present, else fallback to config.index_url
        let index_url = repo_config.index_url.as_ref().unwrap_or(&config.index_url);
        all_repos.push(RepoRevise {
            format: config.format.clone(),
            arch: config.arch.clone(),
            channel: config.channel.clone(),
            repo_name: repo_name.clone(),
            repodata_name: repo_name.clone(),
            index_url: index_url.clone(),
        });
        if let Some(updates_url) = &repo_config.index_url_updates {
            all_repos.push(RepoRevise {
                format: config.format.clone(),
                arch: config.arch.clone(),
                channel: config.channel.clone(),
                repo_name: repo_name.clone(),
                repodata_name: format!("{}-updates", repo_name),
                index_url: updates_url.clone(),
            });
        }
        if let Some(security_url) = &repo_config.index_url_security {
            all_repos.push(RepoRevise {
                format: config.format.clone(),
                arch: config.arch.clone(),
                channel: config.channel.clone(),
                repo_name: repo_name.clone(),
                repodata_name: format!("{}-security", repo_name),
                index_url: security_url.clone(),
            });
        }
    }

    Ok(all_repos)
}

fn revise_repos(format: PackageFormat, all_repos: Vec<RepoRevise>) -> Result<()> {
    let (tx, rx) = mpsc::channel();

    for repo in all_repos {
        let repo = repo.clone();
        let _ = revise_repodata(format.clone(), &repo, &tx);
    }

    // Reader thread (or main thread) waits for all writers to finish
    if config().common.parallel_processing {
        log::debug!("Waiting for revise_repodata() threads");

        drop(tx);
        let mut all_succeed = true;
        while let Ok(succeed) = rx.recv() {
            // wait for all threads dropped its tx channel
            all_succeed = all_succeed && succeed;
        }
        if !all_succeed {
            return Err(eyre::eyre!("Failed to revise repodata"));
        }
    }

    Ok(())
}

pub fn url_to_cache_path(url: &str) -> Result<PathBuf> {
    // Find the '///' and replace everything before it with the cache dir
    let cache_root = dirs().epkg_downloads_cache.clone();
    if let Some(idx) = url.find("///") {
        let rel = &url[idx + 3..];
        Ok(cache_root.join(rel))
    } else if let Some(after_scheme) = url.split("://").nth(1) {
        Ok(cache_root.join(after_scheme))
    } else {
        // this should never happen, error instead
        eyre::bail!("Error: cannot determine cache path for url: {}", url);
    }
}

fn is_file_recent(path: &PathBuf, max_age: Duration) -> Result<bool> {
    let metadata = fs::metadata(path)?;
    let modified = metadata.modified()?;
    let now = SystemTime::now();
    if let Ok(duration) = now.duration_since(modified) {
        Ok(duration < max_age)
    } else {
        Ok(false)
    }
}

fn touch_file_mtime(path: &PathBuf) -> Result<()> {
    let now = SystemTime::now();
    filetime::set_file_mtime(path, filetime::FileTime::from_system_time(now))?;
    Ok(())
}

fn check_repo_index_age(index_path: &PathBuf, duration: std::time::Duration) -> Result<bool> {
    let is_recent = is_file_recent(&index_path, duration)?;
    if !is_recent {
        touch_file_mtime(&index_path)?;
    }
    Ok(is_recent)
}

fn should_skip_duplicate_downloads(path: &PathBuf) -> bool {
    // Prevent duplicate downloads
    use std::sync::LazyLock;
    static DOWNLOADING_RELEASES: LazyLock<std::sync::Mutex<HashSet<PathBuf>>> =
        LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

    // Thread-safe access to static HashSet
    let mut downloading = DOWNLOADING_RELEASES.lock().unwrap();
    if downloading.contains(path) {
        return true;
    }

    downloading.insert(path.clone());
    return false;
}

fn should_refresh_release_file(path: &PathBuf, repo: &RepoRevise) -> Result<bool> {
    let expire_secs = config().common.metadata_expire;

    if !path.exists() {
        return Ok(true);
    }

    let repo_dir = dirs::get_repo_dir(&repo).unwrap();
    let index_path = repo_dir.join("RepoIndex.json");
    if !index_path.exists() {
        return Ok(true);
    }

    // if never auto update
    if expire_secs == 0 && config().subcommand != "update" {
        return Ok(false);
    }

    // if not always update
    if !(expire_secs < 0 || config().subcommand == "update") {
        let duration = std::time::Duration::from_secs(expire_secs.try_into().unwrap());
        // Check if release file is recent
        if is_file_recent(path, duration)? {
            // If release file is recent, check repo index age
            if check_repo_index_age(&index_path, duration)? {
                return Ok(false);
            }
        }
    }

    Ok(true)
}

pub fn refresh_release_file(path: &PathBuf, repo: &RepoRevise) -> Result<()> {
    if !should_refresh_release_file(path, repo)? {
        return Ok(());
    }

    if should_skip_duplicate_downloads(path) {
        return Ok(());
    }

    // Download Release file
    download_urls(vec![repo.index_url.clone()], &dirs().epkg_downloads_cache, 6, false)?;
    Ok(())
}

pub fn revise_repodata(format: PackageFormat, repo: &RepoRevise, result_tx: &mpsc::Sender<bool>) -> Result<bool> {
    let repo_dir = dirs::get_repo_dir(&repo).unwrap();
    let release_path = url_to_cache_path(&repo.index_url)?;

    refresh_release_file(&release_path, &repo)?;

    // Parse Release file
    let release_content = fs::read_to_string(&release_path)
        .with_context(|| format!("Failed to read Release file: {}", release_path.display()))?;
    let release_dir = release_path.parent().unwrap();
    let release_items =
        match format {
            PackageFormat::Deb => crate::deb_repo::parse_release_file(&repo, &release_content, &release_dir.to_path_buf())?,
            _ => return Err(eyre::eyre!("Unsupported package format: {:?}", format))
        };

    let repo_dir = Arc::new(repo_dir.clone());

    // Filter out items that don't need revision
    let release_items_clone = release_items.clone();
    let revises: Vec<_> = release_items_clone.iter()
        .filter(|revise| revise.need_download || revise.need_convert)
        .cloned()
        .collect();

    if revises.is_empty() {
        if config().subcommand == "update" {
            return Ok(false);
        } else {
            // `epkg install/upgrade/remove` need continue to load RepoIndex below
        }
    }

    log::debug!("repo: {:?}", repo);
    log::debug!("revises: {:#?}", revises);

    if config().common.parallel_processing {
        let release_items_clone2 = release_items.clone();
        process_revises_parallel(repo, revises, repo_dir, release_items_clone2, result_tx.clone());
    } else {
        process_revises_sequential(repo, revises, &repo_dir, release_items)?;
    }
    Ok(true)
}

fn process_revises_sequential(
    repo: &RepoRevise,
    revises: Vec<RepoReleaseItem>,
    repo_dir: &PathBuf,
    release_items: Vec<RepoReleaseItem>,
) -> Result<()> {
    let no_revises = revises.is_empty();

    // Process files sequentially
    for revise in &revises {
        download_and_process_item(&revise, repo_dir)?;
    }

    create_load_repoindex(&repo, no_revises, &repo_dir, release_items)?;

    Ok(())
}

fn process_revises_parallel(
    repo: &RepoRevise,
    revises: Vec<RepoReleaseItem>,
    repo_dir: Arc<PathBuf>,
    release_items_clone2: Vec<RepoReleaseItem>,
    result_tx: mpsc::Sender<bool>
) {
    // Clone the repo to avoid lifetime issues
    let repo_clone = repo.clone();
    std::thread::spawn(move || {
        let mut handles = Vec::new();
        let no_revises = revises.is_empty();

        // Process files in parallel std::thread
        for revise in revises {
            let repo_dir = Arc::clone(&repo_dir);
            let revise = revise.clone();

            let handle = std::thread::spawn(move || {
                download_and_process_item(&revise, &repo_dir)
            });

            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            let _ = handle.join().unwrap();
        }

        if let Err(e) = create_load_repoindex(&repo_clone, no_revises, &repo_dir, release_items_clone2) {
            log::error!("Failed to save repo index json: {}", e);
            let _ = result_tx.send(false);
        } else {
            let _ = result_tx.send(true);
        }
    });
}

/// Download and process a single Debian release item
fn download_and_process_item(revise: &RepoReleaseItem, repo_dir: &PathBuf) -> Result<FileInfo> {
    let (data_tx, data_rx) = channel();

    // Create and submit download task
    let task = DownloadTask::new(
        revise.url.clone(),
        dirs().epkg_downloads_cache.clone(),
        6
    ).with_data_channel(data_tx);

    // Submit download task
    submit_download_task(task)?;

    let _ = &DOWNLOAD_MANAGER.start_processing()?;

    log::debug!("process_data for {:?}", revise);
    // Process data blocks as they arrive
    process_data(data_rx, repo_dir, revise)
}

fn create_load_repoindex(
    repo: &RepoRevise,
    no_revises: bool,
    repo_dir: &PathBuf,
    release_items: Vec<RepoReleaseItem>,
) -> Result<()> {
    let mut repo_index: RepoIndex =
        if no_revises {
            mmio::deserialize_repoindex(&repo_dir.join("RepoIndex.json"))?
        } else {
            collect_save_repoindex(&repo, repo_dir, &release_items)?
        };

    if let Some(baseurl) = release_items.get(0).map(|item| item.package_baseurl.clone()) {
        repo_index.package_baseurl = baseurl;
    }

    mmio::populate_repoindex_data(&repo, repo_index)?;

    Ok(())
}

/// Collect packages metafiles and save repo index
fn collect_save_repoindex(repo: &RepoRevise, _repo_dir: &PathBuf, release_items: &[RepoReleaseItem]) -> Result<RepoIndex> {
    let mut packages_metafiles = Vec::new();
    for info in release_items {
        if info.is_packages {
            let json_path = info.output_path.with_extension("json").to_str()
                .ok_or_else(|| eyre::eyre!("Invalid packages metafile path"))?
                .replace("packages", ".packages");
            packages_metafiles.push(PathBuf::from(json_path));
        }
    }
    save_repo_index_json(&repo, packages_metafiles)
}

// When to call: RepoIndex.json not exist, or at least one packages metafile changed
// What to pass: ALL packages metafiles, including the revised AND not changed ones
pub fn save_repo_index_json(repo: &RepoRevise, packages_metafiles: Vec<PathBuf>) -> Result<RepoIndex> {
    log::debug!("save_repo_index_json for {:#?}", packages_metafiles);

    // Get the repo directory from the first metafile
    let cloned = packages_metafiles.clone();
    let repo_dir = cloned[0].parent()
        .ok_or_else(|| eyre::eyre!("Invalid packages metafile path"))?;

    let mut repo_shards = HashMap::new();

    // Process each packages metafile
    for (i, packages_metafile) in packages_metafiles.iter().enumerate() {
        // Load packages info
        let packages_info_str = fs::read_to_string(&packages_metafile)
            .with_context(|| format!("Failed to read packages metafile: {}", packages_metafile.display()))?;
        let packages_info: FileInfo = serde_json::from_str(&packages_info_str)
            .with_context(|| format!("Failed to parse packages info from: {}", packages_metafile.display()))?;

        // Try to load corresponding filelist if it exists
        let mut filelist_info = None;
        let filelist_metafile = packages_metafile.to_str()
            .ok_or_else(|| eyre::eyre!("Invalid packages metafile path"))?
            .replace(".packages", ".filelist");
        if Path::new(&filelist_metafile).exists() {
            let filelist_content = fs::read_to_string(&filelist_metafile)
                .with_context(|| format!("Failed to read filelist: {}", filelist_metafile))?;
            let filelist: FileInfo = serde_json::from_str(&filelist_content)
                .with_context(|| format!("Failed to parse filelist info from: {}", filelist_metafile))?;
            filelist_info = Some(filelist);
        }

        // Use file stem as key, fallback to shard_i
        let key = packages_metafile.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.trim_start_matches('.').to_owned())
            .unwrap_or_else(|| format!("shard_{}", i));
        repo_shards.insert(key, RepoShard {
            packages: packages_info,
            filelist: filelist_info,
            essential_pkgnames: std::collections::HashSet::new(),
            provide2pkgnames:   std::collections::HashMap::new(),
            pkgname2ranges:     std::collections::HashMap::new(),
            packages_mmap:      None,
        });
    }

    // Save the index for the repo
    let repo_index = RepoIndex { repodata_name: repo.repodata_name.clone(), package_baseurl: String::new(), repo_shards };
    let index_path = repo_dir.join("RepoIndex.json");
    fs::write(&index_path, serde_json::to_string_pretty(&repo_index)?)
        .with_context(|| format!("Failed to write repo index to: {}", index_path.display()))?;

    Ok(repo_index)
}

pub fn process_data(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<FileInfo> {
    if revise.is_packages {
        match revise.format {
            PackageFormat::Deb => crate::deb_repo::process_packages_content(data_rx, repo_dir, revise),
            _ => Err(eyre::eyre!("Unsupported package format: {:?}", revise.format))
        }
    } else {
        process_filelist_content(data_rx, repo_dir, revise)
    }
}

pub fn process_filelist_content(data_rx: Receiver<Vec<u8>>, _repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<FileInfo> {
    log::debug!("Processing filelist content for arch: {:?}", revise);
    let mut hasher = Sha256::new();

    // Process data and calculate hash incrementally
    while let Ok(data) = data_rx.recv() {
        hasher.update(&data);
    }

    // Verify hash
    let calculated_hash = hex::encode(hasher.finalize());
    if calculated_hash != revise.hash {
        log::error!("Hash verification failed for {}: expected {}, got {}",
            revise.location, revise.hash, calculated_hash);
        return Err(eyre::eyre!("Hash verification failed for {}: expected {}, got {}",
            revise.location, revise.hash, calculated_hash));
    }
    log::debug!("Hash verification successful for {}", revise.location);

    // Create symbolic link from contents_path to repo_dir
    // "Contents-all.gz"
    let output_path = revise.output_path.clone();
    let json_path = output_path.with_extension("json").to_str()
            .ok_or_else(|| eyre::eyre!("Invalid packages metafile path"))?
            .replace("filelist", ".filelist");
    if output_path.exists() {
        log::debug!("Removing existing filelist at {}", output_path.display());
        fs::remove_file(&output_path)
            .with_context(|| format!("Failed to remove existing filelist at {}", output_path.display()))?;
    }

    log::debug!("Creating symlink from {} to {}", revise.download_path.display(), output_path.display());
    #[cfg(unix)]
    std::os::unix::fs::symlink(revise.download_path.clone(), &output_path)
        .with_context(|| format!("Failed to create symlink from {} to {}",
            revise.download_path.display(), output_path.display()))?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(revise.download_path, &output_path)
        .with_context(|| format!("Failed to create symlink from {} to {}",
            revise.download_path.display(), output_path.display()))?;

    let metadata = fs::metadata(&output_path)
        .with_context(|| format!("Failed to get metadata for {}", output_path.display()))?;
    let file_info = FileInfo {
        filename: output_path.file_name()
            .ok_or_else(|| eyre::eyre!("Failed to get filename from path: {}", output_path.display()))?
            .to_string_lossy()
            .into_owned(),
        sha256sum: calculated_hash,
        datetime: metadata.modified()?
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_secs()
            .to_string(),
        size: metadata.len(),
    };

    log::debug!("Writing filelist metadata to {}", json_path);
    let json_content = serde_json::to_string_pretty(&file_info)
        .with_context(|| format!("Failed to serialize file info to JSON for {}", output_path.display()))?;
    fs::write(&json_path, json_content)
        .with_context(|| format!("Failed to write JSON metadata to {}", json_path))?;

    log::debug!("Successfully processed filelist content for arch: {}", revise.arch);
    Ok(file_info)
}


pub fn list_repos() -> Result<()> {
    let common_env_root = dirs::find_env_root("common")
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
            let repo_url = match &repo_config.index_url {
                Some(url) => url.clone(),
                None => format!(
                    "{}/{}/{}",
                    channel_config.index_url.clone(),
                    &repo_name,
                    config().common.arch,
                )
            };

            println!("{:<30} | {:<15} | {}",
                channel_config.channel,
                repo_name,
                repo_url
            );
        }
    }

    println!("{}", "-".repeat(100));
    Ok(())
}
