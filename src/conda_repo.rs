use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::collections::{HashMap, HashSet, BTreeMap};
use std::fs::OpenOptions;
use std::io::{Write, BufWriter};
use std::fmt::Write as FmtWrite;
use color_eyre::eyre::{Result, WrapErr};
use color_eyre::eyre;
use flate2::read::GzDecoder;
use bzip2::read::BzDecoder;
use sha2::{Sha256, Digest};
use hex;
use time::{OffsetDateTime, format_description};
use serde::de::{Error, Visitor};
use std::fmt;
use crate::models::*;
use crate::dirs;
use crate::repo::*;
use crate::packages_stream;
use crate::mmio;

/// Custom deserializer for noarch field which can be either a string or a boolean.
/// If it's a boolean `true`, converts it to the string "generic".
/// If it's a boolean `false`, returns None.
/// If it's a string, uses it as-is.
fn deserialize_noarch<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct NoarchVisitor;

    impl<'de> Visitor<'de> for NoarchVisitor {
        type Value = Option<String>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a string or boolean")
        }

        fn visit_bool<E>(self, v: bool) -> Result<Option<String>, E>
        where
            E: Error,
        {
            if v {
                // Boolean true means generic noarch
                Ok(Some("generic".to_string()))
            } else {
                // Boolean false means not noarch
                Ok(None)
            }
        }

        fn visit_str<E>(self, v: &str) -> Result<Option<String>, E>
        where
            E: Error,
        {
            Ok(Some(v.to_string()))
        }

        fn visit_string<E>(self, v: String) -> Result<Option<String>, E>
        where
            E: Error,
        {
            Ok(Some(v))
        }

        fn visit_none<E>(self) -> Result<Option<String>, E>
        where
            E: Error,
        {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Option<String>, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            deserializer.deserialize_any(NoarchVisitor)
        }
    }

    deserializer.deserialize_any(NoarchVisitor)
}

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
    #[serde(default, deserialize_with = "deserialize_noarch")]
    pub noarch: Option<String>,
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

        // Combine version and build string using VERSION_BUILD_SEPARATOR
        let version_with_build = if let Some(build) = &self.build {
            format!("{}{}{}", self.version, crate::conda_pkg::VERSION_BUILD_SEPARATOR, build)
        } else {
            self.version.clone()
        };

        Package {
            pkgname: self.name.clone(),
            version: version_with_build,
            arch: arch.to_string(),
            size: self.size.unwrap_or(0) as u32,
            installed_size: 0,
            build_time,
            location: filename.to_string(),
            sha256sum: self.sha256.clone(),
            requires: self.depends.clone(),
            provides: vec![self.name.clone()],
            summary: self.summary.clone().unwrap_or_default(),
            description: self.description.clone(),
            homepage: self.url.clone().unwrap_or_default(),
            format: PackageFormat::Conda,
            ..Default::default()
        }
    }
}

/// Parse repodata.json content from Conda repositories
pub fn parse_repodata_json(repo: &RepoRevise, _release_dir: &PathBuf) -> Result<Vec<RepoReleaseItem>> {
    let mut release_items = Vec::new();

    // Detect if this is a noarch repository by checking URL or repodata_name
    let is_noarch = repo.index_url.contains("/noarch/") || repo.repodata_name.ends_with("-noarch");

    // For noarch repositories, use "all" as the architecture instead of the repo's arch
    // Note: repo.arch is already set to "all" for noarch repos in get_revise_repos(),
    // but we still need effective_arch for the output filename
    let effective_arch = if is_noarch {
        "all".to_string()
    } else {
        map_conda_arch_to_standard(&repo.arch)
    };

    // Use the standard get_repo_dir() - it now works correctly because repo.arch is "all" for noarch
    let repo_dir = dirs::get_repo_dir(&repo);

    let output_path = repo_dir.join(format!("packages-{}.txt", effective_arch));

    let url = repo.index_url.clone();
    // Extract location as the relative path from the base URL (just the filename for conda)
    // This matches the pattern used in deb_repo.rs where location is the relative path
    let location = url.split('/').last().unwrap_or("repodata.json.gz").to_string();

    let package_baseurl = if let Some(last_slash_pos) = url.rfind('/') {
        url[..last_slash_pos].to_string()
    } else {
        url.clone()
    };

    // Use url_to_cache_path to get the download_path, matching the pattern used in rpm_repo.rs
    // This ensures consistency with the download system's path resolution
    let download_path = crate::mirror::Mirrors::url_to_cache_path(&url, &repo.repodata_name)
        .with_context(|| format!("Failed to convert URL to cache path: {}", url))?;

    let need_download = !download_path.exists();
    let need_convert = !output_path.exists() || {
        let repoindex_path = repo_dir.join("RepoIndex.json");
        !repoindex_path.exists()
    };

    release_items.push(RepoReleaseItem {
        repo_revise: repo.clone(),
        need_download,
        need_convert,
        arch: effective_arch.clone(),
        url,
        package_baseurl: package_baseurl.to_string(),
        hash_type: "SHA256".to_string(),
        hash: String::new(),
        size: 0,
        location,
        is_packages: true,
        is_adb: false,
        output_path,
        download_path,
    });

    Ok(release_items)
}

pub fn process_packages_content(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<PackagesFileInfo> {
    log::debug!("Starting to process Conda packages content for {}", revise.location);
    log::debug!("  repo_dir: {:?}", repo_dir);
    log::debug!("  output_path: {:?}", revise.output_path);
    log::debug!("  arch: {}", revise.arch);

    let reader = packages_stream::ReceiverHasher::new(data_rx, String::new());
    log::debug!("Created ReceiverHasher, starting to parse compressed JSON for {}", revise.location);

    let repodata: CondaRepoData = parse_compressed_json(reader, &revise.location)
        .with_context(|| format!("Failed to parse compressed JSON for {}", revise.location))?;

    log::debug!("Successfully parsed JSON, found {} packages + {} packages.conda",
                repodata.packages.len(), repodata.packages_conda.len());

    process_conda_repodata(repo_dir, revise, repodata)
        .with_context(|| format!("Failed to process conda repodata for {}", revise.location))
}

fn parse_compressed_json(reader: packages_stream::ReceiverHasher, location: &str) -> Result<CondaRepoData> {
    log::debug!("parse_compressed_json: Starting for {}", location);

    let result = if location.ends_with(".gz") {
        log::debug!("Using GZIP decoder for {}", location);
        match serde_json::from_reader::<_, CondaRepoData>(GzDecoder::new(reader)) {
            Ok(data) => Ok(data),
            Err(e) => {
                log::error!("GZIP JSON parsing failed for {}: {}", location, e);
                if e.is_io() {
                    log::error!("IO error details: {}", e);
                } else if e.is_data() {
                    log::error!("JSON data error - invalid JSON structure: {}", e);
                } else if e.is_syntax() {
                    log::error!("JSON syntax error: {}", e);
                }
                Err(e)
            }
        }
    } else if location.ends_with(".bz2") {
        log::debug!("Using BZIP2 decoder for {}", location);
        match serde_json::from_reader::<_, CondaRepoData>(BzDecoder::new(reader)) {
            Ok(data) => Ok(data),
            Err(e) => {
                log::error!("BZIP2 JSON parsing failed for {}: {}", location, e);
                Err(e)
            }
        }
    } else {
        log::debug!("Parsing uncompressed JSON for {}", location);
        match serde_json::from_reader::<_, CondaRepoData>(reader) {
            Ok(data) => Ok(data),
            Err(e) => {
                log::error!("Uncompressed JSON parsing failed for {}: {}", location, e);
                Err(e)
            }
        }
    };

    result.map_err(|e| {
        let error_type = if e.is_io() {
            "IO"
        } else if e.is_data() {
            "JSON data"
        } else if e.is_syntax() {
            "JSON syntax"
        } else {
            "JSON parsing"
        };

        let error_msg = format!("{} error for {}: {}", error_type, location, e);
        log::error!("{}", error_msg);

        let mut err = eyre::eyre!("{}", error_msg);
        if e.is_io() {
            err = err.wrap_err("This might indicate download corruption, incomplete download, or hash validation failure.");
        } else if e.is_data() {
            err = err.wrap_err("The JSON structure may be invalid or the file may be corrupted.");
        } else if e.is_syntax() {
            err = err.wrap_err("The JSON syntax is invalid. The file may be corrupted or not valid JSON.");
        }
        err
    })
}

fn process_conda_repodata(
    repo_dir: &PathBuf,
    revise: &RepoReleaseItem,
    repodata: CondaRepoData,
) -> Result<PackagesFileInfo> {
    log::debug!("process_conda_repodata: Starting for {}", revise.location);
    log::debug!("  repo_dir: {:?}", repo_dir);
    log::debug!("  output_path: {:?}", revise.output_path);

    let mut packages: Vec<Package> = Vec::new();
    let mut provide2pkgnames: HashMap<String, Vec<String>> = HashMap::new();
    let essential_pkgnames: HashSet<String> = HashSet::new();
    let mut pkgname2ranges: BTreeMap<String, Vec<PackageRange>> = BTreeMap::new();
    let mut output = String::new();

    let total_packages = repodata.packages.len() + repodata.packages_conda.len();
    log::debug!("Total packages to process: {} ({} packages + {} packages.conda)",
                total_packages, repodata.packages.len(), repodata.packages_conda.len());

    if total_packages > 0 {
        let estimated_total_size = total_packages * 800;
        output.reserve(estimated_total_size);
        log::debug!("Pre-allocated {} bytes for {} packages", estimated_total_size, total_packages);
    }

    let arch = repodata.info
        .as_ref()
        .and_then(|info| info.subdir.as_ref())
        .map(|subdir| {
            let mapped = map_conda_arch_to_standard(subdir);
            log::debug!("Using arch from repodata.info.subdir: {} -> {}", subdir, mapped);
            mapped
        })
        .unwrap_or_else(|| {
            let mapped = map_conda_arch_to_standard(&revise.arch);
            log::debug!("Using arch from revise.arch: {} -> {}", revise.arch, mapped);
            mapped
        });

    log::debug!("Effective arch for processing: {}", arch);

    let removed_packages: HashSet<String> = repodata.removed.into_iter().collect();
    if !removed_packages.is_empty() {
        log::debug!("Found {} removed packages that will be excluded", removed_packages.len());
    }

    let mut processed_count = 0;
    let mut skipped_count = 0;
    let mut error_count = 0;

    let package_sets = [
        (&repodata.packages, "tar.bz2"),
        (&repodata.packages_conda, "conda")
    ];

    for (package_map, package_format) in package_sets {
        log::debug!("Processing {} {} packages", package_map.len(), package_format);
        for (filename, conda_pkg) in package_map {
            if removed_packages.contains(filename) {
                log::trace!("Skipping removed {} package: {}", package_format, filename);
                skipped_count += 1;
                continue;
            }

            match process_conda_package(conda_pkg, filename, package_format, &arch, &mut output, &mut provide2pkgnames, &mut pkgname2ranges) {
                Ok(package) => {
                    packages.push(package);
                    processed_count += 1;
                }
                Err(e) => {
                    error_count += 1;
                    log::error!("Failed to process package {} ({}): {}", filename, package_format, e);
                    // Continue processing other packages instead of failing completely
                    // This allows us to see all errors, not just the first one
                }
            }
        }
    }

    if error_count > 0 {
        return Err(eyre::eyre!("Failed to process {} packages from Conda repodata. Processed: {}, Skipped: {}",
                               error_count, processed_count, skipped_count));
    }

    log::debug!("Processed {} packages from Conda repodata (skipped {} removed packages)", processed_count, skipped_count);

    log::debug!("Writing conda packages output to {:?}", revise.output_path);
    write_conda_packages_output(
        repo_dir,
        revise,
        output,
        provide2pkgnames,
        essential_pkgnames,
        pkgname2ranges,
        packages.len(),
    )
    .with_context(|| format!("Failed to write conda packages output for {}", revise.location))
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
    
    // Version already includes build string from to_package(), so use it directly
    writeln!(output, "version: {}", package.version).unwrap();
    
    writeln!(output, "arch: {}", package.arch).unwrap();
    writeln!(output, "location: {}", package.location).unwrap();

    if package.size > 0 {
        writeln!(output, "size: {}", package.size).unwrap();
    }

    if let Some(build_time) = package.build_time {
        writeln!(output, "buildTime: {}", build_time).unwrap();
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

    if let Some(noarch) = &conda_pkg.noarch {
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
    log::debug!("write_conda_packages_output: Starting");
    log::debug!("  repo_dir: {:?}", repo_dir);
    log::debug!("  output_path: {:?}", revise.output_path);
    log::debug!("  output size: {} bytes", output.len());

    let output_path = &revise.output_path;
    let filename = output_path.file_name()
        .ok_or_else(|| {
            let err = eyre::eyre!("Invalid output path: no filename component: {:?}", output_path);
            log::error!("{}", err);
            err
        })?
        .to_string_lossy();

    log::debug!("Extracted filename: {}", filename);

    let (_, provide2pkgnames_path, essential_pkgnames_path, pkgname2ranges_path) =
        crate::mmio::get_package_paths(repo_dir, &filename);

    let json_path = {
        let file_stem = output_path.file_stem()
            .ok_or_else(|| eyre::eyre!("Invalid output path: no file stem"))?
            .to_string_lossy();
        repo_dir.join(format!(".{}.json", file_stem))
    };

    if let Some(parent) = output_path.parent() {
        log::debug!("Creating output directory: {:?}", parent);
        std::fs::create_dir_all(parent)
            .map_err(|e| {
                let err = eyre::eyre!("Failed to create output directory {:?}: {}", parent, e);
                log::error!("{}", err);
                err
            })?;
        log::debug!("Successfully created output directory: {:?}", parent);
    } else {
        log::warn!("output_path has no parent directory: {:?}", output_path);
    }

    if let Some(parent) = json_path.parent() {
        log::debug!("Creating json directory: {:?}", parent);
        std::fs::create_dir_all(parent)
            .map_err(|e| {
                let err = eyre::eyre!("Failed to create json directory {:?}: {}", parent, e);
                log::error!("{}", err);
                err
            })?;
        log::debug!("Successfully created json directory: {:?}", parent);
    } else {
        log::warn!("json_path has no parent directory: {:?}", json_path);
    }

    let mut hasher = Sha256::new();
    hasher.update(output.as_bytes());
    let sha256sum = hex::encode(hasher.finalize());

    log::debug!("Opening output file: {:?}", output_path);
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(output_path)
        .map_err(|e| {
            let err = eyre::eyre!("Failed to create output file {}: {}", output_path.display(), e);
            log::error!("{}", err);
            err
        })?;
    log::debug!("Successfully opened output file: {:?}", output_path);

    let mut writer = BufWriter::new(file);
    log::debug!("Writing {} bytes to output file", output.as_bytes().len());
    writer.write_all(output.as_bytes())
        .map_err(|e| {
            let err = eyre::eyre!("Failed to write to output file {}: {}", output_path.display(), e);
            log::error!("{}", err);
            err
        })?;
    writer.flush()
        .map_err(|e| {
            let err = eyre::eyre!("Failed to flush output file {}: {}", output_path.display(), e);
            log::error!("{}", err);
            err
        })?;
    log::debug!("Successfully wrote and flushed output file: {:?}", output_path);

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
