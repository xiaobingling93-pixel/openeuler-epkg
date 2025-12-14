use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::io::Read;
use std::collections::{HashMap, VecDeque};
use color_eyre::eyre::{self, eyre, Result, WrapErr};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use crate::models::*;
use crate::repo::{RepoReleaseItem, RepoRevise};
use crate::packages_stream;
use crate::dirs;

/// AUR domain
pub const AUR_DOMAIN: &str = "aur.archlinux.org";

/// Base URL for AUR package snapshots
pub const AUR_BASE_URL: &str = "https://aur.archlinux.org/cgit/aur.git/snapshot";

// wfg /c/os/archlinux/repodata% grep -o '"[a-zA-Z]\+":' packages-meta-ext-v1.json|sc
//  102350 "Version":
//  102350 "URLPath":
//  102350 "URL":
//  102350 "Submitter":
//  102350 "Popularity":
//  102350 "PackageBaseID":
//  102350 "PackageBase":
//  102350 "OutOfDate":
//  102350 "NumVotes":
//  102350 "Name":
//  102350 "Maintainer":
//  102350 "LastModified":
//  102350 "ID":
//  102350 "FirstSubmitted":
//  102350 "Description":
//  101058 "License":
//   84669 "Depends":
//   65896 "MakeDepends":
//   36726 "Provides":
//   35775 "Conflicts":
//   23618 "Keywords":
//   19034 "OptDepends":
//    6382 "CoMaintainers":
//    6263 "Groups":
//    5505 "CheckDepends":
//    2338 "Replaces":

/// AUR package metadata structure matching packages-meta-ext-v1.json format
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AurPackage {
    #[serde(default, rename = "Name")]
    pub name: String,
    #[serde(default, rename = "PackageBase")]
    pub package_base: String,
    #[serde(default, rename = "Version")]
    pub version: String,
    #[serde(default, rename = "Description")]
    pub description: Option<String>,
    #[serde(default, rename = "URL")]
    pub url: Option<String>,
    #[serde(default, rename = "NumVotes")]
    pub num_votes: u32,
    #[serde(default, rename = "Popularity")]
    pub popularity: f64,
    #[serde(default, rename = "OutOfDate")]
    pub out_of_date: Option<u64>,
    #[serde(default, rename = "Maintainer")]
    pub maintainer: Option<String>,
    #[serde(default, rename = "Submitter")]
    pub submitter: Option<String>,
    #[serde(default, rename = "FirstSubmitted")]
    pub first_submitted: u64,
    #[serde(default, rename = "LastModified")]
    pub last_modified: u64,
    #[serde(default, rename = "OptDepends")]
    pub opt_depends: Vec<String>,
    #[serde(default, rename = "Depends")]
    pub depends: Vec<String>,
    #[serde(default, rename = "MakeDepends")]
    pub make_depends: Vec<String>,
    #[serde(default, rename = "CheckDepends")]
    pub check_depends: Vec<String>,
    #[serde(default, rename = "Conflicts")]
    pub conflicts: Vec<String>,
    #[serde(default, rename = "Provides")]
    pub provides: Vec<String>,
    #[serde(default, rename = "Replaces")]
    pub replaces: Vec<String>,
    #[serde(default, rename = "Groups")]
    pub groups: Vec<String>,
    #[serde(default, rename = "Keywords")]
    pub keywords: Vec<String>,
    #[serde(default, rename = "License")]
    pub license: Vec<String>,
    #[serde(default, rename = "CoMaintainers")]
    pub co_maintainers: Vec<String>,
}

/// Parse AUR metadata and return release items
pub fn parse_aur_metadata(repo: &RepoRevise, _release_path: &PathBuf) -> Result<Vec<RepoReleaseItem>> {
    let mut release_items = Vec::new();

    let repo_dir = dirs::get_repo_dir(&repo)
        .map_err(|e| eyre::eyre!("Failed to get repository directory for {}: {}", repo.repo_name, e))?;

    let output_path = repo_dir.join("packages.txt");

    let url = repo.index_url.clone();
    let location = url.split('/').last().unwrap_or("packages-meta-ext-v1.json.gz").to_string();

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
        arch: repo.arch.clone(),
        url,
        package_baseurl: AUR_BASE_URL.to_string(),
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

/// Process AUR packages from JSON format
pub fn process_packages_content(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<PackagesFileInfo> {
    log::debug!("Starting to process AUR packages content for {} (hash: {}, size: {})", revise.location, revise.hash, revise.size);

    // Validate download path
    validate_download_path(revise)?;

    // Create streaming reader from receiver with hash validation
    log::debug!("Creating ReceiverHasher with hash='{}', size={}", revise.hash, revise.size);
    let receiver_reader = packages_stream::ReceiverHasher::new_with_size(
        data_rx,
        revise.hash.clone(),
        revise.size.try_into().map_err(|e| eyre::eyre!("Failed to convert size {} to u64: {}", revise.size, e))?
    );

    // Decompress gzip
    let mut decoder = GzDecoder::new(receiver_reader);
    let mut json_content = String::new();
    decoder.read_to_string(&mut json_content)
        .map_err(|e| eyre::eyre!("Failed to decompress and read AUR JSON: {}", e))?;

    log::debug!("Successfully decompressed AUR JSON, size: {} bytes", json_content.len());

    // Parse JSON - it's a JSON array format (starts with '[' and ends with ']')
    let aur_packages: Vec<AurPackage> = serde_json::from_str(&json_content)
        .map_err(|e| eyre::eyre!("Failed to parse AUR JSON array: {}", e))?;

    log::info!("Parsed {} AUR packages from JSON", aur_packages.len());

    // Initialize packages.txt writer
    let mut derived_files = packages_stream::PackagesStreamline::new(revise, repo_dir, process_line)
        .map_err(|e| eyre::eyre!("Failed to initialize PackagesStreamline for {}: {}", revise.location, e))?;

    // Convert AUR packages to Package structs and write to packages.txt
    let mut nr_packages = 0;
    let mut nr_provides = 0;
    let mut nr_essentials = 0;

    for aur_pkg in &aur_packages {
        // Convert AUR package to Package format
        let package = convert_aur_to_package(aur_pkg, &revise.repo_revise.repodata_name)?;

        // Write package to packages.txt
        write_package_to_stream(&package, &mut derived_files)?;

        nr_packages += 1;
        nr_provides += package.provides.len();
        if package.pkgname == "filesystem" || package.pkgname == "base" {
            nr_essentials += 1;
        }
    }

    // Finalize processing
    finalize_processing(&mut derived_files, repo_dir, revise, nr_packages, nr_provides, nr_essentials)
}

/// Validate download path for already downloaded files
fn validate_download_path(revise: &RepoReleaseItem) -> Result<()> {
    if !revise.need_download && revise.need_convert {
        log::debug!("Processing already downloaded file: {}", revise.download_path.display());
        if !revise.download_path.exists() {
            return Err(eyre::eyre!("Downloaded file does not exist: {}", revise.download_path.display()));
        }

        let metadata = std::fs::metadata(&revise.download_path)
            .map_err(|e| eyre::eyre!("Failed to get metadata for {}: {}", revise.download_path.display(), e))?;

        if metadata.len() == 0 {
            return Err(eyre::eyre!("Downloaded file is empty: {}", revise.download_path.display()));
        }

        log::debug!("Downloaded file size: {} bytes", metadata.len());
    }

    Ok(())
}

/// Convert AUR package to Package struct
fn convert_aur_to_package(aur_pkg: &AurPackage, repodata_name: &str) -> Result<Package> {
    // Determine architecture - AUR packages are typically "any" or "x86_64"
    let arch = "any".to_string(); // AUR packages are usually architecture-independent or x86_64
    let location = format!("{}.tar.gz", aur_pkg.package_base);

    let mut package = Package {
        pkgname: aur_pkg.name.clone(),
        version: aur_pkg.version.clone(),
        arch,
        source: Some(aur_pkg.package_base.clone()),
        location,
        summary: aur_pkg.description.clone().unwrap_or_default(),
        homepage: aur_pkg.url.clone().unwrap_or_default(),
        maintainer: aur_pkg.maintainer.clone().unwrap_or_default(),
        requires: aur_pkg.depends.clone(),
        build_requires: aur_pkg.make_depends.clone(),
        check_requires: aur_pkg.check_depends.clone(),
        recommends: aur_pkg.opt_depends.clone(),
        provides: aur_pkg.provides.clone(),
        conflicts: aur_pkg.conflicts.clone(),
        obsoletes: aur_pkg.replaces.clone(),
        section: Some(aur_pkg.groups.join(", ")),
        tag: Some(aur_pkg.keywords.join(", ")),
        repodata_name: repodata_name.to_string(),
        ..Default::default()
    };

    // Generate pkgkey
    package.pkgkey = crate::package::format_pkgkey(&package.pkgname, &package.version, &package.arch);

    Ok(package)
}

/// Helper function to write a dependency field (joins Vec<String> by space)
fn write_dependency_field(
    deps: &[String],
    field_name: &str,
    output: &mut String,
) {
    let filtered: Vec<&str> = deps.iter()
        .filter(|dep| !dep.is_empty())
        .map(|s| s.as_str())
        .collect();
    if !filtered.is_empty() {
        output.push_str(&format!("{}: {}\n", field_name, filtered.join(" ")));
    }
}

/// Write package to packages.txt stream
fn write_package_to_stream(package: &Package, derived_files: &mut packages_stream::PackagesStreamline) -> Result<()> {
    // Add blank line to start package
    derived_files.output.push_str("\n");
    derived_files.on_new_paragraph();

    // Write package fields
    derived_files.output.push_str(&format!("pkgname: {}\n", package.pkgname));
    derived_files.on_new_pkgname(&package.pkgname);

    derived_files.output.push_str(&format!("version: {}\n", package.version));
    derived_files.output.push_str(&format!("arch: {}\n", package.arch));

    if let Some(ref source) = package.source {
        derived_files.output.push_str(&format!("source: {}\n", source));
    }

    if !package.location.is_empty() {
        derived_files.output.push_str(&format!("location: {}\n", package.location));
    }

    if !package.summary.is_empty() {
        derived_files.output.push_str(&format!("summary: {}\n", package.summary));
    }

    if !package.homepage.is_empty() {
        derived_files.output.push_str(&format!("homepage: {}\n", package.homepage));
    }

    if !package.maintainer.is_empty() {
        derived_files.output.push_str(&format!("maintainer: {}\n", package.maintainer));
    }

    if let Some(ref section) = package.section {
        if !section.is_empty() {
            derived_files.output.push_str(&format!("section: {}\n", section));
        }
    }

    if let Some(ref tag) = package.tag {
        if !tag.is_empty() {
            derived_files.output.push_str(&format!("tag: {}\n", tag));
        }
    }

    // Write dependency fields (join by space)
    write_dependency_field(&package.provides, "provides", &mut derived_files.output);
    write_dependency_field(&package.requires, "requires", &mut derived_files.output);
    write_dependency_field(&package.build_requires, "buildRequires", &mut derived_files.output);
    write_dependency_field(&package.check_requires, "checkRequires", &mut derived_files.output);
    write_dependency_field(&package.recommends, "recommends", &mut derived_files.output);
    write_dependency_field(&package.conflicts, "conflicts", &mut derived_files.output);
    write_dependency_field(&package.obsoletes, "obsoletes", &mut derived_files.output);
    write_dependency_field(&package.requires_pre, "requiresPre", &mut derived_files.output);
    write_dependency_field(&package.suggests, "suggests", &mut derived_files.output);
    write_dependency_field(&package.enhances, "enhances", &mut derived_files.output);
    write_dependency_field(&package.supplements, "supplements", &mut derived_files.output);

    // Flush the accumulated output to packages.txt file
    derived_files.on_output()
        .map_err(|e| eyre::eyre!("Failed to write package output: {}", e))?;

    Ok(())
}

/// Finalize processing
fn finalize_processing(
    derived_files: &mut packages_stream::PackagesStreamline,
    _repo_dir: &PathBuf,
    revise: &RepoReleaseItem,
    nr_packages: usize,
    nr_provides: usize,
    nr_essentials: usize,
) -> Result<PackagesFileInfo> {
    // Ensure the last package gets indexed
    if !derived_files.current_pkgname.is_empty() {
        log::debug!("Finalizing last package: {}", derived_files.current_pkgname);
        derived_files.on_new_paragraph();
    }

    log::debug!("Finalizing processing for {}", revise.location);

    // Finalize processing
    derived_files.on_finish(revise)
        .map_err(|e| eyre::eyre!("Failed to finalize processing for {}: {}", revise.location, e))?;

    Ok(PackagesFileInfo {
        filename: revise.location.clone(),
        sha256sum: revise.hash.clone(),
        datetime: String::new(), // AUR doesn't provide datetime in the same way
        size: revise.size as u64,
        nr_packages,
        nr_provides,
        nr_essentials,
    })
}

/// Helper function to process a single line (placeholder for consistency)
fn process_line(_line: &str, _derived_files: &mut packages_stream::PackagesStreamline) -> Result<()> {
    // This function is required by PackagesStreamline but for AUR packages,
    // we process the data differently (by parsing JSON rather than line by line)
    // so this is mostly a placeholder
    Ok(())
}

impl PackageManager {

    /// Check if a package is an AUR package
    pub fn is_aur_package(&mut self, pkgkey: &str) -> bool {
        if let Ok(package) = self.load_package_info(pkgkey) {
            package.repodata_name == "aur"
        } else {
            false
        }
    }

    /// Extract AUR package source tarball to build directory
    /// Returns the path to the extracted package build directory
    fn extract_aur_source(
        tarball_path: &Path,
        pkgname: &str,
        build_dir: &Path,
    ) -> Result<PathBuf> {
        use std::fs::File;

        // Extract tarball (tarball already contains a top-level pkgname/ directory)
        let pkg_root_dir = build_dir.join(pkgname);
        if pkg_root_dir.exists() {
            std::fs::remove_dir_all(&pkg_root_dir)?;
        }
        std::fs::create_dir_all(build_dir)?;

        // Extract tar.gz with better diagnostics for corrupt downloads
        let tar_gz = File::open(tarball_path)
            .with_context(|| format!("Failed to open tarball {}", tarball_path.display()))?;

        let tar = flate2::read::GzDecoder::new(tar_gz);
        let mut archive = tar::Archive::new(tar);
        archive
            .unpack(build_dir)
            .with_context(|| format!("Failed to unpack tarball {}", tarball_path.display()))?;

        // Find PKGBUILD (usually in a subdirectory)
        let pkgbuild_path = Self::find_pkgbuild(&pkg_root_dir)?;

        // Return the directory containing the PKGBUILD file, not the file itself
        Ok(pkgbuild_path
            .parent()
            .ok_or_else(|| eyre!("PKGBUILD path has no parent directory"))?
            .to_path_buf())
    }

    /// Run makepkg to build the AUR package
    /// Returns the paths to the built package files (handles SPLITPKG)
    fn run_makepkg(
        pkgname: &str,
        pkg_build_dir: &Path,
        build_dir: &Path,
        env_root: &Path,
    ) -> Result<Vec<PathBuf>> {
        // Create build log file
        let log_file = build_dir.join(format!("{}.log", pkgname));
        std::fs::File::create(&log_file)?;

        // Build makepkg command with arguments
        // Use shell redirection to capture output to log file
        let log_file_str = log_file.to_str()
            .ok_or_else(|| eyre!("Invalid UTF-8 in log file path"))?;
        let pkg_build_dir_str = pkg_build_dir.to_str()
            .ok_or_else(|| eyre!("Invalid UTF-8 in build directory path"))?;

        // Let the user know how to watch the build log before starting the long-running build.
        println!("less {}", log_file.display());

        // Construct the command with shell redirection
        // We use bash -c to handle redirection properly
        let sh_path = crate::run::find_command_in_env_path("bash", env_root)
            .map_err(|e| eyre!("Failed to find bash in environment: {}", e))?;

        // We run the build inside a user namespace, so checking USER for root is
        // a more accurate proxy for "running as root" than checking EUID inside makepkg.
        let makepkg_cmd = format!(
            "if [ ! -x /usr/local/bin/makepkg ]; then
                    sed 's/if (( EUID == 0 )); then/if test $USER = root; then/' /usr/bin/makepkg > /usr/local/bin/makepkg && chmod +x /usr/local/bin/makepkg;
            fi;
            export PACMAN=true
            cd {} && /usr/local/bin/makepkg --nodeps --nosign --skippgpcheck --noconfirm --noprogressbar --nocheck --config /etc/makepkg.conf > {} 2>&1",
            pkg_build_dir_str,
            log_file_str
        );

        let run_options = crate::run::RunOptions {
            command: "bash".to_string(),
            args: vec!["-c".to_string(), makepkg_cmd],
            chdir_to_env_root: false, // We're already changing directory in the command
            skip_namespace_isolation: false,
            timeout: 0, // No timeout for makepkg builds
            ..Default::default()
        };

        // Run makepkg using fork_and_execute
        match crate::run::fork_and_execute(env_root, &run_options, &sh_path) {
            Ok(()) => {
                // Success - find the built package files
                let built_pkgs = Self::find_built_package(pkg_build_dir)?;
                for built_pkg in &built_pkgs {
                    log::info!("Built package: {}", built_pkg.display());
                }
                Ok(built_pkgs)
            }
            Err(e) => {
                eprintln!("makepkg failed for {}: {}", pkgname, e);
                Err(eyre!(
                    "makepkg failed for {}: {}",
                    pkgname, e
                ))
            }
        }
    }

    /// Build a single AUR package using makepkg
    /// Returns the paths to the built package files (handles SPLITPKG)
    fn build_single_aur_package(
        &mut self,
        pkgkey: &str,
        build_dir: &Path,
        env_root: &Path,
    ) -> Result<Vec<std::path::PathBuf>> {
        // Directory layout examples (keep sources/builds flat):
        // - Downloaded tarball: ~/.cache/epkg/downloads/aur.archlinux.org/cgit/aur.git/snapshot/wget2.tar.gz
        // - Downloaded git dir: ~/.cache/epkg/aur_builds/wget2 (same with Extracted build dir)
        // - Extracted build dir: ~/.cache/epkg/aur_builds/wget2 (no extra nested wget2/)
        let package = self.load_package_info(pkgkey)?;
        log::info!("Building AUR package: {} ({})", package.pkgname, pkgkey);

        // Extract pkgbase (use package.source as pkgbase when available to share builds)
        let pkgbase = package
            .source
            .as_deref()
            .unwrap_or_else(|| package.pkgname.as_str());

        // Check if git directory already exists in build directory (from git download)
        let git_dir_in_build = build_dir.join(pkgbase);
        let pkg_build_dir = if git_dir_in_build.is_dir() && git_dir_in_build.join(".git").exists() {
            // Git directory exists in build directory - use it directly
            log::info!("Using git directory directly from build dir: {}", git_dir_in_build.display());
            git_dir_in_build
        } else {
            // No git directory in build dir, check for tarball download
            let source_path_str = self.get_package_file_path(pkgkey)?;
            let source_path = PathBuf::from(source_path_str);

            // It's a tarball - extract it
            Self::extract_aur_source(&source_path, pkgbase, build_dir)?
        };

        // Run makepkg to build the package
        let built_pkgs = Self::run_makepkg(&package.pkgname, &pkg_build_dir, build_dir, env_root)?;

        Ok(built_pkgs)
    }

    /// Build and install AUR packages using makepkg
    /// This function handles AUR packages specially by:
    /// 1. Building AUR packages in DAG order based on depend_depth (already calculated in build_installed_package_info_map)
    /// 2. Running makepkg inside the archlinux environment
    /// 3. Unpacking and linking built packages
    /// 4. Run install scripts
    pub fn build_and_install_aur_packages(
        &mut self,
        aur_packages: &HashMap<String, InstalledPackageInfo>,
        plan: &crate::install::InstallationPlan,
        store_root: &Path,
        env_root: &Path,
        package_format: crate::models::PackageFormat,
    ) -> Result<HashMap<String, InstalledPackageInfo>> {
        if aur_packages.is_empty() {
            return Ok(HashMap::new());
        }

        log::info!("Building {} AUR packages", aur_packages.len());

        // Group AUR packages by pkgbase (package.source set from AUR package_base),
        // and sort groups by minimum depend_depth so build dependencies are built first.
        let mut base_to_pkgkeys: HashMap<String, Vec<String>> = HashMap::new();
        let mut base_min_depth: HashMap<String, u16> = HashMap::new();

        for (pkgkey, info) in aur_packages {
            let package = self.load_package_info(pkgkey)
                .with_context(|| format!("Failed to load package info for AUR pkgkey {}", pkgkey))?;
            let pkgbase = package
                .source
                .as_deref()
                .unwrap_or_else(|| package.pkgname.as_str())
                .to_string();

            base_to_pkgkeys.entry(pkgbase.clone()).or_default().push(pkgkey.clone());
            base_min_depth
                .entry(pkgbase)
                .and_modify(|depth| {
                    if info.depend_depth < *depth {
                        *depth = info.depend_depth;
                    }
                })
                .or_insert(info.depend_depth);
        }

        let mut sorted_bases: Vec<(String, u16)> = base_min_depth
            .into_iter()
            .collect();
        sorted_bases.sort_by_key(|(_, depth)| *depth);

        // Build package bases in DAG order (sorted by minimum depend_depth)
        let mut completed_aur_packages = HashMap::new();
        let build_dir = dirs().epkg_aur_builds.clone();
        std::fs::create_dir_all(&build_dir)?;

        for (pkgbase, _depth) in sorted_bases {
            if let Some(pkgkeys) = base_to_pkgkeys.get(&pkgbase) {
                // Use the first pkgkey in this pkgbase group as the representative for building
                // (the tarball and pkgbase are shared across split packages).
                let rep_pkgkey = &pkgkeys[0];

                // Build the package(s) for this pkgbase (may produce multiple artifacts for SPLITPKG)
                let built_pkg_paths = self.build_single_aur_package(rep_pkgkey, &build_dir, env_root)?;

                for built_pkg_path in built_pkg_paths {
                    // Unpack and link each built package
                    let built_pkg_path_str = built_pkg_path.to_str()
                        .ok_or_else(|| eyre!("Invalid UTF-8 in built package path: {}", built_pkg_path.display()))?;

                    // Use a representative InstalledPackageInfo initially; we may override
                    // selected fields based on the actual_pkgkey afterwards.
                    let rep_info = aur_packages
                        .get(rep_pkgkey)
                        .cloned()
                        .ok_or_else(|| eyre!("Representative AUR pkgkey {} not found in aur_packages", rep_pkgkey))?;

                    let (actual_pkgkey, mut completed_info) = self.unpack_and_link_package(
                        built_pkg_path_str,
                        rep_pkgkey,
                        rep_info,
                        store_root,
                        env_root,
                    )
                    .with_context(|| format!("Failed to unpack and link built package: {}", built_pkg_path.display()))?;

                    // If we have specific metadata for this actual_pkgkey in aur_packages
                    // (e.g. depend_depth), propagate it into completed_info.
                    if let Some(aur_info_for_actual) = aur_packages.get(&actual_pkgkey) {
                        completed_info.depend_depth = aur_info_for_actual.depend_depth;
                    }

                    // Process installation results for this package
                    let mut single_package_map = HashMap::new();
                    single_package_map.insert(actual_pkgkey.clone(), completed_info.clone());
                    self.process_installation_results(plan, &single_package_map, store_root, env_root, package_format)?;

                    completed_aur_packages.insert(actual_pkgkey, completed_info);
                }
            }
        }

        Ok(completed_aur_packages)
    }

    /// Find PKGBUILD in extracted directory (up to 3 levels deep)
    fn find_pkgbuild(dir: &Path) -> Result<PathBuf> {
        let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
        queue.push_back((dir.to_path_buf(), 0));

        while let Some((current, depth)) = queue.pop_front() {
            let candidate = current.join("PKGBUILD");
            if candidate.exists() {
                return Ok(candidate);
            }

            if depth >= 3 {
                continue;
            }

            for entry in std::fs::read_dir(&current)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() {
                    queue.push_back((path, depth + 1));
                }
            }
        }

        Err(eyre!("PKGBUILD not found in {}", dir.display()))
    }

    /// Find built package files (handles SPLITPKG)
    pub fn find_built_package(dir: &Path) -> Result<Vec<PathBuf>> {
        let mut built = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if let Some(ext) = path.extension() {
                if ext == "zst" || ext == "xz" {
                    if path.file_name().and_then(|n| n.to_str()).unwrap_or("").contains(".pkg.tar") {
                        built.push(path);
                    }
                }
            }
        }
        if built.is_empty() {
            Err(eyre!("Built package not found in {}", dir.display()))
        } else {
            Ok(built)
        }
    }

}
