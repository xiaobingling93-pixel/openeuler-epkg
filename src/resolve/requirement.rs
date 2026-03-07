//! Dependency requirement processing and conversion
//!
//! This module handles the processing of package dependency requirements and
//! their conversion into resolvo ConditionalRequirement structures. Key features:
//! - Support for multiple dependency fields (Requires, Recommends, Suggests, etc.)
//! - IfInstall conditional dependency handling
//! - Capability normalization and no-install filtering
//! - Missing package/capability checking with ignore_missing support

use resolvo::{Condition, ConditionId, ConditionalRequirement, KnownDependencies, LogicalOperator};
use crate::models::Package;
use crate::parse_requires::{AndDepends, Operator, PkgDepend};
use crate::resolve::types::{NameType, SolverMatchSpec, DependFieldFlags};
use crate::resolve::provider::GenericDependencyProvider;
use color_eyre::Result;
use std::collections::HashSet;

impl GenericDependencyProvider {

    /// Process dependency requirements from a package
    pub fn process_requirements(&self, package: &Package, known_deps: &mut KnownDependencies) {
        let dep_strings = self.get_dependency_strings(package);

        for dep_str in &dep_strings {
            // Parse the dependency string
            let and_depends = match crate::parse_requires::parse_requires(self.format, dep_str) {
                Ok(deps) => deps,
                Err(e) => {
                    log::warn!("[RESOLVO] Failed to parse dependency '{}': {}", dep_str, e);
                    continue;
                }
            };

            match self.convert_and_depends_to_requirements(&and_depends) {
                Ok(reqs) => known_deps.requirements.extend(reqs),
                Err(e) => {
                    log::warn!(
                        "[RESOLVO] Failed to convert dependency specification '{}': {}",
                        dep_str,
                        e
                    );
                }
            }
        }
    }

    /// Get dependency strings from a package based on depend_fields
    fn get_dependency_strings(&self, package: &Package) -> HashSet<String> {
        let mut deps = HashSet::new();

        if self.depend_fields.borrow().contains(DependFieldFlags::REQUIRES) {
            deps.extend(package.requires.iter().cloned());
            deps.extend(package.requires_pre.iter().cloned());
        }

        if self.depend_fields.borrow().contains(DependFieldFlags::RECOMMENDS) {
            deps.extend(package.recommends.iter().cloned());
        }

        if self.depend_fields.borrow().contains(DependFieldFlags::SUGGESTS) {
            deps.extend(package.suggests.iter().cloned());
        }

        if self.depend_fields.borrow().contains(DependFieldFlags::BUILD_REQUIRES)
            && package.repodata_name == "aur"
        {
            deps.extend(package.build_requires.iter().cloned());
        }

        if self.depend_fields.borrow().contains(DependFieldFlags::CHECK_REQUIRES)
        {
            deps.extend(package.check_requires.iter().cloned());
        }

        deps
    }

    /// Convert AndDepends to resolvo ConditionalRequirement list
    /// Handles IfInstall conditions by creating proper ConditionalRequirement with conditions
    pub fn convert_and_depends_to_requirements(
        &self,
        and_depends: &AndDepends,
    ) -> Result<Vec<ConditionalRequirement>> {
        let ignore_missing = crate::models::config().common.ignore_missing;
        let mut requirements = Vec::new();

        for or_depends in and_depends {
            // For each OR group, we need to create a union of version sets
            let mut version_set_ids = Vec::new();
            let mut condition_ids: Vec<Option<ConditionId>> = Vec::new();

            for pkg_depend in or_depends {
                let capability = &pkg_depend.capability;

                // Normalize capability name by stripping architecture suffixes (e.g., python3:any -> python3)
                // This matches the behavior of the simple solver's resolve_single_capability_item
                let normalized_capability = self.normalize_capability_name(capability);

                // Filter out dependencies where the package name is in no-install
                // Check the normalized capability name (which is what we actually use for lookups)
                if !self.no_install.is_empty() && self.no_install.contains(&normalized_capability) {
                    log::debug!(
                        "[RESOLVO] Skipping dependency '{}' (normalized: '{}') - in no-install list",
                        capability,
                        normalized_capability
                    );
                    continue;
                }

                // Check if package/capability exists when ignore_missing is enabled
                if ignore_missing
                    && !self.check_package_or_capability_exists(&normalized_capability)
                {
                    log::info!(
                        "[RESOLVO] Package/capability '{}' not found, skipping (ignore_missing=true)",
                        normalized_capability
                    );
                    continue;
                }

                // Check for IfInstall constraints
                let mut if_install_conditions = Vec::new();
                let mut non_conditional_constraints = Vec::new();

                for constraint in &pkg_depend.constraints {
                    if constraint.operator == Operator::IfInstall {
                        // Create a condition: the operand package must be installed
                        // The operand is the package name that must be installed for this dependency to apply
                        let condition_pkg_name = &constraint.operand;

                        // Check if condition package exists when ignore_missing is enabled
                        if ignore_missing
                            && !self.check_package_or_capability_exists(condition_pkg_name)
                        {
                            log::warn!(
                                "[RESOLVO] IfInstall condition package '{}' not found, skipping condition (ignore_missing=true)",
                                condition_pkg_name
                            );
                            continue;
                        }

                        let condition_name_id = self
                            .pool
                            .intern_package_name(NameType(condition_pkg_name.clone()));

                        // Create a version set that matches any version of the condition package
                        // This condition is true if the condition package is installed (any version)
                        // We create a PkgDepend with no constraints (empty constraints = any version)
                        let pkg_depend = PkgDepend {
                            capability: condition_pkg_name.clone(),
                            constraints: Vec::new(), // No constraints = matches any version
                        };
                        let or_deps = vec![pkg_depend];
                        let and_deps = vec![or_deps];
                        let condition_version_set_id = self.pool.intern_version_set(
                            condition_name_id,
                            SolverMatchSpec::MatchSpec(and_deps),
                        );

                        // Create Condition::Requirement - this is true if the package is installed
                        let condition = Condition::Requirement(condition_version_set_id);
                        let condition_id = self.pool.intern_condition(condition);
                        if_install_conditions.push(condition_id);
                    } else {
                        non_conditional_constraints.push(constraint.clone());
                    }
                }

                // Use the non-conditional constraints
                let final_constraints = non_conditional_constraints;

                // Intern the normalized package name (without arch suffix)
                log::trace!(
                    "[RESOLVO] Processing requirement: original='{}', normalized='{}'",
                    capability,
                    normalized_capability
                );
                let name_id = self
                    .pool
                    .intern_package_name(NameType(normalized_capability.clone()));

                // Create version set from non-conditional constraints using normalized capability
                let pkg_dep = PkgDepend {
                    capability: normalized_capability.clone(),
                    constraints: final_constraints,
                };
                let or_deps = vec![pkg_dep];
                let and_deps = vec![or_deps];

                let version_set_id = self
                    .pool
                    .intern_version_set(name_id, SolverMatchSpec::MatchSpec(and_deps));

                version_set_ids.push(version_set_id);

                // Combine all IfInstall conditions with AND (all must be true)
                if !if_install_conditions.is_empty() {
                    let mut combined_condition = if_install_conditions[0];
                    for &cond_id in &if_install_conditions[1..] {
                        let and_condition =
                            Condition::Binary(LogicalOperator::And, combined_condition, cond_id);
                        combined_condition = self.pool.intern_condition(and_condition);
                    }
                    condition_ids.push(Some(combined_condition));
                } else {
                    condition_ids.push(None);
                }
            }

            if version_set_ids.is_empty() {
                continue;
            }

            // Create requirement (single or union)
            let requirement: resolvo::Requirement = if version_set_ids.len() == 1 {
                version_set_ids[0].into()
            } else {
                let first = version_set_ids[0];
                let union_id = self
                    .pool
                    .intern_version_set_union(first, version_set_ids[1..].iter().copied());
                union_id.into()
            };

            // Combine conditions: if multiple OR alternatives have conditions, we need to OR them
            // For now, if any condition exists, use the first one (simplified)
            // In a full implementation, we'd OR all conditions together
            let final_condition = condition_ids
                .iter()
                .find(|c| c.is_some())
                .copied()
                .flatten();

            requirements.push(ConditionalRequirement {
                requirement,
                condition: final_condition,
            });
        }

        Ok(requirements)
    }

}
