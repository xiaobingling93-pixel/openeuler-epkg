use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc::Receiver};

use color_eyre::eyre::{self, eyre, Result, WrapErr};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};

use crate::dirs;
use crate::plan::InstallationPlan;
use crate::models::*;
use crate::packages_stream;
use crate::repo::{RepoReleaseItem, RepoRevise};
use crate::download::get_package_file_path;
use crate::transaction::run_transaction_batch;

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

    let repo_dir = dirs::get_repo_dir(&repo);

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

/// Check if a package is an AUR package
pub fn is_aur_package(pkgkey: &str) -> bool {
    if let Ok(package) = crate::package_cache::load_package_info(pkgkey) {
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
    let pkgbuild_path = find_pkgbuild(&pkg_root_dir)?;

    // Return the directory containing the PKGBUILD file, not the file itself
    Ok(pkgbuild_path
        .parent()
        .ok_or_else(|| eyre!("PKGBUILD path has no parent directory"))?
        .to_path_buf())
}

/// Run makepkg to build the AUR package
/// Returns Ok(()) on success, Err on failure
fn run_makepkg(
    pkgbase: &str,
    pkg_build_dir: &Path,
    build_dir: &Path,
    env_root: &Path,
) -> Result<()> {
    // Create build log file
    let log_file = build_dir.join(format!("{}.log", pkgbase));
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
        cd {} && /usr/local/bin/makepkg --force --nodeps --nosign --skippgpcheck --noconfirm --noprogressbar --nocheck --config /etc/makepkg.conf > {} 2>&1",
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
            log::info!("makepkg completed successfully for {}", pkgbase);
            Ok(())
        }
        Err(e) => {
            eprintln!("makepkg failed for {}: {}", pkgbase, e);
            Err(eyre!(
                "makepkg failed for {}: {}",
                pkgbase, e
            ))
        }
    }
}


/// Prepare or extract the AUR source directory.
///
/// Directory layout examples (keep sources/builds flat):
/// - Downloaded tarball: ~/.cache/epkg/downloads/aur.archlinux.org/cgit/aur.git/snapshot/wget2.tar.gz
/// - Downloaded git dir: ~/.cache/epkg/aur_builds/wget2 (same with Extracted build dir)
/// - Extracted build dir: ~/.cache/epkg/aur_builds/wget2 (no extra nested wget2/)
fn prepare_aur_source_dir(
    pkgkey: &str,
    pkgbase: &str,
    build_dir: &Path,
) -> Result<PathBuf> {
    // Check if git directory already exists in build directory (from git download)
    let git_dir_in_build = build_dir.join(pkgbase);
    if git_dir_in_build.is_dir() && git_dir_in_build.join(".git").exists() {
        // Git directory exists in build directory - use it directly
        log::info!("Using git directory directly from build dir: {}", git_dir_in_build.display());
        Ok(git_dir_in_build)
    } else {
        // No git directory in build dir, check for tarball download
        let source_path_str = get_package_file_path(pkgkey)?;
        let source_path = PathBuf::from(source_path_str);

        // It's a tarball - extract it
        extract_aur_source(&source_path, pkgbase, build_dir)
    }
}

/// Find and verify all requested built packages, mapping them to planned entries.
///
/// Returns Ok with mapped packages (path, original_key, info) if all
/// requested packages are found, or Err if any are missing.
///
/// For pre-makepkg: returns Ok(empty vec) if not all found (proceed with makepkg),
///                  returns Ok(mapped) if all found (skip makepkg).
/// For post-makepkg: returns Err if not all found (fail installation),
///                   returns Ok(mapped) if all found (proceed with unpack_and_link).
fn find_and_verify_built_packages(
    pkgbase: &str,
    pkgkeys: &[String],
    aur_packages: &InstalledPackagesMap,
    pkg_build_dir: &Path,
    is_post_makepkg: bool,
) -> Result<Vec<(PathBuf, String, Arc<InstalledPackageInfo>)>> {
    use crate::package;

    // Get version from the first pkgkey (all pkgkeys in a pkgbase should have the same version)
    let version = pkgkeys.first()
        .and_then(|pkgkey| package::parse_pkgkey(pkgkey).ok())
        .map(|(_name, version, _arch)| version)
        .ok_or_else(|| eyre!("Failed to parse version from pkgkey"))?;

    // Find all built packages for this pkgbase (may include multiple pkgnames for split packages)
    let all_built = match find_built_package(pkg_build_dir, pkgbase, &version) {
        Ok(packages) => packages,
        Err(_) if !is_post_makepkg => {
            // For pre-check, it's OK if no packages found yet - proceed with makepkg
            return Ok(Vec::new());
        }
        Err(e) => return Err(e),
    };

    // Map built packages to filter to only the ones we need
    let mapped = map_built_aur_packages(&all_built, pkgkeys, aur_packages)?;

    // Check if we found all requested packages
    let found_pkgkeys: std::collections::HashSet<String> = mapped
        .iter()
        .map(|(_path, original_key, _info)| original_key.clone())
        .collect();

    let mut missing = Vec::new();
    for pkgkey in pkgkeys {
        if !found_pkgkeys.contains(pkgkey) {
            if let Ok((name, version, _)) = package::parse_pkgkey(pkgkey) {
                missing.push(format!("{} ({})", name, version));
            }
        }
    }

    if !missing.is_empty() {
        if is_post_makepkg {
            // For post-makepkg, fail installation if not all found
            return Err(eyre!(
                "Missing built packages for: {}",
                missing.join(", ")
            ));
        } else {
            // For pre-check, it's OK if not all packages found yet - proceed with makepkg
            return Ok(Vec::new());
        }
    }

    Ok(mapped)
}


/// Build and install AUR packages using makepkg
///
/// # AUR Package Architecture Handling
///
/// ## Problem Background
///
/// AUR packages have a unique architecture lifecycle that differs from binary packages:
///
/// 1. **Initial State (Repodata)**: All AUR packages in repodata have `arch="any"`, which
///    represents an "undetermined" or "source" state. This is because the actual architecture
///    cannot be determined until the package is built with `makepkg`.
///
/// 2. **After makepkg**: The built package will have a determined architecture - either:
///    - `arch="any"` (if the package is truly architecture-independent), or
///    - `arch="x86_64"` (or the current machine's `std::env::consts::ARCH` for architecture-specific packages)
///
/// 3. **Previously Installed AUR Packages**: These have determined arch values in
///    `installed_packages` (inherited from when they were built), e.g., `arch="x86_64"`.
///
/// This creates an inconsistency: new AUR packages start with `arch="any"` in the installation
/// plan, but after building they may have `arch="x86_64"`. This affects:
///
/// - **pkgkey**: Format is `{pkgname}__{version}__{arch}`, so `resto-rs__0.5.0-1__any` vs
///   `resto-rs__0.5.0-1__x86_64` are different keys
/// - **pkgline**: Format includes arch, so pkglines differ based on arch
/// - **Upgrade detection**: Need to match old installed packages (with determined arch) to
///   new packages (initially with `arch="any"`)
/// - **Plan keys**: `plan.ordered_operations` contains pkgkeys that need to be updated after arch is determined
///
/// ## Solution
///
/// The solution involves multiple coordinated fixes throughout the installation flow:
///
/// 1. **Upgrade Detection** (`find_upgrade_target()` in `install.rs`):
///    - For AUR packages, matches by `pkgname+version` only (ignoring arch differences)
///    - For non-AUR packages, matches by `pkgname+arch` (existing behavior)
///    - This allows matching `resto-rs__0.5.0-1__any` (new) to `resto-rs__0.5.0-1__x86_64` (old)
///
/// 2. **Upgrade Map Building** (in `prepare_installation_plan()` / `find_upgrade_target()` in `install.rs`):
///    - Uses AUR-aware matching logic while constructing the installation plan to build
///      old->new pkgkey mappings (stored in `plan.upgrade_map_old_to_new`)
///    - Handles cases where old package has determined arch but new package starts with `arch="any"`
///
/// 3. **Plan Key Fixup** (`fixup_aur_plan_keys()` in `aur.rs`):
///    - After all AUR packages are built and actual arch is determined, this function:
///      - Maps original pkgkeys (with `arch="any"`) to actual pkgkeys (with determined arch)
///      - Updates pkgkeys in `plan.ordered_operations` (maps new_pkgkey only, old_pkgkey already has correct arch)
///    - This ensures all subsequent operations use the correct pkgkeys
///
/// 4. **AUR Plan Key Fixup** (in this module):
///    - After AUR builds complete and actual architectures are known, `fixup_aur_plan_keys()`
///      remaps plan keys from their pre-build `arch="any"` form to their post-build, real arch.
///    - This keeps the installation plan consistent with what actually gets installed.
///
/// ## Info Flow Verification
///
/// Throughout `resolve_and_install_packages()` and related functions:
///
/// - **resolve_dependencies_adding_makepkg_deps()**: Creates packages with `arch="any"` for AUR ✓
/// - **prepare_installation_plan()**: Uses AUR-aware `find_upgrade_target()` for upgrade detection ✓
/// - **fill_pkglines_in_plan()**: May not find matches for AUR with `arch="any"` (OK, they need building) ✓
/// - **download_and_unpack_packages()**: Separates packages with/without pkglines, separates AUR packages, processes binary packages ✓
/// - **run_transaction_batch()**: For binary packages, uses plan keys directly ✓
/// - **build_and_install_aur_packages()**: Builds AUR, determines actual arch, fixes up plan ✓
/// - **run_transaction_batch()**: For AUR packages, uses fixed plan keys (actual arch) ✓
/// - **execute_expose_operations()**: Uses fixed plan keys that match `installed_packages` ✓
///
/// This function and its helpers handle the "build AUR + plan key fixup + AUR result processing"
/// portion (steps 4-5) of the solution.
///
/// ## Implementation Details
///
/// This function handles AUR packages specially by:
/// 1. Grouping AUR packages by pkgbase and DAG-ordering them using `depend_depth`
///    (via `group_aur_packages_by_base()`).
/// 2. For each pkgbase, building all artifacts with makepkg (via `build_aur_packages_for_base()`)
///    inside the archlinux environment.
/// 3. Unpacking and linking built packages, which determines their real pkgkeys/arches
///    (via `unpack_package()` and `link_package()` inside `unpack_link_built_aur_packages()`).
/// 4. Mapping pre-build AUR pkgkeys (`arch="any"`) to actual post-build pkgkeys, and fixing up
///    the installation plan (`fixup_aur_plan_keys()` inside `postinstall_built_aur_round()`).
/// 5. Normalizing dependency fields and InstalledPackageInfo values in both newly built AUR
///    packages and skipped reinstalls so they point at the post-build pkgkeys
///    (`fixup_installed_packages_values()`).
/// 6. Processing AUR installation results so subsequent rounds and expose operations use pkgkeys
///    that match what is stored in `installed_packages` (`postinstall_built_aur_round()`).
pub fn build_and_install_aur_packages(
    plan: &mut crate::plan::InstallationPlan,
    aur_packages: &InstalledPackagesMap,
) -> Result<InstalledPackagesMap> {
    if aur_packages.is_empty() {
        return Ok(HashMap::new());
    }

    log::info!("Building {} AUR packages", aur_packages.len());

    // Group and sort AUR packages by pkgbase + minimum depend_depth
    let (base_to_pkgkeys, sorted_bases) = group_aur_packages_by_base(aur_packages)?;

    // Build package bases in DAG order (sorted by minimum depend_depth)
    let mut completed_aur_packages = HashMap::new();
    // Accumulated mapping of original (plan) AUR pkgkeys -> actual installed pkgkeys across all rounds.
    let mut aur_mapping_all_rounds: HashMap<String, String> = HashMap::new();
    let build_dir = dirs().user_aur_builds.clone();
    std::fs::create_dir_all(&build_dir)?;

    // Build package bases in DAG order (sorted by minimum depend_depth)
    // For each pkgbase, we:
    //   - build all artifacts for that base
    //   - map each built package to its actual pkgkey
    //   - fix up the plan keys for packages in this round
    //   - normalize dependency fields for this round
    //   - process installation results so subsequent rounds can depend on them
    for (pkgbase, _depth) in sorted_bases {
        if let Some(pkgkeys) = base_to_pkgkeys.get(&pkgbase) {
            // 1) Build all artifacts for this pkgbase (may produce multiple SPLITPKG artifacts)
            // 2) Map built artifacts to planned AUR entries
            let mapped = build_aur_packages_for_base(
                &pkgbase,
                pkgkeys,
                aur_packages,
                &build_dir,
                &plan.env_root,
            )?;

            // 3) Unpack and link built packages
            let (mut this_round_aur_packages, this_round_pkgkey_mapping) =
                unpack_link_built_aur_packages(
                    plan,
                    &mapped,
                    &plan.store_pkglines_by_pkgname,
                )?;

            if !this_round_aur_packages.is_empty() {
                // 4) Install / process results for this round and normalize plan + dependencies
                postinstall_built_aur_round(
                    plan,
                    &mut this_round_aur_packages,
                    &this_round_pkgkey_mapping,
                    &mut aur_mapping_all_rounds,
                )?;

                // 5) Merge this round into overall completed_aur_packages
                completed_aur_packages.extend(this_round_aur_packages.into_iter());
            }
        }
    }

    // Normalize dependencies of skipped reinstalls using the accumulated
    // AUR pkgkey mapping across all rounds, and keep their InstalledPackageInfo
    // values consistent with their pkgkeys.
    if !plan.skipped_reinstalls.is_empty() && !aur_mapping_all_rounds.is_empty() {
        fixup_installed_packages_values(
            &aur_mapping_all_rounds,
            &mut plan.skipped_reinstalls,
        )
        .with_context(|| {
            "Failed to normalize dependency fields for skipped reinstalls using AUR mappings"
        })?;
    }

    Ok(completed_aur_packages)
}

fn group_aur_packages_by_base(
    aur_packages: &InstalledPackagesMap,
) -> Result<(
    HashMap<String, Vec<String>>,
    Vec<(String, u16)>,
)> {
    // Group AUR packages by pkgbase (package.source set from AUR package_base),
    // and compute the minimum depend_depth per group so build dependencies
    // are built first.
    let mut base_to_pkgkeys: HashMap<String, Vec<String>> = HashMap::new();
    let mut base_min_depth: HashMap<String, u16> = HashMap::new();

    for (pkgkey, info) in aur_packages {
        let package = crate::package_cache::load_package_info(pkgkey)
            .with_context(|| {
                format!("Failed to load package info for AUR pkgkey {}", pkgkey)
            })?;

        let pkgbase = package
            .source
            .as_deref()
            .unwrap_or_else(|| package.pkgname.as_str())
            .to_string();

        base_to_pkgkeys
            .entry(pkgbase.clone())
            .or_default()
            .push(pkgkey.clone());

        base_min_depth
            .entry(pkgbase)
            .and_modify(|depth| {
                if info.depend_depth < *depth {
                    *depth = info.depend_depth;
                }
            })
            .or_insert(info.depend_depth);
    }

    let mut sorted_bases: Vec<(String, u16)> = base_min_depth.into_iter().collect();
    sorted_bases.sort_by_key(|(_, depth)| *depth);

    Ok((base_to_pkgkeys, sorted_bases))
}

/// Build all AUR packages for a single pkgbase.
///
/// This helper:
///   - Uses the first pkgkey in the `pkgkeys` slice as the representative for driving the build
///     (tarball and pkgbase are shared across split packages).
///   - Builds all artifacts for this pkgbase.
///
/// Returns:
///   - `mapped`: Vec of (built_pkg_path, original_key, info) tuples for built packages
fn build_aur_packages_for_base(
    pkgbase: &str,
    pkgkeys: &[String],
    aur_packages: &InstalledPackagesMap,
    build_dir: &Path,
    env_root: &Path,
) -> Result<Vec<(PathBuf, String, Arc<InstalledPackageInfo>)>> {
    // Use the first pkgkey in this pkgbase group as the representative for building
    // (the tarball and pkgbase are shared across split packages). This representative
    // is only used to drive the build; we stop using it once artifacts are produced.
    let rep_pkgkey = &pkgkeys[0];
    log::info!(
        "Building AUR pkgbase '{}' using representative pkgkey '{}'",
        pkgbase,
        rep_pkgkey
    );

    // Prepare/extract source directory
    let pkg_build_dir = prepare_aur_source_dir(rep_pkgkey, pkgbase, build_dir)?;

    // Pre-check: try to find already built packages for all requested pkgkeys
    // If all found, skip makepkg; if not all found, proceed with makepkg
    let pre_check_mapped = find_and_verify_built_packages(
        pkgbase,
        pkgkeys,
        aur_packages,
        &pkg_build_dir,
        false, // is_post_makepkg = false for pre-check
    )?;

    let mapped = if !pre_check_mapped.is_empty() {
        // All packages already built, skip makepkg
        log::info!("Found already built packages for pkgbase '{}', skipping build", pkgbase);
        pre_check_mapped
    } else {
        // Not all packages found, run makepkg
        run_makepkg(pkgbase, &pkg_build_dir, build_dir, env_root)?;

        // After makepkg, verify all requested packages were built and get their paths
        // This will fail the installation if not all found
        find_and_verify_built_packages(
            pkgbase,
            pkgkeys,
            aur_packages,
            &pkg_build_dir,
            true, // is_post_makepkg = true after makepkg
        )?
    };

    // Log built packages
    for (built_pkg, _, _) in &mapped {
        log::info!("Built package: {}", built_pkg.display());
    }

    Ok(mapped)
}

/// Unpack and link built AUR packages.
///
/// Inputs:
///   - `mapped_packages`: already mapped packages from `find_and_verify_built_packages()`
///     (path, original_key, info)
///
/// Returns:
///   - `this_round_aur_packages`: actual_pkgkey -> InstalledPackageInfo
///   - `this_round_pkgkey_mapping`: original_pkgkey (plan) -> actual_pkgkey (installed)
fn unpack_link_built_aur_packages(
    plan: &crate::plan::InstallationPlan,
    mapped_packages: &[(PathBuf, String, Arc<InstalledPackageInfo>)],
    store_pkglines_by_pkgname: &HashMap<String, Vec<String>>,
) -> Result<(
    InstalledPackagesMap,
    HashMap<String, String>,
)> {
    use crate::package;

    // This round's completed AUR packages (actual_pkgkey -> info)
    let mut this_round_aur_packages = std::collections::HashMap::new();
    // Mapping from original (plan) pkgkeys to actual pkgkeys for this round
    let mut this_round_pkgkey_mapping: HashMap<String, String> = HashMap::new();

    for (built_pkg_path, original_key, info) in mapped_packages {
        // Unpack and link the mapped package *after* we know its original planned key.
        let built_pkg_path_str = built_pkg_path.to_str().ok_or_else(|| {
            eyre!(
                "Invalid UTF-8 in built package path: {}",
                built_pkg_path.display()
            )
        })?;

        // Unpack the package
        let (actual_pkgkey, pkgline) = crate::store::unpack_package(
            built_pkg_path_str,
            &original_key,
            store_pkglines_by_pkgname,
        )
            .with_context(|| {
                format!(
                    "Failed to unpack built package: {}",
                    built_pkg_path.display()
                )
            })?;

        // Update InstalledPackageInfo with the pkgline
        let mut completed_info = Arc::clone(info);
        Arc::make_mut(&mut completed_info).pkgline = pkgline.clone();

        // Link the package
        let store_fs_dir = plan.store_root.join(&pkgline).join("fs");
        crate::link::link_package(plan, &store_fs_dir)
            .with_context(|| {
                format!(
                    "Failed to link built package: {}",
                    built_pkg_path.display()
                )
            })?;

        // Infer name, version, and arch from the package filename for validation
        let (act_name, act_version, act_arch) = infer_name_version_from_arch_pkgfile(built_pkg_path)
            .with_context(|| format!("Failed to infer package metadata from filename: {}", built_pkg_path.display()))?;

        // Construct expected pkgkey from inferred name, version, and arch
        let expected_pkgkey = package::format_pkgkey(&act_name, &act_version, &act_arch);

        // Validate that the inferred pkgkey matches the actual pkgkey from the unpacked package
        if expected_pkgkey != actual_pkgkey {
            return Err(eyre!(
                "Package key mismatch for '{}': inferred '{}' but unpacked package has '{}'",
                built_pkg_path.display(),
                expected_pkgkey,
                actual_pkgkey
            ));
        }

        // Record mapping from original (plan) pkgkey to actual installed pkgkey
        if original_key != &actual_pkgkey {
            this_round_pkgkey_mapping.insert(original_key.clone(), actual_pkgkey.clone());
        }

        // From this point on, the round operates purely on actual_pkgkey
        this_round_aur_packages.insert(actual_pkgkey.clone(), completed_info);
    }

    Ok((this_round_aur_packages, this_round_pkgkey_mapping))
}

/// Map built AUR artifacts back to planned entries.
///
/// Builds the namever2entry mapping and maps each built package to its planned entry.
///
/// Inputs:
///   - `built_pkg_paths`: paths to built package files
///   - `pkgkeys`: list of pkgkeys for this pkgbase
///   - `aur_packages`: map of pkgkey -> InstalledPackageInfo
///
/// Returns:
///   - Vector of (built_pkg_path, original_key, info)
fn map_built_aur_packages(
    built_pkg_paths: &[PathBuf],
    pkgkeys: &[String],
    aur_packages: &InstalledPackagesMap,
) -> Result<Vec<(PathBuf, String, Arc<InstalledPackageInfo>)>> {
    use crate::package;

    // Pre-build a mapping from (pkgname, version) -> (original_pkgkey, info) for all
    // planned AUR packages in this pkgbase group. This allows us to map built artifacts
    // to planned entries without relying on the representative pkgkey used for the build.
    let mut namever2entry: HashMap<(String, String), (String, Arc<InstalledPackageInfo>)> =
        HashMap::new();
    for original_pkgkey in pkgkeys {
        if let Some(info) = aur_packages.get(original_pkgkey) {
            if let Ok((name, version, _)) = package::parse_pkgkey(original_pkgkey) {
                namever2entry
                    .entry((name, version))
                    .or_insert_with(|| (original_pkgkey.clone(), Arc::clone(info)));
            }
        }
    }

    let mut mapped = Vec::new();
    for built_pkg_path in built_pkg_paths {
        // Map built artifact to a planned AUR entry and extract metadata
        if let Some((original_key, info)) =
            map_built_package_to_entry(built_pkg_path, &namever2entry)
        {
            mapped.push((
                built_pkg_path.clone(),
                original_key,
                info,
            ));
        }
    }

    Ok(mapped)
}

/// Map a built package file to its planned AUR entry.
///
/// Returns `Some((original_key, info))` if the package can be mapped,
/// or `None` if it should be skipped (e.g., debug packages or unmapped artifacts).
fn map_built_package_to_entry(
    built_pkg_path: &Path,
    namever2entry: &HashMap<(String, String), (String, Arc<InstalledPackageInfo>)>,
) -> Option<(String, Arc<InstalledPackageInfo>)> {
    // First, infer (pkgname, version, arch) from the built package filename so we can
    // determine which planned AUR entry this artifact corresponds to. We only
    // unpack/link packages that we can successfully map.
    let (act_name, act_version, _act_arch) =
        match infer_name_version_from_arch_pkgfile(built_pkg_path) {
            Ok((name, version, arch)) => (name, version, arch),
            Err(e) => {
                log::warn!(
                    "AUR build produced package file '{}' with unrecognized filename format: {}",
                    built_pkg_path.display(),
                    e
                );
                return None;
            }
        };

    // Map built artifact to a planned AUR pkgkey using the pre-built map:
    // key: (pkgname, version) -> (original_pkgkey, info)
    if let Some((original_key, info)) =
        namever2entry.get(&(act_name.clone(), act_version.clone()))
    {
        Some((
            original_key.clone(),
            Arc::clone(info),
        ))
    } else {
        // Likely a debug or sub package not in install plan: skip unpacking/linking.
        // Don't emit a warning for common debug split packages to avoid noisy logs.
        log::info!(
            "AUR build produced package '{}' ({}, {}), skipping -- not in install plan",
            built_pkg_path.display(),
            act_name,
            act_version,
        );
        None
    }
}

/// Install / process a single round of built AUR packages and normalize the plan.
///
/// This helper:
///   - Fixes up plan keys for this round using `this_round_pkgkey_mapping`.
///   - Accumulates AUR pkgkey mappings across rounds in `aur_mapping_all_rounds`.
///   - Normalizes dependency fields for this round and for skipped reinstalls.
///   - Processes installation results so subsequent rounds can depend on them.
fn postinstall_built_aur_round(
    plan: &mut InstallationPlan,
    this_round_aur_packages: &mut InstalledPackagesMap,
    this_round_pkgkey_mapping: &HashMap<String, String>,
    aur_mapping_all_rounds: &mut HashMap<String, String>,
) -> Result<()> {
    // Fixup plan keys for this round only (based on this_round_pkgkey_mapping)
    if !this_round_pkgkey_mapping.is_empty() {
        fixup_aur_plan_keys(plan, this_round_pkgkey_mapping)
            .with_context(|| "Failed to fixup AUR plan keys for current round")?;
        // Accumulate mapping across all rounds for use when normalizing
        // skipped_reinstalls dependencies.
        aur_mapping_all_rounds.extend(this_round_pkgkey_mapping.clone());
    }

    // Normalize dependency fields for this round before processing results:
    // map any dependencies that still point at pre-build AUR pkgkeys to the
    // actual pkgkeys produced in this round, and ensure InstalledPackageInfo
    // values (e.g. arch) are consistent with their pkgkeys.
    fixup_installed_packages_values(
        this_round_pkgkey_mapping,
        this_round_aur_packages,
    )
    .with_context(|| {
        "Failed to normalize dependency fields for AUR packages in current round"
    })?;

    // Add AUR packages to plan.batch.new_pkgkeys before processing transaction
    plan.batch.new_pkgkeys.clear();
    for k in this_round_aur_packages.keys() {
        plan.batch.new_pkgkeys.insert(k.clone());
    }

    // Process installation results for this round so that subsequent rounds
    // can depend on these newly installed AUR packages.
    run_transaction_batch(plan)?;

    Ok(())
}

/// Fixup InstallationPlan keys for AUR packages after makepkg determines actual architecture.
/// Maps original pkgkeys (with arch="any") to actual pkgkeys (with determined arch).
/// Updates: upgrades_new, fresh_installs, new_exposes, and upgrade_map_old_to_new.
fn fixup_aur_plan_keys(
    plan: &mut InstallationPlan,
    pkgkey_mapping: &HashMap<String, String>, // original_pkgkey -> actual_pkgkey
) -> Result<()> {
    // Fixup ordered_operations - remap pkgkeys in operations
    for op in &mut plan.ordered_operations {
        // Remap new_pkgkey if present
        if let Some(ref mut pkgkey) = op.new_pkgkey {
            if let Some(mapped_key) = pkgkey_mapping.get(pkgkey) {
                // Update the pkgkey in the operation
                let old_key = pkgkey.clone();
                *pkgkey = mapped_key.clone();

                // Also update pkgline in the package info if needed
                if let Some(pkg_info) = plan.new_pkgs.get_mut(&old_key) {
                    let pkg_info_mut = Arc::make_mut(pkg_info);
                    if pkg_info_mut.pkgline.contains(&old_key) {
                        pkg_info_mut.pkgline = pkg_info_mut.pkgline.replace(&old_key, mapped_key);
                    }
                    // Move the entry to the new key
                    let info = plan.new_pkgs.remove(&old_key).unwrap();
                    plan.new_pkgs.insert(mapped_key.clone(), info);
                }
            }
        }
    }

    Ok(())
}

/// Normalize arch-related values in the given package map using a pre-built pkgkey mapping.
///
/// The normalization works by:
///   - Using the provided `pkgkey_mapping` (typically a mapping from pre-build AUR pkgkeys with
///     `arch="any"` to post-build pkgkeys with the actual architecture).
///   - Rewriting all dependency fields in `pkgs` to use the mapped pkgkeys when a mapping exists.
///   - Ensuring `InstalledPackageInfo` values (in particular `arch`) are consistent with the
///     pkgkeys that index `pkgs`.
fn fixup_installed_packages_values(
    pkgkey_mapping: &HashMap<String, String>,
    pkgs: &mut InstalledPackagesMap,
) -> Result<()> {
    use crate::package;

    // Helper to rewrite a single dependency key using the provided mapping.
    let rewrite_key = |k: &String, mapping: &HashMap<String, String>| -> String {
        if let Some(mapped) = mapping.get(k) {
            mapped.clone()
        } else {
            k.clone()
        }
    };

    // Rewrite dependency fields and normalize InstalledPackageInfo values for each
    // package in `pkgs`.
    for (pkgkey, info) in pkgs.iter_mut() {
        // Keep InstalledPackageInfo.arch in sync with the pkgkey that indexes this entry.
        // This is important for AUR packages where the pkgkey arch is updated from "any"
        // to the actual arch after makepkg has run.
        let info_mut = Arc::make_mut(info);
        if info_mut.arch != std::env::consts::ARCH {
            if let Ok((_name, _version, arch)) = package::parse_pkgkey(pkgkey) {
                info_mut.arch = arch;
            }
        }

        info_mut.depends = info_mut
            .depends
            .iter()
            .map(|k| rewrite_key(k, pkgkey_mapping))
            .collect();

        info_mut.rdepends = info_mut
            .rdepends
            .iter()
            .map(|k| rewrite_key(k, pkgkey_mapping))
            .collect();

        info_mut.bdepends = info_mut
            .bdepends
            .iter()
            .map(|k| rewrite_key(k, pkgkey_mapping))
            .collect();

        info_mut.rbdepends = info_mut
            .rbdepends
            .iter()
            .map(|k| rewrite_key(k, pkgkey_mapping))
            .collect();
    }

    Ok(())
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
pub fn find_built_package(dir: &Path, pkgbase: &str, version: &str) -> Result<Vec<PathBuf>> {
    let mut built = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if let Some(ext) = path.extension() {
            if ext == "zst" || ext == "xz" {
                if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .contains(".pkg.tar")
                {
                    // Parse the filename to extract pkgname and version
                    match infer_name_version_from_arch_pkgfile(&path) {
                        Ok((_name, ver, _arch)) => {
                            // Filter by version. For pkgbase filtering, we return all packages
                            // in the directory since multiple pkgnames can share the same pkgbase (split packages).
                            // The actual pkgbase filtering will be done by map_built_aur_packages().
                            if ver == version {
                                built.push(path);
                            }
                        }
                        Err(_) => {
                            // Skip files that don't match the expected format
                            continue;
                        }
                    }
                }
            }
        }
    }
    if built.is_empty() {
        Err(eyre!("Built package not found in {} for pkgbase {} version {}", dir.display(), pkgbase, version))
    } else {
        Ok(built)
    }
}

/// Infer (pkgname, version, arch) from an Arch Linux package filename.
///
/// Examples:
///   - "resto-rs-0.5.0-1-x86_64.pkg.tar.zst" -> ("resto-rs", "0.5.0-1", "x86_64")
///   - "foo-bar-1.2.3-4-any.pkg.tar.zst"     -> ("foo-bar", "1.2.3-4", "any")
///
/// Arch package format: pkgname-pkgver-pkgrel-arch.pkg.tar.zst
/// where version in pkgkeys is "pkgver-pkgrel"
fn infer_name_version_from_arch_pkgfile(path: &Path) -> Result<(String, String, String)> {
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| eyre!("Invalid UTF-8 in package filename: {}", path.display()))?;

    // Strip known Pacman suffixes
    let base = filename
        .strip_suffix(".pkg.tar.zst")
        .or_else(|| filename.strip_suffix(".pkg.tar.xz"))
        .ok_or_else(|| eyre!("Unsupported Arch package filename (suffix): {}", filename))?;

    // Arch package format: pkgname-pkgver-pkgrel-arch
    // Split from the right: arch (last), pkgrel (second-to-last, numeric), then pkgname-pkgver
    let parts: Vec<&str> = base.rsplitn(3, '-').collect();
    if parts.len() < 3 {
        return Err(eyre!(
            "Invalid package filename format (expected at least 3 components): {}",
            filename
        ));
    }

    let arch = parts[0];
    let pkgrel = parts[1];
    let namever = parts[2];

    // Validate that pkgrel is numeric
    if !pkgrel.chars().all(|c| c.is_ascii_digit()) {
        return Err(eyre!(
            "Invalid pkgrel component (expected numeric): {}",
            filename
        ));
    }

    // Now we need to split namever into pkgname and pkgver
    // Since both can contain dashes, we need to find the boundary
    // The version is "pkgver-pkgrel", so we need to extract pkgver
    // We'll split namever on the last dash to get pkgname and pkgver
    if let Some(idx) = namever.rfind('-') {
        let name = &namever[..idx];
        let pkgver = &namever[idx + 1..];
        if name.is_empty() || pkgver.is_empty() {
            return Err(eyre!(
                "Failed to infer (name, version) from package filename: {}",
                filename
            ));
        }
        // Version is "pkgver-pkgrel"
        let version = format!("{}-{}", pkgver, pkgrel);
        Ok((name.to_string(), version, arch.to_string()))
    } else {
        // No dash in namever, so the entire thing is pkgname and pkgver is empty
        // This shouldn't happen in valid Arch packages, but handle it gracefully
        let version = format!("-{}", pkgrel);
        Ok((namever.to_string(), version, arch.to_string()))
    }
}
