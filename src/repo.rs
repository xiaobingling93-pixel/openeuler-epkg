use std::collections::{HashMap};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseStatus {
    NeedDownload,
    NeedConvert,
    NeedUpdate,
    FineExist,
    FineRecent,
}

#[derive(Clone)]
#[derive(Debug)]
pub struct RepoRevise {
    #[allow(dead_code)]
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
    pub size: usize,
    pub location: String,
    pub is_packages: bool,
    pub download_path: PathBuf,
    pub output_path: PathBuf,
}

#[allow(dead_code)]
impl PackageManager {

    pub fn sync_channel_metadata(&mut self) -> Result<()> {
        let channel_config = crate::models::channel_config();

        let all_repos = get_revise_repos(channel_config.clone())
            .with_context(|| "Failed to get repository revision information")?;
        revise_repos(channel_config.format.clone(), all_repos)
            .with_context(|| "Failed to process repository revisions")?;

        Ok(())
    }

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

    log::debug!("get_revise_repos: {:#?}", all_repos);
    Ok(all_repos)
}

fn revise_repos(format: PackageFormat, all_repos: Vec<RepoRevise>) -> Result<()> {
    let (tx, rx) = mpsc::channel();

    for repo in all_repos {
        let repo = repo.clone();
        sync_repo_metadata(format.clone(), &repo, &tx)?;
    }

    // Reader thread (or main thread) waits for all writers to finish
    if config().common.parallel_processing {
        log::debug!("Waiting for sync_repo_metadata() threads");

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

fn is_file_recent(path: &PathBuf, max_age: Duration) -> Result<bool> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("Failed to get metadata for file: {}", path.display()))?;
    let modified = metadata.modified()
        .with_context(|| format!("Failed to get modification time for file: {}", path.display()))?;
    let now = SystemTime::now();
    if let Ok(duration) = now.duration_since(modified) {
        Ok(duration < max_age)
    } else {
        Ok(false)
    }
}

fn touch_file_mtime(path: &PathBuf) -> Result<()> {
    let now = SystemTime::now();
    filetime::set_file_mtime(path, filetime::FileTime::from_system_time(now))
        .with_context(|| format!("Failed to update modification time for file: {}", path.display()))?;
    Ok(())
}

fn check_repo_index_age(index_path: &PathBuf, duration: std::time::Duration) -> Result<bool> {
    let is_recent = is_file_recent(&index_path, duration)
        .with_context(|| format!("Failed to check if file is recent: {}", index_path.display()))?;
    if !is_recent {
        touch_file_mtime(&index_path)
            .with_context(|| format!("Failed to update modification time for index file: {}", index_path.display()))?;
    }
    Ok(is_recent)
}

pub fn should_refresh_release_file(path: &PathBuf, repo: &RepoRevise) -> Result<ReleaseStatus> {
    let expire_secs = config().common.metadata_expire;

    if !path.exists() {
        return Ok(ReleaseStatus::NeedDownload);
    }

    let repo_dir = dirs::get_repo_dir(&repo)
        .with_context(|| format!("Failed to get repository directory for: {}", repo.repo_name))?;
    let index_path = repo_dir.join("RepoIndex.json");
    if !index_path.exists() {
        return Ok(ReleaseStatus::NeedConvert);
    }

    // if never auto update
    if expire_secs == 0 && config().subcommand != EpkgCommand::Update {
        return Ok(ReleaseStatus::FineExist);
    }

    // if not always update
    if !(expire_secs < 0 || config().subcommand == EpkgCommand::Update) {
        let duration = std::time::Duration::from_secs(expire_secs.try_into()
            .map_err(|e| eyre::eyre!("Failed to convert metadata_expire to u64: {}", e))?);
        // Check if release file is recent
        if is_file_recent(path, duration)? {
            // If release file is recent, check repo index age
            if check_repo_index_age(&index_path, duration)? {
                return Ok(ReleaseStatus::FineRecent);
            }
        }
    }

    Ok(ReleaseStatus::NeedUpdate)
}

pub fn refresh_release_file(path: &PathBuf, repo: &RepoRevise) -> Result<()> {
    let status = should_refresh_release_file(path, repo)
        .with_context(|| format!("Failed to check if release file needs refreshing: {}", path.display()))?;

    if status == ReleaseStatus::FineExist || status == ReleaseStatus::FineRecent {
        return Ok(());
    }

    // Download Release file
    download_urls(vec![repo.index_url.clone()], &dirs().epkg_downloads_cache, 6, false)
        .with_context(|| format!("Failed to download release file from {}", repo.index_url))?;
    Ok(())
}

/// Download/Parse the Release/repomd.xml file for a repository and return the release items
fn sync_from_release_metadata(format: PackageFormat, repo: &RepoRevise, release_path: &PathBuf) -> Result<Vec<RepoReleaseItem>> {
    let release_dir = release_path.parent()
        .ok_or_else(|| eyre::eyre!("Failed to get parent directory of release path: {}", release_path.display()))?;

    fs::create_dir_all(release_dir).with_context(|| format!("Failed to create parent directory for: {}", release_path.display()))?;
    refresh_release_file(&release_path, &repo)
        .with_context(|| format!("Failed to refresh release file for repository: {}", repo.repo_name))?;

    // Parse Release file
    let release_content = fs::read_to_string(&release_path)
        .with_context(|| format!("Failed to read Release file: {}", release_path.display()))?;
    let release_items =
        match format {
            PackageFormat::Deb => crate::deb_repo::parse_release_file(&repo, &release_content, &release_dir.to_path_buf())?,
            PackageFormat::Rpm => crate::rpm_repo::parse_repomd_file(&repo, &release_content, &release_dir.to_path_buf())?,
            _ => return Err(eyre::eyre!("Unsupported package format: {:?}", format))
        };

    Ok(release_items)
}

// index_url: $mirror/v$version/$repo/$arch/APKINDEX.tar.gz
fn sync_from_package_database(format: PackageFormat, repo: &RepoRevise, packages_path: &PathBuf) -> Result<Vec<RepoReleaseItem>> {

    let should_update = should_refresh_release_file(packages_path, repo)?;
    let mut release_items = Vec::new();

    let repo_dir = dirs::get_repo_dir(&repo)
        .with_context(|| format!("Failed to get repository directory for: {}", repo.repo_name))?;

    // Extract package base URL by removing last filename part
    let package_baseurl = if let Some(parent_url) = repo.index_url.rsplitn(2, '/').nth(1) {
        parent_url.to_string()
    } else {
        repo.index_url.clone()
    };

    // Get the filename from the packages_path
    let location = packages_path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string();

    let need_download = should_update == ReleaseStatus::NeedDownload ||
                       should_update == ReleaseStatus::NeedUpdate ||
                       should_update == ReleaseStatus::FineRecent;
    let need_convert = should_update == ReleaseStatus::NeedConvert;

    release_items.push(RepoReleaseItem {
        format: format,
        repo_name: repo.repo_name.clone(),
        repodata_name: repo.repodata_name.to_string(),
        need_download: need_download,
        need_convert: need_convert,
        arch: repo.arch.clone(),
        url: repo.index_url.clone(),
        package_baseurl: package_baseurl,
        hash_type: "".to_string(),
        hash: "".to_string(),
        size: 0,
        location: location,
        is_packages: true,
        output_path: repo_dir.join("packages.txt"),
        download_path: packages_path.clone(),
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
 *   - Custom: https://repo.example.com/packages/database.tar.xz
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

pub fn sync_repo_metadata(format: PackageFormat, repo: &RepoRevise, result_tx: &mpsc::Sender<bool>) -> Result<bool> {
    let repo_dir = dirs::get_repo_dir(&repo)
        .with_context(|| format!("Failed to get repository directory for: {}", repo.repo_name))?;
    let release_path = crate::mirror::Mirrors::url_to_cache_path(&repo.index_url)
        .with_context(|| format!("Failed to convert URL to cache path: {}", repo.index_url))?;

    let release_items = if repo.index_url.ends_with("Release") || repo.index_url.ends_with("repomd.xml") {
        sync_from_release_metadata(format, repo, &release_path)
            .with_context(|| format!("Failed to parse release file for repository: {}", repo.repo_name))?
    } else if repo.index_url.ends_with("/") {
        return crate::index_html::sync_from_directory_index(format, repo, &release_path);
    } else {
        sync_from_package_database(format, repo, &release_path)
            .with_context(|| format!("Failed to check packages file for repository: {}", repo.repo_name))?
    };

    let repo_dir = Arc::new(repo_dir.clone());

    // Filter out items that don't need revision
    let release_items_clone = release_items.clone();
    let revises: Vec<_> = release_items_clone.iter()
        .filter(|revise| revise.need_download || revise.need_convert)
        .filter(|revise| revise.is_packages || config().subcommand == EpkgCommand::Update)
        .cloned()
        .collect();

    if revises.is_empty() {
        if config().subcommand == EpkgCommand::Update {
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
        process_revises_sequential(repo, revises, &repo_dir, release_items)
            .with_context(|| format!("Failed to process repository revisions sequentially for: {}", repo.repo_name))?;
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
        download_and_process_item(&revise, repo_dir)
            .with_context(|| format!("Failed to download and process item: {}", revise.location))?;
    }

    create_load_repoindex(&repo, no_revises, &repo_dir, release_items)
        .with_context(|| format!("Failed to create and load repository index for: {}", repo.repo_name))?;

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
                match download_and_process_item(&revise, &repo_dir) {
                    Ok(_) => true,
                    Err(e) => {
                        log::error!("Failed to download and process item {}: {:#}", revise.location, e);
                        false
                    }
                }
            });

            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            if let Err(e) = handle.join() {
                // Log the error but continue processing
                log::error!("Failed to join thread: {:?}", e);
            } else {
                // Thread completed successfully
            }
        }

        if let Err(e) = create_load_repoindex(&repo_clone, no_revises, &repo_dir, release_items_clone2) {
            log::error!("Failed to save repo index json: {:#}", e);
            if let Err(send_err) = result_tx.send(false) {
                log::error!("Failed to send error status on channel: {}", send_err);
            }
        } else {
            if let Err(send_err) = result_tx.send(true) {
                log::error!("Failed to send success status on channel: {}", send_err);
            }
        }
    });
}

/// Download and process a single Debian release item
fn download_and_process_item(revise: &RepoReleaseItem, repo_dir: &PathBuf) -> Result<FileInfo> {
    let (data_tx, data_rx) = channel();

    // Create and submit download task
    let task = DownloadTask::with_size(
        revise.url.clone(),
        dirs().epkg_downloads_cache.clone(),
        6,
        if revise.size > 0 { Some(revise.size as u64) } else { None }
    ).with_data_channel(data_tx);

    // Submit download task
    submit_download_task(task)
        .with_context(|| format!("Failed to submit download task for URL: {}", revise.url))?;

    DOWNLOAD_MANAGER.start_processing();

    log::debug!("process_data for {:?}", revise);
    // Process data blocks as they arrive
    process_data(data_rx, repo_dir, revise)
        .with_context(|| format!("Failed to process data for item: {} (format: {:?}, size: {}, hash: {})",
            revise.location, revise.format, revise.size, revise.hash))
}

pub fn create_load_repoindex(
    repo: &RepoRevise,
    no_revises: bool,
    repo_dir: &PathBuf,
    release_items: Vec<RepoReleaseItem>,
) -> Result<()> {
    let mut repo_index: RepoIndex =
        if no_revises {
            mmio::deserialize_repoindex(&repo_dir.join("RepoIndex.json"))
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

        // Try to load corresponding filelists if it exists
        let mut filelists_info = None;
        let filelists_metafile = packages_metafile.to_str()
            .ok_or_else(|| eyre::eyre!("Invalid packages metafile path"))?
            .replace(".packages", ".filelists");
        if Path::new(&filelists_metafile).exists() {
            let filelists_content = fs::read_to_string(&filelists_metafile)
                .with_context(|| format!("Failed to read filelists: {}", filelists_metafile))?;
            let filelists: FileInfo = serde_json::from_str(&filelists_content)
                .with_context(|| format!("Failed to parse filelists info from: {}", filelists_metafile))?;
            filelists_info = Some(filelists);
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
            provide2pkgnames:   std::collections::HashMap::new(),
            pkgname2ranges:     std::collections::HashMap::new(),
            packages_mmap:      None,
        });
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
    fs::write(&index_path, serde_json::to_string_pretty(&repo_index)?)
        .with_context(|| format!("Failed to write repo index to: {}", index_path.display()))?;

    Ok(repo_index)
}

pub fn process_data(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<FileInfo> {
    if revise.is_packages {
        match revise.format {
            PackageFormat::Deb => crate::deb_repo::process_packages_content(data_rx, repo_dir, revise).with_context(|| format!("Failed to process Debian packages content for {}", revise.location)),
            PackageFormat::Rpm => crate::rpm_repo::process_packages_content(data_rx, repo_dir, revise).with_context(|| format!("Failed to process RPM packages content for {}", revise.location)),
            PackageFormat::Apk => crate::apk_repo::process_packages_content(data_rx, repo_dir, revise).with_context(|| format!("Failed to process APK packages content for {}", revise.location)),
            PackageFormat::Pacman => crate::arch_repo::process_packages_content(data_rx, repo_dir, revise).with_context(|| format!("Failed to process Pacman packages content for {}", revise.location)),
            _ => Err(eyre::eyre!("Unsupported package format: {:?}", revise.format))
        }
    } else {
        process_filelists_content(data_rx, repo_dir, revise)
            .with_context(|| format!("Failed to process filelists content for {}", revise.location))
    }
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
pub fn process_filelists_content(data_rx: Receiver<Vec<u8>>, _repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<FileInfo> {
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
    let mut hasher = Sha256::new();
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
        fs::remove_file(&output_path)
            .with_context(|| format!("Failed to remove existing filelists at {}", output_path.display()))?;
    } else {
        if let Some(parent_dir) = output_path.parent() {
            if !parent_dir.as_os_str().is_empty() {
                fs::create_dir_all(parent_dir).with_context(|| {
                    format!(
                        "Failed to create parent directory for: {}",
                        output_path.display()
                    )
                })?;
            }
        }
    }

    Ok(output_path)
}

/// Create symbolic link from download path to output path
fn create_filelists_symlink(revise: &RepoReleaseItem, output_path: &PathBuf) -> Result<()> {
    // Create symbolic link
    // /home/wfg/.cache/epkg/channel/debian:trixie/contrib/x86_64/filelists-all.gz =>
    // /home/wfg/.cache/epkg/downloads/debian/dists/trixie/contrib/by-hash/SHA256/9cc88157988a1ccc1240aa749a311bd6c445ecc890d16c431816a409303f3f51
    log::debug!("Creating symlink from {} to {}", revise.download_path.display(), output_path.display());

    // Check if output_path exists and is a valid file/symlink
    if fs::symlink_metadata(output_path).is_ok() {
        log::debug!("Removing existing filelists at {}", output_path.display());
        fs::remove_file(&output_path)
            .with_context(|| format!("Failed to remove existing filelists at {}", output_path.display()))?;
    }

    #[cfg(unix)]
    std::os::unix::fs::symlink(revise.download_path.clone(), output_path)
        .with_context(|| format!("Failed to create symlink from {} to {}",
            revise.download_path.display(), output_path.display()))?;

    #[cfg(windows)]
    std::os::windows::fs::symlink_file(revise.download_path.clone(), output_path)
        .with_context(|| format!("Failed to create symlink from {} to {}",
            revise.download_path.display(), output_path.display()))?;

    Ok(())
}

/// Generate file metadata and write it to a JSON file
pub fn generate_and_write_filelists_metadata(output_path: &PathBuf, calculated_hash: String) -> Result<FileInfo> {
    let metadata = fs::metadata(output_path)
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

    // Write metadata to JSON file
    write_filelists_metadata_json(output_path, &file_info)?;

    Ok(file_info)
}

/// Write filelists metadata to a JSON file
fn write_filelists_metadata_json(output_path: &PathBuf, file_info: &FileInfo) -> Result<()> {
    let json_path = output_path.with_extension("").with_extension("json").to_str()
            .ok_or_else(|| eyre::eyre!("Invalid packages metafile path"))?
            .replace("filelists", ".filelists");

    log::debug!("Writing filelists metadata to {}", json_path);
    let json_content = serde_json::to_string_pretty(file_info)
        .with_context(|| format!("Failed to serialize file info to JSON for {}", output_path.display()))?;
    fs::write(&json_path, json_content)
        .with_context(|| format!("Failed to write JSON metadata to {}", json_path))?;

    Ok(())
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

        let yaml_content = fs::read_to_string(&path)?;
        let channel_config: ChannelConfig = match serde_yaml::from_str(&yaml_content) {
            Ok(cfg) => cfg,
            Err(e) => {
                return Err(eyre::eyre!("Failed to parse YAML file '{}': {}", path.display(), e));
            }
        };

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
                channel_config.distro,
                repo_name,
                repo_url
            );
        }
    }

    println!("{}", "-".repeat(100));
    Ok(())
}
