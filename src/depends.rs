use std::process::exit;
use std::collections::{HashMap, HashSet};

use std::sync::Arc;
use color_eyre::Result;
use color_eyre::eyre;
use log;
use crate::models::*;

use crate::parse_requires::*;
use crate::package;
use crate::version;

/*
 * Debian Multi-Arch and Architecture Suffix Rules
 * ===============================================
 *
 * Debian packages can specify architecture-specific dependencies using suffixes:
 *
 * 1. `:any` Suffix Rules:
 *    - Can ONLY be used with packages that have Multi-Arch: allowed or Multi-Arch: foreign
 *    - Means the dependency can be satisfied by ANY architecture version of the package
 *    - Examples: perl:any, python3:any
 *
 * 2. Multi-Arch Field Values and Their Meanings:
 *    - Multi-Arch: allowed
 *      * Package can be installed for multiple architectures simultaneously
 *      * Different architecture versions can coexist
 *      * CAN satisfy :any dependencies
 *      * Example: interpreters like perl, python3
 *
 *    - Multi-Arch: foreign
 *      * Package can satisfy dependencies of any architecture
 *      * Only one architecture version can be installed at a time
 *      * CAN satisfy :any dependencies
 *      * Example: architecture-independent tools
 *
 *    - Multi-Arch: same
 *      * Must be same architecture as the package that depends on it
 *      * CANNOT satisfy :any dependencies
 *      * Example: shared libraries that must match requestor's architecture
 *
 *    - No Multi-Arch field (or Multi-Arch: no)
 *      * Traditional behavior - architecture specific
 *      * CANNOT satisfy :any dependencies
 *      * Must match the architecture of the requesting package
 *
 * 3. Specific Architecture Suffixes:
 *    - `:amd64`, `:arm64`, etc.
 *    - Forces dependency to specific architecture regardless of Multi-Arch
 *    - Used for cross-compilation or specific architecture requirements
 *
 * 4. No Architecture Suffix:
 *    - Default behavior - architecture specific matching
 *    - Dependency must match the architecture of the requesting package
 *
 * 5. Implementation Rules in this Code:
 *    - For `:any`: Only allow packages with Multi-Arch: allowed/foreign
 *    - For specific arch (`:amd64`): Filter to that architecture only
 *    - For no suffix: Use default same-architecture filtering
 *    - Fallback: If no Multi-Arch packages found for :any, fall back to same-arch
 *
 * References:
 * - https://wiki.debian.org/Multiarch/HOWTO
 * - https://www.debian.org/doc/debian-policy/ch-relationships.html#architecture-restrictions
 */

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
        return self.add_one_package_installing_with_arch_spec(pkg_name, None, depth, ebin_flag, packages, missing_names);
    }

    fn add_one_package_installing_with_arch_spec(&mut self, pkg_name: &str, arch_spec: Option<&str>, depth: u16, ebin_flag: bool,
                                  packages: &mut HashMap<String, InstalledPackageInfo>,
                                  missing_names: &mut Vec<String>) -> Option<String> {
        log::debug!("Attempting to add package '{}' with arch_spec {:?} (depth: {}, ebin_flag: {})", pkg_name, arch_spec, depth, ebin_flag);
        match self.map_pkgname2packages(pkg_name) {
            Ok(unfiltered_packages) => {
                if unfiltered_packages.is_empty() {
                    log::debug!("No packages found for name '{}' by map_pkgname2packages.", pkg_name);
                    missing_names.push(pkg_name.to_string());
                    return None;
                }

                let arch_filtered_packages = self.filter_packages_by_arch_spec(unfiltered_packages, arch_spec);
                if arch_filtered_packages.is_empty() {
                    log::debug!("No packages for name '{}' matched architecture specification '{:?}'.", pkg_name, arch_spec);
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

    /// Parse capability name and extract architecture specification
    /// Returns (base_capability, architecture_spec) where architecture_spec is:
    /// - Some("any") for `:any` suffix
    /// - Some(arch) for specific architecture like `:amd64`
    /// - None for no architecture specification
    fn parse_capability_architecture(&self, capability: &str, format: PackageFormat) -> (String, Option<String>) {
        // Handle based on package format
        if format == PackageFormat::Deb {
                if let Some(colon_pos) = capability.rfind(':') {
                    let base_capability = capability[..colon_pos].to_string();
                    let arch_spec = capability[colon_pos + 1..].to_string();

                    return (base_capability, Some(arch_spec))
                }
        }
        // Other distros do not encode arch in require name.
        // Alpine uses prefixes like: so:, cmd:, pc:, py3.XX:, ocaml4-intf:, dbus:, etc.
        // which are not related to arch.
        (capability.to_string(), None)
    }

    /// Filter packages based on architecture specification
    /// If arch_spec is "any", only allow packages with Multi-Arch: allowed/foreign (per Debian rules)
    /// If arch_spec is specific architecture, filter by that architecture
    /// If arch_spec is None, use default architecture filtering
    fn filter_packages_by_arch_spec(&self, packages: Vec<Package>, arch_spec: Option<&str>) -> Vec<Package> {
        match arch_spec {
            Some("any") => {
                // For :any dependencies, ONLY allow packages that support Multi-Arch: allowed or foreign
                // This is a strict requirement per Debian Multi-Arch specification
                let multiarch_packages: Vec<Package> = packages.iter()
                    .filter(|pkg| {
                        match &pkg.multi_arch {
                            Some(multi_arch) => {
                                let multi_arch_lower = multi_arch.to_lowercase();
                                // Only "allowed" and "foreign" can satisfy :any dependencies
                                // "same" and "no" cannot satisfy :any dependencies
                                multi_arch_lower == "allowed" || multi_arch_lower == "foreign"
                            }
                            None => {
                                // No Multi-Arch field means Multi-Arch: no (traditional behavior)
                                // Cannot satisfy :any dependency per Debian rules
                                false
                            }
                        }
                    })
                    .cloned()
                    .collect();

                if !multiarch_packages.is_empty() {
                    log::trace!(
                        "Filtered packages for :any specification: {} out of {} packages support Multi-Arch (allowed/foreign)",
                        multiarch_packages.len(),
                        packages.len()
                    );
                    multiarch_packages
                } else {
                    log::warn!(
                        "No packages found with Multi-Arch: allowed/foreign for :any dependency. This violates Debian Multi-Arch rules. Falling back to same-architecture packages as last resort."
                    );
                    // Fallback to default architecture filtering as last resort
                    // This is non-standard but provides graceful degradation
                    self.filter_packages_by_arch(packages)
                }
            }
            Some(specific_arch) => {
                // Filter by specific architecture (e.g., :amd64, :arm64)
                // This works regardless of Multi-Arch field value
                let arch_packages: Vec<Package> = packages.iter()
                    .filter(|pkg| !pkg.arch.is_empty() && pkg.arch == specific_arch)
                    .cloned()
                    .collect();

                log::trace!(
                    "Filtered packages by specific architecture '{}': {} out of {} packages matched",
                    specific_arch,
                    arch_packages.len(),
                    packages.len()
                );

                if !arch_packages.is_empty() {
                    arch_packages
                } else {
                    // If no packages match the specific architecture, return empty
                    // (don't fall back to other architectures for explicit arch requests)
                    packages
                }
            }
            None => {
                // No architecture suffix - use traditional same-architecture matching
                // This respects Multi-Arch: same behavior and traditional dependencies
                self.filter_packages_by_arch(packages)
            }
        }
    }

    /// Helper function to check if a package is already satisfied and handle session tracking
    /// Returns Some(pkgkey) if satisfied, None if not satisfied
    fn check_package_satisfaction(
        &mut self,
        pkgkey: &str,
        packages_map: &mut HashMap<String, InstalledPackageInfo>,
    ) -> Option<String> {
        let mut satisfied_by_packages_map = false;
        let mut satisfied_by_installed_pkgs = false;

        if packages_map.contains_key(pkgkey) {
            satisfied_by_packages_map = true;
        }
        // Check self.installed_packages only if not already in packages_map
        if !satisfied_by_packages_map && self.installed_packages.contains_key(pkgkey) {
            satisfied_by_installed_pkgs = true;
        }

        if satisfied_by_packages_map || satisfied_by_installed_pkgs {
            if satisfied_by_installed_pkgs && !satisfied_by_packages_map {
                // Clone from installed_packages into packages_map for session tracking
                if let Some(installed_info) = self.installed_packages.get(pkgkey) {
                    let mut session_info = installed_info.clone();
                    session_info.rdepends = Vec::new();
                    packages_map.insert(pkgkey.to_string(), session_info);
                } else {
                    log::error!("INTERNAL ERROR: pkgkey '{}' not found in self.installed_packages after contains_key check.", pkgkey);
                }
            }
            return Some(pkgkey.to_string());
        }
        None
    }

    /// Helper function to try resolving a package by name with architecture filtering
    /// Returns Some(pkgkey) if resolved and satisfied/installed, None otherwise
    fn try_resolve_package_by_name(
        &mut self,
        pkg_name: &str,
        arch_spec: Option<&str>,
        packages_map: &mut HashMap<String, InstalledPackageInfo>,
        depth: u16,
        ebin_flag: bool,
        missing_items_log: &mut Vec<String>,
        context: &str, // For logging context ("Direct package lookup", "Provider", etc.)
    ) -> Option<String> {
        match self.map_pkgname2packages(pkg_name) {
            Ok(candidate_packages) if !candidate_packages.is_empty() => {
                let arch_filtered = self.filter_packages_by_arch_spec(candidate_packages, arch_spec);
                if !arch_filtered.is_empty() {
                    if let Some(selected_pkg_candidate) = version::select_highest_version(arch_filtered) {
                        let pkgkey = &selected_pkg_candidate.pkgkey;

                        // First check if already satisfied
                        if let Some(satisfied_pkgkey) = self.check_package_satisfaction(pkgkey, packages_map) {
                            log::debug!(
                                "{}: '{}' already satisfied by existing package '{}'",
                                context,
                                pkg_name,
                                satisfied_pkgkey
                            );
                            return Some(satisfied_pkgkey);
                        }

                        // Try to install if not already satisfied
                        log::debug!("{}: found '{}', attempting to install", context, pkg_name);
                        if let Some(added_pkgkey) = self.add_one_package_installing_with_arch_spec(
                            pkg_name,
                            arch_spec,
                            depth.into(),
                            ebin_flag,
                            packages_map,
                            missing_items_log,
                        ) {
                            log::debug!(
                                "{}: successfully installed '{}' (resolved to pkgkey '{}')",
                                context,
                                pkg_name,
                                added_pkgkey
                            );
                            return Some(added_pkgkey);
                        }
                        log::debug!("{}: failed to install '{}'", context, pkg_name);
                    }
                }
            }
            Ok(_) => {
                log::debug!("{}: no packages found for '{}'", context, pkg_name);
            }
            Err(e) => {
                log::debug!("{}: error for '{}': {}", context, pkg_name, e);
            }
        }
        None
    }

    fn resolve_single_capability_item(
        &mut self,
        capability_or_pkg_name: &str,
        packages_map: &mut HashMap<String, InstalledPackageInfo>,
        depth: u16,
        ebin_flag: bool,
        missing_items_log: &mut Vec<String>,
        format: PackageFormat,
    ) -> Result<Option<String>> { // Returns Some(pkgkey) if satisfied, None otherwise
        log::trace!(
            "Resolving single capability item: '{}', depth: {}, ebin_flag: {}, format: {:?}",
            capability_or_pkg_name,
            depth,
            ebin_flag,
            format
        );

        // Parse capability name and architecture specification
        let (base_capability, arch_spec) = self.parse_capability_architecture(capability_or_pkg_name, format);
        let arch_spec_ref = arch_spec.as_deref();

        // Policy Step 0: First try to lookup the name as a direct package name
        // This ensures that when someone explicitly requests "groff-x11", we try to install
        // the actual "groff-x11" package rather than being satisfied by "groff" which provides "groff-x11"
        log::debug!("Attempting direct package name lookup for '{}'", base_capability);
        if let Some(pkgkey) = self.try_resolve_package_by_name(
            &base_capability,
            arch_spec_ref,
            packages_map,
            depth,
            ebin_flag,
            missing_items_log,
            "Direct package lookup",
        ) {
            return Ok(Some(pkgkey));
        }
        log::debug!(
            "Direct package lookup: failed to resolve '{}', falling back to provider lookup",
            base_capability
        );

        // If direct package lookup didn't work, fall back to provider-based logic
        let provider_pkgnames_result = crate::mmio::map_provide2pkgnames(&base_capability);

        let provider_list_to_check: Vec<String> = match provider_pkgnames_result {
            Ok(names) if !names.is_empty() => names,
            _ => vec![base_capability.clone()], // Treat as direct name if no providers or error
        };

        // Policy Step 1: Check if any provider is already satisfied/selected.
        for provider_name in &provider_list_to_check {
            if let Some(pkgkey) = self.try_resolve_package_by_name(
                provider_name,
                arch_spec_ref,
                packages_map,
                depth,
                ebin_flag,
                &mut Vec::new(), // Don't log provider lookup failures yet
                &format!("Provider '{}'", provider_name),
            ) {
                log::debug!(
                    "Capability '{}' satisfied by provider '{}' (resolved to pkgkey '{}')",
                    capability_or_pkg_name,
                    provider_name,
                    pkgkey
                );
                return Ok(Some(pkgkey));
            }
        }

        // Policy Step 2: If not satisfied by any provider, try to install the first provider.
        if !provider_list_to_check.is_empty() {
            let first_provider_to_try = &provider_list_to_check[0];
            log::debug!(
                "Capability '{}': No existing package found or suitable. Attempting to install first provider: '{}'",
                capability_or_pkg_name,
                first_provider_to_try
            );

            if let Some(added_pkgkey) = self.add_one_package_installing_with_arch_spec(
                first_provider_to_try,
                arch_spec_ref,
                depth.into(),
                ebin_flag,
                packages_map,
                missing_items_log,
            ) {
                log::debug!(
                    "Capability '{}' satisfied by installing/finding first provider '{}' (resolved to pkgkey '{}')",
                    capability_or_pkg_name,
                    first_provider_to_try,
                    added_pkgkey
                );
                return Ok(Some(added_pkgkey));
            } else {
                log::debug!(
                    "Capability '{}': First provider '{}' failed to install or resolve. Check missing_items_log.",
                    capability_or_pkg_name, first_provider_to_try
                );
            }
        } else {
            log::debug!("Capability '{}': No providers found in provider_list_to_check.", capability_or_pkg_name);
        }

        // If all steps failed to satisfy the capability
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

    // Implementation that accepts explicit format parameter
    pub fn resolve_package_info(&mut self, capabilities_or_pkg_names: Vec<String>, format: PackageFormat) -> HashMap<String, InstalledPackageInfo> {
        log::debug!("Resolving package info for {} initial capabilities/package names with format {:?}", capabilities_or_pkg_names.len(), format);
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
                format,
            ); // We check missing_items_log at the end, so direct result of call isn't critical here
        }

        // Handle any missing items if needed
        if !missing_items_log.is_empty() {
            eprintln!("Error: The following packages/capabilities could not be resolved:");
            for item in missing_items_log {
                eprintln!("  {}", item);
            }
            exit(1);
        }

        packages_map
    }

    // Backward compatibility implementation that uses a default format
    pub fn resolve_package_info_with_default(&mut self, capabilities_or_pkg_names: Vec<String>) -> HashMap<String, InstalledPackageInfo> {
        // For top-level package requests without a specific format context
        // Use Epkg as default which uses Debian-style parsing
        let default_repo_format = PackageFormat::Epkg;
        self.resolve_package_info(capabilities_or_pkg_names, default_repo_format)
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
        format: PackageFormat,
    ) -> Result<HashMap<String, InstalledPackageInfo>> {
        log::info!(
            "Starting recursive dependency collection for {} initial packages. Repo format: {:?}",
            initial_packages.len(),
            format
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
                format,
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
            // Try to get version and arch from dependency, fallback to defaults if empty
            let version = if dep.version.is_empty() { "unknown" } else { &dep.version };
            let arch = if dep.arch.is_empty() {
                &crate::models::config().common.arch
            } else {
                &dep.arch
            };
            let pkgkey = package::format_pkgkey(&dep.pkgname, version, arch);

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
        format: PackageFormat,
        missing_deps_log: &mut Vec<String>,
    ) -> Result<()> {
        log::trace!("Processing requirements at depth {}: {:?}", depth, requirements_strings);
        for req_string in requirements_strings {
            let and_groups = match parse_requires(format, req_string) {
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
                        format,          // Pass the format
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
        format: PackageFormat,
    ) -> Result<()> {
        log::info!("[Depth {}] Enter collect_depends for {} packages. Repo format: {:?}", depth, current_layer_packages.len(), format);
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
                    format,
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
                    format,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_capability_architecture() {
        let pm = PackageManager {
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
        };

        // Test Debian-style architecture specifications
        let (base, arch_spec) = pm.parse_capability_architecture("perl:any", PackageFormat::Deb);
        assert_eq!(base, "perl");
        assert_eq!(arch_spec, Some("any".to_string()));

        let (base, arch_spec) = pm.parse_capability_architecture("python3:amd64", PackageFormat::Deb);
        assert_eq!(base, "python3");
        assert_eq!(arch_spec, Some("amd64".to_string()));

        // Test no architecture specification
        let (base, arch_spec) = pm.parse_capability_architecture("gcc", PackageFormat::Deb);
        assert_eq!(base, "gcc");
        assert_eq!(arch_spec, None);

        // Test Alpine shared object capabilities (should NOT be parsed as arch specs)
        let (base, arch_spec) = pm.parse_capability_architecture("so:libc.musl-x86_64.so.1", PackageFormat::Apk);
        assert_eq!(base, "so:libc.musl-x86_64.so.1");
        assert_eq!(arch_spec, None);

        let (base, arch_spec) = pm.parse_capability_architecture("so:libzstd.so.1", PackageFormat::Apk);
        assert_eq!(base, "so:libzstd.so.1");
        assert_eq!(arch_spec, None);

        // Test Alpine command capabilities
        let (base, arch_spec) = pm.parse_capability_architecture("cmd:zstd", PackageFormat::Apk);
        assert_eq!(base, "cmd:zstd");
        assert_eq!(arch_spec, None);

        // Test Alpine pkg-config capabilities
        let (base, arch_spec) = pm.parse_capability_architecture("pc:libzstd", PackageFormat::Apk);
        assert_eq!(base, "pc:libzstd");
        assert_eq!(arch_spec, None);

        // Test Alpine Python module capabilities
        let (base, arch_spec) = pm.parse_capability_architecture("py3.12:setuptools", PackageFormat::Apk);
        assert_eq!(base, "py3.12:setuptools");
        assert_eq!(arch_spec, None);

        // Test Alpine ocaml capabilities
        let (base, arch_spec) = pm.parse_capability_architecture("ocaml4-intf:Csexp", PackageFormat::Apk);
        assert_eq!(base, "ocaml4-intf:Csexp");
        assert_eq!(arch_spec, None);

        // Test unknown colon usage in Debian should not be treated as arch spec
        let (base, arch_spec) = pm.parse_capability_architecture("lib:unknown", PackageFormat::Deb);
        assert_eq!(base, "lib:unknown");
        assert_eq!(arch_spec, None);

        // Test Debian package with multiple colons (should take the last one if it's a known arch)
        let (base, arch_spec) = pm.parse_capability_architecture("lib:test:any", PackageFormat::Deb);
        assert_eq!(base, "lib:test");
        assert_eq!(arch_spec, Some("any".to_string()));

        // Test Alpine package with multiple colons (should never split)
        let (base, arch_spec) = pm.parse_capability_architecture("so:lib:test.so.1", PackageFormat::Apk);
        assert_eq!(base, "so:lib:test.so.1");
        assert_eq!(arch_spec, None);
    }

    #[test]
    fn test_filter_packages_by_arch_spec_multiarch() {
        let pm = PackageManager {
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
        };

        // Create test packages covering all Multi-Arch scenarios
        let mut pkg_multiarch_allowed = Package {
            pkgname: "perl".to_string(),
            version: "5.32.0".to_string(),
            arch: "amd64".to_string(),
            multi_arch: Some("allowed".to_string()),  // CAN satisfy :any
            ..Default::default()
        };
        pkg_multiarch_allowed.pkgkey = "perl__5.32.0__amd64".to_string();

        let mut pkg_multiarch_foreign = Package {
            pkgname: "python3".to_string(),
            version: "3.9.0".to_string(),
            arch: "amd64".to_string(),
            multi_arch: Some("foreign".to_string()),  // CAN satisfy :any
            ..Default::default()
        };
        pkg_multiarch_foreign.pkgkey = "python3__3.9.0__amd64".to_string();

        let mut pkg_multiarch_same = Package {
            pkgname: "libc6".to_string(),
            version: "2.31".to_string(),
            arch: "amd64".to_string(),
            multi_arch: Some("same".to_string()),     // CANNOT satisfy :any
            ..Default::default()
        };
        pkg_multiarch_same.pkgkey = "libc6__2.31__amd64".to_string();

        let mut pkg_multiarch_no = Package {
            pkgname: "some-tool".to_string(),
            version: "1.0".to_string(),
            arch: "amd64".to_string(),
            multi_arch: Some("no".to_string()),       // CANNOT satisfy :any (explicit no)
            ..Default::default()
        };
        pkg_multiarch_no.pkgkey = "some-tool__1.0__amd64".to_string();

        let mut pkg_no_multiarch = Package {
            pkgname: "gcc".to_string(),
            version: "10.0.0".to_string(),
            arch: "amd64".to_string(),
            multi_arch: None,                         // CANNOT satisfy :any (implicit no)
            ..Default::default()
        };
        pkg_no_multiarch.pkgkey = "gcc__10.0.0__amd64".to_string();

        // Test case insensitivity - Multi-Arch fields can be in different cases
        let mut pkg_multiarch_allowed_uppercase = Package {
            pkgname: "python3-pip".to_string(),
            version: "20.0".to_string(),
            arch: "amd64".to_string(),
            multi_arch: Some("ALLOWED".to_string()),  // CAN satisfy :any (case insensitive)
            ..Default::default()
        };
        pkg_multiarch_allowed_uppercase.pkgkey = "python3-pip__20.0__amd64".to_string();

        let packages = vec![
            pkg_multiarch_allowed.clone(),
            pkg_multiarch_foreign.clone(),
            pkg_multiarch_same.clone(),
            pkg_multiarch_no.clone(),
            pkg_no_multiarch.clone(),
            pkg_multiarch_allowed_uppercase.clone(),
        ];

        // Test :any filtering - should only return packages with Multi-Arch: allowed or foreign
        // Excludes: same, no, None (missing field)
        let filtered = pm.filter_packages_by_arch_spec(packages.clone(), Some("any"));
        assert_eq!(filtered.len(), 3, "Only packages with Multi-Arch: allowed/foreign should satisfy :any");

        // Should include Multi-Arch: allowed
        assert!(filtered.iter().any(|p| p.pkgkey == pkg_multiarch_allowed.pkgkey),
                "Multi-Arch: allowed should satisfy :any");

        // Should include Multi-Arch: foreign
        assert!(filtered.iter().any(|p| p.pkgkey == pkg_multiarch_foreign.pkgkey),
                "Multi-Arch: foreign should satisfy :any");

        // Should include Multi-Arch: ALLOWED (case insensitive)
        assert!(filtered.iter().any(|p| p.pkgkey == pkg_multiarch_allowed_uppercase.pkgkey),
                "Multi-Arch: ALLOWED (uppercase) should satisfy :any");

        // Should NOT include Multi-Arch: same
        assert!(!filtered.iter().any(|p| p.pkgkey == pkg_multiarch_same.pkgkey),
                "Multi-Arch: same should NOT satisfy :any");

        // Should NOT include Multi-Arch: no
        assert!(!filtered.iter().any(|p| p.pkgkey == pkg_multiarch_no.pkgkey),
                "Multi-Arch: no should NOT satisfy :any");

        // Should NOT include packages with no Multi-Arch field
        assert!(!filtered.iter().any(|p| p.pkgkey == pkg_no_multiarch.pkgkey),
                "Packages without Multi-Arch field should NOT satisfy :any");

        // Test specific architecture filtering - works regardless of Multi-Arch
        let filtered = pm.filter_packages_by_arch_spec(packages.clone(), Some("amd64"));
        assert_eq!(filtered.len(), 6, "All packages should match amd64 architecture");

        // Test no architecture specification (should use default filtering)
        let filtered = pm.filter_packages_by_arch_spec(packages.clone(), None);
        assert_eq!(filtered.len(), 6, "All packages should match default arch filtering");
    }
}
