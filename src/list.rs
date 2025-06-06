use std::collections::HashSet;
use color_eyre::Result;
use crate::models::*;

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

        // Collect package names based on scope
        let package_names = self.collect_package_names_by_scope(&scope)?;

        // Apply pattern filtering
        let filtered_names = self.apply_pattern_filter(package_names, pattern);

        if filtered_names.is_empty() {
            println!("No packages found matching pattern '{}' in scope {:?}", pattern, scope);
            return Ok(());
        }

        // Collect detailed package information
        let mut package_items = self.collect_package_details(filtered_names, &scope)?;

        // Sort by package name for consistent output
        package_items.sort_by(|a, b| a.pkgname.cmp(&b.pkgname));

        // Display results
        self.display_package_list(&package_items)?;

        Ok(())
    }

    /// Collect package names based on the specified scope
    fn collect_package_names_by_scope(&mut self, scope: &ListScope) -> Result<HashSet<String>> {
        let mut names = HashSet::new();

        match scope {
            ListScope::Installed => {
                // Get installed package names by extracting pkgname from pkgkey
                for pkgkey in self.installed_packages.keys() {
                    if let Ok(pkgname) = crate::mmio::pkgkey2pkgname(pkgkey) {
                        names.insert(pkgname);
                    }
                }
            },
            ListScope::Available => {
                // Get all available package names, then exclude installed ones
                let all_available = self.filter_available_pkgnames()?;
                let installed_names: HashSet<String> = self.installed_packages.keys()
                    .filter_map(|pkgkey| crate::mmio::pkgkey2pkgname(pkgkey).ok())
                    .collect();

                names = all_available.difference(&installed_names).cloned().collect();
            },
            ListScope::Upgradable => {
                // Check installed packages for available upgrades
                // Collect keys first to avoid borrowing conflicts
                let pkgkeys: Vec<String> = self.installed_packages.keys().cloned().collect();
                for pkgkey in pkgkeys {
                    if let Ok(pkgname) = crate::mmio::pkgkey2pkgname(&pkgkey) {
                        if let Some(installed_info) = self.installed_packages.get(&pkgkey).cloned() {
                            if self.is_package_upgradable(&pkgname, &installed_info)? {
                                names.insert(pkgname);
                            }
                        }
                    }
                }
            },
            ListScope::All => {
                // Combine installed and available
                let all_available = self.filter_available_pkgnames()?;
                names.extend(all_available);

                // Also include installed packages (in case some are not in repos)
                for pkgkey in self.installed_packages.keys() {
                    if let Ok(pkgname) = crate::mmio::pkgkey2pkgname(pkgkey) {
                        names.insert(pkgname);
                    }
                }
            }
        }

        Ok(names)
    }

    /// Get all available package names from repository indices
    fn filter_available_pkgnames(&self) -> Result<HashSet<String>> {
        let mut pkgnames = HashSet::new();

        let repodata_indice = crate::models::repodata_indice();
        for repo_index in repodata_indice.values() {
            for shard in repo_index.repo_shards.values() {
                pkgnames.extend(shard.pkgname2ranges.keys().cloned());
            }
        }

        Ok(pkgnames)
    }

    /// Check if a package has available upgrades
    fn is_package_upgradable(&mut self, pkgname: &str, installed_info: &InstalledPackageInfo) -> Result<bool> {
        // Get installed version from pkgline using parse_package_line
        let installed_version = self.extract_version_from_installed_info(installed_info)?;

        // Get available packages with the same name
        let available_packages = self.map_pkgname2packages(pkgname)?;

        for pkg in available_packages {
            // Check if same architecture or compatible
            if pkg.arch == config().common.arch || pkg.arch.is_empty() {
                if self.is_version_newer(&pkg.version, &installed_version) {
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    /// Extract version from installed package info using parse_package_line
    fn extract_version_from_installed_info(&self, installed_info: &InstalledPackageInfo) -> Result<String> {
        // Parse the pkgline to get the actual version
        match crate::io::parse_package_line(&installed_info.pkgline) {
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

    /// Apply glob pattern filtering to package names
    fn apply_pattern_filter(&self, names: HashSet<String>, pattern: &str) -> Vec<String> {
        if pattern.is_empty() {
            return names.into_iter().collect();
        }

        names.into_iter()
            .filter(|name| self.matches_glob_pattern(name, pattern))
            .collect()
    }

    /// Check if a name matches a glob pattern
    fn matches_glob_pattern(&self, name: &str, pattern: &str) -> bool {
        // Handle simple cases
        if pattern == "*" {
            return true; // matches everything
        }

        if !pattern.contains('*') {
            // No wildcards, substring match (like original behavior)
            return name.contains(pattern);
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

    /// Collect detailed package information for display
    fn collect_package_details(&mut self, package_names: Vec<String>, scope: &ListScope) -> Result<Vec<PackageListItem>> {
        let mut items = Vec::new();

        for pkgname in package_names {
            // Get all available packages with this name
            let packages = self.map_pkgname2packages(&pkgname)?;

            // Check if this package is installed - clone the info to avoid borrowing conflicts
            let installed_info = self.find_installed_package_info(&pkgname).cloned();

            if packages.is_empty() {
                // Package might be installed but not in repos (orphaned)
                if let Some(info) = &installed_info {
                    // Try to get package info from local store
                    match self.map_pkgline2package(&info.pkgline) {
                        Ok(local_pkg) => {
                            let status = self.determine_status_for_installed(&pkgname, info)?;
                            let item = PackageListItem {
                                pkgname: local_pkg.pkgname.clone(),
                                version: local_pkg.version.clone(),
                                arch: local_pkg.arch.clone(),
                                repodata_name: "local".to_string(),
                                summary: local_pkg.summary.clone(),
                                status,
                                pkgkey: info.pkgline.clone(),
                                installed_info: Some(info.clone()),
                            };
                            items.push(item);
                        },
                        Err(e) => {
                            log::warn!("Failed to load local package info for {}: {}", info.pkgline, e);
                            // Fallback to basic info
                            let status = self.determine_status_for_installed(&pkgname, info)?;
                            let item = PackageListItem {
                                pkgname: pkgname.clone(),
                                version: "unknown".to_string(),
                                arch: config().common.arch.clone(),
                                repodata_name: "orphaned".to_string(),
                                summary: "Package not found in repositories".to_string(),
                                status,
                                pkgkey: info.pkgline.clone(),
                                installed_info: Some(info.clone()),
                            };
                            items.push(item);
                        }
                    }
                }
                continue;
            }

            // Track if we found the exact installed package variant in repos
            let mut found_installed_variant = false;

            // Process each available package variant
            for pkg in packages {
                // Filter by architecture
                if !pkg.arch.is_empty() && pkg.arch != config().common.arch {
                    continue;
                }

                // Check if this specific package variant is installed
                let specific_installed_info = self.installed_packages.get(&pkg.pkgkey).cloned();
                let effective_installed_info = specific_installed_info.or_else(|| {
                    // Check if this package matches the installed one (same pkgname but different version/pkgkey)
                    if let Some(ref info) = installed_info {
                        if pkg.pkgname == pkgname {
                            Some(info.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                });

                if effective_installed_info.is_some() {
                    found_installed_variant = true;
                }

                // Apply scope filtering at the item level
                let should_include = match scope {
                    ListScope::Installed => effective_installed_info.is_some(),
                    ListScope::Available => effective_installed_info.is_none(),
                    ListScope::Upgradable => {
                        if let Some(ref info) = effective_installed_info {
                            self.is_package_upgradable(&pkgname, info).unwrap_or(false)
                        } else {
                            false
                        }
                    },
                    ListScope::All => true,
                };

                if should_include {
                    let status = if let Some(ref info) = effective_installed_info {
                        self.determine_status_for_installed(&pkg.pkgname, info)?
                    } else {
                        self.determine_status_for_available(&pkg.pkgname)?
                    };

                    let item = PackageListItem {
                        pkgname: pkg.pkgname.clone(),
                        version: pkg.version.clone(),
                        arch: pkg.arch.clone(),
                        repodata_name: pkg.repodata_name.clone(),
                        summary: pkg.summary.clone(),
                        status,
                        pkgkey: pkg.pkgkey.clone(),
                        installed_info: effective_installed_info,
                    };
                    items.push(item);
                }
            }

            // If we have an installed package but didn't find its exact variant in repos,
            // add the locally installed version as a separate entry
            if let Some(info) = &installed_info {
                if !found_installed_variant && matches!(scope, ListScope::Installed | ListScope::All) {
                    match self.map_pkgline2package(&info.pkgline) {
                        Ok(local_pkg) => {
                            let status = self.determine_status_for_installed(&pkgname, info)?;
                            let item = PackageListItem {
                                pkgname: local_pkg.pkgname.clone(),
                                version: local_pkg.version.clone(),
                                arch: local_pkg.arch.clone(),
                                repodata_name: "local".to_string(),
                                summary: local_pkg.summary.clone(),
                                status,
                                pkgkey: info.pkgline.clone(),
                                installed_info: Some(info.clone()),
                            };
                            items.push(item);
                        },
                        Err(e) => {
                            log::warn!("Failed to load local package info for {}: {}", info.pkgline, e);
                        }
                    }
                }
            }
        }

        Ok(items)
    }

    /// Find installed package info by package name
    /// TODO: this is loop-in-loop, so not effiecient
    /// TODO: this assumes no duplicate pkgnames in an env
    fn find_installed_package_info(&self, pkgname: &str) -> Option<&InstalledPackageInfo> {
        for (pkgkey, info) in &self.installed_packages {
            if let Ok(installed_pkgname) = crate::mmio::pkgkey2pkgname(pkgkey) {
                if installed_pkgname == pkgname {
                    return Some(info);
                }
            }
        }
        None
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

    /// Legacy determine_status method for backward compatibility
    fn determine_status(&mut self, pkgname: &str, installed_info: Option<&InstalledPackageInfo>) -> Result<String> {
        if let Some(info) = installed_info {
            self.determine_status_for_installed(pkgname, info)
        } else {
            self.determine_status_for_available(pkgname)
        }
    }

    /// Display the package list in a formatted table
    fn display_package_list(&self, items: &[PackageListItem]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }

        // Print status legend (similar to dpkg-query)
        println!("Installation=Exposed/Installed/Available");
        println!("| Depth=0-9/Essential/_(not-installed)");
        println!("|/ Upgrade=Upgradable/ (no-upgrade-available)");
        println!("||/ Name                            Version                        Arch         Repo                 Description");
        println!("+++-===============================-==============================-============-====================-========================================");

        // Print package items
        for item in items {
            let summary = if item.summary.len() > 60 {
                format!("{}..", &item.summary[..58])
            } else {
                item.summary.clone()
            };

            println!("{:<3} {:<31} {:<30} {:<12} {:<20} {}",
                    item.status,
                    item.pkgname,
                    item.version,
                    item.arch,
                    item.repodata_name,
                    summary);
        }

        println!("\nTotal: {} packages", items.len());

        // println!("\nStatus Codes:");
        // println!("  Position 1 - Installation: E=Exposed (in ebin/), I=Installed, A=Available");
        // println!("  Position 2 - Depth: 0-9=installation depth, E=Essential package, _=not installed");
        // println!("  Position 3 - Upgrade: U=upgrade available, (space)=no upgrade available");

        Ok(())
    }

    /// Legacy function for backward compatibility
    pub fn list_packages(&mut self, glob_pattern: &str) -> Result<()> {
        self.list_packages_with_scope(ListScope::Installed, glob_pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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

        // Substring matching when no wildcards
        assert!(pm.matches_glob_pattern("bash", "bash"));
        assert!(pm.matches_glob_pattern("bash-completion", "bash"));
        assert!(pm.matches_glob_pattern("mybash", "bash"));
        assert!(pm.matches_glob_pattern("bash123", "bash"));
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

    #[test]
    fn test_apply_pattern_filter() {
        let pm = create_test_package_manager();
        let mut packages = HashSet::new();
        packages.insert("bash".to_string());
        packages.insert("bash-completion".to_string());
        packages.insert("java-openjdk".to_string());
        packages.insert("python".to_string());
        packages.insert("zsh".to_string());

        // Test empty pattern
        let result = pm.apply_pattern_filter(packages.clone(), "");
        assert_eq!(result.len(), 5);

        // Test substring pattern
        let result = pm.apply_pattern_filter(packages.clone(), "bash");
        let mut result = result;
        result.sort();
        assert_eq!(result, vec!["bash", "bash-completion"]);

        // Test wildcard pattern
        let result = pm.apply_pattern_filter(packages.clone(), "*java*");
        assert_eq!(result, vec!["java-openjdk"]);

        // Test prefix pattern
        let result = pm.apply_pattern_filter(packages.clone(), "bash*");
        let mut result = result;
        result.sort();
        assert_eq!(result, vec!["bash", "bash-completion"]);
    }
}
