
#[cfg(unix)]
use crate::models::*;
#[cfg(unix)]
use crate::mmio;
#[cfg(unix)]
use crate::models::PACKAGE_CACHE;
#[cfg(unix)]
use crate::io::load_installed_packages;
#[cfg(unix)]
use crate::utils::format_size;
#[cfg(unix)]
use color_eyre::Result;
#[cfg(unix)]
use memchr::{memchr, memmem::Finder};
#[cfg(unix)]
use glob::Pattern;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// ======================================================================================
// `epkg list` - Enhanced Package Listing Command
// ======================================================================================

// Static state for accumulating totals across multiple display_package_list() calls
#[cfg(unix)]
static HEADERS_PRINTED: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
static ACCUM_TOTAL_SIZE: AtomicU64 = AtomicU64::new(0);
#[cfg(unix)]
static ACCUM_TOTAL_INSTALLED_SIZE: AtomicU64 = AtomicU64::new(0);
#[cfg(unix)]
static ACCUM_PACKAGE_COUNT: AtomicU64 = AtomicU64::new(0);

/// Reset accumulated totals and header state
#[cfg(unix)]
fn reset_display_state() {
    HEADERS_PRINTED.store(false, Ordering::SeqCst);
    ACCUM_TOTAL_SIZE.store(0, Ordering::SeqCst);
    ACCUM_TOTAL_INSTALLED_SIZE.store(0, Ordering::SeqCst);
    ACCUM_PACKAGE_COUNT.store(0, Ordering::SeqCst);
}
//
// DESCRIPTION:
//   The `epkg list` command provides comprehensive package listing functionality with
//   multiple scope options and detailed status information. It supports filtering by
//   installation status, availability, and upgrade status.
//
// USAGE:
//   epkg list [--installed] [--upgradable] [--available] [--all] [PATTERN]
//
// SCOPE OPTIONS:
//   --installed   List only installed packages (default)
//   --available   List only packages that are available but not installed
//   --upgradable  List only packages that have available updates
//   --all         List all packages (installed, available, and upgradable)
//
// PATTERN FORMATS:
//   xxx     Matches packages containing "xxx" anywhere in the name
//   xxx*    Matches packages starting with "xxx"
//   *xxx    Matches packages ending with "xxx"
//   (empty) Lists all packages in the specified scope
//
// OUTPUT FORMAT:
//   |/ | Depth | Size | Name | Version | Arch | Repo | Description
//
//   |/ (2 characters):
//   - Position 1: E=Exposed/appbin, I=Installed, A=Available
//   - Position 2: U=Upgradable, (space)=no upgrade available
//   Depth: installation depth (0-9+)
//   Size: package download size (human readable)
//
// ======================================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum ListScope {
    Installed,
    Available,
    Upgradable,
    All,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ListType {
    Installed,
    Upgradable,
    Available,
}

#[cfg(unix)]
#[derive(Debug, Clone)]
pub struct PackageListItem {
    pub pkgname:       String,
    pub version:       String,
    pub arch:          String,
    pub repodata_name: String,
    pub summary:       String,
    pub status:        String,
    pub depth:         u16,
    pub size:          u32,
    pub installed_size: u32,
    #[allow(dead_code)]
    pub pkgkey:         String,
    #[allow(dead_code)]
    pub installed_info: Option<InstalledPackageInfo>,
}

/// Main entry point for the enhanced list command
#[cfg(unix)]
pub fn list_packages_with_scope(scope: ListScope, pattern: &str) -> Result<()> {
    // Reset display state before processing any packages
    reset_display_state();
    // Load installed packages first
    load_installed_packages()?;

    let mut packages_found_overall = 0;

    match scope {
        ListScope::Installed => {
            packages_found_overall += process_installed_packages(pattern, ListType::Installed)?;
        },
        ListScope::Upgradable => {
            packages_found_overall += process_installed_packages(pattern, ListType::Upgradable)?;
        },
        ListScope::Available => {
            packages_found_overall += process_available_packages(pattern)?;
        },
        ListScope::All => {
            // Display installed packages first
            let installed_count = process_installed_packages(pattern, ListType::Installed)?;
            packages_found_overall += installed_count;

            PACKAGE_CACHE.pkgkey2package.write().unwrap().clear();
            // Then display available packages
            let available_count = process_available_packages(pattern)?;
            packages_found_overall += available_count;
        }
    }

    // Print accumulated totals if any packages were found
    if packages_found_overall > 0 {
        let total_packages = ACCUM_PACKAGE_COUNT.load(Ordering::SeqCst);
        let total_size = ACCUM_TOTAL_SIZE.load(Ordering::SeqCst);
        let total_installed_size = ACCUM_TOTAL_INSTALLED_SIZE.load(Ordering::SeqCst);
        println!("\nTotal: {} packages, {}, {} if installed", total_packages, format_size(total_size), format_size(total_installed_size));
    } else {
        if pattern.is_empty() {
            println!("No packages found in scope {:?}.", scope);
        } else {
            println!("No packages found matching pattern '{}' in scope {:?}.", pattern, scope);
        }
    }

    Ok(())
}


/// Stream through installed packages, applying filtering and upgrade checking, then sorts and displays them.
/// Returns the number of packages found and processed.
#[cfg(unix)]
fn process_installed_packages(pattern: &str, list_type: ListType) -> Result<usize> {
    let mut local_items = Vec::new();
    let upgradable_only = matches!(list_type, ListType::Upgradable);

    // Pre-compile glob pattern if provided (empty or "*" means match all)
    let pattern_glob = if pattern.is_empty() || pattern == "*" {
        None
    } else {
        match Pattern::new(pattern) {
            Ok(p) => Some(p),
            // Invalid pattern: treat as no matches
            Err(_) => return Ok(0),
        }
    };

    // Collect keys and info to avoid borrowing conflicts
    let installed_data: Vec<(String, Arc<InstalledPackageInfo>)> = PACKAGE_CACHE.installed_packages
        .read()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), Arc::clone(v)))
        .collect();

    for (pkgkey, installed_info) in installed_data {
        // Extract package name from pkgkey
        let pkgname = match crate::package::pkgkey2pkgname(&pkgkey) {
            Ok(name) => name,
            Err(_) => {
                log::debug!("Skipping invalid pkgkey: {}", pkgkey);
                continue;
            }
        };

        // Apply pattern filtering early
        if let Some(ref pat) = pattern_glob {
            if !pat.matches(&pkgname) {
                continue;
            }
        }

        // Check upgrade requirement if needed
        if upgradable_only {
            let is_upgradable = is_package_upgradable(&pkgname, &installed_info).unwrap_or(false);
            if !is_upgradable {
                continue;
            }
        }

        // Create package item for this installed package
        match create_installed_package_item(&pkgname, &pkgkey, &installed_info) {
            Ok(item) => local_items.push(item),
            Err(e) => log::warn!("Failed to create item for installed package {}: {}", pkgname, e),
        }
    }

    let count = local_items.len();
    sort_and_display_packages(&mut local_items, list_type)?;
    Ok(count)
}

#[cfg(unix)]
fn process_available_packages(pattern: &str) -> Result<usize> {
    if pattern.is_empty() || pattern == "*" {
        return process_all_available_packages(ListType::Available);
    } else {
        return process_few_available_packages(pattern, ListType::Available);
    };
}

/// Stream through available packages, applying filtering, excluding installed ones, then sorts and displays them.
/// Returns the number of packages found and processed.
#[cfg(unix)]
fn process_few_available_packages(pattern: &str, list_type: ListType) -> Result<usize> {
    let mut local_items = Vec::new();

    // Collect matching package names with optimizations
    let matching_pkgnames = collect_matching_pkgnames(pattern)?;

    for pkgname in matching_pkgnames {
        // Get package details from repository using crate::mmio directly to skip caching
        match mmio::map_pkgname2packages(&pkgname) {
            Ok(packages) => {
                for pkg in packages {
                    // Skip if package is already installed (for Available scope)
                    if PACKAGE_CACHE.installed_packages.read().unwrap().contains_key(&pkg.pkgkey) {
                        continue;
                    }

                    // Create package item for this available package
                    match create_available_package_item(&pkg) {
                        Ok(item) => local_items.push(item),
                        Err(e) => log::warn!("Failed to create item for available package {}: {}", pkgname, e),
                    }
                }
            },
            Err(e) => log::warn!("Failed to get package details for {}: {}", pkgname, e),
        }
    }

    let count = local_items.len();
    sort_and_display_packages(&mut local_items, list_type)?;
    Ok(count)
}

/// Collect matching package names with optimizations based on pattern type
#[cfg(unix)]
fn collect_matching_pkgnames(pattern: &str) -> Result<Vec<String>> {
    let mut repodata_indice = crate::models::repodata_indice_mut();
    let mut matching_pkgnames = Vec::new();

    // Case 1: Handle exact name match (no wildcards)
    if !pattern.contains('*') {
        for repo_index in repodata_indice.values_mut() {
            for shard in repo_index.repo_shards.values_mut() {
                mmio::ensure_pkgname2ranges_loaded(shard)?;
                if shard.pkgname2ranges.contains_key(pattern) {
                    matching_pkgnames.push(pattern.to_string());
                    return Ok(matching_pkgnames); // Found exact match, no need to continue
                }
            }
        }
        return Ok(matching_pkgnames); // No exact match found
    }

    // Case 2: Handle prefix pattern (e.g., "prefix*")
    if pattern.ends_with('*') && !pattern[..pattern.len()-1].contains('*') {
        let prefix = &pattern[..pattern.len()-1];
        for repo_index in repodata_indice.values_mut() {
            for shard in repo_index.repo_shards.values_mut() {
                mmio::ensure_pkgname2ranges_loaded(shard)?;
                let range = shard.pkgname2ranges.range(prefix.to_string()..);
                for (pkgname, _) in range {
                    if pkgname.starts_with(prefix) {
                        matching_pkgnames.push(pkgname.clone());
                    } else {
                        break; // No more matches with this prefix
                    }
                }
            }
        }
        return Ok(matching_pkgnames);
    }

    // Case 3: Handle other patterns with threading
    let mut handles = Vec::new();
    let pattern = pattern.to_string();

    for repo_index in repodata_indice.values_mut() {
        for shard in repo_index.repo_shards.values_mut() {
            mmio::ensure_pkgname2ranges_loaded(shard)?;
            let shard_pkgnames: Vec<String> = shard.pkgname2ranges.keys().cloned().collect();
            let pattern_clone = pattern.clone();

            let handle = std::thread::spawn(move || {
                let mut local_matches = Vec::new();
                // Compile glob pattern once per thread; empty/"*" should have been handled earlier
                let pat = match Pattern::new(&pattern_clone) {
                    Ok(p) => p,
                    // Invalid pattern: no matches in this shard
                    Err(_) => return local_matches,
                };
                for pkgname in shard_pkgnames {
                    if pat.matches(&pkgname) {
                        local_matches.push(pkgname);
                    }
                }
                local_matches
            });
            handles.push(handle);
        }
    }

    // Collect results from all threads
    for handle in handles {
        match handle.join() {
            Ok(thread_matches) => matching_pkgnames.extend(thread_matches),
            Err(_) => log::warn!("Thread failed to complete"),
        }
    }

    Ok(matching_pkgnames)
}

/// Process all available packages when pattern is empty or "*" - optimized direct scanning
#[cfg(unix)]
fn process_all_available_packages(list_type: ListType) -> Result<usize> {
    let mut repodata_indice = crate::models::repodata_indice_mut();
    let mut count = 0;
    // Pre-size local_items using the sum of nr_packages from all shards
    let mut estimated_total_packages = 0;
    for repo_index in repodata_indice.values_mut() {
        for shard in repo_index.repo_shards.values_mut() {
            estimated_total_packages += shard.packages.nr_packages;
            shard.pkgname2ranges.clear();  // possibly loaded ondemand by process_installed_packages()
        }
    }
    let mut local_items = Vec::with_capacity(estimated_total_packages);

    for repo_index in repodata_indice.values_mut() {
        for shard in repo_index.repo_shards.values_mut() {
            if let Some(mmap) = &shard.packages_mmap {
                count += scan_packages_mmap(
                    mmap,
                    &repo_index.repodata_name,
                    &mut local_items,
                )?;
            }
        }
    }

    sort_and_display_packages(&mut local_items, list_type)?;
    Ok(count)
}

/// Scan a packages_mmap buffer and collect PackageListItems efficiently, dropping past mmap pages by 2MB-aligned granularity
#[cfg(unix)]
fn scan_packages_mmap(
    file_mapper: &crate::mmio::FileMapper,
    repodata_name: &str,
    local_items: &mut Vec<PackageListItem>,
) -> Result<usize> {
    #[cfg(target_os = "linux")]
    use libc::{madvise, c_void, MADV_DONTNEED};
    let data = file_mapper.data();
    let mut count = 0;
    let mut pos = 0;
    let finder = Finder::new(b"pkgname: ");
    // Use plain slices and a found counter
    let mut pkgname: &[u8] = &[];
    let mut version: &[u8] = &[];
    let mut arch: &[u8] = &[];
    let mut summary: &[u8] = &[];
    let mut size: u32 = 0;
    let mut installed_size: u32 = 0;
    let mut nr_found_fields = 0;
    #[cfg(target_os = "linux")]
    let mut last_advised = 0;
    #[cfg(target_os = "linux")]
    const MMAP_DROP_GRANULARITY: usize = 2 * 1024 * 1024; // 2MB
    while pos < data.len() {
        // If we are at the start of a package, use Finder to jump to next 'pkgname: '
        if nr_found_fields == 0 {
            if let Some(found) = finder.find(&data[pos..]) {
                pos += found;
                // Now parse pkgname line
                let line_end = memchr(b'\n', &data[pos..]).map(|i| pos + i).unwrap_or(data.len());
                let line = &data[pos..line_end];
                if line.starts_with(b"pkgname: ") {
                    pkgname = &line[b"pkgname: ".len()..];
                    nr_found_fields = 1;
                    pos = line_end + 1;
                } else {
                    // Should not happen, skip line
                    pos = line_end + 1;
                    continue;
                }
            } else {
                break;
            }
        } else {
            // Parse next lines for version, arch, summary
            let line_end = memchr(b'\n', &data[pos..]).map(|i| pos + i).unwrap_or(data.len());
            let line = &data[pos..line_end];
            if line.is_empty() {
                nr_found_fields = 6;    // print on end of paragraph; summary field is optional
            }
            if line.starts_with(b"version: ") {
                version = &line[b"version: ".len()..];
                nr_found_fields += 1;
            } else if line.starts_with(b"arch: ") {
                arch = &line[b"arch: ".len()..];
                nr_found_fields += 1;
            } else if line.starts_with(b"summary: ") {
                summary = &line[b"summary: ".len()..];
                nr_found_fields += 1;
            } else if line.starts_with(b"size: ") {
                if let Ok(parsed) = std::str::from_utf8(&line[b"size: ".len()..]).unwrap_or("0").trim().parse() {
                    size = parsed;
                    nr_found_fields += 1;
                }
            } else if line.starts_with(b"installedSize: ") {
                if let Ok(parsed) = std::str::from_utf8(&line[b"installedSize: ".len()..]).unwrap_or("0").trim().parse() {
                    installed_size = parsed;
                    nr_found_fields += 1;
                }
            }
            pos = line_end + 1;
            if nr_found_fields >= 6 {
                count += handle_completed_package_bytes(
                    pkgname,
                    version,
                    arch,
                    summary,
                    size,
                    installed_size,
                    repodata_name,
                    local_items,
                )?;
                pkgname = &[];
                version = &[];
                arch = &[];
                summary = &[];
                size = 0;
                installed_size = 0;
                nr_found_fields = 0;
            }
        }
        #[cfg(target_os = "linux")]
        {
            // Drop past mmap pages by 2MB-aligned granularity
            let next_advisable = (pos / MMAP_DROP_GRANULARITY) * MMAP_DROP_GRANULARITY;
            if next_advisable > last_advised {
                let advise_ptr = unsafe { data.as_ptr().add(last_advised) as *mut c_void };
                let advise_len = next_advisable - last_advised;
                unsafe {
                    madvise(advise_ptr, advise_len, MADV_DONTNEED);
                }
                last_advised = next_advisable;
            }
        }
    }
    Ok(count)
}

#[cfg(unix)]
fn handle_completed_package_bytes(
    pkgname: &[u8],
    version: &[u8],
    arch: &[u8],
    summary: &[u8],
    size: u32,
    installed_size: u32,
    repodata_name: &str,
    local_items: &mut Vec<PackageListItem>,
) -> Result<usize> {
    if !pkgname.is_empty() {
        let pkgname_str = std::str::from_utf8(pkgname)?.trim();
        let version_str = std::str::from_utf8(version)?.trim();
        let arch_str = std::str::from_utf8(arch)?.trim();
        let summary_str = std::str::from_utf8(summary).unwrap_or("").trim();
        let pkgkey = crate::package::format_pkgkey(pkgname_str, version_str, arch_str);
        if !PACKAGE_CACHE.installed_packages.read().unwrap().contains_key(&pkgkey) {
            let status = determine_status_for_available(pkgname_str)?;
            let item = PackageListItem {
                pkgname: pkgname_str.to_owned(),
                version: version_str.to_owned(),
                arch: arch_str.to_owned(),
                repodata_name: repodata_name.to_owned(),
                summary: summary_str.to_owned(),
                status,
                depth: 0,
                size,
                installed_size,
                pkgkey: pkgkey.to_owned(),
                installed_info: None,
            };
            local_items.push(item);
            return Ok(1);
        }
    }
    Ok(0)
}


/// Helper to sort and display a list of package items.
/// Takes a mutable reference to `package_items` to sort them in place.
/// `list_type` determines sorting strategy (Available vs Installed/Upgradable).
#[cfg(unix)]
fn sort_and_display_packages(package_items: &mut Vec<PackageListItem>, list_type: ListType) -> Result<()> {
    // Sort by depth then name, except for Available lists which keep alphabetical sorting

    match list_type {
        ListType::Available => {
            package_items.sort_by(|a, b| a.pkgname.cmp(&b.pkgname));
        }
        ListType::Installed | ListType::Upgradable => {
            package_items.sort_by(|a, b| {
                match a.depth.cmp(&b.depth) {
                    std::cmp::Ordering::Equal => a.pkgname.cmp(&b.pkgname),
                    other => other,
                }
            });
        }
    }
    display_package_list(package_items)?;
    Ok(())
}

/// Create a PackageListItem for an installed package
#[cfg(unix)]
fn create_installed_package_item(pkgname: &str, pkgkey: &str, installed_info: &InstalledPackageInfo) -> Result<PackageListItem> {
    // Try to get package details from repository using pkgkey first
    let (version, arch, summary, repodata_name, size, installed_size) = match mmio::map_pkgkey2package(pkgkey) {
        Ok(pkg) => (
            pkg.version.clone(),
            pkg.arch.clone(),
            pkg.summary.clone(),
            pkg.repodata_name.clone(),
            pkg.size,
            pkg.installed_size,
        ),
        Err(_) => {
            // Fallback: try to get package details from local store using pkgline
            match crate::package_cache::map_pkgline2package(&installed_info.pkgline) {
                Ok(local_pkg) => (
                    local_pkg.version.clone(),
                    local_pkg.arch.clone(),
                    local_pkg.summary.clone(),
                    "local".to_string(), // If found via pkgline, assume it's 'local' or specific to installed context
                    local_pkg.size,
                    local_pkg.installed_size,
                ),
                Err(_) => {
                    // Last resort: basic info
                    (
                        "unknown".to_string(),
                        config().common.arch.clone(),
                        "Package not found in repositories or local store".to_string(),
                        "orphaned".to_string(),
                        0,
                        0,
                    )
                }
            }
        }
    };

    let (status, depth) = determine_status_for_installed(pkgname, installed_info)?;

    Ok(PackageListItem {
        pkgname: pkgname.to_string(),
        version,
        arch,
        repodata_name,
        summary,
        status,
        depth,
        size,
        installed_size,
        pkgkey: installed_info.pkgline.clone(),
        installed_info: Some(installed_info.clone()),
    })
}

/// Create a PackageListItem for an available package
#[cfg(unix)]
fn create_available_package_item(pkg: &Package) -> Result<PackageListItem> {
    let status = determine_status_for_available(&pkg.pkgname)?;

    Ok(PackageListItem {
        pkgname: pkg.pkgname.clone(),
        version: pkg.version.clone(),
        arch: pkg.arch.clone(),
        repodata_name: pkg.repodata_name.clone(),
        summary: pkg.summary.clone(),
        status,
        depth: 0,
        size: pkg.size,
        installed_size: pkg.installed_size,
        pkgkey: pkg.pkgkey.clone(),
        installed_info: None,
    })
}

/// Determine the status string and depth for an installed package
#[cfg(unix)]
fn determine_status_for_installed(pkgname: &str, installed_info: &InstalledPackageInfo) -> Result<(String, u16)> {
    // Position 1: Installation/Exposure status
    let pos1 = if installed_info.ebin_exposure { 'E' } else { 'I' };

    // Position 3: Upgradable status
    let pos3 = if is_package_upgradable(pkgname, installed_info).unwrap_or(false) {
        'U'
    } else {
        ' '
    };

    let status = format!("{}{}", pos1, pos3);

    // Depth: always return depend_depth
    let depth = installed_info.depend_depth;

    Ok((status, depth))
}

/// Determine the status string for an available package
#[cfg(unix)]
fn determine_status_for_available(_pkgname: &str) -> Result<String> {
    // Position 1: Available
    let pos1 = 'A';

    // Position 3: No upgrade status for non-installed packages
    let pos3 = ' ';

    Ok(format!("{}{}", pos1, pos3))
}

/// Check if a package has available upgrades
#[cfg(unix)]
fn is_package_upgradable(pkgname: &str, installed_info: &InstalledPackageInfo) -> Result<bool> {
    // Get installed version from pkgline using parse_pkgline
    let installed_version = extract_version_from_installed_info(installed_info)?;

    // Get available packages with the same name
    let available_packages = crate::package_cache::map_pkgname2packages(pkgname)?;

    for pkg in available_packages {
        // Check if same architecture or compatible
        if pkg.arch == installed_info.arch {
            if is_version_newer(&pkg.version, &installed_version) {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// Extract version from installed package info using parse_pkgline
#[cfg(unix)]
fn extract_version_from_installed_info(installed_info: &InstalledPackageInfo) -> Result<String> {
    // Parse the pkgline to get the actual version
    match crate::package::parse_pkgline(&installed_info.pkgline) {
        Ok(package_line) => Ok(package_line.version),
        Err(e) => {
            log::warn!("Failed to parse package line '{}': {}", installed_info.pkgline, e);
            // If parse fails completely, return unknown
            Ok("unknown".to_string())
        }
    }
}

/// Simple version comparison (can be enhanced with proper semver)
#[cfg(unix)]
fn is_version_newer(new_version: &str, current_version: &str) -> bool {
    crate::version_compare::is_version_newer(new_version, current_version)
}

/// Print table headers and separator only once
#[cfg(unix)]
fn print_headers_if_needed(headers: &[&str], col_widths: &[usize]) {
    // Print status legend (similar to dpkg-query) only when printing headers for the first time
    if !HEADERS_PRINTED.load(Ordering::SeqCst) {
        println!("Exposed/Installed/Available");
        println!("| Upgradable");
    }

    // Print header row and separator only when printing headers for the first time
    if !HEADERS_PRINTED.load(Ordering::SeqCst) {
        // Print header row (no truncation, fixed widths as minimum)
        for (i, header) in headers.iter().enumerate() {
            if i == 1 || i == 2 {
                // Depth and Size: right-aligned
                print!("{:>width$}", header, width = col_widths[i]);
            } else {
                print!("{:<width$}", header, width = col_widths[i]);
            }
            if i < headers.len() - 1 {
                print!("  "); // Two spaces between columns
            }
        }
        println!();

        // Print header separator line (using '=')
        for (i, &width) in col_widths.iter().enumerate() {
            print!("{}", "=".repeat(width));
            if i < col_widths.len() - 1 {
                print!("=-");
            }
        }
        println!();

        HEADERS_PRINTED.store(true, Ordering::SeqCst);
    }
}

/// Compute table configuration: column widths, start positions, and alignment
#[cfg(unix)]
fn compute_table_config() -> ([usize; 8], [usize; 8], [bool; 8]) {
    // Fixed widths: status(2), depth(5), size(8), pkgname(36), version(30), arch(11), repo(18)
    let col_widths = [2, 5, 8, 36, 30, 11, 18, 60];
    // Compute start positions for each column (including inter-column spaces)
    let mut starts = [0; 8];
    let mut pos = 0;
    for i in 0..8 {
        starts[i] = pos;
        pos += col_widths[i];
        if i < 7 {
            pos += 2; // inter-column spaces
        }
    }
    // Which columns are right-aligned?
    let right_aligned = [false, true, true, false, false, false, false, false];
    (col_widths, starts, right_aligned)
}

/// Print a single table row with shift absorption for dynamic padding
///
/// Dynamic padding compensation algorithm for long package names:
///
/// REQUIREMENT:
/// - Some package names exceed the Name column width (36 chars)
/// - Must never truncate content
/// - Must maintain at least 2 spaces between columns
/// - Should preserve vertical alignment of later columns (Arch, Repo, Description)
///
/// SOLUTION: Shift absorption with column spare capacity
///
/// 1. When content exceeds column width, overflow becomes "shift"
/// 2. Each column's spare capacity (col_width - content_width) can absorb shift
/// 3. Shift propagates until absorbed by subsequent columns' spare capacity
///
/// Example: 48-char package name in 36-char column
/// - Name overflow = 12 → shift = 12
/// - Version spare = 17 (30 - 13) → absorbs 12 shift → shift = 0
/// - Arch aligns at nominal position → vertical alignment restored
///
/// INVARIANTS:
/// - Always 2 spaces between columns
/// - Never truncates content
/// - Right-alignment preserved for Depth/Size columns
#[cfg(unix)]
fn print_row_with_shift_absorption(
    row_cells: &[&str],
    col_widths: &[usize; 8],
    starts: &[usize; 8],
    right_aligned: &[bool; 8],
) {
    let mut shift = 0; // overflow not yet absorbed
    let mut pos = 0;
    for i in 0..8 {
        let col_width = col_widths[i];
        let cell = row_cells[i];
        let width = cell.len(); // ASCII assumption

        // Try to absorb accumulated shift using this column's spare capacity
        let spare = col_width.saturating_sub(width);
        let absorbed = if spare > 0 { spare.min(shift) } else { 0 };
        shift -= absorbed;

        // Where content actually starts (shifted right by absorbed)
        let content_start = starts[i] + absorbed;

        // Move to content start if needed
        if pos < content_start {
            print!("{:>width$}", "", width = content_start - pos);
            pos = content_start;
        }

        // Print the cell content
        if right_aligned[i] {
            // Right-aligned: pad left to fill column width
            if width <= col_width {
                let left_padding = col_width - width;
                print!("{:>width$}", "", width = left_padding);
                print!("{}", cell);
                pos += col_width;
            } else {
                // Overflow: no padding
                print!("{}", cell);
                pos += width;
            }
        } else {
            // Left-aligned: just print
            print!("{}", cell);
            pos += width;
        }

        // Compute overflow from this column (content beyond column width after absorption)
        let effective_width = col_width - absorbed; // space left after absorption
        let overflow = width.saturating_sub(effective_width);
        if overflow > 0 {
            shift += overflow;
        }

        // Spacing after column i (always at least 2 spaces)
        if i < 7 {
            let spaces_after = 2_usize;
            print!("{: <width$}", "", width = spaces_after);
            pos += spaces_after;
        }
    }
    println!();
}

/// Display the package list in a formatted table
#[cfg(unix)]
fn display_package_list(items: &[PackageListItem]) -> Result<()> {
    // If no items, still may need to accumulate totals (for empty batches in --all mode)
    let has_items = !items.is_empty();

    // Manual table formatting for performance (replaces comfy_table)
    let headers = vec!["|/", "Depth", "Size", "Name", "Version", "Arch", "Repo", "Description"];
    let (col_widths, starts, right_aligned) = compute_table_config();

    // Print headers (only once)
    if has_items {
        print_headers_if_needed(&headers, &col_widths);
    }

    // Print rows directly without collecting
    let mut prev_pkgkey = "";
    let mut batch_package_count = 0;

    if has_items {
        for item in items {
        if item.pkgkey == prev_pkgkey {
            continue;
        }
        prev_pkgkey = &item.pkgkey;
        batch_package_count += 1;

        // Format depth: show digit
        let depth_str = item.depth.to_string();

        // Format size: human readable
        let size_str = format_size(item.size as u64);

        // Format description (summary)
        let description = item.summary.clone();

        // Add to accumulated totals
        ACCUM_TOTAL_SIZE.fetch_add(item.size as u64, Ordering::SeqCst);
        ACCUM_TOTAL_INSTALLED_SIZE.fetch_add(item.installed_size as u64, Ordering::SeqCst);

        // Print row directly
        let row_cells = [
            item.status.as_str(),
            depth_str.as_str(),
            size_str.as_str(),
            item.pkgname.as_str(),
            item.version.as_str(),
            item.arch.as_str(),
            item.repodata_name.as_str(),
            description.as_str(),
        ];

        // Print row with shift absorption using column spare capacity
        print_row_with_shift_absorption(&row_cells, &col_widths, &starts, &right_aligned);
    }
    }

    // Add batch package count to accumulated total
    ACCUM_PACKAGE_COUNT.fetch_add(batch_package_count, Ordering::SeqCst);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dynamic_padding() {
        // Create test items with varying pkgname lengths
        let items = vec![
            PackageListItem {
                pkgname: "short".to_string(),
                version: "1.0".to_string(),
                arch: "x86_64".to_string(),
                repodata_name: "main".to_string(),
                summary: "Test package".to_string(),
                status: "A ".to_string(),
                depth: 0,
                size: 1024,
                installed_size: 2048,
                pkgkey: "short-1.0-x86_64".to_string(),
                installed_info: None,
            },
            PackageListItem {
                pkgname: "a-very-long-package-name-that-exceeds-column-width".to_string(), // 48 chars
                version: "20250814.1-r0".to_string(),
                arch: "x86_64".to_string(),
                repodata_name: "main".to_string(),
                summary: "Long package".to_string(),
                status: "A ".to_string(),
                depth: 0,
                size: 2048,
                installed_size: 4096,
                pkgkey: "long-20250814.1-r0-x86_64".to_string(),
                installed_info: None,
            },
        ];

        // Test that display_package_list doesn't panic
        let result = display_package_list(&items);
        assert!(result.is_ok());
    }
}

