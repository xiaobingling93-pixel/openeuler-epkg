use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::collections::HashMap;
use color_eyre::eyre::{Result, WrapErr};
use flate2::read::GzDecoder;
use crate::models::*;
use crate::dirs;
use crate::repo::*;
use crate::lfs;

/// Brew formula structure from formula.json API
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrewFormula {
    pub name: String,
    #[serde(rename = "full_name")]
    pub full_name: String,
    pub desc: Option<String>,
    pub license: Option<String>,
    pub homepage: Option<String>,
    pub versions: BrewVersions,
    pub revision: u64,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    #[serde(rename = "build_dependencies")]
    pub build_dependencies: Vec<String>,
    #[serde(default)]
    #[serde(rename = "test_dependencies")]
    pub test_dependencies: Vec<String>,
    #[serde(default)]
    #[serde(rename = "runtime_dependencies")]
    pub runtime_dependencies: Vec<String>,
    #[serde(default)]
    #[serde(rename = "recommended_dependencies")]
    pub recommended_dependencies: Vec<String>,
    #[serde(default)]
    #[serde(rename = "optional_dependencies")]
    pub optional_dependencies: Vec<String>,
    #[serde(default)]
    pub conflicts_with: Vec<String>,
    #[serde(default)]
    pub bottle: Option<BrewBottle>,
    #[serde(default)]
    pub variations: HashMap<String, BrewVariation>,
}

/// Variation for specific platform/OS version
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrewVariation {
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    #[serde(rename = "build_dependencies")]
    pub build_dependencies: Vec<String>,
    #[serde(default)]
    #[serde(rename = "test_dependencies")]
    pub test_dependencies: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrewVersions {
    pub stable: Option<String>,
    pub head: Option<String>,
    pub bottle: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrewBottle {
    pub stable: Option<BrewBottleStable>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrewBottleStable {
    pub rebuild: u64,
    #[serde(rename = "root_url")]
    pub root_url: String,
    pub files: HashMap<String, BrewBottleFile>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrewBottleFile {
    pub cellar: String,
    pub url: String,
    pub sha256: String,
}

impl BrewFormula {
    /// Convert to epkg's internal Package format
    pub fn to_package(&self, bottle_tag: &str) -> Option<Package> {
        let bottle = self.bottle.as_ref()?.stable.as_ref()?;
        let bottle_file = bottle.files.get(bottle_tag)?;

        let version = self.versions.stable.clone()?;

        // Start with base dependencies
        let mut deps = self.runtime_dependencies.clone();
        deps.extend(self.dependencies.clone());

        let mut build_deps = self.build_dependencies.clone();
        let mut test_deps = self.test_dependencies.clone();
        let recommended = self.recommended_dependencies.clone();
        let optional = self.optional_dependencies.clone();

        // Apply variations for this bottle_tag if present
        // bottle_tag can be: sonoma, arm64_sonoma, x86_64_linux, arm64_linux, etc.
        // variations keys can be: sequoia, monterey, arm64_monterey, x86_64_linux, etc.
        if let Some(variation) = self.variations.get(bottle_tag) {
            // Variation dependencies replace (not extend) the base dependencies
            if !variation.dependencies.is_empty() {
                deps = variation.dependencies.clone();
            }
            if !variation.build_dependencies.is_empty() {
                build_deps = variation.build_dependencies.clone();
            }
            if !variation.test_dependencies.is_empty() {
                test_deps = variation.test_dependencies.clone();
            }
        }

        // Construct bottle filename following Homebrew's Bottle::Filename format:
        // Reference: Homebrew/Library/Homebrew/bottle.rb
        //
        // Format: {name}-{pkg_version}.{tag}.bottle{.rebuild}.tar.gz
        //
        // Where:
        // - pkg_version = version (when revision == 0) or version_revision (when revision > 0)
        //   See Homebrew/Library/Homebrew/pkg_version.rb PkgVersion#to_str
        // - rebuild suffix is only added when rebuild > 0
        //   See Bottle::Filename#extname: ".#{tag}.bottle#{s}.tar.gz"
        //   where s = rebuild.positive? ? ".#{rebuild}" : ""
        //
        // Examples:
        //   jq-1.8.1.sonoma.bottle.tar.gz           (version=1.8.1, revision=0, rebuild=0)
        //   lz4-1.10.0.sonoma.bottle.1.tar.gz       (version=1.10.0, revision=0, rebuild=1)
        //   aalib-1.4rc5_2.sonoma.bottle.tar.gz     (version=1.4rc5, revision=2, rebuild=0)
        let pkg_version = if self.revision > 0 {
            format!("{}_{}", version, self.revision)
        } else {
            version.clone()
        };
        let bottle_filename = if bottle.rebuild > 0 {
            format!("{}-{}.{}.bottle.{}.tar.gz", self.name, pkg_version, bottle_tag, bottle.rebuild)
        } else {
            format!("{}-{}.{}.bottle.tar.gz", self.name, pkg_version, bottle_tag)
        };

        // Location is just the filename; baseurl is set separately
        let location = bottle_filename;

        // Extract actual arch from bottle_tag
        // bottle_tag formats: sonoma, arm64_sonoma, ventura, arm64_ventura, x86_64_linux, arm64_linux
        // arm64 is prefixed for Apple Silicon, x86_64 is implicit for Intel macOS
        // Linux has explicit arch prefix: x86_64_linux, arm64_linux
        let arch = if bottle_tag.starts_with("arm64_") {
            "arm64"
        } else {
            "x86_64"
        };

        Some(Package {
            pkgname: self.name.clone(),
            version: format!("{}_{}", version, self.revision),
            arch: arch.to_string(),
            location,
            sha256sum: Some(bottle_file.sha256.clone()),
            requires: deps,
            build_requires: build_deps,
            check_requires: test_deps,
            recommends: recommended,
            suggests: optional,
            conflicts: self.conflicts_with.clone(),
            provides: vec![self.name.clone()],
            summary: self.desc.clone().unwrap_or_default(),
            description: self.desc.clone(),
            homepage: self.homepage.clone().unwrap_or_default(),
            license: self.license.clone(),
            // Store bottle_tag (platform info like sonoma, x86_64_linux) in tag field
            tag: Some(bottle_tag.to_string()),
            format: PackageFormat::Brew,
            ..Default::default()
        })
    }
}

/// Get the bottle tag for current OS/arch
pub fn get_bottle_tag() -> String {
    #[cfg(target_os = "macos")]
    {
        // Detect macOS version for bottle tag
        // sequoia (15), sonoma (14), ventura (13), monterey (12), big_sur (11)
        match std::process::Command::new("uname").arg("-r").output() {
            Ok(output) => {
                let version = String::from_utf8_lossy(&output.stdout);
                let parts: Vec<&str> = version.trim().split('.').collect();
                if let Ok(major) = parts[0].parse::<u32>() {
                    // Darwin version to macOS version mapping
                    // Darwin 24 -> macOS 15 (Sequoia)
                    // Darwin 23 -> macOS 14 (Sonoma)
                    // Darwin 22 -> macOS 13 (Ventura)
                    // etc.
                    match major {
                        24 => {
                            #[cfg(target_arch = "aarch64")]
                            return "arm64_sequoia".to_string();
                            #[cfg(target_arch = "x86_64")]
                            return "sequoia".to_string();
                        }
                        23 => {
                            #[cfg(target_arch = "aarch64")]
                            return "arm64_sonoma".to_string();
                            #[cfg(target_arch = "x86_64")]
                            return "sonoma".to_string();
                        }
                        22 => {
                            #[cfg(target_arch = "aarch64")]
                            return "arm64_ventura".to_string();
                            #[cfg(target_arch = "x86_64")]
                            return "ventura".to_string();
                        }
                        21 => {
                            #[cfg(target_arch = "aarch64")]
                            return "arm64_monterey".to_string();
                            #[cfg(target_arch = "x86_64")]
                            return "monterey".to_string();
                        }
                        _ => {
                            #[cfg(target_arch = "aarch64")]
                            return "arm64_ventura".to_string();
                            #[cfg(target_arch = "x86_64")]
                            return "ventura".to_string();
                        }
                    }
                }
            }
            Err(_) => {}
        }
        #[cfg(target_arch = "aarch64")]
        return "arm64_ventura".to_string();
        #[cfg(target_arch = "x86_64")]
        return "ventura".to_string();
    }

    #[cfg(target_os = "linux")]
    {
        #[cfg(target_arch = "aarch64")]
        return "arm64_linux".to_string();
        #[cfg(target_arch = "x86_64")]
        return "x86_64_linux".to_string();
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        "x86_64_linux".to_string()
    }
}

/// Parse formula.json from Brew API
pub fn parse_formula_json(repo: &RepoRevise, _release_dir: &PathBuf) -> Result<Vec<RepoReleaseItem>> {
    let mut release_items = Vec::new();

    let bottle_tag = get_bottle_tag();
    let repo_dir = dirs::get_repo_dir(&repo);
    let output_path = repo_dir.join("packages.txt");

    let url = repo.index_url.clone();
    let location = url.split('/').last().unwrap_or("formula.jws.json.gz").to_string();

    // For brew, use mirror URL as baseurl
    let package_baseurl = "https://mirrors.tuna.tsinghua.edu.cn/homebrew-bottles/bottles".to_string();

    let download_path = crate::mirror::Mirrors::url_to_cache_path(&url, &repo.repodata_name)
        .with_context(|| format!("Failed to convert URL to cache path: {}", url))?;

    let release_status = should_refresh_release_file(&download_path, repo)?;
    let need_download = matches!(release_status, ReleaseStatus::NeedDownload | ReleaseStatus::NeedUpdate);
    let need_convert = !lfs::exists_on_host(&output_path) || {
        let repoindex_path = repo_dir.join("RepoIndex.json");
        !lfs::exists_on_host(&repoindex_path)
    };

    release_items.push(RepoReleaseItem {
        repo_revise: repo.clone(),
        need_download,
        need_convert,
        arch: bottle_tag,
        url,
        package_baseurl,
        hash_type: "SHA256".to_string(),
        location,
        is_packages: true,
        output_path,
        download_path,
        ..Default::default()
    });

    Ok(release_items)
}

/// Process formula.json content
///
/// Handles both regular JSON array format and JWS (JSON Web Signature) format.
/// JWS format has the JSON array followed by a separate signatures object:
///   [{...}, {...}]\n{"signatures": [...]}
/// We extract and parse only the array part.
pub fn process_packages_content(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<PackagesFileInfo> {
    log::debug!("Starting to process Brew formula content for {}", revise.location);
    log::debug!("  repo_dir: {:?}", repo_dir);
    log::debug!("  output_path: {:?}", revise.output_path);
    log::debug!("  arch: {}", revise.arch);

    // Collect all data from the receiver
    let mut all_data = Vec::new();
    for chunk in data_rx {
        all_data.extend_from_slice(&chunk);
    }

    log::debug!("Received {} bytes of data", all_data.len());

    // Handle JWS format: extract just the JSON array part (before {"signatures":...})
    let json_data = extract_json_array_from_jws(&all_data)?;

    // Decompress if needed
    let formulas: Vec<BrewFormula> = if revise.location.ends_with(".gz") {
        let decoder = GzDecoder::new(&json_data[..]);
        serde_json::from_reader(decoder)
            .with_context(|| format!("Failed to parse compressed JSON for {}", revise.location))?
    } else {
        serde_json::from_slice(&json_data)
            .with_context(|| format!("Failed to parse JSON for {}", revise.location))?
    };

    log::debug!("Successfully parsed JSON, found {} formulas", formulas.len());

    process_brew_formulas(repo_dir, revise, formulas)
        .with_context(|| format!("Failed to process brew formulas for {}", revise.location))
}

/// Extract the JSON array part from JWS format
///
/// Handles multiple formats:
/// 1. JWS with JSON-escaped payload string: `{"payload":"[\\n  {...},...]","signatures":[...]}`
/// 2. Plain JSON array (official API format): `[{...}, {...}]`
fn extract_json_array_from_jws(data: &[u8]) -> Result<Vec<u8>> {
    // Try to parse as plain JSON array first (official API format)
    if serde_json::from_slice::<Vec<BrewFormula>>(data).is_ok() {
        return Ok(data.to_vec());
    }

    let data_str = String::from_utf8_lossy(data);

    // Check if it's JWS JSON serialization format with JSON-escaped payload
    // Format: {"payload":"[\n  {...},...]","signatures":[...]}
    if data_str.starts_with("{\"payload\":\"") {
        // Parse as JSON to extract the payload string
        #[derive(serde::Deserialize)]
        struct JwsFormat {
            payload: String,
        }

        if let Ok(jws) = serde_json::from_str::<JwsFormat>(&data_str) {
            log::debug!("Extracted JSON array from JWS payload string ({} bytes)", jws.payload.len());
            return Ok(jws.payload.into_bytes());
        }
    }

    // Fallback: try to parse the whole thing as-is (might be plain JSON)
    log::debug!("Could not detect JWS format, trying to parse as plain JSON");
    Ok(data.to_vec())
}

/// Process brew formulas and convert to packages.txt format
fn process_brew_formulas(repo_dir: &PathBuf, revise: &RepoReleaseItem, formulas: Vec<BrewFormula>) -> Result<PackagesFileInfo> {
    use std::io::Write;
    use std::collections::{HashMap, HashSet, BTreeMap};
    use crate::models::PackageRange;

    let bottle_tag = &revise.arch;
    let output_path = &revise.output_path;

    // Ensure parent directory exists
    if let Some(parent) = output_path.parent() {
        lfs::create_dir_all(parent)?;
    }

    let mut file = std::fs::File::create(output_path)
        .with_context(|| format!("Failed to create output file: {:?}", output_path))?;

    let mut package_count = 0;
    let mut pkgname2ranges: BTreeMap<String, Vec<PackageRange>> = BTreeMap::new();
    let mut current_offset: usize = 0;

    for formula in formulas {
        // Skip formulas without bottles for this tag
        if let Some(package) = formula.to_package(bottle_tag) {
            let package_begin = current_offset;

            // Write package entry in packages.txt format
            // Build the output string with all available fields
            let mut lines = Vec::new();
            lines.push(format!("pkgname: {}", package.pkgname));
            lines.push(format!("version: {}", package.version));
            lines.push(format!("arch: {}", package.arch));
            if let Some(ref tag) = package.tag {
                lines.push(format!("tag: {}", tag));
            }
            lines.push(format!("location: {}", package.location));
            if let Some(ref sha256) = package.sha256sum {
                lines.push(format!("sha256: {}", sha256));
            }
            if !package.requires.is_empty() {
                lines.push(format!("requires: {}", package.requires.join(", ")));
            }
            if !package.build_requires.is_empty() {
                lines.push(format!("buildRequires: {}", package.build_requires.join(", ")));
            }
            if !package.check_requires.is_empty() {
                lines.push(format!("checkRequires: {}", package.check_requires.join(", ")));
            }
            if !package.recommends.is_empty() {
                lines.push(format!("recommends: {}", package.recommends.join(", ")));
            }
            if !package.suggests.is_empty() {
                lines.push(format!("suggests: {}", package.suggests.join(", ")));
            }
            if !package.conflicts.is_empty() {
                lines.push(format!("conflicts: {}", package.conflicts.join(", ")));
            }
            if !package.summary.is_empty() {
                lines.push(format!("summary: {}", package.summary));
            }
            if !package.homepage.is_empty() {
                lines.push(format!("homepage: {}", package.homepage));
            }
            if let Some(ref license) = package.license {
                lines.push(format!("license: {}", license));
            }

            let pkg_block = lines.join("\n") + "\n\n"; // End with blank line between packages

            file.write_all(pkg_block.as_bytes())
                .with_context(|| format!("Failed to write package: {}", package.pkgname))?;

            let package_len = pkg_block.len();
            pkgname2ranges.insert(package.pkgname.clone(), vec![PackageRange { begin: package_begin, len: package_len }]);
            current_offset += package_len;
            package_count += 1;
        }
    }

    file.flush()?;
    log::info!("Converted {} brew formulas for {}", package_count, bottle_tag);

    // Get filename for generating related paths
    let filename = output_path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("packages.txt")
        .to_string();

    // Create provide2pkgnames.rkyv (empty for brew)
    let provide2pkgnames_path = repo_dir.join(filename.replace("packages", "provide2pkgnames")).with_extension("rkyv");
    let empty_provides: HashMap<String, Vec<String>> = HashMap::new();
    crate::mmio::serialize_provide2pkgnames(&provide2pkgnames_path, &empty_provides)
        .with_context(|| "Failed to serialize provide2pkgnames")?;

    // Create essential_pkgnames (empty for brew)
    let essential_pkgnames_path = repo_dir.join(filename.replace("packages", "essential_pkgnames"));
    let empty_essentials: HashSet<String> = HashSet::new();
    crate::mmio::serialize_essential_pkgnames(&essential_pkgnames_path, &empty_essentials)
        .with_context(|| "Failed to serialize essential_pkgnames")?;

    // Create pkgname2ranges.idx
    let pkgname2ranges_path = output_path.with_extension("idx");
    crate::mmio::serialize_pkgname2ranges(&pkgname2ranges_path, &pkgname2ranges)
        .with_context(|| "Failed to serialize pkgname2ranges")?;

    // Create RepoIndex.json
    let repoindex_path = repo_dir.join("RepoIndex.json");
    let repoindex = RepoIndex {
        url: revise.url.clone(),
        package_count,
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };

    let repoindex_json = serde_json::to_string(&repoindex)
        .with_context(|| "Failed to serialize RepoIndex")?;
    lfs::write(&repoindex_path, repoindex_json.as_bytes())?;

    // Create .packages.json metadata file
    let json_path = repo_dir.join(".packages.json");
    let packages_file_info = PackagesFileInfo {
        filename,
        sha256sum: String::new(),
        datetime: String::new(),
        size: 0,
        nr_packages: package_count,
        nr_provides: 0,
        nr_essentials: 0,
    };

    let packages_json = serde_json::to_string(&packages_file_info)
        .with_context(|| "Failed to serialize PackagesFileInfo")?;
    lfs::write(&json_path, packages_json.as_bytes())?;

    Ok(packages_file_info)
}

/// Simple RepoIndex structure for tracking
#[derive(Debug, serde::Serialize)]
struct RepoIndex {
    url: String,
    package_count: usize,
    timestamp: u64,
}
