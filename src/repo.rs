use std::collections::{HashMap, HashSet};
use std::fs;
use std::fs::File;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::time::{SystemTime, Duration};
use filetime;
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre;
use crate::models::*;
use crate::parse_requires::*;
use crate::io::load_package_json;
use crate::dirs::find_env_root;
use crate::dirs::get_repo_dir;
use crate::download::download_urls;

#[derive(Clone)]
#[derive(Debug)]
pub struct RepoRevise {
    pub arch: String,
    pub channel: String,
    pub repo_name: String,
    pub repodata_name: String,
    pub index_url: String,
}

#[allow(dead_code)]
impl Repodata {
    #[allow(dead_code)]
    pub fn save_package_provides(&mut self, path: &PathBuf) -> Result<()> {
        serialize_provide2pkgnames(path, &self.provide2pkgnames)
    }

    pub fn load_package_provides(&mut self, file_path: &PathBuf) -> Result<()> {
        self.provide2pkgnames = deserialize_provide2pkgnames(file_path)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn save_essential_packages(&mut self, path: &PathBuf) -> Result<()> {
        serialize_essential_pkgnames(path, &self.essential_pkgnames)
    }

    pub fn load_essential_packages(&mut self, file_path: &PathBuf) -> Result<()> {
        self.essential_pkgnames = deserialize_essential_pkgnames(file_path)?;
        Ok(())
    }

    #[allow(dead_code)]
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

fn revise_repos(format: PackageFormat, all_repos: Vec<RepoRevise>) -> Result<()> {
    let mut revised = Vec::new();
    let (tx, rx) = mpsc::channel();

    for repo in all_repos {
        let repo = repo.clone();
        match format {
            PackageFormat::Deb => {
                if let Ok(true) = crate::deb_repo::revise_repodata(&repo, &tx) {
                    revised.push(repo);
                }
            },
            PackageFormat::Rpm => {
                if let Err(e) = crate::rpm_repo::revise_repodata(&repo) {
                    eprintln!("Error processing repo: {}", e);
                } else {
                    revised.push(repo);
                }
            },
            _ => eprintln!("Unknown repo type: {:?}", format),
        }
    }

    // Reader thread (or main thread) waits for all writers to finish
    if !revised.is_empty() {
        drop(tx);
        log::debug!("revise repo index for {:#?}", revised);

        while let Ok(packages_metafiles) = rx.recv() {
            save_repo_index_json(packages_metafiles)?;
        }
    }

    Ok(())
}

pub fn save_repo_index_json(packages_metafiles: Vec<PathBuf>) -> Result<()> {
    if packages_metafiles.is_empty() {
        return Ok(());
    }

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

        // Use file stem or index as the key
        let key = packages_metafile.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&format!("shard_{}", i))
            .to_string();
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
    let repo_index = RepoIndex { repo_shards };
    let index_path = repo_dir.join("RepoIndex.json");
    fs::write(&index_path, serde_json::to_string_pretty(&repo_index)?)
        .with_context(|| format!("Failed to write repo index to: {}", index_path.display()))?;

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
    if !path.exists() {
        return Ok(false);
    }
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

fn check_repo_index_age(repo: &RepoRevise) -> Result<bool> {
    let repo_dir = get_repo_dir(&repo).unwrap();
    let index_path = repo_dir.join("RepoIndex.json");
    if !index_path.exists() {
        return Ok(false);
    }
    let is_recent = is_file_recent(&index_path, Duration::from_secs(24 * 60 * 60))?;
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

pub fn refresh_release_file(path: &PathBuf, repo: &RepoRevise) -> Result<()> {
    // Check if release file is recent
    if is_file_recent(path, Duration::from_secs(24 * 60 * 60))? {
        // If release file is recent, check repo index age
        if check_repo_index_age(repo)? {
            return Ok(());
        }
    }

    if should_skip_duplicate_downloads(path) {
        return Ok(());
    }

    // Download Release file
    download_urls(vec![repo.index_url.clone()], dirs().epkg_downloads_cache.to_str().unwrap(), 6, false)?;
    Ok(())
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
            arch: config.arch.clone(),
            channel: config.channel.clone(),
            repo_name: repo_name.clone(),
            repodata_name: repo_name.clone(),
            index_url: index_url.clone(),
        });
        if let Some(updates_url) = &repo_config.index_url_updates {
            all_repos.push(RepoRevise {
                arch: config.arch.clone(),
                channel: config.channel.clone(),
                repo_name: repo_name.clone(),
                repodata_name: format!("{}-updates", repo_name),
                index_url: updates_url.clone(),
            });
        }
        if let Some(security_url) = &repo_config.index_url_security {
            all_repos.push(RepoRevise {
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

/// Serializes essential package names to a file
pub fn serialize_essential_pkgnames(path: &PathBuf, pkgnames: &HashSet<String>) -> Result<()> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    let mut sorted_names: Vec<_> = pkgnames.iter().collect();
    sorted_names.sort();

    for item in sorted_names {
        writeln!(writer, "{}", item)?;
    }

    Ok(())
}

/// Deserializes essential package names from a file
pub fn deserialize_essential_pkgnames(file_path: &PathBuf) -> Result<HashSet<String>> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut hashset: HashSet<String> = HashSet::new();

    for line in reader.lines() {
        let line = line?;
        hashset.insert(line);
    }

    Ok(hashset)
}

/// Serializes package provides mapping to a file
pub fn serialize_provide2pkgnames(path: &PathBuf, provide2pkgnames: &HashMap<String, Vec<String>>) -> Result<()> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    let mut sorted_names: Vec<_> = provide2pkgnames.iter().collect();
    sorted_names.sort_by(|a, b| a.0.cmp(&b.0));

    for (key, values) in sorted_names {
        let line = format!("{}: {}", key, values.join(" "));
        writeln!(writer, "{}", line)?;
    }

    Ok(())
}

/// Deserializes package provides mapping from a file
pub fn deserialize_provide2pkgnames(file_path: &PathBuf) -> Result<HashMap<String, Vec<String>>> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    for (line_num, line_result) in reader.lines().enumerate() {
        let line = line_result.context(format!("Failed to read line {} from {}", line_num + 1, file_path.display()))?;
        if let Some((key, values)) = line.split_once(": ") {
            let values: Vec<String> = values.split(" ").map(|s| s.to_string()).collect();
            map.insert(key.to_string(), values);
        }
    }

    Ok(map)
}

// Function to serialize pkgname2ranges to a file
pub fn serialize_pkgname2offsets(path: &PathBuf, pkgname2ranges: &HashMap<String, Vec<PackageRange>>) -> Result<()> {
    let mut file = fs::File::create(path)
        .with_context(|| format!("Failed to create index file: {}", path.display()))?;

    // Sort package names before writing
    let mut sorted_packages: Vec<_> = pkgname2ranges.iter().collect();
    sorted_packages.sort_by(|a, b| a.0.cmp(b.0));

    for (pkgname, offsets) in sorted_packages {
        let offset_str = offsets.iter()
            .map(|o| format!("{:x} {:x}", o.begin, o.len))
            .collect::<Vec<_>>()
            .join(" ");
        writeln!(file, "{}: {}", pkgname, offset_str)
            .with_context(|| format!("Failed to write to index file: {}", path.display()))?;
    }
    Ok(())
}

// Function to deserialize pkgname2ranges from a file
pub fn deserialize_pkgname2offsets(path: &PathBuf) -> Result<HashMap<String, Vec<PackageRange>>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read index file: {}", path.display()))?;

    let mut pkgname2ranges = HashMap::new();
    for line in content.lines() {
        if let Some((pkgname, offsets_str)) = line.split_once(": ") {
            let offsets: Vec<PackageRange> = offsets_str
                .split_whitespace()
                .collect::<Vec<_>>()
                .chunks(2)
                .filter_map(|chunk| {
                    if chunk.len() == 2 {
                        let begin = usize::from_str_radix(chunk[0], 16).ok()?;
                        let len = usize::from_str_radix(chunk[1], 16).ok()?;
                        Some(PackageRange {
                            begin,
                            len,
                        })
                    } else {
                        None
                    }
                })
                .collect();
            if !offsets.is_empty() {
                pkgname2ranges.insert(pkgname.to_string(), offsets);
            }
        }
    }
    Ok(pkgname2ranges)
}

