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
    #[serde(default)]
    pub oldnames: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub desc: Option<String>,
    pub license: Option<String>,
    pub homepage: Option<String>,
    pub versions: BrewVersions,
    pub revision: u64,
    #[serde(default)]
    pub caveats: Option<String>,
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
    #[serde(default)]
    pub post_install_defined: bool,
    #[serde(default)]
    pub service: Option<BrewService>,
    #[serde(default, rename = "keg_only")]
    pub keg_only: bool,
    #[serde(default, rename = "keg_only_reason")]
    pub keg_only_reason: Option<BrewKegOnlyReason>,
    #[serde(default)]
    pub requirements: Vec<BrewRequirement>,
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

/// Service definition from brew formula
/// Reference: Homebrew/Library/Homebrew/service.rb to_hash method
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrewService {
    #[serde(default)]
    pub name: Option<BrewServiceName>,
    #[serde(default)]
    pub run: Option<serde_json::Value>,  // Can be string, array, or {macos:, linux:} hash
    #[serde(default, rename = "run_type")]
    pub run_type: Option<String>,
    #[serde(default)]
    pub interval: Option<u64>,
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default, rename = "keep_alive")]
    pub keep_alive: Option<serde_json::Value>,
    #[serde(default, rename = "launch_only_once")]
    pub launch_only_once: Option<bool>,
    #[serde(default, rename = "require_root")]
    pub require_root: Option<bool>,
    #[serde(default, rename = "environment_variables")]
    pub environment_variables: Option<HashMap<String, String>>,
    #[serde(default, rename = "working_dir")]
    pub working_dir: Option<String>,
    #[serde(default, rename = "root_dir")]
    pub root_dir: Option<String>,
    #[serde(default, rename = "log_path")]
    pub log_path: Option<String>,
    #[serde(default, rename = "error_log_path")]
    pub error_log_path: Option<String>,
    #[serde(default, rename = "restart_delay")]
    pub restart_delay: Option<u64>,
    #[serde(default)]
    pub sockets: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrewServiceName {
    #[serde(default)]
    pub macos: Option<String>,
    #[serde(default)]
    pub linux: Option<String>,
}

/// Keg-only reason from brew formula
/// Reference: Homebrew/Library/Homebrew/keg_only_reason.rb
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrewKegOnlyReason {
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub explanation: String,
}

/// Requirement from brew formula
/// Examples: macos, linux, arch, xcode, java, etc.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrewRequirement {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub cask: Option<String>,
    #[serde(default)]
    pub download: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub contexts: Vec<String>,
    #[serde(default)]
    pub specs: Vec<String>,
}

/// Get fallback bottle tags for macOS when the primary tag is not available.
/// Returns a list of tags to try in order of preference.
/// For arm64: tahoe -> sequoia -> sonoma -> ventura -> monterey
/// For x86_64: tahoe -> sequoia -> sonoma -> ventura -> monterey
fn get_bottle_tag_fallbacks(bottle_tag: &str) -> Vec<String> {
    // Check if it's arm64 or x86_64
    let (arm_prefix, tags): (&str, &[&str]) = if bottle_tag.starts_with("arm64_") {
        ("arm64_", &["tahoe", "sequoia", "sonoma", "ventura", "monterey"])
    } else if bottle_tag.starts_with("x86_64_") {
        // Linux uses x86_64_linux, no fallback needed
        return vec![bottle_tag.to_string()];
    } else {
        // Intel macOS tags don't have prefix
        ("", &["tahoe", "sequoia", "sonoma", "ventura", "monterey"])
    };

    // Find the position of current tag in the list
    let current_base = bottle_tag.strip_prefix(arm_prefix).unwrap_or(bottle_tag);
    let current_idx = tags.iter().position(|&t| t == current_base).unwrap_or(0);

    // Return tags from current position onwards (older versions as fallback)
    tags[current_idx..]
        .iter()
        .map(|&t| format!("{}{}", arm_prefix, t))
        .collect()
}

impl BrewFormula {
    /// Find the best available bottle for the requested tag.
    /// Falls back to older macOS versions if the exact tag is not available.
    fn find_best_bottle<'a>(&'a self, bottle: &'a BrewBottleStable, bottle_tag: &str) -> Option<(&'a BrewBottleFile, String)> {
        // First, try the exact bottle_tag
        if let Some(file) = bottle.files.get(bottle_tag) {
            return Some((file, bottle_tag.to_string()));
        }

        // Try "all" for noarch packages
        if let Some(file) = bottle.files.get("all") {
            return Some((file, "all".to_string()));
        }

        // Try fallback tags for older macOS versions
        for fallback_tag in get_bottle_tag_fallbacks(bottle_tag) {
            if fallback_tag != bottle_tag {
                if let Some(file) = bottle.files.get(&fallback_tag) {
                    log::debug!("Using fallback bottle tag {} instead of {} for {}",
                               fallback_tag, bottle_tag, self.name);
                    return Some((file, fallback_tag));
                }
            }
        }

        None
    }

    /// Convert to epkg's internal Package format
    pub fn to_package(&self, bottle_tag: &str) -> Option<Package> {
        let bottle = self.bottle.as_ref()?.stable.as_ref()?;

        // Try the specific bottle_tag first, then try fallback tags for older macOS versions
        // This handles cases where a package doesn't have a bottle for the newest macOS
        let (bottle_file, actual_tag) = self.find_best_bottle(bottle, bottle_tag)?;

        // Determine if this is a noarch package
        let is_noarch = bottle.files.contains_key("all") && !bottle.files.contains_key(bottle_tag);

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
            format!("{}-{}.{}.bottle.{}.tar.gz", self.name, pkg_version, actual_tag, bottle.rebuild)
        } else {
            format!("{}-{}.{}.bottle.tar.gz", self.name, pkg_version, actual_tag)
        };

        // Location is just the filename; baseurl is set separately
        let location = bottle_filename;

        // Determine arch: "all" for noarch packages, otherwise extract from bottle_tag
        // bottle_tag formats: sonoma, arm64_sonoma, ventura, arm64_ventura, x86_64_linux, arm64_linux
        // arm64 is prefixed for Apple Silicon, x86_64 is implicit for Intel macOS
        // Linux has explicit arch prefix: x86_64_linux, arm64_linux
        let arch = if is_noarch {
            "all"
        } else if bottle_tag.starts_with("arm64_") {
            "arm64"
        } else {
            "x86_64"
        };

        // Build provides: pkgname + aliases + oldnames
        let mut provides = vec![self.name.clone()];
        provides.extend(self.aliases.clone());
        provides.extend(self.oldnames.clone());

        // Serialize service to JSON if present
        let service_json = self.service.as_ref()
            .and_then(|s| serde_json::to_string(s).ok());

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
            provides,
            summary: self.desc.clone().unwrap_or_default(),
            description: self.desc.clone(),
            homepage: self.homepage.clone().unwrap_or_default(),
            caveats: self.caveats.clone(),
            license: self.license.clone(),
            // Store bottle_tag (platform info like sonoma, x86_64_linux, or "all") in tag field
            tag: Some(actual_tag.to_string()),
            service_json,
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
        // tahoe (26), sequoia (15), sonoma (14), ventura (13), monterey (12), big_sur (11)
        match std::process::Command::new("uname").arg("-r").output() {
            Ok(output) => {
                let version = String::from_utf8_lossy(&output.stdout);
                let parts: Vec<&str> = version.trim().split('.').collect();
                if let Ok(major) = parts[0].parse::<u32>() {
                    // Darwin version to macOS version mapping
                    // Darwin 25 -> macOS 26 (Tahoe)
                    // Darwin 24 -> macOS 15 (Sequoia)
                    // Darwin 23 -> macOS 14 (Sonoma)
                    // Darwin 22 -> macOS 13 (Ventura)
                    // etc.
                    match major {
                        25 => {
                            #[cfg(target_arch = "aarch64")]
                            return "arm64_tahoe".to_string();
                            #[cfg(target_arch = "x86_64")]
                            return "tahoe".to_string();
                        }
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
                            // For newer macOS versions not yet in mapping, use tahoe
                            // Homebrew typically supports the latest macOS with backward compatibility
                            if major > 25 {
                                #[cfg(target_arch = "aarch64")]
                                return "arm64_tahoe".to_string();
                                #[cfg(target_arch = "x86_64")]
                                return "tahoe".to_string();
                            } else {
                                #[cfg(target_arch = "aarch64")]
                                return "arm64_ventura".to_string();
                                #[cfg(target_arch = "x86_64")]
                                return "ventura".to_string();
                            }
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

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return "arm64_linux".to_string();
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return "x86_64_linux".to_string();

    #[cfg(not(any(
        target_os = "macos",
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
    )))]
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

fn brew_package_entry_lines(package: &Package) -> Vec<String> {
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
    if !package.provides.is_empty() {
        lines.push(format!("provides: {}", package.provides.join(", ")));
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
    if let Some(ref caveats) = package.caveats {
        let caveats_lines: Vec<String> = caveats.lines().enumerate().map(|(i, line)| {
            if i == 0 {
                format!("caveats: {}", line)
            } else {
                format!(" {}", line)
            }
        }).collect();
        lines.push(caveats_lines.join("\n"));
    }
    if let Some(ref service_json) = package.service_json {
        lines.push(format!("serviceJson: {}", service_json));
    }
    lines
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
    let mut provide2pkgnames: HashMap<String, Vec<String>> = HashMap::new();
    let mut current_offset: usize = 0;

    for formula in formulas {
        // Skip formulas without bottles for this tag
        if let Some(package) = formula.to_package(bottle_tag) {
            let package_begin = current_offset;

            // Build provide2pkgnames from aliases and oldnames
            let pkgname = &package.pkgname;
            for alias in &formula.aliases {
                provide2pkgnames.entry(alias.clone())
                    .or_insert_with(Vec::new)
                    .push(pkgname.clone());
            }
            for oldname in &formula.oldnames {
                provide2pkgnames.entry(oldname.clone())
                    .or_insert_with(Vec::new)
                    .push(pkgname.clone());
            }

            let lines = brew_package_entry_lines(&package);
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

    let nr_provides = provide2pkgnames.len();

    // Create provide2pkgnames.rkyv from aliases and oldnames
    let provide2pkgnames_path = repo_dir.join(filename.replace("packages", "provide2pkgnames")).with_extension("rkyv");
    crate::mmio::serialize_provide2pkgnames(&provide2pkgnames_path, &provide2pkgnames)
        .with_context(|| "Failed to serialize provide2pkgnames")?;

    // Create essential_pkgnames for brew
    // glibc is essential for Linux brew bottles to have proper dynamic linker
    let essential_pkgnames_path = repo_dir.join(filename.replace("packages", "essential_pkgnames"));
    #[allow(unused_mut)]
    let mut essential_pkgnames: HashSet<String> = HashSet::new();
    #[cfg(target_os = "linux")]
    {
        essential_pkgnames.insert("glibc".to_string());
    }
    crate::mmio::serialize_essential_pkgnames(&essential_pkgnames_path, &essential_pkgnames)
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

    let nr_essentials = essential_pkgnames.len();

    // Create .packages.json metadata file
    let json_path = repo_dir.join(".packages.json");
    let packages_file_info = PackagesFileInfo {
        filename,
        sha256sum: String::new(),
        datetime: String::new(),
        size: 0,
        nr_packages: package_count,
        nr_provides,
        nr_essentials,
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
