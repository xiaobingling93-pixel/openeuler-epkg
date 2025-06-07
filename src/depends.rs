use std::process::exit;
use std::collections::{HashMap};

use std::sync::Arc;
use color_eyre::Result;
use color_eyre::eyre;
use log;
use crate::models::*;

use crate::parse_requires::*;
use crate::package;
use crate::version;

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

    fn add_one_package_installing(&mut self, pkg_name: &str, depth: u16, ebin_flag: bool,
                                  packages: &mut HashMap<String, InstalledPackageInfo>,
                                  missing_names: &mut Vec<String>) -> Option<String> {
        log::debug!("Attempting to add package '{}' (depth: {}, ebin_flag: {})", pkg_name, depth, ebin_flag);
        match self.map_pkgname2packages(pkg_name) {
            Ok(unfiltered_packages) => {
                if unfiltered_packages.is_empty() {
                    log::debug!("No packages found for name '{}' by map_pkgname2packages.", pkg_name);
                    missing_names.push(pkg_name.to_string());
                    return None;
                }

                let arch_filtered_packages = self.filter_packages_by_arch(unfiltered_packages);
                if arch_filtered_packages.is_empty() {
                    log::debug!("No packages for name '{}' matched current architecture.", pkg_name);
                    missing_names.push(format!("{} (no matching arch)", pkg_name));
                    return None;
                }

                if let Some(package_to_add) = version::select_highest_version(arch_filtered_packages) {
                    if packages.contains_key(&package_to_add.pkgkey) {
                        log::debug!("Package {} already in target map, not re-adding.", package_to_add.pkgkey);
                        return Some(package_to_add.pkgkey.clone()); // Already there, effectively 'added' for satisfaction purposes
                    }
                    log::info!("Selected package {} version {} for {}", package_to_add.pkgkey, package_to_add.version, pkg_name);
                    packages.insert(
                        package_to_add.pkgkey.clone(),
                        crate::models::InstalledPackageInfo::new(
                            String::new(), // pkgline will be filled later in installation
                            package_to_add.arch.clone(),   // arch
                            depth,                         // depend_depth
                            ebin_flag                      // appbin_flag
                        ),
                    );
                    return Some(package_to_add.pkgkey.clone());
                } else {
                    log::warn!("No suitable package found for '{}' after arch filtering and version selection.", pkg_name);
                    missing_names.push(format!("{} (version selection failed)", pkg_name));
                    return None;
                }
            },
            Err(e) => {
                log::warn!("Error mapping package name '{}': {}", pkg_name, e);
                missing_names.push(pkg_name.to_string());
                return None;
            }
        }
    }

    /// convert user provided @capabilities to exact packages hash
    fn resolve_single_capability_item(
        &mut self,
        capability_or_pkg_name: &str,
        packages_map: &mut HashMap<String, InstalledPackageInfo>,
        depth: u16,
        ebin_flag: bool,
        missing_items_log: &mut Vec<String>,
    ) -> Result<Option<String>> { // Returns Some(pkgkey) if satisfied, None otherwise
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
                        let pkgkey_to_check = &selected_pkg_candidate.pkgkey;
                        let mut satisfied_by_packages_map = false;
                        let mut satisfied_by_installed_pkgs = false;

                        if packages_map.contains_key(pkgkey_to_check) {
                            satisfied_by_packages_map = true;
                        }
                        // Check self.installed_packages only if not already in packages_map, to avoid redundant work if it was already cloned.
                        if !satisfied_by_packages_map && self.installed_packages.contains_key(pkgkey_to_check) {
                            satisfied_by_installed_pkgs = true;
                        }

                        if satisfied_by_packages_map || satisfied_by_installed_pkgs {
                            if satisfied_by_installed_pkgs && !satisfied_by_packages_map {
                                // If satisfied by self.installed_packages and not yet in packages_map,
                                // clone it into packages_map for this session's rdepends tracking.
                                if let Some(installed_info) = self.installed_packages.get(pkgkey_to_check) {
                                    let mut session_info = installed_info.clone();
                                    // rdepends for already installed packages are tracked for this session.
                                    // If there's a persistent rdepends strategy later, this might change.
                                    session_info.rdepends = Vec::new();
                                    packages_map.insert(pkgkey_to_check.clone(), session_info);
                                } else {
                                    // Should not happen due to contains_key check, but log if it does.
                                    log::error!("INTERNAL ERROR: pkgkey '{}' not found in self.installed_packages after contains_key check.", pkgkey_to_check);
                                }
                            }
                            log::debug!(
                                "Capability '{}' already satisfied by package '{}' (provider: '{}')",
                                capability_or_pkg_name,
                                pkgkey_to_check,
                                provider_name
                            );
                            return Ok(Some(pkgkey_to_check.clone()));
                        }
                    }
                }
                Err(e) => {
                    log::trace!("Error mapping provider name '{}' to packages: {}. Skipping provider.", provider_name, e);
                }
            }
        }

        // Policy Step 2: If not satisfied by an existing package, try to install the first provider.
        if !provider_list_to_check.is_empty() {
            let first_provider_to_try = &provider_list_to_check[0];
            log::debug!(
                "Capability '{}': No existing package found or suitable. Attempting to install first provider: '{}'",
                capability_or_pkg_name,
                first_provider_to_try
            );

            // `add_one_package_installing` returns Some(pkgkey) if it successfully adds the package
            // or if the package (with the correct version/arch) is already in `packages_map`.
            // It returns None if it fails and adds to `missing_items_log` for that specific provider.
            if let Some(added_pkgkey) = self.add_one_package_installing(
                first_provider_to_try,
                depth.into(),
                ebin_flag,
                packages_map, // This is the map it adds to or checks against
                missing_items_log, // This is the log it appends to on failure for this provider
            ) {
                log::debug!(
                    "Capability '{}' satisfied by installing/finding first provider '{}' (resolved to pkgkey '{}')",
                    capability_or_pkg_name,
                    first_provider_to_try,
                    added_pkgkey
                );
                return Ok(Some(added_pkgkey));
            } else {
                // add_one_package_installing returned None, meaning it failed for first_provider_to_try
                // and should have added an entry to missing_items_log for it.
                log::debug!(
                    "Capability '{}': First provider '{}' failed to install or resolve. Check missing_items_log.",
                    capability_or_pkg_name, first_provider_to_try
                );
            }
        } else {
            log::debug!("Capability '{}': No providers found in provider_list_to_check.", capability_or_pkg_name);
        }

        // If Policy Step 1 and Policy Step 2 (for the first provider) both failed to satisfy:
        // The original code had a fall-through to a generic missing_items_log.push here.
        // This is correct. If we reach here, the capability is not satisfied.
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
        Ok(None)
    }

    // Refactored resolve_package_info
    pub fn resolve_package_info(&mut self, capabilities_or_pkg_names: Vec<String>) -> HashMap<String, InstalledPackageInfo> {
        log::debug!("Resolving package info for {} initial capabilities/package names", capabilities_or_pkg_names.len());
        log::trace!("Initial items: {:?}", capabilities_or_pkg_names);
        let mut packages_map = HashMap::new();
        let mut missing_items_log = Vec::new();
        let depth = 0;
        let ebin_flag_for_explicit_req = true; // For explicit user requests

        for cap_or_name in capabilities_or_pkg_names {
            let _ = self.resolve_single_capability_item(
                &cap_or_name,
                &mut packages_map,
                depth,
                ebin_flag_for_explicit_req,
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

    pub fn collect_recursive_depends(
        &mut self,
        initial_packages: &HashMap<String, InstalledPackageInfo>,
        repo_format: PackageFormat,
    ) -> Result<HashMap<String, InstalledPackageInfo>> {
        log::info!(
            "Starting recursive dependency collection for {} initial packages. Repo format: {:?}",
            initial_packages.len(),
            repo_format
        );

        let mut all_collected_deps: HashMap<String, InstalledPackageInfo> = HashMap::new();
        let mut current_layer_to_process: HashMap<String, InstalledPackageInfo> = initial_packages.clone();
        let mut depth: u16 = 1;

        while !current_layer_to_process.is_empty() {
            log::info!(
                "[Depth {}] Processing {} packages in current_layer. Total collected so far: {}",
                depth,
                current_layer_to_process.len(),
                all_collected_deps.len()
            );

            let mut deps_found_this_layer: HashMap<String, InstalledPackageInfo> = HashMap::new();
            self.collect_depends(
                &current_layer_to_process,
                &mut deps_found_this_layer, // collect_depends populates this with direct deps of current_layer
                depth,
                repo_format,
            )?;

            let mut next_layer_to_process: HashMap<String, InstalledPackageInfo> = HashMap::new();
            for (pkgkey, pkg_info) in deps_found_this_layer.iter() {
                let in_all_collected = all_collected_deps.contains_key(pkgkey);
                let in_initial = initial_packages.contains_key(pkgkey);

                if !in_all_collected && !in_initial {
                    // Add to next_layer only if it's not something we've already fully processed or was an initial package
                    log::info!("[Depth {}] Adding NEW dependency to next_layer_to_process: {} ({}). Not in all_collected_deps ({}), Not in initial_packages ({}).",
                             depth, pkg_info.pkgline, pkgkey, !in_all_collected, !in_initial);
                    next_layer_to_process.insert(pkgkey.clone(), pkg_info.clone());
                    all_collected_deps.insert(pkgkey.clone(), pkg_info.clone()); // Also add to our master list
                } else {
                    log::debug!("[Depth {}] Dependency {} ({}) already processed or initial. In all_collected_deps: {}, In initial_packages: {}. Not adding to next_layer.",
                              depth, pkg_info.pkgline, pkgkey, in_all_collected, in_initial);
                }
            }

            log::info!(
                "[Depth {}] Found {} new, unique dependencies for the next layer.",
                depth,
                next_layer_to_process.len()
            );

            current_layer_to_process = next_layer_to_process;
            depth += 1;

            if depth > 50 { // Safety break
                log::error!(
                    "[Depth {}] Exceeded maximum recursion depth (50). Breaking loop.",
                    depth
                );
                log::error!("Total collected dependencies so far: {:#?}", all_collected_deps.keys());
                log::error!("Last 'current_layer_to_process' (would have been next): {:#?}", current_layer_to_process.keys());
                // Depending on desired behavior, you might return an error or the partial list.
                // For now, returning the partial list.
                break;
            }
        }

        log::info!("Recursive dependency collection finished. Total {} dependencies collected.", all_collected_deps.len());
        Ok(all_collected_deps)
    }

    fn process_dependencies(
        &mut self,
        dependencies: &Vec<Dependency>,
        packages: &HashMap<String, InstalledPackageInfo>,
        depend_packages: &mut HashMap<String, InstalledPackageInfo>,
        depth: u16,
        missing_deps: &mut Vec<String>,
    ) -> Result<()> {
        log::trace!("Dependencies: {:?}", dependencies);
        for dep in dependencies {
            let pkgkey = package::format_pkgkey(&dep.pkgname, &dep.ca_hash);

            if !packages.contains_key(&pkgkey) &&
                !depend_packages.contains_key(&pkgkey) {
                match self.load_package_info(&pkgkey) {
                    Ok(package) => {
                        depend_packages.insert(
                            pkgkey,
                            crate::models::InstalledPackageInfo::new(
                                String::new(), // pkgline will be filled later in installation
                                package.arch.clone(),
                                depth,
                                false),
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
        requiring_pkgkey: &str, // The package that has these requirements
        requirements_strings: &Vec<String>,
        _current_iteration_packages: &HashMap<String, InstalledPackageInfo>,
        depend_packages: &mut HashMap<String, InstalledPackageInfo>, // This is the main map to check and add to.
        depth: u16,
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
                    match self.resolve_single_capability_item(
                        &dep_capability_info.capability,
                        depend_packages, // Pass the main accumulating map
                        depth,
                        false,           // ebin_flag is false for dependencies
                        missing_deps_log,
                    )? {
                        Some(satisfied_by_pkgkey_b) => {
                            // Capability satisfied by pkgkey_b. Update its rdepends.
                            if let Some(info_b) = depend_packages.get_mut(&satisfied_by_pkgkey_b) {
                                if !info_b.rdepends.contains(&requiring_pkgkey.to_string()) {
                                    info_b.rdepends.push(requiring_pkgkey.to_string());
                                }
                            } else {
                                // This case should ideally not happen if resolve_single_capability_item guarantees the key is in depend_packages upon Some return.
                                // Or, if satisfied_by_pkgkey_b was from self.installed_packages, we'd need to update it there (more complex).
                                // For now, assume depend_packages is the primary target for new/selected dependencies.
                                log::warn!("Could not find {} in depend_packages to update rdepends for {}", satisfied_by_pkgkey_b, requiring_pkgkey);
                            }
                            or_group_satisfied = true;
                            log::debug!(
                                "OR group for '{}' satisfied by capability '{}' (via pkgkey '{}')",
                                requiring_pkgkey, dep_capability_info.capability, satisfied_by_pkgkey_b
                            );
                            break; // This OR group is satisfied, move to the next AND group
                        }
                        None => { /* Current capability in OR group not satisfied, try next */ }
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

    pub fn collect_depends(
        &mut self,
        current_layer_packages: &HashMap<String, InstalledPackageInfo>,
        depend_packages: &mut HashMap<String, InstalledPackageInfo>,
        depth: u16,
        repo_format: PackageFormat,
    ) -> Result<()> {
        log::info!("[Depth {}] Enter collect_depends for {} packages. Repo format: {:?}", depth, current_layer_packages.len(), repo_format);
        let mut missing_deps = Vec::new();
        for (requiring_pkgkey, _package_info) in current_layer_packages.iter() {
            log::info!("[Depth {}] Analyzing dependencies for package: {}", depth, requiring_pkgkey);
            let pkg_info = self.load_package_info(requiring_pkgkey)?;

            if !pkg_info.requires_pre.is_empty() {
                self.process_requirements(
                    requiring_pkgkey, // Pass the key of the package whose dependencies are being processed
                    &pkg_info.requires_pre,
                    current_layer_packages,
                    depend_packages,
                    depth,
                    repo_format,
                    &mut missing_deps,
                )?;
            }
            if !pkg_info.depends.is_empty() {
                self.process_dependencies(
                    &pkg_info.depends,
                    current_layer_packages,
                    depend_packages,
                    depth,
                    &mut missing_deps,
                )?;
            } else if !pkg_info.requires.is_empty() { // This 'else if' is important
                self.process_requirements(
                    requiring_pkgkey, // Pass the key of the package whose dependencies are being processed
                    &pkg_info.requires,
                    current_layer_packages,
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
        let pkgname = package::pkgkey2pkgname(pkgkey)?;
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
