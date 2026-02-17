use std::collections::HashMap;
use std::fs;
use crate::lfs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::time::{SystemTime, Duration};
use sha2::{Sha256, Sha512, Digest};
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use crate::models::*;
use crate::dirs;

use crate::download::DownloadTask;
use crate::download::{submit_download_task, has_download_task, DownloadStatus};
use crate::download::DOWNLOAD_MANAGER;
use crate::io::read_json_file;
use crate::mmio;
use crate::utils::append_suffix;
use crate::posix::posix_utime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseStatus {
    NeedDownload,
    NeedUpdate,
    FineExist,
    FineRecent,
}

#[derive(Clone)]
#[derive(Debug)]
#[derive(Default)]
pub struct RepoRevise {
    #[allow(dead_code)]
    pub format: PackageFormat,
    pub arch: String,
    pub channel: String,
    pub repo_name: String,
    pub repodata_name: String,
    pub index_url: String,
    pub components: Vec<String>, // DEB specific: filter components from Release file
}

#[allow(dead_code)]
#[derive(Default, Debug, Clone)]
pub struct RepoReleaseItem {
    pub repo_revise: RepoRevise,        // Repository information
    pub need_download: bool,
    pub need_convert: bool,
    pub arch: String,
    pub url: String,
    pub package_baseurl: String,
    pub hash_type: String,
    pub hash: String,
    pub size: usize,
    pub location: String,
    pub is_packages: bool,
    pub is_adb: bool,                   // Whether this is an ADB (Alpine/Arch Database) file
    pub download_path: PathBuf,
    pub output_path: PathBuf,
}

pub enum HashType {
    Sha256(Sha256),
    Sha512(Sha512),
}

impl HashType {
    pub fn update(&mut self, data: &[u8]) {
        match self {
            HashType::Sha256(hasher) => hasher.update(data),
            HashType::Sha512(hasher) => hasher.update(data),
        }
    }

    fn finalize(self) -> Vec<u8> {
        match self {
            HashType::Sha256(hasher) => hasher.finalize().to_vec(),
            HashType::Sha512(hasher) => hasher.finalize().to_vec(),
        }
    }

    pub fn finalize_reset(&mut self) -> Vec<u8> {
        match self {
            HashType::Sha256(hasher) => hasher.finalize_reset().to_vec(),
            HashType::Sha512(hasher) => hasher.finalize_reset().to_vec(),
        }
    }
}

pub fn sync_channel_metadata() -> Result<()> {
    let channel_configs = crate::models::channel_configs();
    let mut all_repos = Vec::new();

    // Collect all repos first
    for channel_config in channel_configs {
        let repos = get_revise_repos(channel_config.clone())
            .with_context(|| "Failed to get repository revision information")?;

        crate::mirror::extend_repodata_name2distro_dirs(&channel_config, &repos)
            .with_context(|| "Failed to set up repodata_name2distro_dirs hashmap")?;

        all_repos.extend(repos);
    }

    revise_repos(all_repos)
        .with_context(|| "Failed to process repository revisions")?;

    Ok(())
}

/// Download a single file using DownloadTask with repodata_name
pub fn download_file_with_repodata_name(url: &str, repodata_name: &str) -> Result<()> {
    use crate::download::DownloadFlags;
    let task = DownloadTask::with_size(
        url.to_string(),
        None,
        repodata_name.to_string(),
        DownloadFlags::empty()  // Not an ADB file
    )
    .with_context(|| format!("Failed to create download task for URL: {}", url))?;
    submit_download_task(task)
        .with_context(|| format!("Failed to submit download task for {}", url))?;
    DOWNLOAD_MANAGER.start_processing();
    // Wait for the download to complete
    let (status, _) = DOWNLOAD_MANAGER.wait_for_task(url.to_string())
        .with_context(|| format!("Failed to wait for download from {}", url))?;
    if let DownloadStatus::Failed(err_msg) = status {
        return Err(eyre::eyre!("Download failed for {}: {}", url, err_msg));
    }
    Ok(())
}

fn get_revise_repos(config: ChannelConfig) -> Result<Vec<RepoRevise>> {
    let mut all_repos: Vec<RepoRevise> = Vec::new();

    // config.repos should never be empty for valid configurations
    for (repo_name, repo_config) in &config.repos {
        // Skip disabled repos
        if !repo_config.enabled {
            continue;
        }

        all_repos.push(RepoRevise {
            format: config.format.clone(),
            arch: config.arch.clone(),
            channel: config.channel.clone(),
            repo_name: repo_name.clone(),
            repodata_name: repo_name.clone(),
            // Channel defaults have already been merged by merge_channel_defaults_into_repos()
            index_url: repo_config.index_url.clone(),
            components: repo_config.components.clone(),
        });

        for (suffix, url) in &repo_config.amend_index_urls {
            let (repodata_suffix, arch) = match suffix.as_str() {
                "noarch" => ("noarch",   "all"), // Use "all" for noarch, not the system arch
                _ => (suffix.as_str(), config.arch.as_str()),
            };

            all_repos.push(RepoRevise {
                format: config.format.clone(),
                arch: arch.to_string(),
                channel: config.channel.clone(),
                repo_name: repo_name.clone(),
                repodata_name: format!("{}-{}", repo_name, repodata_suffix),
                index_url: url.clone(),
                components: repo_config.components.clone(),
            });
        }
    }

    log::debug!("get_revise_repos: {:#?}", all_repos);
    Ok(all_repos)
}

/// Process all repository items in parallel with deduplication
fn revise_repos(all_repos: Vec<RepoRevise>) -> Result<()> {
    log::debug!("Starting with {} repositories", all_repos.len());

    // Collect all release items from all repositories
    let all_release_items = if all_repos.len() > 1 && config().common.parallel_processing {
        let items = collect_all_repo_metadata_parallel(all_repos)?;
        items
    } else {
        let mut items = Vec::new();
        for repo in all_repos {
            let release_items = collect_repo_metadata(&repo)?;
            items.extend(release_items);
        }
        items
    };

    process_all_release_items(all_release_items)
}

fn collect_all_repo_metadata_parallel(all_repos: Vec<RepoRevise>) -> Result<Vec<RepoReleaseItem>> {
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();

    for repo in all_repos {
        let tx = tx.clone();
        let repo_clone = repo.clone();

        let handle = std::thread::spawn(move || {
            let result = collect_repo_metadata(&repo_clone);
            if let Err(ref e) = result {
                log::error!("Failed to collect metadata for repo {}: {:#}", repo_clone.repo_name, e);
            }

            if let Err(e) = tx.send(result) {
                log::error!("Failed to send metadata collection result for repo {}: {}", repo_clone.repo_name, e);
            }
        });

        handles.push(handle);
    }

    // Drop the original sender so the receiver loop will exit once all
    // worker threads have finished sending their results.
    drop(tx);

    let mut all_succeed = true;
    let mut all_release_items = Vec::new();

    while let Ok(result) = rx.recv() {
        match result {
            Ok(mut items) => {
                all_release_items.append(&mut items);
            }
            Err(e) => {
                all_succeed = false;
                log::error!("Error while collecting repository metadata in parallel: {:#}", e);
            }
        }
    }

    // Ensure all worker threads have completed successfully
    for handle in handles {
        if let Err(e) = handle.join() {
            log::error!("Metadata collection thread panicked: {:?}", e);
            all_succeed = false;
        }
    }

    if !all_succeed {
        return Err(eyre::eyre!("Failed to collect repository metadata for one or more repositories"));
    }

    Ok(all_release_items)
}

/// Determine if filelists are needed based on the current command and options
pub fn need_filelists() -> bool {
    (config().subcommand == EpkgCommand::Update && config().update.need_files) ||
    (config().subcommand == EpkgCommand::Search && (config().search.files || config().search.paths))
}

fn process_all_release_items(all_release_items: Vec<RepoReleaseItem>) -> Result<()> {
    log::debug!("Collected {} total release items", all_release_items.len());

    // Deduplicate release items based on URL to prevent downloading the same file multiple times
    let deduplicated_items = deduplicate_release_items_by_url(all_release_items);
    log::debug!("After deduplication: {} items", deduplicated_items.len());
    log::debug!("Unique revises: {:#?}", deduplicated_items);

    // Filter out items that don't need revision
    let need_filelists = need_filelists();

    let revises: Vec<_> = deduplicated_items.iter()
        .filter(|revise| revise.need_download || revise.need_convert)
        .filter(|revise| revise.is_packages || need_filelists)
        .cloned()
        .collect();

    if revises.is_empty() {
        if config().subcommand == EpkgCommand::Update {
            log::debug!("No items need processing");
            return Ok(());
        } else {
            // `epkg install/upgrade/remove` need continue to load RepoIndex below
        }
    }

    log::debug!("Filtered revises: {:#?}", revises);

    // Process all items in parallel or sequentially
    if config().common.parallel_processing {
        process_revises_parallel(revises.clone())?;
    } else {
        process_revises_sequential(revises.clone())?;
    }

    // Group items by repository and create indexes
    create_repository_indexes(revises, deduplicated_items)?;

    Ok(())
}

/// Group items by repository and create indexes
fn create_repository_indexes(revises: Vec<RepoReleaseItem>, deduplicated_items: Vec<RepoReleaseItem>) -> Result<()> {
    log::debug!("Creating repository indexes for {} items", deduplicated_items.len());

    // Group items by repository
    let mut repo_items_map: std::collections::HashMap<String, Vec<RepoReleaseItem>> = std::collections::HashMap::new();
    let mut revises_map: std::collections::HashMap<String, Vec<RepoReleaseItem>> = std::collections::HashMap::new();
    for revise in deduplicated_items.iter() {
        repo_items_map.entry(revise.repo_revise.repodata_name.clone()).or_default().push(revise.clone());
    }
    for revise in revises.iter() {
        revises_map.entry(revise.repo_revise.repodata_name.clone()).or_default().push(revise.clone());
    }

    // Create repo indexes for each repository
    for (repodata_name, release_items) in repo_items_map {
        if let Some(first_item) = release_items.first() {
            let repo = &first_item.repo_revise;
            // Use the standard get_repo_dir() - repo.arch is already set correctly (e.g., "all" for noarch)
            let repo_dir = dirs::get_repo_dir(repo);
            let no_revises = !revises_map.contains_key(&repodata_name);
            create_load_repoindex(repo, no_revises, &repo_dir, release_items.clone())
                .with_context(|| format!("Failed to create and load repository index for: {}", repodata_name))?;
        }
    }

    Ok(())
}

/// Deduplicate release items based on URL to prevent downloading the same file multiple times
fn deduplicate_release_items_by_url(release_items: Vec<RepoReleaseItem>) -> Vec<RepoReleaseItem> {
    let mut seen_urls = std::collections::HashSet::new();
    let mut filtered_items = Vec::new();
    let mut duplicates_removed = 0;

    log::debug!("Starting deduplication of {} release items", release_items.len());

    for item in release_items {
        let key = item.url.clone();
        if seen_urls.insert(key) {
            filtered_items.push(item);
        } else {
            duplicates_removed += 1;
            log::debug!("Skipping duplicate download for URL {} (location: {}, repodata_name: {})",
                       item.url, item.location, item.repo_revise.repodata_name);
        }
    }

    if duplicates_removed > 0 {
        log::info!("Removed {} duplicate download entries to prevent race conditions", duplicates_removed);
    }

    log::debug!("Deduplication complete: {} items remaining after removing {} duplicates",
                filtered_items.len(), duplicates_removed);

    filtered_items
}

fn has_recent_download(path: &PathBuf, max_age: Duration) -> Result<bool> {
    if is_file_recent(path, &max_age)? {
        return Ok(true);
    }
    let etag_path = append_suffix(path, "etag.json");
    log::debug!("has_recent_download: checking etag file, path={}, etag_path={}, max_age={}s", path.display(), etag_path.display(), max_age.as_secs());
    if is_file_recent(&etag_path, &max_age)? {
        Ok(true)
    } else {
        let _ = posix_utime(etag_path, None, None);
        Ok(false)
    }
}

fn is_file_recent(path: &PathBuf, max_age: &Duration) -> Result<bool> {
    if !path.exists() {
        log::debug!("is_file_recent: file does not exist, returning false, path={}, max_age={}s", path.display(), max_age.as_secs());
        return Ok(false);
    }
    let metadata = fs::metadata(path)
        .with_context(|| format!("Failed to get metadata for file: {}", path.display()))?;
    let modified = metadata.modified()
        .with_context(|| format!("Failed to get modification time for file: {}", path.display()))?;
    let now = SystemTime::now();
    if let Ok(age) = now.duration_since(modified) {
        let is_recent = age < *max_age;
        log::debug!("is_file_recent: file age={}s, max_age={}s, is_recent={}, path={}", age.as_secs(), max_age.as_secs(), is_recent, path.display());
        Ok(is_recent)
    } else {
        log::debug!("is_file_recent: modified time is in the future, returning false, path={}, max_age={}s", path.display(), max_age.as_secs());
        Ok(false)
    }
}

pub fn should_refresh_release_file(path: &PathBuf, repo: &RepoRevise) -> Result<ReleaseStatus> {
    // Check if this URL is already being processed by the download manager
    if has_download_task(&repo.index_url) {
        log::debug!("should_refresh_release_file: URL {} already being processed, returning FineRecent", repo.index_url);
        return Ok(ReleaseStatus::FineRecent);
    }

    if !path.exists() {
        log::debug!("should_refresh_release_file: path does not exist: {}, returning NeedDownload", path.display());
        return Ok(ReleaseStatus::NeedDownload);
    }

    if config().subcommand != EpkgCommand::Update {
        let expire_secs = config().common.metadata_expire;

        if expire_secs == 0 {
            log::debug!("should_refresh_release_file: never auto update, returning FineExist");
            return Ok(ReleaseStatus::FineExist);
        }

        if expire_secs > 0 {
            let duration = std::time::Duration::from_secs(expire_secs.try_into()
                .map_err(|e| eyre::eyre!("Failed to convert metadata_expire to u64: {}", e))?);
            // Check if release file download cache is recent
            if has_recent_download(&path, duration)? {
                log::debug!("should_refresh_release_file: all files are recent, returning FineRecent");
                return Ok(ReleaseStatus::FineRecent);
            }
        }
    }

    log::debug!("should_refresh_release_file: returning NeedUpdate");
    Ok(ReleaseStatus::NeedUpdate)
}

fn refresh_release_file(path: &PathBuf, repo: &RepoRevise) -> Result<()> {
    let status = should_refresh_release_file(path, repo)
        .with_context(|| format!("Failed to check if release file needs refreshing: {}", path.display()))?;

    if status == ReleaseStatus::FineExist || status == ReleaseStatus::FineRecent {
        return Ok(());
    }

    // Download Release file using the new helper function
    download_file_with_repodata_name(&repo.index_url, &repo.repodata_name)
        .with_context(|| format!("Failed to download release file from {}", repo.index_url))?;
    Ok(())
}

/// Download/Parse the Release/repomd.xml file for a repository and return the release items
fn sync_from_release_metadata(repo: &RepoRevise, release_path: &PathBuf) -> Result<Vec<RepoReleaseItem>> {
    let release_dir = release_path.parent()
        .ok_or_else(|| eyre::eyre!("Failed to get parent directory of release path: {}", release_path.display()))?;

    lfs::create_dir_all(release_dir)?;
    refresh_release_file(&release_path, &repo)
        .with_context(|| format!("Failed to refresh release file for repository: {}", repo.repo_name))?;

    // Parse Release file
    let release_content = fs::read_to_string(&release_path)
        .with_context(|| format!("Failed to read Release file: {}", release_path.display()))?;
    let release_items =
        match repo.format {
            PackageFormat::Deb => crate::deb_repo::parse_release_file(&repo, &release_content, &release_dir.to_path_buf())?,
            PackageFormat::Rpm => crate::rpm_repo::parse_repomd_file(&repo, &release_content, &release_dir.to_path_buf())?,
            _ => return Err(eyre::eyre!("Unsupported package format: {:?}", repo.format)),
        };

    Ok(release_items)
}

// index_url: $mirror/v$version/$repo/$arch/APKINDEX.tar.gz
fn sync_from_package_database(repo: &RepoRevise, packages_path: &mut PathBuf) -> Result<Vec<RepoReleaseItem>> {

    // For Pacman format, conditionally modify index_url based on need_filelists
    let mut index_url = repo.index_url.clone();
    if repo.format == PackageFormat::Pacman {
        let need_filelists = need_filelists();
        if need_filelists {
            // Change .db.tar to .files.tar
            index_url = index_url.replace(".db.tar", ".files.tar");
            let path_str = packages_path.to_string_lossy().to_string();
            *packages_path = PathBuf::from(path_str.replace(".db.tar", ".files.tar"));
        } else {
            // Change .files.tar to .db.tar
            index_url = index_url.replace(".files.tar", ".db.tar");
            let path_str = packages_path.to_string_lossy().to_string();
            *packages_path = PathBuf::from(path_str.replace(".files.tar", ".db.tar"));
        }
    }

    let should_update = should_refresh_release_file(packages_path, repo)?;
    let mut release_items = Vec::new();

    let repo_dir = dirs::get_repo_dir(&repo);

    // Extract package base URL by removing last filename part
    let package_baseurl = if let Some(parent_url) = index_url.rsplitn(2, '/').nth(1) {
        parent_url.to_string()
    } else {
        index_url.clone()
    };

    // Get the filename from the packages_path
    let location = packages_path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string();

    let need_download = should_update == ReleaseStatus::NeedDownload ||
                        should_update == ReleaseStatus::NeedUpdate;

    let need_convert = if !need_download {
        // Check if output file exists and is non-empty
        // .files.tar.gz archives contain both desc and files, but processing may fail
        // or be interrupted, leaving packages.txt empty. Force conversion in such cases.
        let output_path = repo_dir.join("packages.txt");
        let output_exists = output_path.exists();
        let output_size = if output_exists {
            std::fs::metadata(&output_path).ok().map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };
        let need_convert = !output_exists || output_size == 0;
        log::debug!("need_convert check: output_path={}, exists={}, size={}, need_convert={}",
                   output_path.display(), output_exists, output_size, need_convert);
        need_convert
    } else {
        false
    };

    release_items.push(RepoReleaseItem {
        repo_revise: RepoRevise {
            repodata_name: repo.repodata_name.to_string(),
            ..repo.clone()
        },
        need_download: need_download,
        need_convert: need_convert,
        arch: repo.arch.clone(),
        url: index_url,
        package_baseurl: package_baseurl,
        location: location,
        is_packages: true,
        is_adb: true,  // Mark as ADB file (Alpine/Arch Database)
        output_path: repo_dir.join("packages.txt"),
        download_path: packages_path.clone(),
        ..Default::default()
    });

    Ok(release_items)
}

/*
 * REPOSITORY ARCHITECTURE DISPATCHER - Three-Tier Repository Support
 *
 * This function routes repository processing based on URL patterns, supporting
 * three distinct repository architectures with different trade-offs:
 *
 * TYPE 1: Release/repomd.xml Repositories (Enterprise/Distribution Standard)
 * ========================================================================
 * Detection: URLs ending with "Release" or "repomd.xml"
 * Examples:
 *   - Debian: https://deb.debian.org/debian/dists/bookworm/Release
 *   - Ubuntu: http://archive.ubuntu.com/ubuntu/dists/jammy/Release
 *   - CentOS: https://mirror.stream.centos.org/9-stream/BaseOS/x86_64/os/repodata/repomd.xml
 *
 * Architecture:
 *   Release/repomd.xml (metadata index)
 *   ├── Contains SHA256/MD5 hashes for all package database files
 *   ├── Cryptographic signatures for tamper detection
 *   ├── Size verification for download integrity
 *   └── Points to → Packages.xz/primary.xml.gz (rich package databases)
 *       ├── Full dependency information
 *       ├── Package descriptions and metadata
 *       ├── File lists and provides/requires data
 *       └── Hash-verified content addressing
 *
 * Benefits: Maximum reliability, integrity verification, rich metadata
 * Handler: sync_from_release_metadata() → deb_repo.rs / rpm_repo.rs
 *
 * TYPE 2: Direct Package Database Files (Simplified Architecture)
 * ==============================================================
 * Detection: URLs NOT ending with "/" or known metadata files
 * Examples:
 *   - Alpine: https://dl-cdn.alpinelinux.org/alpine/v3.18/main/x86_64/APKINDEX.tar.gz
 *   - Archlinux: https://mirrors.ustc.edu.cn/archlinux/core/os/x86_64/core.files.tar.gz
 *
 * Architecture:
 *   APKINDEX.tar.gz (direct package database)
 *   ├── Contains rich package information directly
 *   ├── No separate metadata layer
 *   ├── Single-file simplicity
 *   └── Repository-specific format (APK, etc.)
 *
 * Benefits: Simpler structure, still rich package info
 * Limitations: No hash verification, potential consistency issues
 * Handler: sync_from_package_database() → format-specific processing
 *
 * TYPE 3: Plain HTML Directory Listings (Maximum Compatibility)
 * ============================================================
 * Detection: URLs ending with "/"
 * Examples:
 *   - Simple mirrors: https://mirror.example.com/packages/
 *   - Basic HTTP: http://internal.company.com/rpms/
 *   - File servers: https://releases.project.org/binaries/
 *
 * Architecture:
 *   index.html (HTTP directory listing)
 *   ├── Simple file listing with minimal metadata
 *   ├── Package info extracted from filenames via regex
 *   ├── No integrity verification available
 *   └── Works with any HTTP server
 *
 * Benefits: Universal compatibility, works anywhere
 * Limitations: Minimal info, no verification, parsing fragility
 * Handler: sync_from_directory_index() → index_html.rs
 *
 * ROUTING LOGIC:
 * The function examines repo.index_url patterns to determine repository type
 * and dispatch to the appropriate handler, enabling unified package management
 * across diverse repository infrastructures.
 */

/// Collect repository metadata and return release items without processing them
fn collect_repo_metadata(repo: &RepoRevise) -> Result<Vec<RepoReleaseItem>> {
    log::debug!("Starting for repo: {} with index_url: {}", repo.repo_name, repo.index_url);

    let repo_dir = dirs::get_repo_dir(&repo);
    log::debug!("Got repo_dir: {:?}", repo_dir);

    let mut release_path = crate::mirror::Mirrors::url_to_cache_path(&repo.index_url, &repo.repodata_name)
        .with_context(|| format!("Failed to convert URL to cache path: {}", repo.index_url))?;
    log::debug!("Got release_path: {:?}", release_path);

    log::debug!("Determining release_items based on index_url: {}", repo.index_url);
    let release_items = if repo.index_url.ends_with("Release") || repo.index_url.ends_with("repomd.xml") {
        sync_from_release_metadata(repo, &release_path)
            .with_context(|| format!("Failed to parse release file for repository: {}", repo.repo_name))?
    } else if repo.index_url.ends_with("/") {
        crate::index_html::sync_from_directory_index(repo.format, repo, &release_path)?;
        Vec::new()
    } else if repo.format == PackageFormat::Conda && (repo.index_url.ends_with("repodata.json") || repo.index_url.ends_with("repodata.json.gz") || repo.index_url.ends_with("repodata.json.bz2")) {
        crate::conda_repo::parse_repodata_json(repo, &release_path.parent().unwrap().to_path_buf())
            .with_context(|| format!("Failed to parse conda repodata.json for repository: {}", repo.repo_name))?
    } else if repo.format == PackageFormat::Pacman && (repo.index_url.contains("packages-meta-ext-v1.json") || repo.repo_name == "aur") {
        // AUR repository - use AUR-specific processing
        crate::aur::parse_aur_metadata(repo, &release_path)
            .with_context(|| format!("Failed to parse AUR metadata for repository: {}", repo.repo_name))?
    } else {
        sync_from_package_database(repo, &mut release_path)
            .with_context(|| format!("Failed to check packages file for repository: {}", repo.repo_name))?
    };
    log::debug!("Got {} release_items", release_items.len());

    Ok(release_items)
}

/// Process all repository items sequentially globally
fn process_revises_sequential(revises: Vec<RepoReleaseItem>) -> Result<()> {
    log::debug!("Starting with {} items", revises.len());

    // Process each item sequentially
    for revise in &revises {
        let repo_dir = dirs::get_repo_dir(&revise.repo_revise);
        download_and_process_item(revise, &repo_dir)
            .with_context(|| format!("Failed to download and process item: {}", revise.location))?;
    }

    Ok(())
}

/// Process all repository items in parallel globally
fn process_revises_parallel(revises: Vec<RepoReleaseItem>) -> Result<()> {
    log::debug!("Starting with {} items", revises.len());

    // Use a bounded channel so any future senders are naturally throttled
    // by the receiver. Capacity is tied to the number of items to avoid
    // unbounded growth if sends are added later.
    let (tx, rx) = mpsc::sync_channel::<Result<(), eyre::Report>>(revises.len().max(1));
    let mut handles = Vec::new();
    let mut errors = Vec::new();

    // Process each item in a separate thread
    for revise in revises {
        let tx = tx.clone();
        let repo_dir = dirs::get_repo_dir(&revise.repo_revise);
        let handle = std::thread::spawn(move || {
            match download_and_process_item(&revise, &repo_dir) {
                Ok(_) => {
                    log::debug!("Successfully processed: {}", revise.download_path.display());
                    let _ = tx.send(Ok(()));
                },
                Err(e) => {
                    log::error!("Failed to process {}, retry fix with 'epkg -e {} update'", revise.location, config().common.env_name);
                    let _ = tx.send(Err(e));
                }
            }
        });
        handles.push(handle);
    }

    // Wait for all threads to complete
    drop(tx);
    while let Ok(result) = rx.recv() {
        if let Err(e) = result {
            errors.push(e);
        }
    }

    // Wait for all handles to complete
    for handle in handles {
        if let Err(e) = handle.join() {
            log::error!("Thread panicked: {:?}", e);
            errors.push(eyre::eyre!("Thread panicked: {:?}", e));
        }
    }

    if !errors.is_empty() {
        if errors.len() == 1 {
            return Err(errors.into_iter().next().unwrap());
        } else {
            let error_msg = format!("Failed to process {} repository items: {}", errors.len(), errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; "));
            return Err(eyre::eyre!(error_msg));
        }
    }

    Ok(())
}

/// Download and process a single Debian release item
fn download_and_process_item(revise: &RepoReleaseItem, repo_dir: &PathBuf) -> Result<()> {
    // Use a bounded channel so the download side is naturally throttled
    // by the processing side. Each message is a ~64KB chunk, so a small
    // buffer keeps at most a few chunks in memory per item.
    const DATA_CHANNEL_BUFFER: usize = 16;
    let (data_tx, data_rx) = mpsc::sync_channel(DATA_CHANNEL_BUFFER);

    // Create and submit download task
    use crate::download::DownloadFlags;
    let flags = if revise.is_adb {
        DownloadFlags::ADB
    } else {
        DownloadFlags::empty()
    };
    let task = DownloadTask::with_size(
        revise.url.clone(),
        if revise.size > 0 { Some(revise.size as u64) } else { None },
        revise.repo_revise.repodata_name.clone(),
        flags
    )
    .with_context(|| format!("Failed to create download task for URL: {}", revise.url))?
    .with_data_channel(data_tx);

    // Submit download task
    submit_download_task(task)
        .with_context(|| format!("Failed to submit download task for URL: {}", revise.url))?;

    DOWNLOAD_MANAGER.start_processing();

    log::debug!("process_data for {:?}", revise);
    log::debug!("Using repo_dir: {:?} for item: {}", repo_dir, revise.location);
    // Process data blocks as they arrive
    process_data(data_rx, repo_dir, revise)
        .with_context(|| format!("Failed to process data for item: {} (format: {:?}, size: {}, hash: {})",
            revise.download_path.display(), revise.repo_revise.format, revise.size, revise.hash))?;
    Ok(())
}

pub fn create_load_repoindex(
    repo: &RepoRevise,
    no_revises: bool,
    repo_dir: &PathBuf,
    release_items: Vec<RepoReleaseItem>,
) -> Result<()> {
    let mut repo_index: RepoIndex =
        if no_revises {
            read_json_file(&repo_dir.join("RepoIndex.json"))
                .with_context(|| format!("Failed to deserialize RepoIndex.json for repository: {}", repo.repo_name))?
        } else {
            collect_save_repoindex(&repo, repo_dir, &release_items)
                .with_context(|| format!("Failed to collect and save repository index for: {}", repo.repo_name))?
        };

    if let Some(baseurl) = release_items.get(0).map(|item| item.package_baseurl.clone()) {
        repo_index.package_baseurl = baseurl;
    }

    mmio::populate_repoindex_data(&repo, repo_index)
        .with_context(|| format!("Failed to populate repository index data for: {}", repo.repo_name))?;

    Ok(())
}

/// Collect packages metafiles and save repo index
fn collect_save_repoindex(repo: &RepoRevise, _repo_dir: &PathBuf, release_items: &[RepoReleaseItem]) -> Result<RepoIndex> {
    log::debug!("Starting for repository: {} with {} release items", repo.repo_name, release_items.len());

    let mut packages_metafiles = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();

    for (i, info) in release_items.iter().enumerate() {
        if info.is_packages {
            log::debug!("Processing packages item {}: {}", i, info.location);
            let json_path = info.output_path.with_extension("json").to_str()
                .ok_or_else(|| eyre::eyre!("Invalid packages metafile path for item {}: {}", i, info.location))?
                .replace("packages", ".packages");
            let metafile_path = PathBuf::from(json_path);
            log::debug!("Generated metafile path: {}", metafile_path.display());

            // Only add if we haven't seen this path before
            if seen_paths.insert(metafile_path.clone()) {
                packages_metafiles.push(metafile_path);
            } else {
                log::debug!("Skipping duplicate metafile path: {}", metafile_path.display());
            }
        }
    }

    log::debug!("Found {} unique packages metafiles for repository: {}", packages_metafiles.len(), repo.repo_name);
    save_repo_index_json(&repo, packages_metafiles)
}

// When to call: RepoIndex.json not exist, or at least one packages metafile changed
// What to pass: ALL packages metafiles, including the revised AND not changed ones
fn save_repo_index_json(repo: &RepoRevise, packages_metafiles: Vec<PathBuf>) -> Result<RepoIndex> {
    log::debug!("save_repo_index_json for {:#?}", packages_metafiles);

    // Check if we have any packages metafiles
    if packages_metafiles.is_empty() {
        log::warn!("No packages metafiles provided for repository: {}. This indicates that packages need to be processed first.", repo.repo_name);
        return Err(eyre::eyre!("No packages metafiles provided for repository: {}. Packages need to be downloaded and processed before creating repo index. Expected metafiles would be generated from packages items in release_items. Current packages_metafiles: {:#?}. This typically means no release_items with is_packages=true were found, or the packages processing step failed to generate the expected .packages.json files.", repo.repo_name, packages_metafiles));
    }

    // Get the repo directory from the first metafile
    let cloned = packages_metafiles.clone();
    let repo_dir = cloned[0].parent()
        .ok_or_else(|| eyre::eyre!("Invalid packages metafile path"))?;

    let mut repo_shards = HashMap::new();

    // Process each packages metafile
    for (i, packages_metafile) in packages_metafiles.iter().enumerate() {
        log::debug!("Processing packages_metafile: {}", packages_metafile.display());

        // Check if the packages metafile exists
        if !packages_metafile.exists() {
            log::warn!("Packages metafile does not exist: {}. This may indicate that packages haven't been processed yet, or processing failed. Check earlier error logs.", packages_metafile.display());
            return Err(eyre::eyre!("Packages metafile does not exist: {}. This may indicate that packages haven't been processed yet, or processing failed. Check earlier error logs.", packages_metafile.display()));
        }

        // Load packages info
        let packages_info: PackagesFileInfo = read_json_file(&packages_metafile)?;

        // Try to load corresponding filelists if it exists
        let mut filelists_info = None;
        let filelists_metafile = packages_metafile.to_str()
            .ok_or_else(|| eyre::eyre!("Invalid packages metafile path: {}", packages_metafile.display()))?
            .replace(".packages", ".filelists");
        if Path::new(&filelists_metafile).exists() {
            log::debug!("Found filelists metafile: {}", filelists_metafile);
            let filelists: FilelistsFileInfo = read_json_file(Path::new(&filelists_metafile))?;
            filelists_info = Some(filelists);
        } else {
            log::debug!("Filelists metafile does not exist: {}", filelists_metafile);
        }

        // Use file stem as key, fallback to shard_i
        let key = packages_metafile.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.trim_start_matches('.').to_owned())
            .unwrap_or_else(|| format!("shard_{}", i));
        repo_shards.insert(key, RepoShard {
            packages: packages_info,
            filelists: filelists_info,
            essential_pkgnames: std::collections::HashSet::new(),
            provide2pkgnames:   None,
            pkgname2ranges:     std::collections::BTreeMap::new(),
            packages_mmap:      None,
            pkgname2ranges_path: None,
        });
    }

    // Check if we found any valid packages metafiles
    if repo_shards.is_empty() {
        log::warn!("No valid packages metafiles found for repository: {}. This indicates that packages need to be processed first.", repo.repo_name);
        return Err(eyre::eyre!("No valid packages metafiles found for repository: {}. Packages need to be downloaded and processed before creating repo index.", repo.repo_name));
    }

    // Save the index for the repo
    let repo_index = RepoIndex {
        repodata_name: repo.repodata_name.clone(),
        package_baseurl: String::new(),
        repo_dir_path: String::new(),
        format: repo.format, // Use the format from the repo configuration
        repo_shards
    };
    let index_path = repo_dir.join("RepoIndex.json");

    // Ensure parent directory exists
    if let Some(parent) = index_path.parent() {
        lfs::create_dir_all(parent)?;
    }

    // Serialize to JSON with proper error handling
    let json_content = serde_json::to_string_pretty(&repo_index)
        .wrap_err_with(|| format!("Failed to serialize repo index for repository: {}", repo.repo_name))?;

    // Write to file with proper error handling
    lfs::write(&index_path, json_content)?;

    log::debug!("Successfully wrote repo index to {}", index_path.display());

    Ok(repo_index)
}

fn process_data(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<()> {
    if revise.is_packages {
        match revise.repo_revise.format {
            PackageFormat::Deb => crate::deb_repo::process_packages_content(data_rx, repo_dir, revise).with_context(|| format!("Failed to process Debian packages content for {}", revise.download_path.display()))?,
            PackageFormat::Rpm => crate::rpm_repo::process_packages_content(data_rx, repo_dir, revise).with_context(|| format!("Failed to process RPM packages content for {}", revise.download_path.display()))?,
            PackageFormat::Apk => crate::apk_repo::process_packages_content(data_rx, repo_dir, revise).with_context(|| format!("Failed to process APK packages content for {}", revise.download_path.display()))?,
            PackageFormat::Pacman => {
                // Check if this is an AUR repository
                if revise.location.contains("packages-meta-ext-v1.json") || revise.repo_revise.repo_name == "aur" {
                    crate::aur::process_packages_content(data_rx, repo_dir, revise).with_context(|| format!("Failed to process AUR packages content for {}", revise.download_path.display()))?
                } else {
                    crate::arch_repo::process_packages_content(data_rx, repo_dir, revise).with_context(|| format!("Failed to process Pacman packages content for {}", revise.download_path.display()))?
                }
            },
            PackageFormat::Conda => crate::conda_repo::process_packages_content(data_rx, repo_dir, revise).with_context(|| format!("Failed to process Conda packages content for {}", revise.download_path.display()))?,
            _ => return Err(eyre::eyre!("Unsupported package format: {:?}", revise.repo_revise.format)),
        };
    } else {
        process_filelists_content(data_rx, repo_dir, revise)
            .with_context(|| format!("Failed to process filelists content for {}", revise.download_path.display()))?;
    }
    Ok(())
}

/**
 * Processes the content of filelists files.
 *
 * Vision for filelists handling:
 * Currently, `deb_repo.rs` and `rpm_repo.rs` primarily handle two main types of files:
 * 1. Entry index files: `Release` (for Debian repositories) or `repomd.xml` (for RPM repositories).
 * 2. Package database files: `Packages.xz` (for Debian) or `primary.xml.zst` (for RPM).
 *
 * The `filelists` file, which contains a list of all files in all packages, is also downloaded
 * (unified download logic is in `repo.rs`). However, due to its potentially very large size after
 * decompression (often exceeding 10GB), converting it to a unified format would incur significant
 * time and disk space costs.
 *
 * Therefore, the decision has been made to *not* perform format conversion on `filelists`.
 * Instead, when users need to search for files within packages, the search functionality
 * will be implemented separately for each package format (Debian and RPM) directly on their
 * respective `filelists` formats.
 */
/// Process filelists content by verifying hash, creating symlinks, and generating metadata
fn process_filelists_content(data_rx: Receiver<Vec<u8>>, _repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<FilelistsFileInfo> {
    log::debug!("Processing filelists content for arch: {:?}", revise);

    // Step 1: Verify the hash of the received data
    let calculated_hash = verify_filelists_hash(data_rx, revise)?;

    // Step 2: Prepare the output path (remove existing file, create directories)
    let output_path = prepare_filelists_output_path(revise)?;

    // Step 3: Create symbolic link from download path to output path
    create_filelists_symlink(revise, &output_path)?;

    // Step 4: Generate and write file metadata
    let file_info = generate_and_write_filelists_metadata(&output_path, calculated_hash)?;

    log::debug!("Successfully processed filelists content for arch: {}", revise.arch);
    Ok(file_info)
}

/// Verify the hash of the received filelists data
fn verify_filelists_hash(data_rx: Receiver<Vec<u8>>, revise: &RepoReleaseItem) -> Result<String> {
    let mut hasher = match revise.hash_type.as_str() {
        "SHA512" => HashType::Sha512(Sha512::new()),
        _ => HashType::Sha256(Sha256::new()),
    };
    let mut total_bytes = 0;

    // Process data and calculate hash incrementally
    while let Ok(data) = data_rx.recv() {
        hasher.update(&data);
        total_bytes += data.len();
    }

    // Verify hash
    let calculated_hash = hex::encode(hasher.finalize());
    if calculated_hash != revise.hash {
        if total_bytes == revise.size {
            log::error!("Hash verification failed for {}: expected {}, got {}",
                revise.download_path.display(), revise.hash, calculated_hash);
        }
        return Err(eyre::eyre!("Hash verification failed for {}: expected {}, got {}",
            revise.location, revise.hash, calculated_hash));
    }
    log::debug!("Hash verification successful for {}", revise.location);

    Ok(calculated_hash)
}

/// Prepare the output path for filelists by removing existing files and creating directories
fn prepare_filelists_output_path(revise: &RepoReleaseItem) -> Result<PathBuf> {
    let output_path = revise.output_path.clone();

    if output_path.exists() {
        log::debug!("Removing existing filelists at {}", output_path.display());
        lfs::remove_file(&output_path)?;
    } else {
        if let Some(parent_dir) = output_path.parent() {
            if !parent_dir.as_os_str().is_empty() {
                lfs::create_dir_all(parent_dir)?;
            }
        }
    }

    Ok(output_path)
}

/// Create symbolic link from download path to output path
fn create_filelists_symlink(revise: &RepoReleaseItem, output_path: &PathBuf) -> Result<()> {
    // Create symbolic link
    // /home/wfg/.cache/epkg/channels/debian-trixie/contrib/x86_64/filelists-all.gz =>
    // /home/wfg/.cache/epkg/downloads/debian/dists/trixie/contrib/by-hash/SHA256/9cc88157988a1ccc1240aa749a311bd6c445ecc890d16c431816a409303f3f51
    log::debug!("Creating symlink from {} to {}", revise.download_path.display(), output_path.display());

    // Check if output_path exists and is a valid file/symlink
    if lfs::symlink_metadata(output_path).is_ok() {
        log::debug!("Removing existing filelists at {}", output_path.display());
        lfs::remove_file(&output_path)?;
    }

    #[cfg(unix)]
    lfs::symlink(&revise.download_path, output_path)?;

    #[cfg(windows)]
    std::os::windows::fs::symlink_file(revise.download_path.clone(), output_path)
        .with_context(|| format!("Failed to create symlink from {} to {}",
            revise.download_path.display(), output_path.display()))?;

    Ok(())
}

/// Generate file metadata and write it to a JSON file
pub fn generate_and_write_filelists_metadata(output_path: &PathBuf, calculated_hash: String) -> Result<FilelistsFileInfo> {
    let metadata = fs::metadata(output_path)
        .with_context(|| format!("Failed to get metadata for {}", output_path.display()))?;

    let file_info = FilelistsFileInfo {
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

    // Write metadata to JSON file
    write_filelists_metadata_json(output_path, &file_info)?;

    Ok(file_info)
}

/// Write filelists metadata to a JSON file
fn write_filelists_metadata_json(output_path: &PathBuf, file_info: &FilelistsFileInfo) -> Result<()> {
    let json_path = output_path.with_extension("").with_extension("json").to_str()
            .ok_or_else(|| eyre::eyre!("Invalid packages metafile path"))?
            .replace("filelists", ".filelists");

    log::debug!("Writing filelists metadata to {}", json_path);
    let json_content = serde_json::to_string_pretty(file_info)
        .with_context(|| format!("Failed to serialize file info to JSON for {}", output_path.display()))?;
    lfs::write(&json_path, json_content)?;

    Ok(())
}


pub fn list_repos() -> Result<()> {
    let self_env_root = dirs::find_env_root(SELF_ENV)
                .ok_or_else(|| eyre::eyre!("Self environment not found"))?;
    let manager_channel_dir = self_env_root.join("usr/src/epkg/sources");
    if !manager_channel_dir.exists() {
        return Ok(());
    }

    // Collect all entries first
    let mut entries: Vec<(String, String, String, String)> = Vec::new();

    for entry in fs::read_dir(&manager_channel_dir)? {
        let path = entry?.path();
        if !path.is_file() || path.extension().unwrap_or_default() != "yaml" {
            continue;
        }

        let yaml_content = fs::read_to_string(&path)?;
        let channel_config: ChannelConfig = match serde_yaml::from_str(&yaml_content) {
            Ok(cfg) => cfg,
            Err(e) => {
                return Err(eyre::eyre!("Failed to parse YAML file '{}': {}", path.display(), e));
            }
        };

        // Get default version (first version from versions array, or single version field)
        let default_version = if !channel_config.versions.is_empty() {
            &channel_config.versions[0]
        } else {
            &channel_config.version
        };

        // Extract first numeric version from the version string
        let clean_version = default_version
            .split_whitespace()
            .find(|s| s.chars().next().map_or(false, |c| c.is_ascii_digit()))
            .unwrap_or(default_version);

        // Collect all enabled repos for this channel
        let mut enabled_repos: Vec<String> = Vec::new();
        for (repo_name, repo_config) in &channel_config.repos {
            if repo_config.enabled {
                enabled_repos.push(repo_name.clone());
            }
        }

        if enabled_repos.is_empty() {
            continue;
        }

        // Join repos with commas
        let repos_str = enabled_repos.join(",");
        let index_url = channel_config.index_url.clone();

        // Filter out non-x86_64 architectures if URL contains x86_64
        if config().common.arch != "x86_64" && index_url.contains("x86_64") {
            continue;
        }

        // Truncate URL if it's too long to prevent wrapping
        let max_url_length = 80;
        let display_url = if index_url.len() > max_url_length {
            format!("{}...", &index_url[..max_url_length-3])
        } else {
            index_url.clone()
        };

        entries.push((
            channel_config.distro.clone(),
            clean_version.to_string(),
            repos_str,
            display_url
        ));
    }

    // Sort entries by channel name
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    // Print header
    println!("{}", "-".repeat(140));
    println!("{:<20} | {:<15} | {:<45} | {}", "channel", "default version", "repos", "index_url");
    println!("{}", "-".repeat(140));

    // Print sorted entries
    for (channel, version, repos, url) in entries {
        println!("{:<20} | {:<15} | {:<45} | {}", channel, version, repos, url);
    }

    println!("{}", "-".repeat(140));
    Ok(())
}
