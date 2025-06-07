use color_eyre::Result;
use crate::models::*;
use std::sync::Arc;

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
    pub pkgkey: String,
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
                packages_found_overall += self.process_available_packages(pattern, "Available Packages (not installed)")?;
            },
            ListScope::All => {
                // Display installed packages first
                let installed_count = self.process_installed_packages(pattern, false, "Installed Packages")?;
                packages_found_overall += installed_count;

                // Then display available packages
                let available_count = self.process_available_packages(pattern, "Available Packages (not installed)")?;
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
            if !self.matches_glob_pattern(&pkgname, pattern) {
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

    /// Stream through available packages, applying filtering, excluding installed ones, then sorts and displays them.
    /// Returns the number of packages found and processed.
    fn process_available_packages(&mut self, pattern: &str, list_title: &str) -> Result<usize> {
        let mut local_items = Vec::new();
        let repodata_indice = crate::models::repodata_indice();

        for repo_index in repodata_indice.values() {
            for shard in repo_index.repo_shards.values() {
                for pkgname in shard.pkgname2ranges.keys() {
                    // Apply pattern filtering early
                    if !self.matches_glob_pattern(pkgname, pattern) {
                        continue;
                    }

                    // Get package details from repository
                    match self.map_pkgname2packages(pkgname) {
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
            }
        }

        let count = local_items.len();
        self.sort_and_display_packages(&mut local_items, list_title)?;
        Ok(count)
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
        let pos1 = if installed_info.appbin_flag { 'E' } else { 'I' };

        // Position 2: Depth/Essential status
        let pos2 = if crate::mmio::is_essential_pkgname(pkgname) {
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

    /// Check if a name matches a glob pattern
    fn matches_glob_pattern(&self, name: &str, pattern: &str) -> bool {
        // Handle simple cases
        if pattern == "*" {
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

        // Print package items
        for item in items {
            let summary = if item.summary.len() > 60 {
                format!("{}..", &item.summary[..58])
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    // Helper function to create a dummy PackageManager for testing
    fn create_test_package_manager() -> PackageManager {
        PackageManager {
            envs_config: HashMap::new(),
            channels_config: HashMap::new(),
            repos_data: Vec::new(),
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            appbin_source: HashSet::new(),
            installed_packages: HashMap::new(),
            mirrors: HashMap::new(),
            has_worker_process: false,
            ipc_socket: String::new(),
            ipc_stream: None,
            child_pid: None,
        }
    }

    #[test]
    fn test_matches_glob_pattern_no_wildcards() {
        let pm = create_test_package_manager();

        // Exact matching when no wildcards
        assert!(pm.matches_glob_pattern("bash", "bash"));
        assert!(!pm.matches_glob_pattern("bash-completion", "bash"));
        assert!(!pm.matches_glob_pattern("mybash", "bash"));
        assert!(!pm.matches_glob_pattern("bash123", "bash"));
        assert!(!pm.matches_glob_pattern("bsh", "bash"));
        assert!(!pm.matches_glob_pattern("base", "bash"));
    }

    #[test]
    fn test_matches_glob_pattern_prefix() {
        let pm = create_test_package_manager();

        // Prefix matching (pattern*)
        assert!(pm.matches_glob_pattern("bash", "bash*"));
        assert!(pm.matches_glob_pattern("bash-completion", "bash*"));
        assert!(pm.matches_glob_pattern("bashrc", "bash*"));
        assert!(!pm.matches_glob_pattern("mybash", "bash*"));
        assert!(!pm.matches_glob_pattern("abc", "bash*"));

        assert!(pm.matches_glob_pattern("java-8", "java*"));
        assert!(pm.matches_glob_pattern("java", "java*"));
        assert!(!pm.matches_glob_pattern("python-java", "java*"));
    }

    #[test]
    fn test_matches_glob_pattern_suffix() {
        let pm = create_test_package_manager();

        // Suffix matching (*pattern)
        assert!(pm.matches_glob_pattern("bash", "*bash"));
        assert!(pm.matches_glob_pattern("mybash", "*bash"));
        assert!(pm.matches_glob_pattern("zsh-bash", "*bash"));
        assert!(!pm.matches_glob_pattern("bash-completion", "*bash"));
        assert!(!pm.matches_glob_pattern("bashrc", "*bash"));

        assert!(pm.matches_glob_pattern("lib-dev", "*dev"));
        assert!(pm.matches_glob_pattern("something-dev", "*dev"));
        assert!(!pm.matches_glob_pattern("development", "*dev"));
    }

    #[test]
    fn test_matches_glob_pattern_contains() {
        let pm = create_test_package_manager();

        // Contains matching (*pattern*)
        assert!(pm.matches_glob_pattern("bash", "*bash*"));
        assert!(pm.matches_glob_pattern("mybash", "*bash*"));
        assert!(pm.matches_glob_pattern("bash-completion", "*bash*"));
        assert!(pm.matches_glob_pattern("my-bash-script", "*bash*"));
        assert!(!pm.matches_glob_pattern("bsh", "*bash*"));
        assert!(!pm.matches_glob_pattern("base", "*bash*"));

        assert!(pm.matches_glob_pattern("java-openjdk", "*java*"));
        assert!(pm.matches_glob_pattern("javac", "*java*"));
        assert!(!pm.matches_glob_pattern("python", "*java*"));
    }

    #[test]
    fn test_matches_glob_pattern_complex() {
        let pm = create_test_package_manager();

        // Complex patterns (pre*suf)
        assert!(pm.matches_glob_pattern("bash", "b*sh"));
        assert!(pm.matches_glob_pattern("bush", "b*sh"));
        assert!(pm.matches_glob_pattern("brush", "b*sh"));
        assert!(!pm.matches_glob_pattern("zsh", "b*sh"));
        assert!(!pm.matches_glob_pattern("bash-completion", "b*sh"));

        // Multiple wildcards
        assert!(pm.matches_glob_pattern("java-openjdk-headless", "java*openjdk*headless"));
        assert!(pm.matches_glob_pattern("java-11-openjdk-headless", "java*openjdk*headless"));
        assert!(pm.matches_glob_pattern("java-17-openjdk-devel-headless", "java*openjdk*headless"));
        assert!(!pm.matches_glob_pattern("java-openjdk", "java*openjdk*headless"));
        assert!(!pm.matches_glob_pattern("openjdk-headless", "java*openjdk*headless"));

        // Pattern with parts in different positions
        assert!(pm.matches_glob_pattern("abc-def-ghi", "a*d*i"));
        assert!(pm.matches_glob_pattern("apple-dog-igloo", "a*d*o"));
        assert!(!pm.matches_glob_pattern("abc-ghi", "a*d*i"));
        assert!(!pm.matches_glob_pattern("def-ghi", "a*d*i"));
    }

    #[test]
    fn test_matches_glob_pattern_edge_cases() {
        let pm = create_test_package_manager();

        // Just wildcard
        assert!(pm.matches_glob_pattern("anything", "*"));
        assert!(pm.matches_glob_pattern("", "*"));
        assert!(pm.matches_glob_pattern("bash", "*"));

        // Multiple consecutive wildcards
        assert!(pm.matches_glob_pattern("bash", "**bash**"));
        assert!(pm.matches_glob_pattern("mybash", "**bash**"));
        assert!(pm.matches_glob_pattern("bash-completion", "**bash**"));

        // Empty pattern parts
        assert!(pm.matches_glob_pattern("bash", "*bash*"));
        assert!(pm.matches_glob_pattern("bash", "bash**"));
        assert!(pm.matches_glob_pattern("bash", "**bash"));

        // Pattern longer than name
        assert!(!pm.matches_glob_pattern("sh", "bash"));
        assert!(!pm.matches_glob_pattern("ba", "bash*"));

        // Empty name
        assert!(!pm.matches_glob_pattern("", "bash"));
        assert!(pm.matches_glob_pattern("", "*"));
        assert!(!pm.matches_glob_pattern("", "a*"));
    }

    #[test]
    fn test_matches_glob_pattern_real_world_examples() {
        let pm = create_test_package_manager();

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
            .filter(|&pkg| pm.matches_glob_pattern(pkg, "*bash*"))
            .collect();
        assert_eq!(bash_matches, vec![&"bash", &"bash-completion", &"bash-static"]);

        // Test java* pattern
        let java_matches: Vec<_> = packages.iter()
            .filter(|&pkg| pm.matches_glob_pattern(pkg, "java*"))
            .collect();
        assert_eq!(java_matches, vec![
            &"java-1.8.0-openjdk",
            &"java-1.8.0-openjdk-headless",
            &"java-11-openjdk-headless",
            &"java-17-openjdk-headless"
        ]);

        // Test *dev pattern
        let dev_matches: Vec<_> = packages.iter()
            .filter(|&pkg| pm.matches_glob_pattern(pkg, "*dev"))
            .collect();
        assert_eq!(dev_matches, vec![&"lib-dev", &"python3-dev"]);

        // Test *openjdk*headless pattern
        let openjdk_headless_matches: Vec<_> = packages.iter()
            .filter(|&pkg| pm.matches_glob_pattern(pkg, "*openjdk*headless"))
            .collect();
        assert_eq!(openjdk_headless_matches, vec![
            &"java-1.8.0-openjdk-headless",
            &"java-11-openjdk-headless",
            &"java-17-openjdk-headless"
        ]);
    }
}
