use std::process::exit;
use std::collections::{HashMap};
use std::time::{SystemTime, UNIX_EPOCH};
use std::sync::Arc;
use color_eyre::Result;
use color_eyre::eyre;
use log;
use crate::models::*;

use crate::parse_requires::*;
use crate::version;

impl InstalledPackageInfo {
    fn new(depth: u8, appbin_flag: bool, arch: String) -> Self {
        Self {
            pkgline: String::new(),
            arch,
            install_time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            depend_depth: depth,
            appbin_flag,
        }
    }
}

impl PackageManager {
    pub fn record_appbin_source(&mut self, packages: &mut HashMap<String, InstalledPackageInfo>) -> Result<()> {
        log::debug!("Recording appbin source for {} packages", packages.len());
        for pkgkey in packages.keys() {
            let pkg_json = self.load_package_info(pkgkey)?;
            if pkg_json.source.is_some() {
                self.appbin_source.insert(pkg_json.source.as_ref().unwrap().clone());
            }
        }
        Ok(())
    }

    pub fn change_appbin_flag_same_source(&mut self, packages: &mut HashMap<String, InstalledPackageInfo>) -> Result<()> {
        log::debug!("Checking appbin flag for {} packages with same source", packages.len());
        for (pkgkey, package_info) in packages.iter_mut() {
            let pkg_json = self.load_package_info(pkgkey.as_str())?;
            if package_info.appbin_flag == false && pkg_json.source.is_some() {
                let Some(source) = &pkg_json.source else { continue };
                if self.appbin_source.contains(source) {
                    package_info.appbin_flag = true;
                }
            }
        }
        Ok(())
    }

    fn add_one_package_installing(&mut self, pkg_name: &str, depth: u8, ebin_flag: bool,
                                  packages: &mut HashMap<String, InstalledPackageInfo>,
                                  missing_names: &mut Vec<String>) {
        log::debug!("Attempting to add package '{}' (depth: {}, ebin_flag: {})", pkg_name, depth, ebin_flag);
        match self.map_pkgname2packages(pkg_name) {
            Ok(unfiltered_packages) => {
                if unfiltered_packages.is_empty() {
                    log::debug!("No packages found for name '{}' by map_pkgname2packages.", pkg_name);
                    missing_names.push(pkg_name.to_string());
                    return;
                }

                let arch_filtered_packages = self.filter_packages_by_arch(unfiltered_packages);
                if arch_filtered_packages.is_empty() {
                    log::debug!("No packages for name '{}' matched current architecture.", pkg_name);
                    missing_names.push(format!("{} (no matching arch)", pkg_name));
                    return;
                }

                if let Some(package_to_add) = version::select_highest_version(arch_filtered_packages) {
                    if packages.contains_key(&package_to_add.pkgkey) {
                        log::debug!("Package {} already in target map, not re-adding.", package_to_add.pkgkey);
                        return;
                    }
                    log::info!("Selected package {} version {} for {}", package_to_add.pkgkey, package_to_add.version, pkg_name);
                    packages.insert(
                        package_to_add.pkgkey.clone(),
                        InstalledPackageInfo::new(depth, ebin_flag, package_to_add.arch.clone()),
                    );
                } else {
                    log::warn!("No suitable package found for '{}' after arch filtering and version selection.", pkg_name);
                    missing_names.push(format!("{} (version selection failed)", pkg_name));
                }
            },
            Err(e) => {
                log::warn!("Error mapping package name '{}': {}", pkg_name, e);
                missing_names.push(pkg_name.to_string());
            }
        }
    }

    /// convert user provided @capabilities to exact packages hash
    fn resolve_single_capability_item(
        &mut self,
        capability_or_pkg_name: &str,
        packages_map: &mut HashMap<String, InstalledPackageInfo>,
        depth: u8,
        ebin_flag: bool,
        missing_items_log: &mut Vec<String>,
    ) -> Result<bool> { // Returns true if capability is satisfied, false otherwise
        log::trace!(
            "Resolving single capability item: '{}', depth: {}, ebin_flag: {}",
            capability_or_pkg_name,
            depth,
            ebin_flag
        );

        let provider_pkgnames_result = crate::mmio::map_provide2pkgnames(capability_or_pkg_name);

        let provider_list_to_check: Vec<String> = match provider_pkgnames_result {
            Ok(names) if !names.is_empty() => names,
            _ => vec![capability_or_pkg_name.to_string()], // Treat as direct name if no providers or error
        };

        // Policy Step 1: Check if any provider is already satisfied/selected.
        for provider_name in &provider_list_to_check {
            match self.map_pkgname2packages(provider_name) {
                Ok(candidate_packages) => {
                    if candidate_packages.is_empty() { continue; }
                    let arch_filtered = self.filter_packages_by_arch(candidate_packages);
                    if arch_filtered.is_empty() { continue; }
                    if let Some(selected_pkg_candidate) = version::select_highest_version(arch_filtered) {
                        if packages_map.contains_key(&selected_pkg_candidate.pkgkey) ||
                            self.installed_packages.contains_key(&selected_pkg_candidate.pkgkey) {
                            log::debug!(
                                "Capability '{}' already satisfied by existing package '{}' (provider: '{}')",
                                capability_or_pkg_name,
                                selected_pkg_candidate.pkgkey,
                                provider_name
                            );
                            return Ok(true);
                        }
                    }
                }
                Err(_) => { /* Failed to map provider_name, try next */ }
            }
        }

        // Policy Step 2: If not satisfied by an existing package, try to install the first provider.
        if !provider_list_to_check.is_empty() {
            let first_provider_to_try = &provider_list_to_check[0];
            log::debug!(
                "Capability '{}': No existing package found. Attempting to install first provider: '{}'",
                capability_or_pkg_name,
                first_provider_to_try
            );

            let initial_missing_count = missing_items_log.len();
            self.add_one_package_installing(
                first_provider_to_try,
                depth,
                ebin_flag,
                packages_map,
                missing_items_log,
            );

            if missing_items_log.len() == initial_missing_count {
                log::debug!(
                    "Capability '{}' satisfied by installing first provider '{}'",
                    capability_or_pkg_name,
                    first_provider_to_try
                );
                return Ok(true); // Successfully added/found via add_one_package_installing
            } else {
                 log::debug!(
                    "Capability '{}': First provider '{}' failed to install or resolve. Missing log: {:?}",
                    capability_or_pkg_name, first_provider_to_try, missing_items_log.last()
                );
                // If add_one_package_installing added to missing_items_log for this *specific* first_provider_to_try,
                // we don't want to *also* add a generic message for capability_or_pkg_name unless all providers failed.
                // The current logic will fall through to the generic message if this first provider fails.
            }
        }

        // If still not satisfied:
        log::warn!(
            "Capability '{}' could not be satisfied by any means (checked existing or tried first provider '{}').",
            capability_or_pkg_name,
            provider_list_to_check.get(0).map_or("N/A", |s| s.as_str())
        );
        missing_items_log.push(format!(
            "{} (could not be resolved/installed, provider list: {:?})",
            capability_or_pkg_name,
            provider_list_to_check
        ));
        Ok(false)
    }

    // Refactored resolve_package_info
    pub fn resolve_package_info(&mut self, capabilities_or_pkg_names: Vec<String>) -> HashMap<String, InstalledPackageInfo> {
        log::debug!("Resolving package info for {} initial capabilities/package names", capabilities_or_pkg_names.len());
        log::trace!("Initial items: {:?}", capabilities_or_pkg_names);
        let mut packages_map = HashMap::new();
        let mut missing_items_log = Vec::new();
        let depth = 0;
        let ebin_flag = true; // For explicit user requests

        for cap_or_name in capabilities_or_pkg_names {
            let _ = self.resolve_single_capability_item(
                &cap_or_name,
                &mut packages_map,
                depth,
                true,
                &mut missing_items_log,
            ); // We check missing_items_log at the end, so direct result of call isn't critical here
        }

        if !missing_items_log.is_empty() {
            eprintln!("Error: The following packages/capabilities could not be resolved:");
            for item in &missing_items_log {
                eprintln!("  - {}", item);
            }
            if !config().common.ignore_missing {
                log::error!("Exiting due to missing packages/capabilities.");
                exit(1);
            }
        }
        packages_map
    }

    pub fn collect_essential_packages(&mut self, packages: &mut HashMap<String, InstalledPackageInfo>) -> Result<()> {
        log::debug!("Collecting essential packages");
        let mut missing_names = Vec::new();
        let essential_pkgnames = crate::mmio::get_essential_pkgnames()?;
        for essential_pkgname in &essential_pkgnames {
            self.add_one_package_installing(essential_pkgname.as_str(), 0, false, packages, &mut missing_names);
        }
        if !missing_names.is_empty() {
            println!("Missing packages: {:#?}", missing_names);
            if !config().common.ignore_missing {
                exit(1);
            }
        }

        Ok(())
    }

    pub fn collect_recursive_depends(&mut self,
        packages: &mut HashMap<String, InstalledPackageInfo>
    ) -> Result<()> {
        log::debug!("Starting recursive dependency collection for {} packages", packages.len());
        let mut depend_packages: HashMap<String, InstalledPackageInfo> = HashMap::new();
        let mut depth = 1;
        let channel_config = self.get_channel_config(config().common.env.clone())?;
        let repo_format = channel_config.format;

        self.collect_depends(&packages, &mut depend_packages, depth, repo_format)?;

        while !depend_packages.is_empty() {
            log::debug!("Found {} new dependencies at depth {}", depend_packages.len(), depth);
            packages.extend(depend_packages);
            depend_packages = HashMap::new();
            depth += 1;
            self.collect_depends(&packages, &mut depend_packages, depth, repo_format)?;
        }

        Ok(())
    }

    fn process_dependencies(
        &mut self,
        dependencies: &Vec<Dependency>,
        packages: &HashMap<String, InstalledPackageInfo>,
        depend_packages: &mut HashMap<String, InstalledPackageInfo>,
        depth: u8,
        missing_deps: &mut Vec<String>,
    ) -> Result<()> {
        log::trace!("Dependencies: {:?}", dependencies);
        for dep in dependencies {
            let pkgkey = crate::mmio::format_pkgkey(&dep.pkgname, &dep.ca_hash);

            if !packages.contains_key(&pkgkey) &&
                !depend_packages.contains_key(&pkgkey) {
                match self.load_package_info(&pkgkey) {
                    Ok(package) => {
                        depend_packages.insert(
                            pkgkey,
                            InstalledPackageInfo::new(depth, false, package.arch.clone()),
                        );
                    }
                    Err(_) => {
                        missing_deps.push(pkgkey);
                    }
                }
            }
        }

        Ok(())
    }

    fn process_requirements(
        &mut self,
        requirements_strings: &Vec<String>,
        _current_iteration_packages: &HashMap<String, InstalledPackageInfo>, // Parameter kept for signature compatibility if collect_depends passes it, but new logic uses depend_packages as the main map.
        depend_packages: &mut HashMap<String, InstalledPackageInfo>, // This is the main map to check and add to.
        depth: u8,
        repo_format: PackageFormat,
        missing_deps_log: &mut Vec<String>,
    ) -> Result<()> {
        log::trace!("Processing requirements at depth {}: {:?}", depth, requirements_strings);
        for req_string in requirements_strings {
            let and_groups = match parse_requires(repo_format, req_string) {
                Ok(groups) => groups,
                Err(e) => {
                    missing_deps_log.push(format!("Failed to parse requirement string '{}': {}", req_string, e));
                    continue; // Move to the next requirement string
                }
            };

            for or_group in and_groups { // Each or_group is Vec<Dependency>, representing an OR choice
                if or_group.is_empty() { continue; }

                let mut or_group_satisfied = false;
                for dep_capability_info in &or_group { // dep_capability_info.capability is the string
                    if self.resolve_single_capability_item(
                        &dep_capability_info.capability,
                        depend_packages, // Pass the main accumulating map
                        depth,
                        false,           // ebin_flag is false for dependencies
                        missing_deps_log,
                    )? {
                        or_group_satisfied = true;
                        log::debug!("OR group satisfied by capability '{}'", dep_capability_info.capability);
                        break; // This OR group is satisfied, move to the next AND group
                    }
                }

                if !or_group_satisfied {
                    let group_caps: Vec<&str> = or_group.iter().map(|d| d.capability.as_str()).collect();
                    log::warn!("Failed to satisfy OR dependency group: {:?}", group_caps);
                    missing_deps_log.push(format!("Dependency group not satisfied: {:?}", group_caps));
                    // If one AND group (which is a collection of OR choices) fails, the whole requirement might be considered failed.
                    // Depending on strictness, we could 'return Ok(())' or 'return Err(...)' or just continue to collect all failures.
                    // Current logic: collect all failures and let handle_missing_dependencies deal with it.
                }
            }
        }
        Ok(())
    }

    // Filter packages based on architecture that matches config().common.arch
    // This is to handle situation when both x86_64 and i686 packages are available with same
    // pkgname and version, e.g. fedora fcitx5-qt 5.1.9-3.fc42 has 2 packages for x86_64/i686.
    fn filter_packages_by_arch(&self, packages: Vec<Package>) -> Vec<Package> {
        let target_arch = crate::models::config().common.arch.as_str();

        // If there are no packages with matching architecture, return all packages
        let arch_packages: Vec<Package> = packages.iter()
            .filter(|pkg| !pkg.arch.is_empty() && pkg.arch == target_arch)
            .cloned()
            .collect();

        log::trace!(
            "Filtered packages by architecture '{}': {} out of {} packages matched",
            target_arch,
            arch_packages.len(),
            packages.len()
        );

        if !arch_packages.is_empty() {
            arch_packages
        } else {
            packages
        }

    }

    fn collect_depends(
        &mut self,
        packages: &HashMap<String, InstalledPackageInfo>,
        depend_packages: &mut HashMap<String, InstalledPackageInfo>,
        depth: u8,
        repo_format: PackageFormat,
    ) -> Result<()> {
        log::debug!("Collecting dependencies for {} packages at depth {}", packages.len(), depth);
        let mut missing_deps = Vec::new();
        for pkgkey in packages.keys() {
            let pkg_info = self.load_package_info(pkgkey)?;

            if !pkg_info.requires_pre.is_empty() {
                self.process_requirements(
                    &pkg_info.requires_pre,
                    packages,
                    depend_packages,
                    depth,
                    repo_format,
                    &mut missing_deps,
                )?;
            }
            if !pkg_info.depends.is_empty() {
                self.process_dependencies(
                    &pkg_info.depends,
                    packages,
                    depend_packages,
                    depth,
                    &mut missing_deps,
                )?;
            } else if !pkg_info.requires.is_empty() { // This 'else if' is important
                self.process_requirements(
                    &pkg_info.requires,
                    packages,
                    depend_packages,
                    depth,
                    repo_format,
                    &mut missing_deps,
                )?;
            }
        }

        self.handle_missing_dependencies(missing_deps)?;
        Ok(())
    }

    fn handle_missing_dependencies(&self, missing_deps: Vec<String>) -> Result<()> {
        if !missing_deps.is_empty() {
            log::error!("Missing dependencies:");
            for dep in missing_deps {
                log::error!("  - {}", dep);
            }
            eprintln!("Error: Missing dependencies. Check log for details.");
            exit(1);
        }
        Ok(())
    }


    pub fn map_pkgname2packages(&mut self, pkgname: &str) -> Result<Vec<Package>> {
        match crate::mmio::map_pkgname2packages(pkgname) {
            Ok(packages_list) => {
                for package in &packages_list {
                    // cache for later references
                    log::trace!("Caching package: {}", package.pkgkey);
                    self.pkgkey2package.insert(package.pkgkey.clone(), Arc::new(package.clone()));
                }
                return Ok(packages_list);
            },
            Err(e) => Err(e)
        }
    }

    pub fn map_pkgline2package(&mut self, pkgline: &str) -> Result<Arc<Package>> {
        // Check cache first
        if let Some(package) = self.pkgline2package.get(pkgline) {
            log::trace!("Found cached package info for pkgline '{}'", pkgline);
            return Ok(Arc::clone(package));
        }

        // Load from mmio function
        match crate::mmio::map_pkgline2package(pkgline) {
            Ok(package) => {
                log::trace!("Caching package from pkgline: {}", pkgline);
                let arc_package = Arc::new(package);
                self.pkgline2package.insert(pkgline.to_string(), Arc::clone(&arc_package));
                Ok(arc_package)
            },
            Err(e) => Err(e)
        }
    }

    pub fn load_package_info(&mut self, pkgkey: &str) -> Result<Arc<Package>> {
        log::trace!("Loading package info for '{}'", pkgkey);
        // Try to find by pkgkey first
        if let Some(package) = self.pkgkey2package.get(pkgkey) {
            log::trace!("Found cached package info for '{}'", pkgkey);
            return Ok(Arc::clone(package));
        }

        // Extract package name from pkgkey and try to load all packages with that name
        log::debug!("Package '{}' not in cache, extracting package name", pkgkey);
        let pkgname = crate::mmio::pkgkey2pkgname(pkgkey)?;
        self.map_pkgname2packages(&pkgname)?;

        // Try to find the package again after loading
        if let Some(package) = self.pkgkey2package.get(pkgkey) {
            log::debug!("Found package '{}' after loading", pkgkey);
            return Ok(Arc::clone(package));
        }

        log::warn!("Package not found: {}", pkgkey);
        Err(eyre::eyre!("Package not found: {}", pkgkey))
    }

}
