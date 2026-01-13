//! Generic resolvo-based dependency solver for all package formats
//!
//! This module implements a dependency resolver using the resolvo SAT solver
//! that works across all supported package formats (RPM, DEB, APK, Conda, ArchLinux/Pacman).
//!
//! Key features:
//! - Lazy/on-demand package loading (only loads packages accessed during solving)
//! - Uses pkgkey as unique package identifier
//! - Supports multiple dependency fields (Requires, BuildRequires, Recommends, Suggests)
//! - Format-aware version comparison and constraint checking

use std::borrow::Cow;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::ops::{BitOr, BitOrAssign};

use color_eyre::Result;
use resolvo::utils::VersionSet;
use crate::models::PACKAGE_CACHE;
use resolvo::{
    Candidates, Condition, ConditionId, ConditionalRequirement, Dependencies, DependencyProvider,
    HintDependenciesAvailable, Interner, KnownDependencies, LogicalOperator, NameId, SolvableId,
    SolverCache, StringId, VersionSetId, VersionSetUnionId,
};

use crate::models::{config, channel_config};
use crate::models::{Package, PackageFormat};
use crate::package::pkgkey2pkgname;
use crate::parse_requires::{AndDepends, Operator, PkgDepend};
use crate::parse_requires::VersionConstraint;
use crate::parse_provides::parse_provides;

/// Represents a set of dependency fields to consider during resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DependFieldFlags(u32);

impl DependFieldFlags {
    pub const NONE: Self = Self(0);
    pub const REQUIRES: Self = Self(1 << 0);
    pub const BUILD_REQUIRES: Self = Self(1 << 1);
    pub const CHECK_REQUIRES: Self = Self(1 << 2);
    pub const RECOMMENDS: Self = Self(1 << 3);
    pub const SUGGESTS: Self = Self(1 << 4);

    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }
}

impl Default for DependFieldFlags {
    fn default() -> Self {
        Self::NONE
    }
}

impl BitOr for DependFieldFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for DependFieldFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Package record for use in resolvo pool
/// Uses pkgkey as unique identifier: {pkgname}__{version}__{arch}
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SolverPackageRecord {
    pub pkgkey: String,
    pub pkgname: String,
    pub version: String,
    pub arch: String,
    pub format: PackageFormat,
}

impl SolverPackageRecord {
    pub fn from_package(package: &Package, format: PackageFormat) -> Self {
        Self {
            pkgkey: package.pkgkey.clone(),
            pkgname: package.pkgname.clone(),
            version: package.version.clone(),
            arch: package.arch.clone(),
            format,
        }
    }
}

impl Ord for SolverPackageRecord {
    fn cmp(&self, other: &Self) -> Ordering {
        self.pkgkey.cmp(&other.pkgkey)
    }
}

impl PartialOrd for SolverPackageRecord {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Display for SolverPackageRecord {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.pkgkey)
    }
}

/// Version set representation for resolvo
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum SolverMatchSpec {
    /// A parsed dependency specification
    MatchSpec(AndDepends),
}

impl VersionSet for SolverMatchSpec {
    type V = SolverPackageRecord;
}

impl Display for SolverMatchSpec {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            SolverMatchSpec::MatchSpec(and_depends) => {
                // Format the dependency specification with constraints
                let parts: Vec<String> = and_depends
                    .iter()
                    .map(|or_depends| {
                        or_depends
                            .iter()
                            .map(|pkg_dep| {
                                if pkg_dep.constraints.is_empty() {
                                    pkg_dep.capability.clone()
                                } else {
                                    let constraint_strs: Vec<String> = pkg_dep.constraints
                                        .iter()
                                        .map(|c| {
                                            let op_str = match c.operator {
                                                crate::parse_requires::Operator::VersionEqual => "=",
                                                crate::parse_requires::Operator::VersionNotEqual => "!=",
                                                crate::parse_requires::Operator::VersionGreaterThanEqual => ">=",
                                                crate::parse_requires::Operator::VersionGreaterThan => ">",
                                                crate::parse_requires::Operator::VersionLessThanEqual => "<=",
                                                crate::parse_requires::Operator::VersionLessThan => "<",
                                                crate::parse_requires::Operator::VersionCompatible => "~",
                                                crate::parse_requires::Operator::IfInstall => "if",
                                            };
                                            format!("{}{}", op_str, c.operand)
                                        })
                                        .collect();
                                    format!("{}({})", pkg_dep.capability, constraint_strs.join(","))
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(" | ")
                    })
                    .collect();
                write!(f, "{}", parts.join(", "))
            }
        }
    }
}

/// Package name type for resolvo pool
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct NameType(pub String);

impl Display for NameType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Ord for NameType {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl PartialOrd for NameType {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Generic dependency provider for all package formats
pub struct GenericDependencyProvider {
    /// Resolvo pool for storing packages and dependencies
    pub pool: resolvo::utils::Pool<SolverMatchSpec, NameType>,

    /// Cache of loaded packages by name/capability
    /// Maps name -> Vec<pkgkey>
    loaded_packages: RefCell<HashMap<String, Vec<String>>>,

    /// Maps NameId -> Vec<SolvableId> for candidates
    name_to_solvables: RefCell<HashMap<NameId, Vec<SolvableId>>>,

    /// Maps pkgkey -> SolvableId to prevent duplicate solvables
    /// This ensures the same package (by pkgkey) is only added once to the pool
    pkgkey_to_solvable: RefCell<HashMap<String, SolvableId>>,

    /// Package format being resolved
    pub format: PackageFormat,

    /// Which dependency fields to use (using RefCell for interior mutability)
    depend_fields: RefCell<DependFieldFlags>,

    /// Package names in delta_world (packages being upgraded/installed)
    /// Used to determine which packages should not be favored during upgrade
    delta_world_keys: std::collections::HashSet<String>,

    /// Packages in no-install list (should not be installed as dependencies)
    no_install: std::collections::HashSet<String>,

    /// Cached installed packages (pkgname -> Vec<pkgkey>) to avoid repeated computation
    /// Allows direct lookup by package name instead of iterating through all candidates
    installed_pkgname2keys: RefCell<Option<HashMap<String, Vec<String>>>>,
}

impl GenericDependencyProvider {
    /// Create a new dependency provider
    pub fn new(
        format: PackageFormat,
        depend_fields: DependFieldFlags,
        delta_world_keys: std::collections::HashSet<String>,
        no_install: std::collections::HashSet<String>,
    ) -> Self {
        Self {
            pool: resolvo::utils::Pool::default(),
            loaded_packages: RefCell::new(HashMap::new()),
            name_to_solvables: RefCell::new(HashMap::new()),
            pkgkey_to_solvable: RefCell::new(HashMap::new()),
            format,
            depend_fields: RefCell::new(depend_fields),
            delta_world_keys,
            no_install,
            installed_pkgname2keys: RefCell::new(None),
        }
    }

    /// Load packages for a given name or capability (lazy loading)
    /// This is called on-demand when get_candidates() is invoked
    fn load_packages_for_name(&self, name: &str) -> Result<Vec<String>> {
        // Check cache first
        if let Some(pkgkeys) = self.check_package_cache(name) {
            return Ok(pkgkeys);
        }

        log::debug!(
            "[RESOLVO] Lazy loading packages for name/capability: {}",
            name
        );

        // Try to find packages by name or capability
        let packages = self.lookup_packages(name)?;

        if packages.is_empty() {
            log::info!("[RESOLVO] No packages found for name/capability: {}", name);
            self.cache_empty_packages(name);
            return Ok(Vec::new());
        }

        // Add packages to pool and return pkgkeys
        // Pass the lookup name so packages found via capability lookup are associated with the capability
        let pkgkeys = self.add_packages_to_pool(name, packages)?;
        Ok(pkgkeys)
    }

    /// Check if packages for a name are already cached
    fn check_package_cache(&self, name: &str) -> Option<Vec<String>> {
        self.loaded_packages.borrow().get(name).cloned()
    }

    /// Initialize or get cached installed packages map (pkgname -> Vec<pkgkey>)
    fn get_installed_pkgname2keys(&self) -> Result<HashMap<String, Vec<String>>> {
        let mut cache = self.installed_pkgname2keys.borrow_mut();
        if cache.is_none() {
            let mut installed_map: HashMap<String, Vec<String>> = HashMap::new();
            for (pkgkey, _) in PACKAGE_CACHE.installed_packages.read().unwrap().iter() {
                if let Ok(pkgname) = pkgkey2pkgname(pkgkey.as_str()) {
                    installed_map.entry(pkgname).or_insert_with(Vec::new).push(pkgkey.clone());
                }
            }
            *cache = Some(installed_map.clone());
            Ok(installed_map)
        } else {
            Ok(cache.as_ref().unwrap().clone())
        }
    }

    /// Update dependency fields for subsequent solving passes
    pub fn update_depend_fields(&self, new_fields: DependFieldFlags) {
        *self.depend_fields.borrow_mut() = new_fields;
    }

    /// Normalize capability name by stripping architecture suffixes like `:any`
    /// Returns the base capability name used for package/provide lookup
    ///
    /// IMPORTANT: This function handles `:any` style arch suffixes (Debian format),
    /// but for RPM-style `(arch)` suffixes, the capability is cap_with_arch which
    /// should NOT be split. When capability contains `(arch)`, we preserve it as-is
    /// for provide lookups, as provide2pkgnames is keyed by cap_with_arch.
    fn normalize_capability_name(&self, capability: &str) -> String {
        let (base_capability, arch_spec) =
            crate::package::parse_capability_architecture(capability, self.format);

        // If arch_spec is Some, it means we successfully parsed an arch suffix
        // For RPM format with (arch), the capability is cap_with_arch and should
        // be preserved as-is for provide lookups. Only strip :any style suffixes.
        if base_capability.is_empty() {
            capability.to_string()
        } else if self.format == PackageFormat::Rpm && arch_spec.is_some() {
            // RPM format: capability is cap_with_arch (e.g., "libfoo(x86-64)")
            // Preserve it as-is for provide lookups, as provide2pkgnames is keyed by cap_with_arch
            capability.to_string()
        } else {
            // Debian format with :any or other formats: use base_capability
            // Note: For provides stored as cap_with_arch, this might not match,
            // but :any dependencies are handled differently in Debian
            base_capability
        }
    }



    /// Lookup packages by name, falling back to capability/provide lookup
    ///
    /// IMPORTANT: When doing capability/provide lookups, the name parameter may be
    /// cap_with_arch (e.g., "libfoo(x86-64)"), which is an atomic tag that should
    /// NEVER be split. The provide2pkgnames index is keyed by cap_with_arch, not
    /// by cap alone. We use the original name for capability lookups to preserve
    /// cap_with_arch integrity.
    fn lookup_packages(&self, name: &str) -> Result<Vec<Package>> {
        // Parse potential architecture suffixes (e.g., python3:any, wine(x86-64))
        // This is only used for package name lookups and arch filtering, NOT for
        // capability lookups (which must use the original name to preserve cap_with_arch)
        let (base_capability, arch_spec) = crate::package::parse_capability_architecture(name, self.format);
        let lookup_name = if base_capability.is_empty() {
            name.to_string()
        } else {
            base_capability
        };

        log::debug!(
            "[RESOLVO] lookup_packages requested='{}', using base='{}', arch_spec={:?}",
            name,
            lookup_name,
            arch_spec
        );

        // Try direct package name lookup first (using base_capability for package names)
        let mut packages = match crate::package_cache::map_pkgname2packages(&lookup_name) {
            Ok(pkgs) => {
                log::debug!(
                    "[RESOLVO] Found {} packages for lookup '{}'",
                    pkgs.len(),
                    lookup_name
                );
                for pkg in &pkgs {
                    log::debug!(
                        "[RESOLVO]   Package: {} (version: {}, arch: {})",
                        pkg.pkgkey,
                        pkg.version,
                        pkg.arch
                    );
                }
                pkgs
            }
            Err(e) => {
                log::info!(
                    "[RESOLVO] No packages found for lookup '{}': {}",
                    lookup_name,
                    e
                );
                Vec::new()
            }
        };

        // If no packages found, try capability/provide lookup
        // CRITICAL: Use the original name (which may be cap_with_arch) for capability
        // lookups, NOT base_capability. The provide2pkgnames index is keyed by
        // cap_with_arch, which is atomic and should never be split.
        let found_via_capability = packages.is_empty();
        if found_via_capability {
            packages = self.lookup_packages_by_capability(name)?;
        }

        // Apply architecture-specific filtering when required (e.g., :any, :amd64)
        // IMPORTANT: Capability lookups are architecture-agnostic tags (either pure cap
        // or cap_with_arch). The provide2pkgnames index is keyed by these tags, and
        // architecture filtering should NOT be applied to capability lookups. We return
        // all packages that provide the exact capability tag, regardless of their architecture.
        // The provide2pkgnames lookup already filters by the exact tag (e.g., "libX11.so.6"
        // vs "libX11.so.6()(64bit)" are different tags in the index).
        //
        // Example: When an i686 package requires "libX11.so.6" (no architecture suffix):
        // - Looking up "libX11.so.6" in provide2pkgnames returns only packages that provide
        //   the exact tag "libX11.so.6" (typically i686 packages)
        // - Packages that provide "libX11.so.6()(64bit)" are NOT returned because that's
        //   a different tag in the provide2pkgnames index
        // - We skip architecture filtering here because capabilities are tags, not architecture-
        //   specific package names. The tag matching already happened in lookup_packages_by_capability
        let filtered_packages = if found_via_capability {
            // Capability lookup: skip architecture filtering entirely
            // Capabilities are just tags (pure cap or cap_with_arch), not architecture-specific
            log::debug!(
                "[RESOLVO] Capability lookup: skipping arch filtering for '{}' (keeping {} packages)",
                name,
                packages.len()
            );
            packages
        } else {
            // Package name lookup: apply normal architecture filtering
            log::debug!(
                "[RESOLVO] Before arch filtering: {} packages for '{}' (arch_spec: {:?}, format: {:?})",
                packages.len(),
                name,
                arch_spec,
                self.format
            );
            let filtered = crate::package::filter_packages_by_arch_spec(packages, arch_spec.as_deref(), self.format);
            log::debug!(
                "[RESOLVO] After arch filtering: {} packages for '{}'",
                filtered.len(),
                name
            );
            filtered
        };

        Ok(filtered_packages)
    }

    /// Lookup packages by capability/provide
    ///
    /// IMPORTANT: The name parameter is cap_with_arch (e.g., "libfoo(x86-64)"),
    /// which is an atomic tag that should NEVER be split. The provide2pkgnames
    /// index is keyed by cap_with_arch, not by cap alone. We use name directly
    /// without any splitting or arch stripping.
    ///
    /// ## Bundled Software Support (Fedora Policy)
    ///
    /// This function also checks for `bundled()` variants. According to Fedora's
    /// Bundled Software Policy (https://docs.fedoraproject.org/en-US/fesco/Bundled_Software_policy/),
    /// packages that bundle libraries must include `Provides: bundled(library) = version`
    /// in their RPM spec file.
    ///
    /// When a dependency requires a capability like "php-composer(doctrine/cache)",
    /// this function will also check for "bundled(php-composer(doctrine/cache))"
    /// since packages may provide the capability as bundled software. This allows
    /// dependencies to be satisfied by packages that bundle the required library,
    /// which is essential for resolving dependencies in Fedora where many packages
    /// bundle PHP Composer libraries.
    ///
    /// Example: If a package requires "php-composer(doctrine/cache)" and another
    /// package provides "bundled(php-composer(doctrine/cache))", the resolver will
    /// find and use the bundled provider to satisfy the dependency.
    fn lookup_packages_by_capability(
        &self,
        name: &str,
    ) -> Result<Vec<Package>> {
        let mut packages = Vec::new();
        let mut found_pkgnames = std::collections::HashSet::new();

        // Generate capability variants to check:
        // 1. The original capability (name)
        // 2. bundled(name) - in case the capability is provided as bundled
        let capability_variants = vec![
            name.to_string(),
            format!("bundled({})", name),
        ];

        // name is cap_with_arch (atomic, never split)
        // First, try mmio lookup (for production/repo data)
        for variant in &capability_variants {
            match crate::mmio::map_provide2pkgnames(variant) {
                Ok(provider_pkgnames) => {
                    log::debug!(
                        "[RESOLVO] map_provide2pkgnames('{}') returned {} provider names",
                        variant,
                        provider_pkgnames.len()
                    );
                    if !provider_pkgnames.is_empty() {
                        log::debug!(
                            "[RESOLVO] First provider: '{}', last provider: '{}'",
                            provider_pkgnames.first().unwrap(),
                            provider_pkgnames.last().unwrap()
                        );
                    }
                    for provider_pkgname in provider_pkgnames {
                        found_pkgnames.insert(provider_pkgname);
                    }
                }
                Err(e) => {
                    log::debug!(
                        "[RESOLVO] map_provide2pkgnames('{}') failed: {}",
                        variant,
                        e
                    );
                }
            }
        }

        // Also check PackageManager's in-memory index (for tests and in-memory packages)
        // name is cap_with_arch (atomic, never split)
        for variant in &capability_variants {
            if let Some(provider_pkgnames) = PACKAGE_CACHE.provide2pkgnames.read().unwrap().get(variant) {
                log::debug!(
                    "[RESOLVO] Found {} provider names in PackageManager cache for '{}'",
                    provider_pkgnames.len(),
                    variant
                );
                for provider_pkgname in provider_pkgnames.iter() {
                    found_pkgnames.insert(provider_pkgname.clone());
                }
            }
        }

        log::debug!(
            "[RESOLVO] Total {} unique provider names found for capability '{}'",
            found_pkgnames.len(),
            name
        );

        // Convert to sorted vector for deterministic iteration order (helps with debugging)
        let mut provider_list: Vec<String> = found_pkgnames.into_iter().collect();
        provider_list.sort();

        log::debug!(
            "[RESOLVO] Processing {} providers for capability '{}' (first: {:?}, last: {:?})",
            provider_list.len(),
            name,
            provider_list.first(),
            provider_list.last()
        );

        // Look up packages for all found provider names
        let mut success_count = 0;
        let mut fail_count = 0;
        for (idx, provider_pkgname) in provider_list.iter().enumerate() {
            match crate::package_cache::map_pkgname2packages(provider_pkgname) {
                Ok(mut provider_packages) => {
                    success_count += 1;
                    log::debug!(
                        "[RESOLVO] [{}/{}] Found {} packages for provider '{}'",
                        idx + 1,
                        provider_list.len(),
                        provider_packages.len(),
                        provider_pkgname
                    );
                    packages.append(&mut provider_packages);
                }
                Err(e) => {
                    fail_count += 1;
                    log::debug!(
                        "[RESOLVO] [{}/{}] Failed to load packages for provider '{}': {}",
                        idx + 1,
                        provider_list.len(),
                        provider_pkgname,
                        e
                    );
                    continue;
                }
            }
        }

        log::debug!(
            "[RESOLVO] Provider lookup summary for '{}': {} succeeded, {} failed",
            name,
            success_count,
            fail_count
        );

        log::debug!(
            "[RESOLVO] lookup_packages_by_capability('{}') returning {} total packages",
            name,
            packages.len()
        );

        Ok(packages)
    }

    /// Add packages to the resolvo pool and update indexes
    fn add_packages_to_pool(
        &self,
        lookup_name: &str,
        packages: Vec<Package>,
    ) -> Result<Vec<String>> {
        let mut pkgkeys = Vec::new();

        log::debug!(
            "[RESOLVO] Adding {} packages to pool for lookup name '{}'",
            packages.len(),
            lookup_name
        );

        // Create NameId for the lookup name (could be package name or capability)
        let lookup_name_id = self
            .pool
            .intern_package_name(NameType(lookup_name.to_string()));

        for package in packages {
            let pkgkey = package.pkgkey.clone();
            let record = SolverPackageRecord::from_package(&package, self.format);

            // Check if this package (by pkgkey) has already been added to the pool
            let solvable_id = {
                let pkgkey_map = self.pkgkey_to_solvable.borrow();
                if let Some(&existing_solvable_id) = pkgkey_map.get(&pkgkey) {
                    // Package already exists in pool, reuse existing solvable
                    log::debug!(
                        "[RESOLVO] Package {} already exists in pool as solvable {}, reusing",
                        pkgkey,
                        existing_solvable_id.0
                    );
                    existing_solvable_id
                } else {
                    drop(pkgkey_map); // Release borrow before creating new solvable

                    // Use the package's actual pkgname to create the name_id for the solvable
                    let package_name_id = self
                        .pool
                        .intern_package_name(NameType(package.pkgname.clone()));

                    log::debug!(
                        "[RESOLVO] Adding package {} (pkgname: {}, version: {}) with name_id for '{}'",
                        pkgkey,
                        package.pkgname,
                        package.version,
                        package.pkgname
                    );

                    // Create solvable in pool (associated with package name)
                    let new_solvable_id = self.pool.intern_solvable(package_name_id, record);

                    // Store mapping from pkgkey to solvable_id
                    self.pkgkey_to_solvable
                        .borrow_mut()
                        .insert(pkgkey.clone(), new_solvable_id);

                    new_solvable_id
                }
            };

            // Use the package's actual pkgname to create the name_id for indexing
            let package_name_id = self
                .pool
                .intern_package_name(NameType(package.pkgname.clone()));

            // Update indexes - associate solvable with the package's actual name
            // Only add if not already present (avoid duplicates)
            let mut name_map = self.name_to_solvables.borrow_mut();
            let solvables_for_name = name_map.entry(package_name_id).or_insert_with(Vec::new);
            if !solvables_for_name.contains(&solvable_id) {
                solvables_for_name.push(solvable_id);
            }

            // If lookup name is different from package name (capability lookup),
            // also associate the solvable with the lookup name (capability)
            if lookup_name != package.pkgname {
                let solvables_for_lookup = name_map.entry(lookup_name_id).or_insert_with(Vec::new);
                if !solvables_for_lookup.contains(&solvable_id) {
                    solvables_for_lookup.push(solvable_id);
                }
            }
            drop(name_map); // Release borrow

            pkgkeys.push(pkgkey);
        }

        // Cache the result (still using lookup name for cache key)
        self.loaded_packages
            .borrow_mut()
            .insert(lookup_name.to_string(), pkgkeys.clone());

        log::debug!(
            "[RESOLVO] Loaded {} packages for '{}'",
            pkgkeys.len(),
            lookup_name
        );
        Ok(pkgkeys)
    }

    /// Cache empty result for a package name
    fn cache_empty_packages(&self, name: &str) {
        self.loaded_packages
            .borrow_mut()
            .insert(name.to_string(), Vec::new());
    }

    /// Check if a package or capability exists
    /// Returns true if the package/capability exists, false otherwise
    fn check_package_or_capability_exists(&self, name: &str) -> bool {
        // Check cache first - if it's in cache, it was already looked up
        if let Some(pkgkeys) = self.check_package_cache(name) {
            return !pkgkeys.is_empty();
        }

        match self.lookup_packages(name) {
            Ok(packages) => !packages.is_empty(),
            Err(_) => false,
        }
    }

    /// Load package info for a solvable, returning StringId for error message if failed
    fn load_package_for_solvable(&self, pkgkey: &str) -> Result<Package, StringId> {

        match crate::package_cache::load_package_info(pkgkey) {
            Ok(pkg) => Ok((*pkg).clone()),
            Err(e) => {
                let reason = self
                    .pool
                    .intern_string(format!("Failed to load package {}: {}", pkgkey, e));
                Err(reason)
            }
        }
    }

    /// Check if a package provides a given capability
    ///
    /// IMPORTANT: The capability parameter should be cap_with_arch (e.g., "libfoo(x86-64)")
    /// if it includes architecture information. The function strips version constraints
    /// but preserves cap_with_arch, which is then matched against provide names that
    /// are also cap_with_arch (preserved by parse_provides).
    ///
    /// ## Bundled Software Support (Fedora Policy)
    ///
    /// This function also checks for `bundled()` variants. According to Fedora's
    /// Bundled Software Policy (https://docs.fedoraproject.org/en-US/fesco/Bundled_Software_policy/),
    /// packages that bundle libraries must include `Provides: bundled(library) = version`.
    ///
    /// When checking if a package provides a capability, this function will match both:
    /// - Direct provides: if package provides "php-composer(doctrine/cache)"
    /// - Bundled provides: if package provides "bundled(php-composer(doctrine/cache))"
    ///
    /// This ensures that packages providing bundled libraries can satisfy dependencies
    /// that require the unbundled capability name.
    pub fn package_provides_capability(&self, pkgkey: &str, capability: &str) -> bool {
        // Try to load package info
        let package = match self.load_package_for_solvable(pkgkey) {
            Ok(pkg) => pkg,
            Err(_) => return false,
        };

        // Strip version constraints from capability (if any)
        // This preserves cap_with_arch (e.g., "libfoo(x86-64)=2.0" -> "libfoo(x86-64)")
        let (cap_without_version, _) =
            crate::parse_requires::parse_package_spec_with_version(capability, self.format);

        // Check if any provide matches the capability
        // provide_map from parse_provides contains cap_with_arch keys (atomic, never split)
        // Also check for bundled() variants: if looking for "cap", also check "bundled(cap)"
        let bundled_variant = format!("bundled({})", cap_without_version);
        for provide_str in &package.provides {
            let provide_map = parse_provides(provide_str, self.format);
            for (provide_name, _version) in provide_map {
                // Both provide_name and cap_without_version are cap_with_arch (atomic)
                // Check direct match
                if provide_name == cap_without_version {
                    log::debug!(
                        "[RESOLVO] package_provides_capability: {} provides '{}' via provide '{}'",
                        pkgkey,
                        cap_without_version,
                        provide_str
                    );
                    return true;
                }
                // Check bundled variant: if package provides "bundled(cap)", it also provides "cap"
                if provide_name == bundled_variant {
                    log::debug!(
                        "[RESOLVO] package_provides_capability: {} provides '{}' via bundled provide '{}'",
                        pkgkey,
                        cap_without_version,
                        provide_str
                    );
                    return true;
                }
            }
        }

        // If capability looks like a file path (starts with '/'), also check the files field
        if cap_without_version.starts_with('/') {
            for file_path in &package.files {
                // Check for exact match
                if file_path == &cap_without_version {
                    log::debug!(
                        "[RESOLVO] package_provides_capability: {} provides '{}' via file '{}'",
                        pkgkey,
                        cap_without_version,
                        file_path
                    );
                    return true;
                }
            }
        }

        log::trace!(
            "[RESOLVO] package_provides_capability: {} does NOT provide '{}'",
            pkgkey,
            cap_without_version
        );
        false
    }

    /// Process dependency requirements from a package
    fn process_requirements(&self, package: &Package, known_deps: &mut KnownDependencies) {
        let dep_strings = self.get_dependency_strings(package);

        for dep_str in dep_strings {
            // Parse the dependency string
            let and_depends = match crate::parse_requires::parse_requires(self.format, &dep_str) {
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

    /// Process conflicts or obsoletes from a package
    ///
    /// Both conflicts and obsoletes prevent installation of matching packages and need constraint
    /// inversion because resolvo's constraint mechanism forbids packages that DON'T match the
    /// constraint. So if package A conflicts with or obsoletes b>1, we need to create a constraint
    /// b<=1, which will cause resolvo to forbid packages that don't match b<=1 (i.e., b version 2).
    fn process_conflicts_or_obsoletes(
        &self,
        dependency_strings: &[String],
        dep_type: &str,
        known_deps: &mut KnownDependencies,
        package: &Package,
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
                        let is_self_constraint = if let Ok(pkgname) = pkgkey2pkgname(pkgkey) {
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
                        let provider_pkg = match crate::package_cache::load_package_info(pkgkey) {
                            Ok(pkg) => pkg,
                            Err(_) => return Some(pkg_dep.clone()), // Can't load package, allow constraint
                        };

                        // Check if the package's provided version satisfies the constraints
                        match crate::provides::check_provider_satisfies_constraints(
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
    fn convert_and_depends_to_requirements(
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
                    log::warn!(
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

        // Direct lookup by package name
        if let Some(installed_pkgkeys) = installed_pkgname2keys.get(name_string) {
            // Find the first candidate whose pkgkey is in the installed list
            candidates_vec.iter().find_map(|&solvable_id| {
                let record = &self.pool.resolve_solvable(solvable_id).record;
                if installed_pkgkeys.contains(&record.pkgkey) {
                    // For upgrade/install commands, check if package is in delta_world
                    // If so, don't favor installed version to allow upgrade/install to latest
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
            })
        } else {
            None
        }
    }

    /// Log detailed debug information about dependencies
    fn log_dependencies_debug(&self, known_deps: &KnownDependencies) {
        // Log individual requirements if debug level
        if log::log_enabled!(log::Level::Debug) {
            for req in &known_deps.requirements {
                let req_str = match &req.requirement {
                    resolvo::Requirement::Single(version_set_id) => {
                        let req_name = self.version_set_name(*version_set_id);
                        let req_name_str = self.display_name(req_name);
                        let req_spec = self.display_version_set(*version_set_id);
                        format!("{}: {}", req_name_str, req_spec)
                    }
                    resolvo::Requirement::Union(union_id) => {
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
}

impl Interner for GenericDependencyProvider {
    fn display_solvable(&self, solvable: SolvableId) -> impl Display + '_ {
        let record = &self.pool.resolve_solvable(solvable).record;
        format!("{}", record.pkgkey)
    }

    fn resolve_condition(&self, condition: ConditionId) -> Condition {
        self.pool.resolve_condition(condition).clone()
    }

    fn version_sets_in_union(
        &self,
        version_set_union: VersionSetUnionId,
    ) -> impl Iterator<Item = VersionSetId> {
        self.pool.resolve_version_set_union(version_set_union)
    }

    fn display_merged_solvables(&self, solvables: &[SolvableId]) -> impl Display + '_ {
        if solvables.is_empty() {
            return String::new();
        }

        let versions: Vec<String> = solvables
            .iter()
            .map(|&id| {
                let record = &self.pool.resolve_solvable(id).record;
                record.version.clone()
            })
            .collect();

        let name = self.display_solvable_name(solvables[0]);
        format!("{} {}", name, versions.join(" | "))
    }

    fn display_name(&self, name: NameId) -> impl Display + '_ {
        self.pool.resolve_package_name(name)
    }

    fn display_version_set(&self, version_set: VersionSetId) -> impl Display + '_ {
        self.pool.resolve_version_set(version_set)
    }

    fn display_string(&self, string_id: StringId) -> impl Display + '_ {
        self.pool.resolve_string(string_id)
    }

    fn version_set_name(&self, version_set: VersionSetId) -> NameId {
        self.pool.resolve_version_set_package_name(version_set)
    }

    fn solvable_name(&self, solvable: SolvableId) -> NameId {
        self.pool.resolve_solvable(solvable).name
    }
}

impl DependencyProvider for GenericDependencyProvider {
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
