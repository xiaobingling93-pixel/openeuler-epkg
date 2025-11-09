use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::collections::{HashMap, HashSet, BTreeMap};
use std::fs::OpenOptions;
use std::io::{Write, BufWriter};
use std::fmt::Write as FmtWrite;
use color_eyre::eyre::Result;
use color_eyre::eyre;
use flate2::read::GzDecoder;
use bzip2::read::BzDecoder;
use sha2::{Sha256, Digest};
use hex;
use time::{OffsetDateTime, format_description};
use crate::models::*;
use crate::dirs;
use crate::repo::*;
use crate::packages_stream;
use crate::mmio;

// Conda-specific structures for repodata.json deserialization
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CondaRepoData {
    #[serde(default)]
    pub info: Option<CondaRepoInfo>,
    #[serde(default)]
    pub packages: HashMap<String, CondaPackage>,
    #[serde(default, rename = "packages.conda")]
    pub packages_conda: HashMap<String, CondaPackage>,
    #[serde(default)]
    pub removed: Vec<String>,
    #[serde(default)]
    pub repodata_version: Option<u32>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CondaRepoInfo {
    #[serde(default)]
    pub subdir: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CondaPackage {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub build: Option<String>,
    #[serde(default)]
    pub build_number: Option<u64>,
    #[serde(default)]
    pub subdir: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub timestamp: Option<u64>,

    // Package metadata
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub license_family: Option<String>,

    // Dependencies
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub constrains: Vec<String>,

    // File information
    #[serde(default, rename = "fn")]
    pub filename: Option<String>,
    #[serde(default)]
    pub md5: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,

    // Build information
    #[serde(default)]
    pub noarch: Option<bool>,
    #[serde(default)]
    pub platform: Option<String>,

    // Optional fields
    #[serde(default)]
    pub track_features: Option<String>,
    #[serde(default)]
    pub features: Option<String>,
    #[serde(default)]
    pub preferred_env: Option<String>,
}

impl CondaPackage {
    /// Convert to epkg's internal Package format
    pub fn to_package(&self, filename: &str, _package_format: &str, arch: &str) -> Package {
        let build_time = self.timestamp.map(|ts| {
            // Convert timestamp to seconds if it's in milliseconds
            if ts > 10000000000 { (ts / 1000) as u32 } else { ts as u32 }
        });

        Package {
            pkgname: self.name.clone(),
            version: self.version.clone(),
            arch: arch.to_string(),
            size: self.size.unwrap_or(0) as u32,
            installed_size: 0, // Conda doesn't typically provide this
            build_time,
            source: None,
            location: filename.to_string(),
            ca_hash: None,
            sha256sum: self.sha256.clone(),
            sha1sum: None,
            depends: Vec::new(), // Will be populated separately if needed
            requires_pre: Vec::new(),
            requires: self.depends.clone(), // Store dependencies as-is, let parse_requires handle them
            provides: vec![self.name.clone()], // Package provides itself
            recommends: Vec::new(),
            suggests: Vec::new(),
            conflicts: Vec::new(),
            summary: self.summary.clone().unwrap_or_default(),
            description: self.description.clone(),
            homepage: self.url.clone().unwrap_or_default(),
            section: None,
            priority: None,
            maintainer: String::new(),
            tag: None,
            origin_url: None,
            multi_arch: None,
            pkgkey: String::new(), // Will be set later
            repodata_name: String::new(), // Will be set later
            package_baseurl: String::new(), // Will be set later
        }
    }
}

/// Returns (host, path) tuple
fn parse_url_components(url: &str) -> Result<(String, String)> {
    // Find scheme end
    let after_scheme = if let Some(scheme_end) = url.find("://") {
        &url[scheme_end + 3..]
    } else {
        return Err(eyre::eyre!("Invalid URL: missing scheme in '{}'", url));
    };

    // Find host end (first '/' or end of string)
    let host_end = after_scheme.find('/').unwrap_or(after_scheme.len());
    let host = after_scheme[..host_end].to_string();

    // Extract path part (everything after host)
    let path_part = if host_end < after_scheme.len() {
        after_scheme[host_end + 1..].to_string()
    } else {
        String::new()
    };

    if host.is_empty() {
        return Err(eyre::eyre!("Invalid URL: missing host in '{}'", url));
    }

    Ok((host, path_part))
}

/// Parse repodata.json content from Conda repositories
pub fn parse_repodata_json(repo: &RepoRevise, _release_dir: &PathBuf) -> Result<Vec<RepoReleaseItem>> {
    let mut release_items = Vec::new();

    let repo_dir = dirs::get_repo_dir(&repo)
        .map_err(|e| eyre::eyre!("Failed to get repository directory for {}: {}", repo.repo_name, e))?;

    let arch = map_conda_arch_to_standard(&repo.arch);
    let output_path = repo_dir.join(format!("packages-{}.txt", arch));

    let url = repo.index_url.clone();
    let location = url.split('/').last().unwrap_or("repodata.json.gz").to_string();

    let package_baseurl = if let Some(last_slash_pos) = url.rfind('/') {
        url[..last_slash_pos].to_string()
    } else {
        url.clone()
    };

    let (host, path_part) = parse_url_components(&url)?;

    let mut download_dir = dirs().epkg_channel_cache.join(host);

    if !path_part.is_empty() {
        let path_segments: Vec<&str> = path_part.split('/').collect();
        for segment in path_segments.iter().take(path_segments.len().saturating_sub(1)) {
            if !segment.is_empty() {
                download_dir = download_dir.join(segment);
            }
        }
    }

    let unique_filename = format!("{}-{}", repo.repo_name, location);
    let download_path = download_dir.join(&unique_filename);

    let need_download = !download_path.exists();
    let need_convert = !output_path.exists() || {
        let repoindex_path = repo_dir.join("RepoIndex.json");
        !repoindex_path.exists()
    };

    release_items.push(RepoReleaseItem {
        repo_revise: repo.clone(),
        need_download,
        need_convert,
        arch: arch.clone(),
        url,
        package_baseurl: package_baseurl.to_string(),
        hash_type: "SHA256".to_string(),
        hash: String::new(),
        size: 0,
        location,
        is_packages: true,
        output_path,
        download_path,
    });

    Ok(release_items)
}

pub fn process_packages_content(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<PackagesFileInfo> {
    log::debug!("Starting to process Conda packages content for {} (hash: {})", revise.location, revise.hash);

    let reader = if !revise.hash.is_empty() && revise.size > 0 {
        log::debug!("Creating ReceiverHasher with expected hash: {}, size: {} bytes", revise.hash, revise.size);
        packages_stream::ReceiverHasher::new_with_size(data_rx, revise.hash.clone(), revise.size.try_into().unwrap())
    } else {
        if revise.hash.is_empty() {
            log::warn!("No hash provided for {}, integrity verification will be skipped", revise.location);
        }
        if revise.size == 0 {
            log::warn!("No size provided for {}, download completeness verification will be skipped", revise.location);
        }
        packages_stream::ReceiverHasher::new(data_rx, String::new())
    };

    let repodata: CondaRepoData = parse_compressed_json(reader, &revise.location)?;
    process_conda_repodata(repo_dir, revise, repodata)
}

fn parse_compressed_json(reader: packages_stream::ReceiverHasher, location: &str) -> Result<CondaRepoData> {
    let result = if location.ends_with(".gz") {
        log::debug!("Using GZIP decoder for {}", location);
        serde_json::from_reader(GzDecoder::new(reader))
    } else if location.ends_with(".bz2") {
        log::debug!("Using BZIP2 decoder for {}", location);
        serde_json::from_reader(BzDecoder::new(reader))
    } else {
        log::debug!("Parsing uncompressed JSON for {}", location);
        serde_json::from_reader(reader)
    };

    result.map_err(|e| {
        let error_type = if e.is_io() { "IO" } else { "JSON parsing" };
        eyre::eyre!("{} error for {}: {}. {}",
            error_type, location, e,
            if e.is_io() { "This might indicate download corruption or hash validation failure." } else { "" }
        )
    })
}

fn process_conda_repodata(
    repo_dir: &PathBuf,
    revise: &RepoReleaseItem,
    repodata: CondaRepoData,
) -> Result<PackagesFileInfo> {
    let mut packages: Vec<Package> = Vec::new();
    let mut provide2pkgnames: HashMap<String, Vec<String>> = HashMap::new();
    let essential_pkgnames: HashSet<String> = HashSet::new();
    let mut pkgname2ranges: BTreeMap<String, Vec<PackageRange>> = BTreeMap::new();
    let mut output = String::new();

    let total_packages = repodata.packages.len() + repodata.packages_conda.len();
    if total_packages > 0 {
        let estimated_total_size = total_packages * 800;
        output.reserve(estimated_total_size);
        log::debug!("Pre-allocated {} bytes for {} packages", estimated_total_size, total_packages);
    }

    let arch = repodata.info
        .as_ref()
        .and_then(|info| info.subdir.as_ref())
        .map(|subdir| map_conda_arch_to_standard(subdir))
        .unwrap_or_else(|| map_conda_arch_to_standard(&revise.arch));

    let removed_packages: HashSet<String> = repodata.removed.into_iter().collect();
    if !removed_packages.is_empty() {
        log::debug!("Found {} removed packages that will be excluded", removed_packages.len());
    }

    let mut processed_count = 0;
    let mut skipped_count = 0;

    let package_sets = [
        (&repodata.packages, "tar.bz2"),
        (&repodata.packages_conda, "conda")
    ];

    for (package_map, package_format) in package_sets {
        for (filename, conda_pkg) in package_map {
            if removed_packages.contains(filename) {
                log::trace!("Skipping removed {} package: {}", package_format, filename);
                skipped_count += 1;
                continue;
            }

            let package = process_conda_package(conda_pkg, filename, package_format, &arch, &mut output, &mut provide2pkgnames, &mut pkgname2ranges)?;
            packages.push(package);
            processed_count += 1;
        }
    }

    log::debug!("Processed {} packages from Conda repodata (skipped {} removed packages)", processed_count, skipped_count);

    write_conda_packages_output(
        repo_dir,
        revise,
        output,
        provide2pkgnames,
        essential_pkgnames,
        pkgname2ranges,
        packages.len(),
    )
}

fn process_conda_package(
    conda_pkg: &CondaPackage,
    filename: &str,
    package_format: &str,
    arch: &str,
    output: &mut String,
    provide2pkgnames: &mut HashMap<String, Vec<String>>,
    pkgname2ranges: &mut BTreeMap<String, Vec<PackageRange>>,
) -> Result<Package> {
    let package_start_offset = output.len();

    let package = conda_pkg.to_package(filename, package_format, arch);

    output.push('\n');

    writeln!(output, "pkgname: {}", package.pkgname).unwrap();
    writeln!(output, "version: {}", package.version).unwrap();
    writeln!(output, "arch: {}", package.arch).unwrap();
    writeln!(output, "location: {}", package.location).unwrap();

    if package.size > 0 {
        writeln!(output, "size: {}", package.size).unwrap();
    }

    if let Some(build_time) = package.build_time {
        writeln!(output, "buildTime: {}", build_time).unwrap();
    }

    if let Some(build) = &conda_pkg.build {
        writeln!(output, "buildString: {}", build).unwrap();
    }

    if let Some(build_number) = conda_pkg.build_number {
        writeln!(output, "buildNumber: {}", build_number).unwrap();
    }

    if !package.summary.is_empty() {
        writeln!(output, "summary: {}", package.summary).unwrap();
    }

    if let Some(description) = &package.description {
        writeln!(output, "description: {}", description).unwrap();
    }

    if !package.homepage.is_empty() {
        writeln!(output, "homepage: {}", package.homepage).unwrap();
    }

    if let Some(license) = &conda_pkg.license {
        writeln!(output, "license: {}", license).unwrap();
    }

    if let Some(license_family) = &conda_pkg.license_family {
        writeln!(output, "licenseFamily: {}", license_family).unwrap();
    }

    if !package.requires.is_empty() {
        writeln!(output, "requires: {}", package.requires.join(", ")).unwrap();
    }

    if !conda_pkg.constrains.is_empty() {
        writeln!(output, "constrains: {}", conda_pkg.constrains.join(", ")).unwrap();
    }

    if let Some(sha256) = &package.sha256sum {
        writeln!(output, "sha256: {}", sha256).unwrap();
    }

    if let Some(md5) = &conda_pkg.md5 {
        writeln!(output, "md5sum: {}", md5).unwrap();
    }

    if let Some(noarch) = conda_pkg.noarch {
        writeln!(output, "noarch: {}", noarch).unwrap();
    }

    if let Some(platform) = &conda_pkg.platform {
        writeln!(output, "platform: {}", platform).unwrap();
    }

    if let Some(track_features) = &conda_pkg.track_features {
        if !track_features.is_empty() {
            writeln!(output, "trackFeatures: {}", track_features).unwrap();
        }
    }

    if let Some(features) = &conda_pkg.features {
        if !features.is_empty() {
            writeln!(output, "features: {}", features).unwrap();
        }
    }

    writeln!(output, "packageFormat: {}", package_format).unwrap();

    provide2pkgnames
        .entry(package.pkgname.clone())
        .or_insert(Vec::new())
        .push(package.pkgname.clone());

    let package_end_offset = output.len();
    pkgname2ranges
        .entry(package.pkgname.clone())
        .or_insert(Vec::new())
        .push(PackageRange {
            begin: package_start_offset,
            len: package_end_offset - package_start_offset,
        });

    Ok(package)
}

/// Map Conda architecture names to standard epkg architecture names
fn map_conda_arch_to_standard(conda_arch: &str) -> String {
    match conda_arch {
        "linux-64" => "x86_64".to_string(),
        "linux-aarch64" => "aarch64".to_string(),
        "linux-armv6l" => "armv6l".to_string(),
        "linux-armv7l" => "armv7l".to_string(),
        "linux-ppc64le" => "ppc64le".to_string(),
        "linux-s390x" => "s390x".to_string(),
        "osx-64" => "x86_64".to_string(),
        "osx-arm64" => "aarch64".to_string(),
        "win-64" => "x86_64".to_string(),
        "win-32" => "i686".to_string(),
        "noarch" => "all".to_string(),
        _ => conda_arch.to_string(),
    }
}


fn write_conda_packages_output(
    repo_dir: &PathBuf,
    revise: &RepoReleaseItem,
    output: String,
    provide2pkgnames: HashMap<String, Vec<String>>,
    essential_pkgnames: HashSet<String>,
    pkgname2ranges: BTreeMap<String, Vec<PackageRange>>,
    _package_count: usize,
) -> Result<PackagesFileInfo> {
    let output_path = &revise.output_path;
    let filename = output_path.file_name()
        .ok_or_else(|| eyre::eyre!("Invalid output path: no filename component"))?
        .to_string_lossy();

    let (_, provide2pkgnames_path, essential_pkgnames_path, pkgname2ranges_path) =
        crate::mmio::get_package_paths(repo_dir, &filename);

    let json_path = {
        let file_stem = output_path.file_stem()
            .ok_or_else(|| eyre::eyre!("Invalid output path: no file stem"))?
            .to_string_lossy();
        repo_dir.join(format!(".{}.json", file_stem))
    };

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| eyre::eyre!("Failed to create output directory: {}", e))?;
    }

    if let Some(parent) = json_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| eyre::eyre!("Failed to create json directory: {}", e))?;
    }

    let mut hasher = Sha256::new();
    hasher.update(output.as_bytes());
    let sha256sum = hex::encode(hasher.finalize());

    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(output_path)
        .map_err(|e| eyre::eyre!("Failed to create output file {}: {}", output_path.display(), e))?;

    let mut writer = BufWriter::new(file);
    writer.write_all(output.as_bytes())
        .map_err(|e| eyre::eyre!("Failed to write to output file: {}", e))?;
    writer.flush()
        .map_err(|e| eyre::eyre!("Failed to flush output file: {}", e))?;

    log::debug!("Serializing pkgname2ranges to {:?}", pkgname2ranges_path);
    mmio::serialize_pkgname2ranges(&pkgname2ranges_path, &pkgname2ranges)
        .map_err(|e| eyre::eyre!("Failed to serialize package ranges: {}", e))?;

    log::debug!("Serializing provide2pkgnames to {:?}", provide2pkgnames_path);
    mmio::serialize_provide2pkgnames(&provide2pkgnames_path, &provide2pkgnames)
        .map_err(|e| eyre::eyre!("Failed to serialize provide-to-package mappings: {}", e))?;

    log::debug!("Serializing essential_pkgnames to {:?}", essential_pkgnames_path);
    mmio::serialize_essential_pkgnames(&essential_pkgnames_path, &essential_pkgnames)
        .map_err(|e| eyre::eyre!("Failed to serialize essential package names: {}", e))?;

    log::debug!("Saving file metadata to {:?}", json_path);
    save_packages_metadata(
        output_path,
        &json_path,
        sha256sum,
        pkgname2ranges.len(),
        provide2pkgnames.len(),
        essential_pkgnames.len(),
    )
    .map_err(|e| eyre::eyre!("Failed to save file metadata: {}", e))
}

fn save_packages_metadata(
    output_path: &PathBuf,
    json_path: &PathBuf,
    sha256sum: String,
    nr_packages: usize,
    nr_provides: usize,
    nr_essentials: usize,
) -> Result<PackagesFileInfo> {
    let metadata = std::fs::metadata(output_path)
        .map_err(|e| eyre::eyre!("Failed to get file metadata: {}", e))?;

    let datetime = {
        let system_time = metadata.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let offset_datetime = OffsetDateTime::from(system_time);

        let format = format_description::parse("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z")
            .map_err(|e| eyre::eyre!("Failed to create datetime format: {}", e))?;

        offset_datetime.format(&format)
            .map_err(|e| eyre::eyre!("Failed to format datetime: {}", e))?
    };

    let packages_file_info = PackagesFileInfo {
        filename: output_path.file_name()
            .ok_or_else(|| eyre::eyre!("Invalid output path: no filename component"))?
            .to_string_lossy()
            .to_string(),
        sha256sum,
        datetime,
        size: metadata.len(),
        nr_packages,
        nr_provides,
        nr_essentials,
    };

    let json_content = serde_json::to_string_pretty(&packages_file_info)
        .map_err(|e| eyre::eyre!("Failed to serialize packages metadata: {}", e))?;

    std::fs::write(json_path, json_content)
        .map_err(|e| eyre::eyre!("Failed to write packages metadata to {}: {}", json_path.display(), e))?;

    Ok(packages_file_info)
}
