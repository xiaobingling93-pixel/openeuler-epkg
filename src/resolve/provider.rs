//! Generic dependency provider implementation
//!
//! This module implements the main GenericDependencyProvider struct that serves
//! as the bridge between the package ecosystem and resolvo's SAT solver. Key features:
//! - Lazy package loading (on-demand from disk/cache)
//! - Package-to-capability lookup with bundled software support
//! - Architecture-aware package filtering
//! - Efficient caching and indexing of loaded packages
//! - Support for multiple dependency fields (Requires, Recommends, etc.)

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Display;
use color_eyre::Result;
use resolvo::{NameId, SolvableId, StringId, ConditionId, Condition, VersionSetUnionId, VersionSetId, Interner};
use crate::PackageFormat;
use crate::Package;
use crate::PACKAGE_CACHE;
use crate::package::pkgkey2pkgname;
use crate::resolve::types::*;

/// Generic dependency provider for all package formats
pub struct GenericDependencyProvider {
    /// Resolvo pool for storing packages and dependencies
    pub pool: resolvo::utils::Pool<SolverMatchSpec, NameType>,

    /// Cache of loaded packages by name/capability
    /// Maps name -> Vec<pkgkey>
    pub loaded_packages: RefCell<HashMap<String, Vec<String>>>,

    /// Maps NameId -> Vec<SolvableId> for candidates
    pub name_to_solvables: RefCell<HashMap<NameId, Vec<SolvableId>>>,

    /// Maps pkgkey -> SolvableId to prevent duplicate solvables
    /// This ensures the same package (by pkgkey) is only added once to the pool
    pub pkgkey_to_solvable: RefCell<HashMap<String, SolvableId>>,

    /// Package format being resolved
    pub format: PackageFormat,

    /// Which dependency fields to use (using RefCell for interior mutability)
    pub depend_fields: RefCell<DependFieldFlags>,

    /// Package names in delta_world (packages being upgraded/installed)
    /// Used to determine which packages should not be favored during upgrade
    pub delta_world_keys: std::collections::HashSet<String>,

    /// Packages in no-install list (should not be installed as dependencies)
    pub no_install: std::collections::HashSet<String>,

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
    pub fn load_packages_for_name(&self, name: &str) -> Result<Vec<String>> {
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
    pub fn get_installed_pkgname2keys(&self) -> Result<HashMap<String, Vec<String>>> {
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
        for (idx, provider_pkgname) in provider_list.iter().enumerate() {
            match crate::package_cache::map_pkgname2packages(provider_pkgname) {
                Ok(mut provider_packages) => {
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
    pub fn check_package_or_capability_exists(&self, name: &str) -> bool {
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
    pub fn load_package_for_solvable(&self, pkgkey: &str) -> Result<Package, StringId> {

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
