//! Dependency resolution and candidate selection implementation
//!
//! This module implements the core resolvo DependencyProvider trait for the generic
//! dependency resolver. It handles:
//! - Dependency resolution for packages (get_dependencies)
//! - Candidate package selection and filtering (get_candidates, filter_candidates)
//! - Package sorting and prioritization (sort_candidates)
//! - Favoring logic for installed vs upgrade packages

use std::cmp::Ordering;
use resolvo::{DependencyProvider, KnownDependencies, SolvableId, Candidates, Dependencies, NameId, VersionSetId, SolverCache, Requirement, Condition, LogicalOperator, HintDependenciesAvailable, Interner};
use crate::resolve::provider::GenericDependencyProvider;
use crate::resolve::types::SolverMatchSpec;
use crate::parse_requires::{VersionConstraint, Operator};
use crate::config;
use crate::package::pkgkey2pkgname;

impl DependencyProvider for GenericDependencyProvider {

    async fn get_dependencies(&self, solvable: SolvableId) -> Dependencies {
        let record = &self.pool.resolve_solvable(solvable).record;
        let pkgkey = &record.pkgkey;

        log::debug!(
            "[RESOLVO] get_dependencies for solvable {} (pkgkey: {}, version: {})",
            solvable.0,
            pkgkey,
            record.version
        );

        // Load package info
        let package = match self.load_package_for_solvable(pkgkey) {
            Ok(pkg) => pkg,
            Err(reason) => {
                let reason_str = self.display_string(reason).to_string();
                log::debug!(
                    "[RESOLVO] Failed to load package {}: {}",
                    pkgkey,
                    reason_str
                );
                return Dependencies::Unknown(reason);
            }
        };

        // Process all dependency types
        let mut known_deps = KnownDependencies::default();
        self.process_requirements(&package, &mut known_deps);
        self.process_conflicts_or_obsoletes(&package.conflicts, "conflict", &mut known_deps, &package, pkgkey);
        self.process_conflicts_or_obsoletes(&package.obsoletes, "obsolete", &mut known_deps, &package, pkgkey);

        log::debug!(
            "[RESOLVO] get_dependencies for {}: {} requirements, {} constraints",
            pkgkey,
            known_deps.requirements.len(),
            known_deps.constrains.len()
        );

        self.log_dependencies_debug(&known_deps);

        Dependencies::Known(known_deps)
    }

    async fn get_candidates(&self, name: NameId) -> Option<Candidates> {
        let name_str = self.pool.resolve_package_name(name);
        let name_string = name_str.0.clone();

        // Lazy load packages if not already loaded
        let needs_load = { !self.loaded_packages.borrow().contains_key(&name_string) };
        if needs_load {
            if let Err(e) = self.load_packages_for_name(&name_string) {
                log::warn!(
                    "[RESOLVO] Failed to load packages for '{}': {}",
                    name_string,
                    e
                );
                return None;
            }
        }

        // Get solvables for this name
        let name_map = self.name_to_solvables.borrow();
        let candidates_vec = name_map.get(&name).cloned().unwrap_or_default();
        drop(name_map); // Release borrow before getting package manager

        log::debug!(
            "[RESOLVO] get_candidates for '{}' (name_id: {:?}) found {} candidates",
            name_string,
            name,
            candidates_vec.len()
        );
        for &solvable_id in &candidates_vec {
            let record = &self.pool.resolve_solvable(solvable_id).record;
            log::debug!(
                "[RESOLVO]   Candidate: {} (version: {})",
                record.pkgkey,
                record.version
            );
        }

        if candidates_vec.is_empty() {
            log::info!("[RESOLVO] No candidates found for '{}' (name_id: {:?})! This might indicate a bug in package association.",
                name_string, name);
        }

        let favored = if candidates_vec.len() == 1 {
            None
        } else {
            self.find_favored_candidate(&candidates_vec, &name_string)
        };

        Some(Candidates {
            candidates: candidates_vec,
            favored,
            locked: None,
            excluded: Vec::new(),
            hint_dependencies_available: HintDependenciesAvailable::All,
        })
    }

    async fn filter_candidates(
        &self,
        candidates: &[SolvableId],
        version_set: VersionSetId,
        inverse: bool,
    ) -> Vec<SolvableId> {
        let spec = self.pool.resolve_version_set(version_set);

        match spec {
            SolverMatchSpec::MatchSpec(and_depends) => {
                candidates
                    .iter()
                    .copied()
                    .filter(|&c| {
                        let record = &self.pool.resolve_solvable(c).record;

                        // Check if this package matches the version set
                        let matches = and_depends.iter().any(|or_depends| {
                            or_depends.iter().any(|pkg_depend| {
                                // Capability is already normalized (without arch suffix) when creating version set
                                let capability = &pkg_depend.capability;

                                // Check if package name matches OR if package provides the capability
                                let is_direct_match = capability == &record.pkgname;
                                let provides_capability = if is_direct_match {
                                    false // Already matched by name, no need to check provides
                                } else {
                                    // Check if package provides this capability
                                    let provides = self.package_provides_capability(&record.pkgkey, capability);
                                    log::debug!(
                                        "[RESOLVO] filter_candidates: checking if {} provides '{}': {}",
                                        record.pkgkey,
                                        capability,
                                        provides
                                    );
                                    provides
                                };

                                if !is_direct_match && !provides_capability {
                                    log::debug!(
                                        "[RESOLVO] filter_candidates: {} does not match '{}' (direct_match={}, provides={})",
                                        record.pkgkey,
                                        capability,
                                        is_direct_match,
                                        provides_capability
                                    );
                                    return false;
                                }

                                // Check version constraints
                                if pkg_depend.constraints.is_empty() {
                                    return true; // No constraints means any version matches
                                }

                                // Filter out IfInstall constraints for version checking
                                let non_conditional_constraints: Vec<VersionConstraint> = pkg_depend.constraints
                                    .iter()
                                    .filter(|c| c.operator != Operator::IfInstall)
                                    .cloned()
                                    .collect();

                                if non_conditional_constraints.is_empty() {
                                    return true; // Only conditional constraints, so this satisfies
                                }

                                // Check version constraints
                                // If this is a direct match (package name), check against package version
                                // If this is a provider match, use check_provider_satisfies_constraints
                                if is_direct_match {
                                    // Direct match: check constraints against package version only
                                    // When the constraint is for the package's own name, we should only
                                    // check the package version, not provided capabilities. The provided
                                    // capabilities check is only for cases where the constraint refers to
                                    // a different capability name (handled in the else branch below).
                                    non_conditional_constraints.iter().all(|constraint| {
                                        match crate::version_constraint::check_version_constraint(
                                            &record.version,
                                            constraint,
                                            self.format,
                                        ) {
                                            Ok(satisfies) => satisfies,
                                            Err(_) => false,
                                        }
                                    })
                                } else {
                                    // Provider match: use proper provider constraint checking
                                    // This checks the provided capability's version (if specified),
                                    // not the provider's version
                                    match crate::package_cache::load_package_info(&record.pkgkey) {
                                        Ok(provider_pkg) => {
                                            match crate::provides::check_provider_satisfies_constraints(
                                                &provider_pkg,
                                                capability,
                                                &non_conditional_constraints,
                                                self.format,
                                            ) {
                                                Ok(satisfies) => satisfies,
                                                Err(_) => false,
                                            }
                                        }
                                        Err(_) => false,
                                    }
                                }
                            })
                        });

                        matches != inverse
                    })
                    .collect()
            }
        }
    }

    async fn sort_candidates(&self, _solver: &SolverCache<Self>, solvables: &mut [SolvableId]) {
        // Get set of already-loaded package keys (packages that are likely in the solution)
        let loaded_pkgkeys: std::collections::HashSet<String> = self
            .loaded_packages
            .borrow()
            .values()
            .flatten()
            .cloned()
            .collect();

        // Get set of package names that were loaded by direct package name lookup
        // (not just by capability lookup). These are packages that are explicitly required.
        let loaded_packages_map = self.loaded_packages.borrow();
        let directly_loaded_pkgnames: std::collections::HashSet<String> = loaded_packages_map
            .iter()
            .filter_map(|(lookup_name, pkgkeys)| {
                // If the lookup name matches any of the package names in the result,
                // it means this package was loaded by direct package name lookup (required)
                if !pkgkeys.is_empty() {
                    // Check if lookup_name is a package name (not a capability like "so:...")
                    if let Ok(pkgname_from_key) = pkgkey2pkgname(&pkgkeys[0]) {
                        if lookup_name == &pkgname_from_key {
                            return Some(pkgname_from_key);
                        }
                    }
                }
                None
            })
            .collect();
        drop(loaded_packages_map);

        // Sort candidates with preference for:
        // 1. Packages whose names were loaded by direct package name lookup (explicitly required) - HIGHEST PRIORITY
        // 2. Already-loaded packages by pkgkey (likely already in solution) - prefer these to avoid conflicts
        // 3. Version (newer first)
        // Note: Installed packages are already favored in get_candidates(), so sorting
        // here is only for non-favored candidates
        solvables.sort_by(|&a, &b| {
            let rec_a = &self.pool.resolve_solvable(a).record;
            let rec_b = &self.pool.resolve_solvable(b).record;

            // FIRST: Check if package names were loaded by direct package name lookup (explicitly required)
            // This is the highest priority to avoid conflicts with already-required packages
            let a_directly_loaded = directly_loaded_pkgnames.contains(&rec_a.pkgname);
            let b_directly_loaded = directly_loaded_pkgnames.contains(&rec_b.pkgname);

            match (a_directly_loaded, b_directly_loaded) {
                (true, false) => {
                    log::debug!(
                        "[RESOLVO] Preferring {} over {} ({} was directly loaded/required)",
                        rec_a.pkgkey,
                        rec_b.pkgkey,
                        rec_a.pkgname
                    );
                    Ordering::Less // a was directly loaded (required), prefer a
                }
                (false, true) => {
                    log::debug!(
                        "[RESOLVO] Preferring {} over {} ({} was directly loaded/required)",
                        rec_b.pkgkey,
                        rec_a.pkgkey,
                        rec_b.pkgname
                    );
                    Ordering::Greater // b was directly loaded (required), prefer b
                }
                _ => {
                    // Both or neither directly loaded - check if already loaded by pkgkey
                    let a_loaded_by_key = loaded_pkgkeys.contains(&rec_a.pkgkey);
                    let b_loaded_by_key = loaded_pkgkeys.contains(&rec_b.pkgkey);

                    match (a_loaded_by_key, b_loaded_by_key) {
                        (true, false) => Ordering::Less,  // a is loaded, prefer a
                        (false, true) => Ordering::Greater, // b is loaded, prefer b
                        _ => {
                            // Both or neither loaded - compare by version
                            // Default: newer first, but respect prefer_low_version setting
                            let prefer_low_version = config().install.prefer_low_version;
                            match crate::version_compare::compare_versions(&rec_b.version, &rec_a.version, self.format) {
                                Some(Ordering::Less) => {
                                    if prefer_low_version {
                                        Ordering::Greater // Prefer older (a comes first)
                                    } else {
                                        Ordering::Less // Prefer newer (a comes first)
                                    }
                                },
                                Some(Ordering::Greater) => {
                                    if prefer_low_version {
                                        Ordering::Less // Prefer older (b comes first)
                                    } else {
                                        Ordering::Greater // Prefer newer (b comes first)
                                    }
                                },
                                Some(Ordering::Equal) | None => Ordering::Equal,
                            }
                        }
                    }
                }
            }
        });
    }

    fn should_cancel_with_value(&self) -> Option<Box<dyn std::any::Any>> {
        None // No cancellation support for now
    }

}

impl GenericDependencyProvider {

    /// Find the favored candidate from the list of candidates
    /// Uses direct lookup by pkgname for efficiency
    fn find_favored_candidate(
        &self,
        candidates_vec: &[SolvableId],
        name_string: &str,
    ) -> Option<SolvableId> {
        if config().upgrade.full_upgrade {
            // Full upgrade: don't favor any packages
            return None;
        }

        // Get installed packages map for favored candidate lookup
        let installed_pkgname2keys = self.get_installed_pkgname2keys().unwrap_or_default();

        // Direct lookup by package name (when name_string is a pkgname, e.g. "bash")
        if let Some(installed_pkgkeys) = installed_pkgname2keys.get(name_string) {
            return candidates_vec.iter().find_map(|&solvable_id| {
                let record = &self.pool.resolve_solvable(solvable_id).record;
                if installed_pkgkeys.contains(&record.pkgkey) {
                    if self.delta_world_keys.contains(name_string) {
                        log::debug!("[RESOLVO] Package '{}' (pkgkey: {}) is installed and in delta_world, not favoring to allow upgrade", name_string, record.pkgkey);
                        None
                    } else {
                        log::debug!("[RESOLVO] Package '{}' (pkgkey: {}) is installed but not in delta_world, favoring to prevent auto-upgrade", name_string, record.pkgkey);
                        Some(solvable_id)
                    }
                } else {
                    None
                }
            });
        }

        // Capability lookup: name_string is a capability (e.g. "/bin/sh"), not a pkgname.
        // Check each candidate by its pkgname so we favor an already-installed provider.
        candidates_vec.iter().find_map(|&solvable_id| {
            let record = &self.pool.resolve_solvable(solvable_id).record;
            let installed_pkgkeys = installed_pkgname2keys.get(&record.pkgname)?;
            if !installed_pkgkeys.contains(&record.pkgkey) {
                return None;
            }
            if self.delta_world_keys.contains(&record.pkgname) {
                log::debug!("[RESOLVO] Capability provider '{}' (pkgkey: {}) is installed and in delta_world, not favoring", record.pkgname, record.pkgkey);
                return None;
            }
            log::debug!("[RESOLVO] Capability '{}' provider '{}' (pkgkey: {}) is installed, favoring to avoid file conflicts", name_string, record.pkgname, record.pkgkey);
            Some(solvable_id)
        })
    }


    /// Log detailed debug information about dependencies
    fn log_dependencies_debug(&self, known_deps: &KnownDependencies) {
        // Log individual requirements if debug level
        if log::log_enabled!(log::Level::Debug) {
            for req in &known_deps.requirements {
                let req_str = match &req.requirement {
                    Requirement::Single(version_set_id) => {
                        let req_name = self.version_set_name(*version_set_id);
                        let req_name_str = self.display_name(req_name);
                        let req_spec = self.display_version_set(*version_set_id);
                        format!("{}: {}", req_name_str, req_spec)
                    }
                    Requirement::Union(union_id) => {
                        let version_sets: Vec<String> = self
                            .version_sets_in_union(*union_id)
                            .map(|vs_id| {
                                let vs_name = self.version_set_name(vs_id);
                                let vs_name_str = self.display_name(vs_name);
                                let vs_spec = self.display_version_set(vs_id);
                                format!("{}: {}", vs_name_str, vs_spec)
                            })
                            .collect();
                        format!("({})", version_sets.join(" | "))
                    }
                };
                let condition_str = if let Some(cond_id) = req.condition {
                    let condition = self.resolve_condition(cond_id);
                    format!(" [condition: {}]", self.format_condition(&condition))
                } else {
                    String::new()
                };
                log::debug!("[RESOLVO]   Requirement: {}{}", req_str, condition_str);
            }
            for constraint in &known_deps.constrains {
                let constraint_name = self.version_set_name(*constraint);
                let constraint_name_str = self.display_name(constraint_name);
                let constraint_spec = self.display_version_set(*constraint);
                log::debug!(
                    "[RESOLVO]   Constraint: {}: {}",
                    constraint_name_str,
                    constraint_spec
                );
            }
        }
    }

    /// Format a condition in a human-readable way
    fn format_condition(&self, condition: &Condition) -> String {
        match condition {
            Condition::Requirement(version_set_id) => {
                let req_name = self.version_set_name(*version_set_id);
                let req_name_str = self.display_name(req_name);
                let req_spec = self.display_version_set(*version_set_id);
                format!("if {}: {}", req_name_str, req_spec)
            }
            Condition::Binary(op, left_id, right_id) => {
                let left = self.resolve_condition(*left_id);
                let right = self.resolve_condition(*right_id);
                let left_str = self.format_condition(&left);
                let right_str = self.format_condition(&right);
                let op_str = match op {
                    LogicalOperator::And => "AND",
                    LogicalOperator::Or => "OR",
                };
                format!("({} {} {})", left_str, op_str, right_str)
            }
        }
    }

}
