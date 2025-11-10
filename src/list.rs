use std::sync::Arc;
use crate::models::*;
use crate::mmio;
use color_eyre::Result;
use memchr::{memchr, memmem::Finder};

// ======================================================================================
// `epkg list` - Enhanced Package Listing Command
// ======================================================================================
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
//   STATUS | PKGNAME | VERSION | ARCH | REPODATA_NAME | SUMMARY
//
//   STATUS (3 characters):
//   - Position 1: E=Exposed/appbin, I=Installed, A=Available
//   - Position 2: 0-9=install depth, _=not installed, E=Essential
//   - Position 3: U=Upgradable, (space)=no upgrade available
//
// ======================================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum ListScope {
    Installed,
    Available,
    Upgradable,
    All,
}

#[derive(Debug, Clone)]
pub struct PackageListItem {
    pub pkgname: String,
    pub version: String,
    pub arch: String,
    pub repodata_name: String,
    pub summary: String,
    pub status: String,
    #[allow(dead_code)]
    pub pkgkey: String,
    #[allow(dead_code)]
    pub installed_info: Option<InstalledPackageInfo>,
}

impl PackageManager {
    /// Main entry point for the enhanced list command
    pub fn list_packages_with_scope(&mut self, scope: ListScope, pattern: &str) -> Result<()> {
        // Load installed packages first
        self.load_installed_packages()?;

        let mut packages_found_overall = 0;

        match scope {
            ListScope::Installed => {
                packages_found_overall += self.process_installed_packages(pattern, false, "Installed Packages")?;
            },
            ListScope::Upgradable => {
                packages_found_overall += self.process_installed_packages(pattern, true, "Upgradable Packages")?;
            },
            ListScope::Available => {
                packages_found_overall += self.process_available_packages(pattern)?;
            },
            ListScope::All => {
                // Display installed packages first
                let installed_count = self.process_installed_packages(pattern, false, "Installed Packages")?;
                packages_found_overall += installed_count;

                self.pkgkey2package.clear();
                // Then display available packages
                let available_count = self.process_available_packages(pattern)?;
                packages_found_overall += available_count;
            }
        }

        if packages_found_overall > 0 {
            println!("\nTotal: {} packages", packages_found_overall);
        } else {
            println!("No packages found matching pattern '{}' in scope {:?}.", pattern, scope);
        }

        Ok(())
    }


    /// Stream through installed packages, applying filtering and upgrade checking, then sorts and displays them.
    /// Returns the number of packages found and processed.
    fn process_installed_packages(&mut self, pattern: &str, upgradable_only: bool, list_title: &str) -> Result<usize> {
        let mut local_items = Vec::new();

        // Collect keys and info to avoid borrowing conflicts
        let installed_data: Vec<(String, InstalledPackageInfo)> = self.installed_packages
            .iter()
            .map(|(key, info)| (key.clone(), info.clone()))
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
            if !matches_glob_pattern(&pkgname, pattern) {
                continue;
            }

            // Check upgrade requirement if needed
            if upgradable_only {
                let is_upgradable = self.is_package_upgradable(&pkgname, &installed_info).unwrap_or(false);
                if !is_upgradable {
                    continue;
                }
            }

            // Create package item for this installed package
            match self.create_installed_package_item(&pkgname, &pkgkey, &installed_info) {
                Ok(item) => local_items.push(item),
                Err(e) => log::warn!("Failed to create item for installed package {}: {}", pkgname, e),
            }
        }

        let count = local_items.len();
        self.sort_and_display_packages(&mut local_items, list_title)?;
        Ok(count)
    }

    fn process_available_packages(&mut self, pattern: &str) -> Result<usize> {
        let list_title = "Available Packages (not installed)";
        if pattern.is_empty() || pattern == "*" {
            return self.process_all_available_packages(&list_title);
        } else {
            return self.process_few_available_packages(pattern, &list_title);
        };
    }

    /// Stream through available packages, applying filtering, excluding installed ones, then sorts and displays them.
    /// Returns the number of packages found and processed.
    fn process_few_available_packages(&mut self, pattern: &str, list_title: &str) -> Result<usize> {
        let mut local_items = Vec::new();

        // Collect matching package names with optimizations
        let matching_pkgnames = self.collect_matching_pkgnames(pattern)?;

        for pkgname in matching_pkgnames {
            // Get package details from repository using crate::mmio directly to skip caching
            match mmio::map_pkgname2packages(&pkgname) {
                Ok(packages) => {
                    for pkg in packages {
                        // Skip if package is already installed (for Available scope)
                        if self.installed_packages.contains_key(&pkg.pkgkey) {
                            continue;
                        }

                        // Create package item for this available package
                        match self.create_available_package_item(&pkg) {
                            Ok(item) => local_items.push(item),
                            Err(e) => log::warn!("Failed to create item for available package {}: {}", pkgname, e),
                        }
                    }
                },
                Err(e) => log::warn!("Failed to get package details for {}: {}", pkgname, e),
            }
        }

        let count = local_items.len();
        self.sort_and_display_packages(&mut local_items, list_title)?;
        Ok(count)
    }

    /// Collect matching package names with optimizations based on pattern type
    fn collect_matching_pkgnames(&self, pattern: &str) -> Result<Vec<String>> {
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
                    for pkgname in shard_pkgnames {
                        // Simple glob pattern matching (can be optimized further)
                        if matches_glob_pattern(&pkgname, &pattern_clone) {
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
    fn process_all_available_packages(&mut self, list_title: &str) -> Result<usize> {
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
                    count += self.scan_packages_mmap(
                        mmap,
                        &repo_index.repodata_name,
                        &mut local_items,
                    )?;
                }
            }
        }

        self.sort_and_display_packages(&mut local_items, list_title)?;
        Ok(count)
    }

    /// Scan a packages_mmap buffer and collect PackageListItems efficiently, dropping past mmap pages by 2MB-aligned granularity
    fn scan_packages_mmap(
        &self,
        file_mapper: &crate::mmio::FileMapper,
        repodata_name: &str,
        local_items: &mut Vec<PackageListItem>,
    ) -> Result<usize> {
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
        let mut nr_found_fields = 0;
        let mut last_advised = 0;
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
                    nr_found_fields = 4;    // print on end of paragraph; summary field is optional
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
                }
                pos = line_end + 1;
                if nr_found_fields >= 4 {
                    count += self.handle_completed_package_bytes(
                        pkgname,
                        version,
                        arch,
                        summary,
                        repodata_name,
                        local_items,
                    )?;
                    pkgname = &[];
                    version = &[];
                    arch = &[];
                    summary = &[];
                    nr_found_fields = 0;
                }
            }
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
        Ok(count)
    }

    fn handle_completed_package_bytes(
        &self,
        pkgname: &[u8],
        version: &[u8],
        arch: &[u8],
        summary: &[u8],
        repodata_name: &str,
        local_items: &mut Vec<PackageListItem>,
    ) -> Result<usize> {
        if !pkgname.is_empty() {
            let pkgname_str = std::str::from_utf8(pkgname)?.trim();
            let version_str = std::str::from_utf8(version)?.trim();
            let arch_str = std::str::from_utf8(arch)?.trim();
            let summary_str = std::str::from_utf8(summary).unwrap_or("").trim();
            let pkgkey = crate::package::format_pkgkey(pkgname_str, version_str, arch_str);
            if !self.installed_packages.contains_key(&pkgkey) {
                let item = self.create_available_package_item_from_data_borrowed(
                    pkgname_str,
                    version_str,
                    arch_str,
                    summary_str,
                    repodata_name,
                    &pkgkey,
                )?;
                local_items.push(item);
                return Ok(1);
            }
        }
        Ok(0)
    }

    /// Create a PackageListItem for an available package from borrowed data (no unnecessary allocations)
    fn create_available_package_item_from_data_borrowed(
        &self,
        pkgname: &str,
        version: &str,
        arch: &str,
        summary: &str,
        repodata_name: &str,
        pkgkey: &str,
    ) -> Result<PackageListItem> {
        let status = self.determine_status_for_available(pkgname)?;
        Ok(PackageListItem {
            pkgname: pkgname.to_owned(),
            version: version.to_owned(),
            arch: arch.to_owned(),
            repodata_name: repodata_name.to_owned(),
            summary: summary.to_owned(),
            status,
            pkgkey: pkgkey.to_owned(),
            installed_info: None,
        })
    }

    /// Helper to sort and display a list of package items.
    /// Takes a mutable reference to `package_items` to sort them in place.
    /// `list_title` is used to print a header before displaying the list.
    fn sort_and_display_packages(&self, package_items: &mut Vec<PackageListItem>, _list_title: &str) -> Result<()> {
        // _list_title is intentionally unused for now

        if package_items.is_empty() {
            return Ok(());
        }

        package_items.sort_by(|a, b| a.pkgname.cmp(&b.pkgname));
        self.display_package_list(package_items)?;
        Ok(())
    }

    /// Create a PackageListItem for an installed package
    fn create_installed_package_item(&mut self, pkgname: &str, pkgkey: &str, installed_info: &InstalledPackageInfo) -> Result<PackageListItem> {
        // Try to get package details from repository using pkgkey first
        let (version, arch, summary, repodata_name) = match self.map_pkgkey2package(pkgkey) {
            Ok(Some(pkg)) => (
                pkg.version.clone(),
                pkg.arch.clone(),
                pkg.summary.clone(),
                pkg.repodata_name.clone(),
            ),
            Ok(None) | Err(_) => {
                // Fallback: try to get package details from local store using pkgline
                match self.map_pkgline2package(&installed_info.pkgline) {
                    Ok(local_pkg) => (
                        local_pkg.version.clone(),
                        local_pkg.arch.clone(),
                        local_pkg.summary.clone(),
                        "local".to_string(), // If found via pkgline, assume it's 'local' or specific to installed context
                    ),
                    Err(_) => {
                        // Last resort: basic info
                        (
                            "unknown".to_string(),
                            config().common.arch.clone(),
                            "Package not found in repositories or local store".to_string(),
                            "orphaned".to_string(),
                        )
                    }
                }
            }
        };

        let status = self.determine_status_for_installed(pkgname, installed_info)?;

        Ok(PackageListItem {
            pkgname: pkgname.to_string(),
            version,
            arch,
            repodata_name,
            summary,
            status,
            pkgkey: installed_info.pkgline.clone(),
            installed_info: Some(installed_info.clone()),
        })
    }

    /// Create a PackageListItem for an available package
    fn create_available_package_item(&self, pkg: &Package) -> Result<PackageListItem> {
        let status = self.determine_status_for_available(&pkg.pkgname)?;

        Ok(PackageListItem {
            pkgname: pkg.pkgname.clone(),
            version: pkg.version.clone(),
            arch: pkg.arch.clone(),
            repodata_name: pkg.repodata_name.clone(),
            summary: pkg.summary.clone(),
            status,
            pkgkey: pkg.pkgkey.clone(),
            installed_info: None,
        })
    }

    /// Determine the status string for an installed package
    fn determine_status_for_installed(&mut self, pkgname: &str, installed_info: &InstalledPackageInfo) -> Result<String> {
        // Position 1: Installation/Exposure status
        let pos1 = if installed_info.ebin_exposure { 'E' } else { 'I' };

        // Position 2: Depth/Essential status
        let pos2 = if mmio::is_essential_pkgname(pkgname) {
            'E'
        } else {
            char::from_digit(installed_info.depend_depth as u32, 10).unwrap_or('9')
        };

        // Position 3: Upgradable status
        let pos3 = if self.is_package_upgradable(pkgname, installed_info).unwrap_or(false) {
            'U'
        } else {
            ' '
        };

        Ok(format!("{}{}{}", pos1, pos2, pos3))
    }

    /// Determine the status string for an available package
    fn determine_status_for_available(&self, _pkgname: &str) -> Result<String> {
        // Position 1: Available
        let pos1 = 'A';

        // Position 2: Not installed
        let pos2 = '_';

        // Position 3: No upgrade status for non-installed packages
        let pos3 = ' ';

        Ok(format!("{}{}{}", pos1, pos2, pos3))
    }

    /// Check if a package has available upgrades
    fn is_package_upgradable(&mut self, pkgname: &str, installed_info: &InstalledPackageInfo) -> Result<bool> {
        // Get installed version from pkgline using parse_pkgline
        let installed_version = self.extract_version_from_installed_info(installed_info)?;

        // Get available packages with the same name
        let available_packages = self.map_pkgname2packages(pkgname)?;

        for pkg in available_packages {
            // Check if same architecture or compatible
            if pkg.arch == installed_info.arch {
                if self.is_version_newer(&pkg.version, &installed_version) {
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    /// Extract version from installed package info using parse_pkgline
    fn extract_version_from_installed_info(&self, installed_info: &InstalledPackageInfo) -> Result<String> {
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
    fn is_version_newer(&self, new_version: &str, current_version: &str) -> bool {
        crate::version::is_version_newer(new_version, current_version)
    }

    /// Display the package list in a formatted table
    fn display_package_list(&self, items: &[PackageListItem]) -> Result<()> {
        use std::sync::atomic::{AtomicBool, Ordering};
        static LEGEND_PRINTED_THIS_INVOCATION: AtomicBool = AtomicBool::new(false);

        if items.is_empty() {
            return Ok(());
        }

        // If LEGEND_PRINTED_THIS_INVOCATION was false, swap sets it to true and returns false.
        // So, if it returns false, it means this is the first time, and we should print.
        if !LEGEND_PRINTED_THIS_INVOCATION.swap(true, Ordering::SeqCst) {
            // Print status legend (similar to dpkg-query)
            println!("Installation=Exposed/Installed/Available");
            println!("| Depth=0-9/Essential/_(not-installed)");
            println!("|/ Upgrade=Upgradable/ (no-upgrade-available)");
            println!("||/ Name                           Version                        Arch         Repo               Description");
            println!("+++-==============================-==============================-============-==================-========================================");
        }

        let mut prev_pkgkey = "";
        for item in items {
            if item.pkgkey == prev_pkgkey {
                continue;
            }
            prev_pkgkey = &item.pkgkey;

            let summary = if item.summary.chars().count() > 60 {
                let truncated: String = item.summary.chars().take(58).collect();
                format!("{}..", truncated)
            } else {
                item.summary.clone()
            };

            println!("{:<3} {:<30} {:<30} {:<12} {:<18} {}",
                     item.status,
                     item.pkgname,
                     item.version,
                     item.arch,
                     item.repodata_name,
                     summary);
        }

        // println!("\nTotal: {} packages", items.len());

        // println!("\nStatus Codes:");
        // println!("  Position 1 - Installation: E=Exposed (in ebin/), I=Installed, A=Available");
        // println!("  Position 2 - Depth: 0-9=installation depth, E=Essential package, _=not installed");
        // println!("  Position 3 - Upgrade: U=upgrade available, (space)=no upgrade available");

        Ok(())
    }

    /// Get a specific package by pkgkey from repositories
    /// First calls map_pkgname2packages() then selects the package matching pkgkey
    fn map_pkgkey2package(&mut self, pkgkey: &str) -> Result<Option<Arc<Package>>> {
        // Extract package name from pkgkey
        let pkgname = crate::package::pkgkey2pkgname(pkgkey)?;

        // Get all packages with this name
        let packages = self.map_pkgname2packages(&pkgname)?;

        // Find the specific package matching the pkgkey
        for pkg in packages {
            if pkg.pkgkey == pkgkey {
                return Ok(Some(Arc::new(pkg)));
            }
        }

        Ok(None)
    }
}

/// Static version of matches_glob_pattern for use in threads
fn matches_glob_pattern(name: &str, pattern: &str) -> bool {
    // Handle simple cases
    if pattern.is_empty() || pattern == "*" {
        return true; // matches everything
    }

    if !pattern.contains('*') {
        // No wildcards, exact match
        return name == pattern;
    }

    // Split pattern by '*' to get parts that must be matched in order
    let parts: Vec<&str> = pattern.split('*').collect();

    // Filter out empty parts (from consecutive '*' or leading/trailing '*')
    let parts: Vec<&str> = parts.into_iter().filter(|&p| !p.is_empty()).collect();

    if parts.is_empty() {
        return true; // Pattern is all '*', matches everything
    }

    let mut search_start = 0;

    for (i, part) in parts.iter().enumerate() {
        if i == 0 && !pattern.starts_with('*') {
            // First part and pattern doesn't start with '*' - must match at beginning
            if !name.starts_with(part) {
                return false;
            }
            search_start = part.len();
        } else if i == parts.len() - 1 && !pattern.ends_with('*') {
            // Last part and pattern doesn't end with '*' - must match at end
            if !name[search_start..].ends_with(part) {
                return false;
            }
        } else {
            // Middle part or flexible end - find it after current position
            if let Some(pos) = name[search_start..].find(part) {
                search_start += pos + part.len();
            } else {
                return false;
            }
        }
    }

    true
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_glob_pattern_no_wildcards() {
        // Exact matching when no wildcards
        assert!(matches_glob_pattern("bash", "bash"));
        assert!(!matches_glob_pattern("bash-completion", "bash"));
        assert!(!matches_glob_pattern("mybash", "bash"));
        assert!(!matches_glob_pattern("bash123", "bash"));
        assert!(!matches_glob_pattern("bsh", "bash"));
        assert!(!matches_glob_pattern("base", "bash"));
    }

    #[test]
    fn test_matches_glob_pattern_prefix() {
        // Prefix matching (pattern*)
        assert!(matches_glob_pattern("bash", "bash*"));
        assert!(matches_glob_pattern("bash-completion", "bash*"));
        assert!(matches_glob_pattern("bashrc", "bash*"));
        assert!(!matches_glob_pattern("mybash", "bash*"));
        assert!(!matches_glob_pattern("abc", "bash*"));

        assert!(matches_glob_pattern("java-8", "java*"));
        assert!(matches_glob_pattern("java", "java*"));
        assert!(!matches_glob_pattern("python-java", "java*"));
    }

    #[test]
    fn test_matches_glob_pattern_suffix() {
        // Suffix matching (*pattern)
        assert!(matches_glob_pattern("bash", "*bash"));
        assert!(matches_glob_pattern("mybash", "*bash"));
        assert!(matches_glob_pattern("zsh-bash", "*bash"));
        assert!(!matches_glob_pattern("bash-completion", "*bash"));
        assert!(!matches_glob_pattern("bashrc", "*bash"));

        assert!(matches_glob_pattern("lib-dev", "*dev"));
        assert!(matches_glob_pattern("something-dev", "*dev"));
        assert!(!matches_glob_pattern("development", "*dev"));
    }

    #[test]
    fn test_matches_glob_pattern_contains() {
        // Contains matching (*pattern*)
        assert!(matches_glob_pattern("bash", "*bash*"));
        assert!(matches_glob_pattern("mybash", "*bash*"));
        assert!(matches_glob_pattern("bash-completion", "*bash*"));
        assert!(matches_glob_pattern("my-bash-script", "*bash*"));
        assert!(!matches_glob_pattern("bsh", "*bash*"));
        assert!(!matches_glob_pattern("base", "*bash*"));

        assert!(matches_glob_pattern("java-openjdk", "*java*"));
        assert!(matches_glob_pattern("javac", "*java*"));
        assert!(!matches_glob_pattern("python", "*java*"));
    }

    #[test]
    fn test_matches_glob_pattern_complex() {
        // Complex patterns (pre*suf)
        assert!(matches_glob_pattern("bash", "b*sh"));
        assert!(matches_glob_pattern("bush", "b*sh"));
        assert!(matches_glob_pattern("brush", "b*sh"));
        assert!(!matches_glob_pattern("zsh", "b*sh"));
        assert!(!matches_glob_pattern("bash-completion", "b*sh"));

        // Multiple wildcards
        assert!(matches_glob_pattern("java-openjdk-headless", "java*openjdk*headless"));
        assert!(matches_glob_pattern("java-11-openjdk-headless", "java*openjdk*headless"));
        assert!(matches_glob_pattern("java-17-openjdk-devel-headless", "java*openjdk*headless"));
        assert!(!matches_glob_pattern("java-openjdk", "java*openjdk*headless"));
        assert!(!matches_glob_pattern("openjdk-headless", "java*openjdk*headless"));

        // Pattern with parts in different positions
        assert!(matches_glob_pattern("abc-def-ghi", "a*d*i"));
        assert!(matches_glob_pattern("apple-dog-igloo", "a*d*o"));
        assert!(!matches_glob_pattern("abc-ghi", "a*d*i"));
        assert!(!matches_glob_pattern("def-ghi", "a*d*i"));
    }

    #[test]
    fn test_matches_glob_pattern_edge_cases() {
        // Empty pattern should match everything (fix for "epkg list" without pattern)
        assert!(matches_glob_pattern("anything", ""));
        assert!(matches_glob_pattern("bash", ""));
        assert!(matches_glob_pattern("", ""));
        assert!(matches_glob_pattern("java-openjdk", ""));

        // Just wildcard
        assert!(matches_glob_pattern("anything", "*"));
        assert!(matches_glob_pattern("", "*"));
        assert!(matches_glob_pattern("bash", "*"));

        // Multiple consecutive wildcards
        assert!(matches_glob_pattern("bash", "**bash**"));
        assert!(matches_glob_pattern("mybash", "**bash**"));
        assert!(matches_glob_pattern("bash-completion", "**bash**"));

        // Empty pattern parts
        assert!(matches_glob_pattern("bash", "*bash*"));
        assert!(matches_glob_pattern("bash", "bash**"));
        assert!(matches_glob_pattern("bash", "**bash"));

        // Pattern longer than name
        assert!(!matches_glob_pattern("sh", "bash"));
        assert!(!matches_glob_pattern("ba", "bash*"));

        // Empty name
        assert!(!matches_glob_pattern("", "bash"));
        assert!(matches_glob_pattern("", "*"));
        assert!(!matches_glob_pattern("", "a*"));
    }

    #[test]
    fn test_matches_glob_pattern_real_world_examples() {
        // Real package names and patterns
        let packages = vec![
            "bash",
            "bash-completion",
            "bash-static",
            "java-1.8.0-openjdk",
            "java-1.8.0-openjdk-headless",
            "java-11-openjdk-headless",
            "java-17-openjdk-headless",
            "lib-dev",
            "python3-dev",
            "gcc-c++",
            "kernel-headers",
        ];

        // Test *bash* pattern
        let bash_matches: Vec<_> = packages.iter()
            .filter(|&pkg| matches_glob_pattern(pkg, "*bash*"))
            .collect();
        assert_eq!(bash_matches, vec![&"bash", &"bash-completion", &"bash-static"]);

        // Test java* pattern
        let java_matches: Vec<_> = packages.iter()
            .filter(|&pkg| matches_glob_pattern(pkg, "java*"))
            .collect();
        assert_eq!(java_matches, vec![
            &"java-1.8.0-openjdk",
            &"java-1.8.0-openjdk-headless",
            &"java-11-openjdk-headless",
            &"java-17-openjdk-headless"
        ]);

        // Test *dev pattern
        let dev_matches: Vec<_> = packages.iter()
            .filter(|&pkg| matches_glob_pattern(pkg, "*dev"))
            .collect();
        assert_eq!(dev_matches, vec![&"lib-dev", &"python3-dev"]);

        // Test *openjdk*headless pattern
        let openjdk_headless_matches: Vec<_> = packages.iter()
            .filter(|&pkg| matches_glob_pattern(pkg, "*openjdk*headless"))
            .collect();
        assert_eq!(openjdk_headless_matches, vec![
            &"java-1.8.0-openjdk-headless",
            &"java-11-openjdk-headless",
            &"java-17-openjdk-headless"
        ]);
    }
}
