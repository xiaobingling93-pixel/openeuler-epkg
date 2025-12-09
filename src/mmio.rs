use std::fs;
use std::fs::File;
use std::path::PathBuf;
use std::collections::{HashMap, HashSet, BTreeMap};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use memmap2::Mmap;
use color_eyre::eyre::{Result, WrapErr};
use color_eyre::eyre;
use crate::models::*;
use crate::repo::RepoRevise;
use crate::package;

// Global status to track if provide2pkgnames data has been loaded
static PROVIDE2PKGNAMES_LOADED: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
pub struct FileMapper {
    #[allow(dead_code)]
    file: File,
    mmap: Mmap,
}

impl FileMapper {
    pub fn new(file_path: &str) -> std::io::Result<Self> {
        let file = File::open(file_path)?;
        // Memory map the file (unsafe because we must ensure the file isn't modified externally)
        let mmap = unsafe { Mmap::map(&file)? };
        Ok(Self { file, mmap })
    }

    /// Get the entire mapped data
    #[allow(dead_code)]
    pub fn data(&self) -> &[u8] {
        &self.mmap
    }

    /// Get a specific range of the mapped data
    /// Panics if range is out of bounds
    pub fn range(&self, range: &PackageRange) -> &[u8] {
        &self.mmap[range.begin..(range.begin + range.len)]
    }

    /// Safe range access with bounds checking
    pub fn checked_range(&self, range: &PackageRange) -> Option<&[u8]> {
        if range.begin + range.len <= self.mmap.len() {
            Some(&self.range(range))
        } else {
            None
        }
    }
}

// // Example usage
// fn main() -> std::io::Result<()> {
//     let mapper = FileMapper::new("example.txt")?;
//
//     // Access first 100 bytes
//     if let Some(data) = mapper.checked_range(0..100) {
//         println!("First 100 bytes: {:?}", data);
//     }
//
//     // Process the entire file in chunks
//     let chunk_size = 4096;
//     for chunk in mapper.data().chunks(chunk_size) {
//         // Process each chunk
//         println!("Chunk length: {}", chunk.len());
//     }
//
//     Ok(())
// }

/// Deserializes essential package names from a file
pub fn deserialize_repoindex(file_path: &PathBuf) -> Result<RepoIndex> {
    let contents = fs::read_to_string(&file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

    let repoindex: RepoIndex = serde_json::from_str(&contents)
        .with_context(|| format!("Failed to parse JSON from file: {}", file_path.display()))?;

    Ok(repoindex)
}

/// Get standard package-related paths based on a base packages path
pub fn get_package_paths(repo_dir: &PathBuf, packages_filename: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let packages_path = repo_dir.join(packages_filename);
    let provide2pkgnames_path = repo_dir.join(packages_filename.replace("packages", "provide2pkgnames")).with_extension("yaml");
    let essential_pkgnames_path = repo_dir.join(packages_filename.replace("packages", "essential_pkgnames"));
    let pkgname2ranges_path = packages_path.with_extension("idx");

    (packages_path, provide2pkgnames_path, essential_pkgnames_path, pkgname2ranges_path)
}

pub fn populate_repoindex_data(repo: &RepoRevise, mut repo_index: RepoIndex) -> Result<()> {
    let repo_dir = crate::dirs::get_repo_dir(&repo)?;

    let load_mappings = crate::models::config().subcommand != EpkgCommand::Search;

    if load_mappings {
        for (_, shard) in &mut repo_index.repo_shards {
            let filename = shard.packages.filename.clone();
            let (packages_path, _provide2pkgnames_path, essential_pkgnames_path, pkgname2ranges_path) =
                get_package_paths(&repo_dir, &filename);
            shard.packages_mmap = Some(FileMapper::new(packages_path.to_str().unwrap())?);
            shard.essential_pkgnames = deserialize_essential_pkgnames(&essential_pkgnames_path)?;
            shard.pkgname2ranges_path = Some(pkgname2ranges_path);
        }
    }

    // Store the repo directory path in the RepoIndex for later use
    repo_index.repo_dir_path = repo_dir.to_string_lossy().to_string();

    {
        let mut repodata_indice = repodata_indice_mut();
        repodata_indice.insert(repo.repodata_name.clone(), repo_index);
    }
    Ok(())
}

/// Load provide2pkgnames data on demand for all repository shards
pub fn ensure_provide2pkgnames_loaded() -> Result<()> {
    // Check if already loaded using atomic flag
    if PROVIDE2PKGNAMES_LOADED.load(Ordering::Relaxed) {
        return Ok(());
    }

    // During tests, repodata_indice will be empty, so skip loading
    let repodata_indice_check = repodata_indice();
    if repodata_indice_check.is_empty() {
        // During tests, no repos are loaded, so mark as loaded to avoid repeated checks
        PROVIDE2PKGNAMES_LOADED.store(true, Ordering::Relaxed);
        return Ok(());
    }
    drop(repodata_indice_check);

    let mut repodata_indice = repodata_indice_mut();

    for repo_index in repodata_indice.values_mut() {
        // Use the stored repo directory path
        let repo_dir = PathBuf::from(&repo_index.repo_dir_path);

        for shard in repo_index.repo_shards.values_mut() {
            let filename = shard.packages.filename.clone();
            let (_packages_path, provide2pkgnames_path, _essential_pkgnames_path, _pkgname2ranges_path) =
                get_package_paths(&repo_dir, &filename);

            // Load provide2pkgnames data from file
            match deserialize_provide2pkgnames(&provide2pkgnames_path) {
                Ok(provide2pkgnames) => {
                    shard.provide2pkgnames = provide2pkgnames;
                },
                Err(e) => {
                    log::warn!("Failed to load provide2pkgnames from {}: {}", provide2pkgnames_path.display(), e);
                    // Set empty HashMap if loading fails
                    shard.provide2pkgnames = std::collections::HashMap::new();
                }
            }
        }
    }

    // Mark as loaded after processing all shards
    PROVIDE2PKGNAMES_LOADED.store(true, Ordering::Relaxed);

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
        // Filter out trivial entries where key equals value
        // Also filter out values that equal the key
        // Deduplicate values using HashSet
        let filtered_values: HashSet<String> = values.iter()
            .filter(|value| *value != key)
            .cloned()
            .collect();

        // Only write the line if there are non-trivial values
        if !filtered_values.is_empty() {
            // Convert to sorted Vec for consistent output
            let mut sorted_values: Vec<String> = filtered_values.into_iter().collect();
            sorted_values.sort();
            let line = format!("{}: {}", key, sorted_values.join(" "));
            writeln!(writer, "{}", line)?;
        }
    }

    Ok(())
}

/// Estimate the number of lines in a file based on its size and an average bytes-per-line value
fn estimate_lines_from_file_size(file_path: &std::path::Path, bytes_per_line: u64, fallback: usize) -> usize {
    let file_size = std::fs::metadata(file_path)
        .map(|m| m.len())
        .unwrap_or(0);
    if file_size > 0 {
        (file_size / bytes_per_line) as usize
    } else {
        fallback
    }
}

/// Deserializes package provides mapping from a file
pub fn deserialize_provide2pkgnames(file_path: &PathBuf) -> Result<HashMap<String, Vec<String>>> {
    log::debug!("deserialize_provide2pkgnames for {}", file_path.display());

    let file = File::open(file_path)?;
    let reader = BufReader::new(file);

    // Estimate number of entries based on file size and average bytes per line (46 bytes/line)
    let estimated_lines = estimate_lines_from_file_size(file_path, 46, 64);
    let mut map: HashMap<String, Vec<String>> = HashMap::with_capacity(estimated_lines);

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
pub fn serialize_pkgname2ranges(path: &PathBuf, pkgname2ranges: &BTreeMap<String, Vec<PackageRange>>) -> Result<()> {
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
pub fn deserialize_pkgname2ranges(path: &PathBuf) -> Result<BTreeMap<String, Vec<PackageRange>>> {
    log::debug!("deserialize_pkgname2ranges for {}", path.display());

    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read index file: {}", path.display()))?;

    let mut pkgname2ranges = BTreeMap::new();
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

pub fn deserialize_package(paragraph: &str) -> Result<Package> {
    let mut package = Package {
        pkgname: String::new(),
        version: String::new(),
        arch: String::new(),
        size: 0,
        installed_size: 0,
        build_time: None,
        source: None,
        location: String::new(),

        // caHash is only available in installed epkg_store/fs/package.txt,
        // when the struct is loaded by map_pkgline2package()
        ca_hash: None,

        // Apk only has sha1sum; other formats only have sha256sum
        sha256sum: None,
        sha1sum: None,

        depends: Vec::new(),
        requires_pre: Vec::new(),
        requires: Vec::new(),
        build_requires: Vec::new(),
        check_requires: Vec::new(),
        provides: Vec::new(),
        recommends: Vec::new(),
        suggests: Vec::new(),
        conflicts: Vec::new(),
        obsoletes: Vec::new(),
        enhances: Vec::new(),
        supplements: Vec::new(),
        files: Vec::new(),
        summary: String::new(),
        description: None,
        homepage: String::new(),
        section: None,
        priority: None,
        maintainer: String::new(),
        tag: None,
        origin_url: None,
        multi_arch: None,
        format: PackageFormat::default(),
        pkgkey: String::new(),
        repodata_name: String::new(),
        package_baseurl: String::new(),
    };

    // Track the current key and value for multi-line handling
    let mut current_key = String::new();
    let mut current_value = String::new();

    for line in paragraph.lines() {
        if let Some((key, value)) = line.split_once(": ") {
            // If we have a previous key/value pair, process it before starting a new one
            if !current_key.is_empty() {
                process_key_value(&mut package, &current_key, &current_value)?;
                current_key.clear();
                current_value.clear();
            }

            current_key = key.trim().to_string();
            current_value = value.trim().to_string();
        } else if line.starts_with(" ") && !current_key.is_empty() {
            // This is a continuation line (indented follow-up line)
            // Add it to the current value with a newline
            current_value.push('\n');
            current_value.push_str(line.trim());
        }
    }

    // Process the last key/value pair if any
    if !current_key.is_empty() {
        process_key_value(&mut package, &current_key, &current_value)?;
    }
    if package.location.is_empty() { // APKINDEX misses location field
        package.location = format!("{}-{}.apk", package.pkgname, package.version);
    }
    package.pkgkey = package::format_pkgkey(&package.pkgname, &package.version, &package.arch);

    Ok(package)
}

// Helper function to process a key/value pair
fn process_key_value(package: &mut Package, key: &str, value: &str) -> Result<()> {
    match key {
        "format" => {
            package.format = PackageFormat::from_str(value)?;
            return Ok(());
        }
        _ => {}
    }

    match key {
        "pkgname"           => package.pkgname      = value.to_string(),
        "version"           => package.version      = value.to_string(),
        "arch"              => package.arch         = value.to_string(),
        "summary"           => package.summary      = value.to_string(),
        "description"       => package.description  = Some(value.to_string()),
        "location"          => package.location     = value.to_string(),
        "homepage"          => package.homepage     = value.to_string(),
        "maintainer"        => package.maintainer   = value.to_string(),
        "section"           => package.section      = Some(value.to_string()),
        "priority"          => package.priority     = Some(value.to_string()),
        "size"              => if let Ok(size)      = value.parse() { package.size = size; },
        "installedSize"     => if let Ok(size)      = value.parse() { package.installed_size = size; },
        "buildTime"         => if let Ok(time)      = value.parse() { package.build_time = Some(time); },
        "sha256"            => package.sha256sum    = Some(value.to_string()),
        "sha1"              => package.sha1sum      = Some(value.to_string()),
        "tag"               => package.tag          = Some(value.to_string()),
        "multiArch"         => package.multi_arch   = Some(value.to_string()),
        "requiresPre"       => package.requires_pre = value.split(", ").map(|s| s.to_string()).collect(),
        "requires"          => package.requires     = value.split(", ").map(|s| s.to_string()).collect(),
        "buildRequires"     => package.build_requires = value.split(", ").map(|s| s.to_string()).collect(),
        "checkRequires"     => package.check_requires = value.split(", ").map(|s| s.to_string()).collect(),
        "provides"          => package.provides     = value.split(", ").map(|s| s.to_string()).collect(),
        "recommends"        => package.recommends   = value.split(", ").map(|s| s.to_string()).collect(),
        "suggests"          => package.suggests     = value.split(", ").map(|s| s.to_string()).collect(),
        "conflicts"         => package.conflicts    = value.split(", ").map(|s| s.to_string()).collect(),
        "obsoletes"         => package.obsoletes    = value.split(", ").map(|s| s.to_string()).collect(),
        "enhances"          => package.enhances     = value.split(", ").map(|s| s.to_string()).collect(),
        "supplements"       => package.supplements  = value.split(", ").map(|s| s.to_string()).collect(),
        "files"             => package.files        = value.split(", ").map(|s| s.to_string()).collect(),
        "source"            => package.source       = Some(value.to_string()),
        "originUrl"         => package.origin_url   = Some(value.to_string()),
        "repo"              => package.repodata_name = value.to_string(),
        _                   => {
            // Unknown field, ignore or log
        }
    }

    Ok(())
}

pub fn ensure_pkgname2ranges_loaded(shard: &mut RepoShard) -> Result<()> {
    if shard.pkgname2ranges.is_empty() {
        if let Some(ref path) = shard.pkgname2ranges_path {
            shard.pkgname2ranges = deserialize_pkgname2ranges(path)?;
        }
    }
    Ok(())
}

fn lookup_in_packages(
    pkgname: &str,
    repodata_name: &str,
    package_baseurl: &str,
    format: PackageFormat,
    shard: &mut RepoShard,
) -> Result<Vec<Package>> {
    ensure_pkgname2ranges_loaded(shard)?;
    let pkgname2ranges = &shard.pkgname2ranges;
    let packages_mmap = &shard.packages_mmap;
    let mut packages = Vec::new();
    if let Some(ranges) = pkgname2ranges.get(pkgname) {
        if let Some(mmap) = packages_mmap {
            for range in ranges {
                if let Some(data) = mmap.checked_range(&range) {
                    if let Ok(paragraph) = std::str::from_utf8(data) {
                        match deserialize_package(paragraph) {
                            Ok(mut package) => {
                                package.repodata_name = repodata_name.to_string();
                                package.package_baseurl = package_baseurl.to_string();
                                package.format = format;
                                packages.push(package);
                            }
                            Err(e) => {
                                log::debug!("Failed to deserialize repodata '{}' for package '{}': {}", shard.packages.filename, pkgname, e);
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(packages)
}

pub fn map_pkgname2packages(pkgname: &str) -> Result<Vec<Package>> {
    let mut packages = Vec::new();
    let mut repodata_indice = repodata_indice_mut();
    for repo_index in repodata_indice.values_mut() {
        for shard in repo_index.repo_shards.values_mut() {
            if let Ok(mut shard_packages) = lookup_in_packages(pkgname,
                        &repo_index.repodata_name,
                        &repo_index.package_baseurl,
                        repo_index.format,
                        shard) {
                packages.append(&mut shard_packages);
            }
        }
    }
    Ok(packages)
}

/// Maps a pkgkey to a Package by extracting the package name and finding the specific package
pub fn map_pkgkey2package(pkgkey: &str) -> Result<Package> {
    // Extract package name from pkgkey
    let pkgname = crate::package::pkgkey2pkgname(pkgkey)?;

    // Get all packages with this name
    let packages = map_pkgname2packages(&pkgname)?;

    // Find the specific package matching the pkgkey
    for package in packages {
        if package.pkgkey == pkgkey {
            return Ok(package);
        }
    }

    Err(eyre::eyre!("Package not found for pkgkey: {}", pkgkey))
}

/// Lookup package names that provide a given capability.
///
/// IMPORTANT: The capability parameter must be cap_with_arch (e.g., "libfoo(x86-64)"),
/// which is an atomic tag that should NEVER be split. The provide2pkgnames index
/// is keyed by cap_with_arch, not by cap alone. Never strip the arch from cap_with_arch
/// when calling this function.
pub fn map_provide2pkgnames(capability: &str) -> Result<Vec<String>> {
    // First, ensure provide2pkgnames data is loaded
    ensure_provide2pkgnames_loaded()?;

    let mut pkgnames = Vec::new();

    let repodata_indice = repodata_indice();
    for repo_index in repodata_indice.values() {
        for shard in repo_index.repo_shards.values() {
            // capability is cap_with_arch (atomic, never split)
            if let Some(shard_pkgnames) = shard.provide2pkgnames.get(capability) {
                pkgnames.extend(shard_pkgnames.clone());
            }
        }
    }

    Ok(pkgnames)
}

pub fn get_essential_pkgnames() -> Result<HashSet<String>> {
    let mut pkgnames = HashSet::new();

    let repodata_indice = repodata_indice();
    for repo_index in repodata_indice.values() {
        for shard in repo_index.repo_shards.values() {
            pkgnames.extend(shard.essential_pkgnames.clone());
        }
    }

    Ok(pkgnames)
}

pub fn is_essential_pkgname(pkgname: &str) -> bool {
    // During tests, repodata_indice will be empty (no repos loaded)
    // Check if it's empty first to avoid any potential config access
    let repodata_indice = repodata_indice();
    if repodata_indice.is_empty() {
        // During tests, no repos are loaded, so no packages are essential
        return false;
    }
    for repo_index in repodata_indice.values() {
        for shard in repo_index.repo_shards.values() {
            if shard.essential_pkgnames.contains(pkgname) {
                return true;
            }
        }
    }
    return false;
}

/// Maps a pkgline (from installed packages) to a Package by deserializing from local store
pub fn map_pkgline2package(pkgline: &str) -> Result<Package> {
    // The pkgline should be the path/identifier for the package in the store
    // Read the package.txt file from the store directory
    let store_path = crate::models::dirs().epkg_store.join(pkgline).join("info/package.txt");

    if !store_path.exists() {
        return Err(eyre::eyre!("Package info not found in store: {}", store_path.display()));
    }

    let content = fs::read_to_string(&store_path)
        .wrap_err_with(|| format!("Failed to read package info: {}", store_path.display()))?;

    // Reuse the existing deserialize_package function
    let mut package = deserialize_package(&content)
        .wrap_err_with(|| format!("Failed to deserialize package from store: {}", store_path.display()))?;

    // Set a default repodata_name for locally installed packages
    package.repodata_name = "local".to_string();

    Ok(package)
}
