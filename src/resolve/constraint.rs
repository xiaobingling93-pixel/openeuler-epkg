//! Conflict and obsolete constraint processing and inversion
//!
//! This module handles the processing of package conflicts and obsoletes,
//! converting them into resolvo constraints. Key features:
//! - Constraint inversion for conflicts/obsoletes (resolvo forbids what doesn't match)
//! - Self-version constraint normalization (RPM format specific)
//! - Self-contradictory constraint filtering to prevent solver panics
//! - Support for OpenEuler-specific constraint filtering

use std::borrow::Cow;
use resolvo::{ConditionalRequirement, KnownDependencies};
use resolvo::Interner;

use crate::models::channel_config;
use crate::package_cache;
use crate::parse_requires::VersionConstraint;
use crate::provides::check_provider_satisfies_constraints;

impl crate::resolve::provider::GenericDependencyProvider {

    /// Process conflicts or obsoletes from a package
    ///
    /// Both conflicts and obsoletes prevent installation of matching packages and need constraint
    /// inversion because resolvo's constraint mechanism forbids packages that DON'T match the
    /// constraint. So if package A conflicts with or obsoletes b>1, we need to create a constraint
    /// b<=1, which will cause resolvo to forbid packages that don't match b<=1 (i.e., b version 2).
    pub fn process_conflicts_or_obsoletes(
        &self,
        dependency_strings: &[String],
        dep_type: &str,
        known_deps: &mut KnownDependencies,
        package: &crate::models::Package,
        pkgkey: &str,
    ) {
        for dep_str in dependency_strings {
            if let Ok(mut and_depends) = crate::parse_requires::parse_requires(self.format, dep_str) {
                // Normalize constraints: if we see "cap <= pkg.version" where pkg.version matches
                // the package's own version, change it to "cap < pkg.version" to avoid self-conflicts.
                // This is a common packaging mistake where packages obsolete capabilities at their
                // own version, which after inversion becomes "cap > pkg.version", conflicting with
                // same-version providers.
                self.fixup_self_version_constraints(&mut and_depends, &package.version);

                // Always invert constraints for conflicts and obsoletes
                let inverted_and_depends = self.invert_constraints_for_conflicts(&and_depends);

                // Filter out constraints that would conflict with the package's own provides
                let filtered_and_depends = self.filter_self_contradictory_constraints(
                    &inverted_and_depends,
                    pkgkey,
                );

                match self.convert_and_depends_to_requirements(&filtered_and_depends) {
                    Ok(reqs) => {
                        self.add_requirements_to_constraints(reqs, known_deps);
                    }
                    Err(e) => {
                        log::warn!(
                            "[RESOLVO] Failed to convert {} '{}': {}",
                            dep_type,
                            dep_str,
                            e
                        );
                    }
                }
            }
        }
    }

    /// Filter out constraints that would conflict with the package's own provides
    /// This prevents self-contradictory constraints that can cause resolvo to panic
    ///
    /// # Example
    ///
    /// Consider a package `tcl` that:
    /// - Provides: `tcl-tcldict = 8.6.14`
    /// - Obsoletes: `tcl-tcldict<=8.6.14`
    ///
    /// When processing obsoletes, the constraint `tcl-tcldict<=8.6.14` gets inverted to
    /// `tcl-tcldict>8.6.14`. However, the package itself provides `tcl-tcldict = 8.6.14`,
    /// which does NOT satisfy `>8.6.14`. This creates a self-contradictory constraint:
    /// the package requires `tcl-tcldict>8.6.14` but provides `tcl-tcldict = 8.6.14`.
    ///
    /// This function detects such cases and filters out the self-contradictory constraint,
    /// preventing resolvo from panicking with "watched_literals[0] != watched_literals[1]".
    fn filter_self_contradictory_constraints<'a>(
        &self,
        and_depends: &'a crate::parse_requires::AndDepends,
        pkgkey: &str,
    ) -> Cow<'a, crate::parse_requires::AndDepends> {
        use crate::parse_requires::PkgDepend;

        // Only apply the filtering for OpenEuler; other distros keep constraints unchanged.
        if channel_config().distro != "openeuler" {
            return Cow::Borrowed(and_depends);
        }

        let filtered = and_depends
            .iter()
            .map(|or_depends| {
                or_depends
                    .iter()
                    .filter_map(|pkg_dep| {
                        let capability = &pkg_dep.capability;

                        // First check: if the package name itself matches the capability,
                        // or if the package provides the capability, this is a self-constraint
                        // that would create duplicate watched literals.
                        // Example: package `mesa-dri-drivers` listing a conflict/obsolete
                        // `mesa-dri-drivers` (possibly via inversion), which would require the
                        // solver to forbid the package from co-existing with itself.
                        // Example: package `texlive-plain-generic` provides `tex4ht` and also
                        // conflicts with `tex4ht`, which is a self-constraint.
                        let is_self_constraint = if let Ok(pkgname) = crate::package::pkgkey2pkgname(pkgkey) {
                            pkgname == *capability
                        } else {
                            false
                        } || self.package_provides_capability(pkgkey, capability);

                        if is_self_constraint {
                            log::debug!(
                                "[RESOLVO] Skipping self-constraint: package {} constrains itself by name/provide '{}'",
                                pkgkey,
                                capability
                            );
                            return None;
                        }

                        // Check if this package provides the capability
                        if !self.package_provides_capability(pkgkey, capability) {
                            // Package doesn't provide this capability, so the constraint is valid
                            return Some(pkg_dep.clone());
                        }

                        // Package provides this capability - check if the constraint would conflict
                        // with what the package provides
                        if pkg_dep.constraints.is_empty() {
                            // No version constraints, so this would conflict (package provides it)
                            log::debug!(
                                "[RESOLVO] Skipping self-contradictory constraint: package {} provides '{}' without version constraints",
                                pkgkey,
                                capability
                            );
                            return None;
                        }

                        // Filter out IfInstall constraints for version checking
                        let non_conditional_constraints: Vec<VersionConstraint> =
                            pkg_dep.constraints
                                .iter()
                                .filter(|c| c.operator != crate::parse_requires::Operator::IfInstall)
                                .cloned()
                                .collect();

                        if non_conditional_constraints.is_empty() {
                            // Only conditional constraints, so this is valid
                            return Some(pkg_dep.clone());
                        }

                        // Load the provider package to check its version
                        let provider_pkg = match package_cache::load_package_info(pkgkey) {
                            Ok(pkg) => pkg,
                            Err(_) => return Some(pkg_dep.clone()), // Can't load package, allow constraint
                        };

                        // Check if the package's provided version satisfies the constraints
                        match check_provider_satisfies_constraints(
                            &provider_pkg,
                            capability,
                            &non_conditional_constraints,
                            self.format,
                        ) {
                            Ok(true) => {
                                // Package provides a version that satisfies the constraint
                                // This is valid - the constraint is not self-contradictory
                                Some(pkg_dep.clone())
                            }
                            Ok(false) => {
                                // Package provides a version that doesn't satisfy the constraint
                                // This would create a self-contradictory constraint - skip it
                                log::debug!(
                                    "[RESOLVO] Skipping self-contradictory constraint: package {} provides '{}' with version that doesn't satisfy constraints {:?}",
                                    pkgkey,
                                    capability,
                                    non_conditional_constraints
                                );
                                None
                            }
                            Err(e) => {
                                log::warn!(
                                    "[RESOLVO] Error checking constraint for {}: {}, allowing constraint",
                                    capability,
                                    e
                                );
                                Some(pkg_dep.clone()) // On error, allow it
                            }
                        }
                    })
                    .collect()
            })
            .filter(|or_depends: &Vec<PkgDepend>| !or_depends.is_empty()) // Remove empty OR groups
            .collect();

        Cow::Owned(filtered)
    }

    /// Normalize constraints in conflicts/obsoletes to avoid self-conflicts (RPM format only)
    ///
    /// If a package has conflicts/obsoletes like "cap <= pkg.version" where pkg.version matches
    /// the package's own version, automatically change it to "cap < pkg.version". This prevents
    /// self-contradictory constraints after inversion (which would become "cap > pkg.version").
    ///
    /// This normalization is only applied to RPM format packages, as this is where this packaging
    /// mistake commonly occurs.
    fn fixup_self_version_constraints(
        &self,
        and_depends: &mut crate::parse_requires::AndDepends,
        package_version: &str,
    ) {
        // Only apply this fix to RPM format
        if self.format != crate::models::PackageFormat::Rpm {
            return;
        }

        use crate::parse_requires::Operator;
        use crate::parse_version::PackageVersion;

        let pkg_upstream = PackageVersion::parse(package_version)
            .ok()
            .map(|v| v.upstream);

        for or_depends in and_depends.iter_mut() {
            for pkg_dep in or_depends.iter_mut() {
                for constraint in pkg_dep.constraints.iter_mut() {
                    // Check if this is a "<= package_version" constraint
                    // If so, change it to "< package_version" to avoid self-conflicts after inversion
                    // Also handle cases where the constraint operand matches the upstream version
                    // even if the package version has epoch prefix and release part.
                    // Example: package version "1:8.6.14-1.oe2403sp1" vs constraint "8.6.14"
                    let matches = constraint.operator == Operator::VersionLessThanEqual
                        && (constraint.operand == package_version
                            || (pkg_upstream.is_some()
                                && PackageVersion::parse(&constraint.operand)
                                    .ok()
                                    .map(|v| v.upstream)
                                    == pkg_upstream));

                    if matches {
                        log::debug!(
                            "[RESOLVO] Normalizing self-version constraint (RPM): changing '{} <= {}' to '{} < {}' to avoid self-conflict",
                            pkg_dep.capability,
                            constraint.operand,
                            pkg_dep.capability,
                            constraint.operand
                        );
                        // Change <= to <
                        constraint.operator = Operator::VersionLessThan;
                    }
                }
            }
        }
    }

    /// Invert constraints for conflicts/obsoletes
    ///
    /// Resolvo's constraint mechanism forbids packages that DON'T match the constraint.
    /// So if we want to forbid packages matching "b>1", we need to create a constraint "b<=1",
    /// which will cause resolvo to forbid packages that don't match "b<=1" (i.e., b version 2).
    fn invert_constraints_for_conflicts(
        &self,
        and_depends: &crate::parse_requires::AndDepends,
    ) -> crate::parse_requires::AndDepends {
        use crate::parse_requires::{PkgDepend, VersionConstraint};

        and_depends
            .iter()
            .map(|or_depends| {
                or_depends
                    .iter()
                    .map(|pkg_dep| {
                        let inverted_constraints: Vec<VersionConstraint> =
                            pkg_dep
                                .constraints
                                .iter()
                                .map(|c| self.invert_constraint(c))
                                .collect();
                        PkgDepend {
                            capability: pkg_dep.capability.clone(),
                            constraints: inverted_constraints,
                        }
                    })
                    .collect()
            })
            .collect()
    }

    /// Invert a single constraint operator
    ///
    /// Inverts the operator so that packages matching the original constraint will NOT match
    /// the inverted constraint, and vice versa.
    fn invert_constraint(
        &self,
        constraint: &VersionConstraint,
    ) -> VersionConstraint {
        use crate::parse_requires::Operator;

        let inverted_op = match constraint.operator {
            Operator::VersionGreaterThan => Operator::VersionLessThanEqual,
            Operator::VersionGreaterThanEqual => Operator::VersionLessThan,
            Operator::VersionLessThan => Operator::VersionGreaterThanEqual,
            Operator::VersionLessThanEqual => Operator::VersionGreaterThan,
            Operator::VersionEqual => Operator::VersionNotEqual,
            _ => constraint.operator.clone(), // Keep other operators as-is for now
        };

        VersionConstraint {
            operator: inverted_op,
            operand: constraint.operand.clone(),
        }
    }

    /// Add requirements as constraints to known_deps
    fn add_requirements_to_constraints(
        &self,
        reqs: Vec<ConditionalRequirement>,
        known_deps: &mut KnownDependencies,
    ) {
        for req in reqs {
            match req.requirement {
                resolvo::Requirement::Single(version_set_id) => {
                    known_deps.constrains.push(version_set_id);
                }
                resolvo::Requirement::Union(union_id) => {
                    // Add all version sets in the union
                    for version_set_id in self.version_sets_in_union(union_id) {
                        known_deps.constrains.push(version_set_id);
                    }
                }
            }
        }
    }

}
