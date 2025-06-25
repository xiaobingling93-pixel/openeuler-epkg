use std::process::exit;
use std::collections::HashMap;
use std::collections::HashSet;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use color_eyre::Result;
use color_eyre::eyre;
use log;
use crate::models::{config, *};

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
    pub fn extend_appbin_by_source(&mut self, packages: &mut HashMap<String, InstalledPackageInfo>) -> Result<HashMap<String, InstalledPackageInfo>> {
        log::debug!("Setting ebin_exposure for {} packages based on source matching.", packages.len());

        let mut user_requested_sources = std::collections::HashSet::new();
        let mut packages_to_expose = HashMap::new();

        // First, collect all source package names from user-requested packages (depth 0)
        for (pkgkey, info) in packages.iter() {
            if info.ebin_exposure == true {
                // Ensure ebin_exposure is true for explicitly requested packages
                // This will be done in the next loop, but good to note.
                match self.load_package_info(pkgkey) {
                    Ok(pkg_details) => {
                        if let Some(source_name) = &pkg_details.source {
                            if !source_name.is_empty() {
                                user_requested_sources.insert(source_name.clone());
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("Failed to load package info for {}: {} during appbin source collection. Skipping.", pkgkey, e);
                        // Decide if this should be a hard error or just a warning.
                        // For now, it skips, potentially leading to incorrect ebin_exposures for related packages.
                    }
                }
            }
        }
        log::debug!("User-requested sources for ebin_exposure logic: {:?}", user_requested_sources);

        // Now, iterate again to set the ebin_exposure for all packages
        for (pkgkey, info) in packages.iter_mut() {
            if info.ebin_exposure == false {
                // For dependencies, check if their source matches any user-requested source
                match self.load_package_info(pkgkey) {
                    Ok(pkg_details) => {
                        if let Some(source_name) = &pkg_details.source {
                            if !source_name.is_empty() && user_requested_sources.contains(source_name) {
                                info.ebin_exposure = true;
                                packages_to_expose.insert(pkgkey.clone(), info.clone());
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("Failed to load package info for {}: {} during ebin_exposure setting. Defaulting ebin_exposure to false.", pkgkey, e);
                        info.ebin_exposure = false; // Default to false if info can't be loaded
                    }
                }
            }
        }

        Ok(packages_to_expose)
    }

    fn add_one_package_installing(&mut self, pkg_name: &str, depth: u16, ebin_flag: bool,
                                  packages: &mut HashMap<String, InstalledPackageInfo>,
                                  missing_names: &mut Vec<String>) -> Option<String> {
        return self.add_one_package_installing_with_arch_spec(pkg_name, None, depth, ebin_flag, packages, missing_names);
    }

    pub fn add_one_package_installing_with_arch_spec(&mut self, pkg_name: &str, arch_spec: Option<&str>, candidate_depth: u16, ebin_flag: bool,
                                  packages: &mut HashMap<String, InstalledPackageInfo>,
                                  missing_names: &mut Vec<String>) -> Option<String> {
        log::debug!("Attempting to add package '{}' (dependency of a depth {} package) with arch_spec {:?} (ebin_flag: {})", pkg_name, candidate_depth, arch_spec, ebin_flag);

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
                    if let Some(existing_info) = packages.get_mut(&package_to_add.pkgkey) {
                        log::debug!(
                            "Package {} already in target map. Current depth: {}. New path depth: {}. Updating if shorter.",
                            package_to_add.pkgkey, existing_info.depend_depth, candidate_depth
                        );
                        existing_info.depend_depth = std::cmp::min(existing_info.depend_depth, candidate_depth);
                        // Appbin flag is true if its effective depth is 0, false otherwise.
                        existing_info.ebin_exposure = ebin_flag;
                        log::trace!("Updated package {} in map. New depth: {}, New ebin_exposure: {}", package_to_add.pkgkey, existing_info.depend_depth, existing_info.ebin_exposure);
                        return Some(package_to_add.pkgkey.clone());
                    }

                    log::info!("Selected package {} version {} for {}. Adding with depth {}.", package_to_add.pkgkey, package_to_add.version, pkg_name, candidate_depth);
                    packages.insert(
                        package_to_add.pkgkey.clone(),
                        InstalledPackageInfo {
                            pkgline: String::new(),
                            arch: package_to_add.arch.clone(),
                            depend_depth: candidate_depth,
                            install_time: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
                            ebin_exposure: ebin_flag,
                            rdepends: Vec::new(),
                            depends: Vec::new(), // Will be populated by process_requirements
                            ebin_links: Vec::new(),
                        }
                    );
                    log::trace!("Added package {} to map. Depth: {}, Appbin_flag: {}", package_to_add.pkgkey, candidate_depth, ebin_flag);
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

        if !satisfied_by_packages_map && self.installed_packages.contains_key(pkgkey) {
            satisfied_by_installed_pkgs = true;
        }

        if satisfied_by_packages_map || satisfied_by_installed_pkgs {
            if satisfied_by_installed_pkgs && !satisfied_by_packages_map {
                // If the package is in self.installed_packages but not yet in the current session's packages_map,
                // add it to packages_map. This ensures its presence for subsequent operations in the current resolution.
                if let Some(installed_info) = self.installed_packages.get(pkgkey) {
                    let mut session_info = installed_info.clone();
                    // rdepends are specific to a resolution context and should be fresh.
                    session_info.rdepends = Vec::new();
                    // `depends` are inherent to the package, can be cloned.
                    // `depend_depth` and `ebin_exposure` from `installed_info` will be used as a base
                    // and then updated by the caller (try_resolve_package_by_name) based on the current path.
                    packages_map.insert(pkgkey.to_string(), session_info);
                    log::trace!("Added installed pkg '{}' to session map from self.installed_packages for current resolution context.", pkgkey);
                } else {
                    // This should not happen if contains_key was true.
                    log::error!("INTERNAL ERROR: pkgkey '{}' was reported in self.installed_packages but now not found for cloning.", pkgkey);
                    return None; // Cannot satisfy if we can't get its info.
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
        candidate_depth: u16,
        ebin_flag: bool,
        missing_items_log: &mut Vec<String>,
        context: &str, // For logging context ("Direct package lookup", "Provider", etc.)
    ) -> Option<String> {
        match self.map_pkgname2packages(pkg_name) {
            Ok(candidate_packages) if !candidate_packages.is_empty() => {
                let arch_filtered = self.filter_packages_by_arch_spec(candidate_packages, arch_spec);
                if !arch_filtered.is_empty() {
                    if let Some(selected_pkg_candidate) = version::select_highest_version(arch_filtered) {
                        let pkgkey_of_candidate = &selected_pkg_candidate.pkgkey;

                        // First, check if this specific candidate version is already satisfied (e.g., from self.installed_packages or already processed in this run)
                        if let Some(satisfied_pkgkey) = self.check_package_satisfaction(pkgkey_of_candidate, packages_map) {
                            log::debug!(
                                "{}: Candidate package {} ({}) already satisfied/selected.",
                                context, pkg_name, satisfied_pkgkey
                            );
                            // Ensure its depth and ebin_exposure in packages_map are updated for this path
                            // After the fix to check_package_satisfaction, this get_mut should ALWAYS succeed.
                            let info = packages_map.get_mut(&satisfied_pkgkey)
                                .expect(&format!("{}: BUG: Package {} reported as satisfied but NOT in packages_map for update", context, satisfied_pkgkey));

                            let old_depth = info.depend_depth;
                            let old_ebin = info.ebin_exposure;

                            info.depend_depth = std::cmp::min(info.depend_depth, candidate_depth);

                            // If ebin_flag is true for the current path, then ebin_exposure becomes true.
                            // If ebin_flag is false, existing ebin_exposure (if true) should persist.
                            info.ebin_exposure = info.ebin_exposure || ebin_flag;

                            log::trace!("{}: Updated package {} in map. Depth: {}->{}, Ebin: {}->{}. (Context ebin_flag: {})",
                            context, satisfied_pkgkey, old_depth, info.depend_depth, old_ebin, info.ebin_exposure, ebin_flag);

                            return Some(satisfied_pkgkey);
                        }

                        // If not satisfied by check_package_satisfaction (meaning it's not in self.installed_packages or not yet in packages_map with the right state),
                        // then try to add it to packages_map.
                        log::debug!("{}: Candidate {} ({}) not satisfied by prior state, attempting to add/select for current resolution.", context, pkg_name, pkgkey_of_candidate);
                        if let Some(added_pkgkey) = self.add_one_package_installing_with_arch_spec(
                            pkg_name, // Use original pkg_name for selection logic within add_one...
                            arch_spec, // Architecture spec for filtering within add_one...
                            candidate_depth,  // Depth of the *current* package
                            ebin_flag, // ebin_flag from the requiring context
                            packages_map, // The map to update
                            missing_items_log,
                        ) {
                            log::debug!(
                                "{}: Package {} successfully added/selected as {}. add_one_package_installing_with_arch_spec handled depth update.",
                                context, pkg_name, added_pkgkey
                            );
                            return Some(added_pkgkey);
                        }
                        log::debug!("{}: Failed to add/select package {} (candidate {}) for current resolution.", context, pkg_name, pkgkey_of_candidate);
                    }
                }
            } // This closes 'if !arch_filtered.is_empty()'
            Ok(_) => { // This means map_pkgname2packages was Ok, but arch_filtered was empty or select_highest_version was None
                log::debug!("{}: no suitable packages found for '{}' after filtering or version selection", context, pkg_name);
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
            _ => vec![base_capability.to_string()], // Treat as direct name if no providers or error
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
    #[allow(dead_code)]
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

    /// Collects all recursive dependencies for a given set of initial packages.
    ///
    /// This function is central to the dependency resolution process. It starts with an
    /// `initial_packages` map (typically representing user-requested packages or a set of packages
    /// to keep during a removal operation) and iteratively discovers all their dependencies.
    ///
    /// Key operations and behaviors:
    /// - It maintains a comprehensive `all_pkgs_info_map` which stores `InstalledPackageInfo`
    ///   for every package encountered during the resolution (both initial and dependencies).
    ///   This map is mutated throughout the process to update dependency information.
    /// - A queue (`processing_queue`) is used to manage packages whose dependencies still need
    ///   to be processed. Initially, this queue is populated from `initial_packages`.
    /// - For each package dequeued, its raw requirements (from `Requires` and `Requires(pre)`)
    ///   are parsed and resolved via `process_requirements`.
    /// - `process_requirements` attempts to find suitable providers for each requirement. If a
    ///   provider is found and added to `all_pkgs_info_map` (or an existing entry is updated),
    ///   it might be added to the `processing_queue` if its own dependencies haven't been
    ///   processed yet in the current context.
    /// - Crucially, this function (via `add_one_package_installing_with_arch_spec` and
    ///   `try_resolve_package_by_name` called by `process_requirements`) ensures that:
    ///     - `depend_depth` is correctly calculated for each package. For a dependency, this is
    ///       `depth_of_requiring_package + 1`. If a package is reached via multiple paths,
    ///       its `depend_depth` is set to the minimum depth found.
    ///     - `ebin_exposure` is set to `true` if and only if the package's directly requested by
    ///     user or essential_pkgname or has same source with them
    ///     - `depends` (list of packages this package directly depends on) and `rdepends`
    ///       (list of packages that directly depend on this package) are populated within
    ///       the `InstalledPackageInfo` structs in `all_pkgs_info_map`.
    ///
    /// The function returns the `all_pkgs_info_map` containing all initial packages and their
    /// fully resolved recursive dependencies, with accurate metadata.
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

        let mut all_pkgs_info_map: HashMap<String, InstalledPackageInfo> = initial_packages.clone();
        let mut current_layer_pkg_keys: Vec<String> = initial_packages.keys().cloned().collect();
        let mut processed_requiring_pkg_keys: HashSet<String> = HashSet::new();
        let mut missing_deps_log: Vec<String> = Vec::new();
        let mut iteration_count: u16 = 0; // For safety break and logging

        while !current_layer_pkg_keys.is_empty() {
            iteration_count += 1;
            log::info!(
                "[Iteration {}] Processing {} package(s) in current layer. Total packages in map: {}. Processed requiring keys: {}",
                iteration_count,
                current_layer_pkg_keys.len(),
                all_pkgs_info_map.len(),
                processed_requiring_pkg_keys.len()
            );

            if iteration_count > 50 { // Safety break
                log::error!(
                    "[Iteration {}] Exceeded maximum recursion depth (50). Breaking loop.",
                    iteration_count
                );
                log::error!("Total packages in map: {:#?}", all_pkgs_info_map.keys());
                log::error!("Last 'current_layer_pkg_keys' (would have been next): {:#?}", current_layer_pkg_keys);
                // Consider returning an error or the partial map based on desired strictness
                break;
            }

            let mut next_layer_pkg_keys_candidates: Vec<String> = Vec::new();

            for requiring_pkgkey_str in &current_layer_pkg_keys {
                let requiring_pkgkey = requiring_pkgkey_str.as_str();

                if processed_requiring_pkg_keys.contains(requiring_pkgkey) {
                    log::trace!("[Iteration {}] Skipping already processed requiring package: {}", iteration_count, requiring_pkgkey);
                    continue;
                }

                // Get current package depth from the InstalledPackageInfo in our map
                let current_pkg_depth = {
                    let pkg_info_installed = all_pkgs_info_map.get(requiring_pkgkey).expect(&format!(
                        "BUG: Requiring package {} (InstalledPackageInfo) not found in all_pkgs_info_map during iteration {}",
                        requiring_pkgkey, iteration_count
                    ));
                    pkg_info_installed.depend_depth
                };

                // Load full package metadata to get raw requirement strings
                let pkg_full_info = match self.load_package_info(requiring_pkgkey) {
                    Ok(info) => info,
                    Err(e) => {
                        log::error!(
                            "[Iteration {}] Failed to load full package info for {}: {}. Skipping its requirements processing.",
                            iteration_count, requiring_pkgkey, e
                        );
                        missing_deps_log.push(format!("Failed to load full info for {}: {}", requiring_pkgkey, e));
                        // Mark as processed to avoid retrying, even though its deps weren't processed
                        processed_requiring_pkg_keys.insert(requiring_pkgkey.to_string());
                        continue;
                    }
                };

                let mut all_resolved_direct_deps_for_current_pkg: Vec<String> = Vec::new();
                // Variable to track if a critical error occurred while processing requirements for *this* package.
                // If so, we might not want to add its (partially) resolved dependencies to the next layer.
                let mut critical_processing_error_occurred = false;

                // Process Pre-Depends (requires_pre)
                if !pkg_full_info.requires_pre.is_empty() {
                    log::debug!(
                        "[Iteration {}] Processing pre-requirements for package {} (Depth: {})
                         Requirements: {:?}",
                        iteration_count, requiring_pkgkey, current_pkg_depth, pkg_full_info.requires_pre
                    );
                    match self.process_requirements(
                        requiring_pkgkey,
                        &pkg_full_info.requires_pre,
                        &mut all_pkgs_info_map,
                        current_pkg_depth,
                        format,
                        &mut missing_deps_log,
                    ) {
                        Ok(resolved_keys) => {
                            log::trace!("[Iteration {}] Package {} resolved pre-dependencies: {:?}", iteration_count, requiring_pkgkey, resolved_keys);
                            all_resolved_direct_deps_for_current_pkg.extend(resolved_keys);
                        }
                        Err(e) => {
                            log::error!(
                                "Error processing pre-requirements for package {}: {}. Dependencies from this step might be incomplete.",
                                requiring_pkgkey, e
                            );
                            missing_deps_log.push(format!("Error processing pre-requirements for {}: {}", requiring_pkgkey, e));
                            critical_processing_error_occurred = true;
                        }
                    }
                }

                // Process Depends (requires) only if no critical error occurred during pre-depends
                if !critical_processing_error_occurred && !pkg_full_info.requires.is_empty() {
                     log::debug!(
                        "[Iteration {}] Processing requirements for package {} (Depth: {})
                         Requirements: {:?}",
                        iteration_count, requiring_pkgkey, current_pkg_depth, pkg_full_info.requires
                    );
                    match self.process_requirements(
                        requiring_pkgkey,
                        &pkg_full_info.requires,
                        &mut all_pkgs_info_map,
                        current_pkg_depth,
                        format,
                        &mut missing_deps_log,
                    ) {
                        Ok(resolved_keys) => {
                            log::trace!("[Iteration {}] Package {} resolved dependencies: {:?}", iteration_count, requiring_pkgkey, resolved_keys);
                            all_resolved_direct_deps_for_current_pkg.extend(resolved_keys);
                        }
                        Err(e) => {
                            log::error!(
                                "Error processing requirements for package {}: {}. Dependencies from this step might be incomplete.",
                                requiring_pkgkey, e
                            );
                            missing_deps_log.push(format!("Error processing requirements for {}: {}", requiring_pkgkey, e));
                            critical_processing_error_occurred = true;
                        }
                    }
                }

                if pkg_full_info.requires_pre.is_empty() && pkg_full_info.requires.is_empty() {
                    log::trace!("[Iteration {}] Package {} has no 'requires_pre' or 'requires' fields to process.", iteration_count, requiring_pkgkey);
                }

                // Only add resolved dependencies to the next layer if no critical error occurred during *this* package's requirement processing.
                if !critical_processing_error_occurred {
                    for dep_key in all_resolved_direct_deps_for_current_pkg {
                        if !processed_requiring_pkg_keys.contains(&dep_key) {
                            if !all_pkgs_info_map.contains_key(&dep_key) {
                                log::warn!(
                                    "[Iteration {}] Dependency key '{}' (from '{}') resolved by process_requirements was not found in all_pkgs_info_map.
                                     This might indicate an issue where resolve_single_capability_item did not add it, or it was unexpectedly removed.
                                     It will still be added to the next layer for processing.",
                                    iteration_count, dep_key, requiring_pkgkey
                                );
                                // Potentially, we might need to ensure it's added to all_pkgs_info_map here if that's a strict invariant,
                                // but process_requirements is expected to add new packages to the map.
                            }
                            next_layer_pkg_keys_candidates.push(dep_key);
                        }
                    }
                } else {
                    log::warn!(
                        "[Iteration {}] Due to critical errors processing requirements for {}, its resolved dependencies (if any) will not be added to the next processing layer to prevent cascading issues.",
                        iteration_count, requiring_pkgkey
                    );
                }
                processed_requiring_pkg_keys.insert(requiring_pkgkey.to_string());
            }

            // Deduplicate keys for the next layer
            current_layer_pkg_keys = next_layer_pkg_keys_candidates.into_iter().collect::<HashSet<_>>().into_iter().collect();

            if current_layer_pkg_keys.is_empty() {
                log::info!("[Iteration {}] No new packages for the next layer. Dependency resolution complete for this branch.", iteration_count);
            }
        }

        if !missing_deps_log.is_empty() {
            log::warn!("Recursive dependency collection encountered unresolved dependencies or errors for some packages. See log above. Missing/Error count: {}", missing_deps_log.len());
            self.handle_missing_dependencies(missing_deps_log)?; // Decide if this should be an error or just a warning state
        }

        log::info!(
            "Recursive dependency collection finished. Total {} packages in the final map (initial + dependencies).",
            all_pkgs_info_map.len()
        );
        Ok(all_pkgs_info_map)
    }

    fn process_requirements(
        &mut self,
        requiring_pkgkey: &str, // The package that has these requirements
        requirements_strings: &Vec<String>,
        all_pkgs_info_map: &mut HashMap<String, InstalledPackageInfo>, // This is the main map to check and add to.
        current_pkg_depth: u16,
        format: PackageFormat,
        missing_deps_log: &mut Vec<String>,
    ) -> Result<Vec<String>> { // Changed return type
        log::trace!("Processing requirements for {} at depth {}: {:?}", requiring_pkgkey, current_pkg_depth, requirements_strings);
        let mut resolved_direct_dependencies: Vec<String> = Vec::new(); // To store keys of resolved direct dependencies

        for req_string in requirements_strings {
            let and_groups = match parse_requires(format, req_string) {
                Ok(groups) => groups,
                Err(e) => {
                    missing_deps_log.push(format!("Failed to parse requirement string \"{}\" for {}: {}", req_string, requiring_pkgkey, e));
                    continue; // Move to the next requirement string
                }
            };

            for or_group in and_groups { // Each or_group is Vec<Dependency>, representing an OR choice
                if or_group.is_empty() { continue; }

                let mut or_group_satisfied = false;
                for dep_capability_info in &or_group { // dep_capability_info.capability is the string
                    match self.resolve_single_capability_item(
                        &dep_capability_info.capability,
                        all_pkgs_info_map, // Pass the main accumulating map
                        current_pkg_depth + 1,
                        false,           // ebin_flag is false for dependencies
                        missing_deps_log,
                        format,          // Pass the format
                    )? {
                        Some(satisfied_by_pkgkey_b) => {
                            // Capability satisfied by pkgkey_b.

                            // Add to the 'depends' list of the requiring package
                            let requiring_pkg_info = all_pkgs_info_map.get_mut(requiring_pkgkey)
                                .expect(&format!("BUG: Requiring package {} not found in all_pkgs_info_map", requiring_pkgkey));
                            if !requiring_pkg_info.depends.contains(&satisfied_by_pkgkey_b) {
                                requiring_pkg_info.depends.push(satisfied_by_pkgkey_b.clone());
                            }

                            // Update rdepends of the satisfied package
                            let satisfied_pkg_info = all_pkgs_info_map.get_mut(&satisfied_by_pkgkey_b)
                                .expect(&format!("BUG: Satisfied package {} not found in all_pkgs_info_map", satisfied_by_pkgkey_b));
                            if !satisfied_pkg_info.rdepends.contains(&requiring_pkgkey.to_string()) {
                                satisfied_pkg_info.rdepends.push(requiring_pkgkey.to_string());
                            }

                            // Record this as a direct dependency for the current requiring_pkgkey
                            if !resolved_direct_dependencies.contains(&satisfied_by_pkgkey_b) {
                                resolved_direct_dependencies.push(satisfied_by_pkgkey_b.clone());
                            }

                            or_group_satisfied = true;
                            log::debug!(
                                "OR group for {} satisfied by capability \"{}\" (via pkgkey \"{}\")",
                                requiring_pkgkey, dep_capability_info.capability, satisfied_by_pkgkey_b
                            );
                            break; // This OR group is satisfied, move to the next AND group
                        }
                        None => { /* Current capability in OR group not satisfied, try next */ }
                    }
                }

                if !or_group_satisfied {
                    let group_caps: Vec<&str> = or_group.iter().map(|d| d.capability.as_str()).collect();
                    // Removed single quotes from the format string here
                    let error_msg = format!("For package {}, dependency group not satisfied: {:?}", requiring_pkgkey, group_caps);
                    log::warn!("{}", error_msg);
                    missing_deps_log.push(error_msg);
                    // If one AND group (which is a collection of OR choices) fails, the whole requirement might be considered failed.
                    // Current logic: collect all failures and let handle_missing_dependencies deal with it.
                }
            }
        }
        Ok(resolved_direct_dependencies) // Return the list of direct dependencies found
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
            repos_data: Vec::new(),
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            installed_packages: HashMap::new(),
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
            repos_data: Vec::new(),
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            installed_packages: HashMap::new(),
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
