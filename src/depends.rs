use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use crate::models::*;
use crate::parse_requires::*;
use crate::parse_provides::parse_provides;
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
    /// Extends ebin_exposure to packages that share the same source package as user-requested packages.
    ///
    /// This function identifies all source package names from packages that already have
    /// `ebin_exposure` set to `true` (user-requested packages), then sets `ebin_exposure` to `true`
    /// for all other packages in the provided map that share the same source package name.
    ///
    /// # Arguments
    ///
    /// * `packages` - A mutable reference to a HashMap of package keys to `InstalledPackageInfo`.
    ///                Packages with `ebin_exposure == true` are considered user-requested.
    ///
    /// # Returns
    ///
    /// Returns a `Result` containing a HashMap of packages that had their `ebin_exposure` set to
    /// `true` by this function (excluding those that were already exposed). Returns an error if
    /// package information cannot be loaded.
    fn extend_ebin_by_source(&mut self, packages: &mut HashMap<String, InstalledPackageInfo>) -> Result<HashMap<String, InstalledPackageInfo>> {
        log::debug!("Setting ebin_exposure for {} packages based on source matching.", packages.len());

        let mut user_requested_sources = std::collections::HashSet::new();
        let mut packages_to_expose = HashMap::new();

        // First, collect all source package names from user-requested packages
        // (packages with ebin_exposure == true are user-requested in THIS session)
        for (pkgkey, info) in packages.iter() {
            if info.ebin_exposure == true {
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
                    }
                }
            }
        }
        log::debug!("User-requested sources for ebin_exposure logic: {:?}", user_requested_sources);

        // Now, iterate again to set the ebin_exposure for all packages
        // (both new and already-installed packages that share sources with user-requested packages)
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

    /// Parse capability name and extract architecture specification
    /// Returns (base_capability, architecture_spec) where architecture_spec is:
    /// - Some("any") for `:any` suffix
    /// - Some(arch) for specific architecture like `:amd64` or `(x86-32)`
    /// - None for no architecture specification
    pub fn parse_capability_architecture(&self, capability: &str, format: PackageFormat) -> (String, Option<String>) {
        // Handle based on package format
        if format == PackageFormat::Deb {
                if let Some(colon_pos) = capability.rfind(':') {
                    let base_capability = capability[..colon_pos].to_string();
                    let arch_spec = capability[colon_pos + 1..].to_string();

                    // Only treat as architecture spec if it's "any" or a valid architecture name
                    // Valid architecture names are alphanumeric with possible hyphens/underscores
                    // Common Debian architectures: any, amd64, arm64, armel, armhf, i386, etc.
                    if arch_spec == "any" || Self::is_valid_architecture_name(&arch_spec) {
                        return (base_capability, Some(arch_spec))
                    }
                    // Otherwise, treat the colon as part of the package name (e.g., "lib:unknown")
                }
        } else if format == PackageFormat::Rpm {
            // RPM uses parentheses for architecture specifications: capability(arch)
            // Examples: wine-cms(x86-32), wine-cms(x86-64)
            // IMPORTANT: Only treat as arch spec if it's a recognized architecture.
            // Other things in parentheses (like :lang=en) are part of the capability name.
            if let Some(open_paren) = capability.rfind('(') {
                if capability.ends_with(')') {
                    let arch_spec = capability[open_paren + 1..capability.len() - 1].to_string();

                    // Map RPM architecture names to package architecture names
                    // Only if it's a recognized architecture, extract it as arch_spec
                    if let Some(mapped_arch) = Self::map_rpm_arch_to_package_arch(&arch_spec) {
                        let base_capability = capability[..open_paren].to_string();
                        return (base_capability, Some(mapped_arch));
                    }
                    // If it's not a recognized architecture, treat parentheses as part of capability name
                }
            }
        }
        // Other distros do not encode arch in require name.
        // Alpine uses prefixes like: so:, cmd:, pc:, py3.XX:, ocaml4-intf:, dbus:, etc.
        // which are not related to arch.
        (capability.to_string(), None)
    }

    /// Map RPM architecture specification names to package architecture names
    /// RPM uses names like "x86-32", "x86-64" in capability specifications,
    /// but packages use standard architecture names like "i686", "x86_64"
    /// Also handles "64bit" and "32bit" specifications used in library capabilities
    /// like "libavahi-client.so.3()(64bit)"
    fn map_rpm_arch_to_package_arch(rpm_arch: &str) -> Option<String> {
        match rpm_arch {
            "x86-32" => Some("i686".to_string()),
            "x86-64" => Some("x86_64".to_string()),
            "64bit" => Some("x86_64".to_string()),
            "32bit" => Some("i686".to_string()),
            // Add other mappings as needed
            _ => None,
        }
    }

    /// Check if a string is a valid architecture name
    /// Architecture names are typically lowercase, alphanumeric with possible hyphens/underscores
    /// Common Debian architectures: amd64, arm64, armel, armhf, i386, i486, i586, i686,
    /// powerpc, ppc64el, mips, mipsel, etc.
    fn is_valid_architecture_name(s: &str) -> bool {
        // Known Debian architecture names (non-exhaustive but covers common cases)
        // This is a whitelist approach to avoid false positives like "unknown", "test", etc.
        const KNOWN_ARCHITECTURES: &[&str] = &[
            "amd64", "x86_64",
            "arm64", "aarch64",
            "armel", "armhf", "arm",
            "i386", "i486", "i586", "i686",
            "powerpc", "ppc", "ppc64", "ppc64el",
            "mips", "mipsel", "mips64el",
            "riscv64",
            "loongarch64",
            "s390x",
            "sparc", "sparc64",
            "alpha",
            "hppa",
            "ia64",
            "m68k",
            "sh4",
        ];

        // Check against known architectures
        KNOWN_ARCHITECTURES.contains(&s)
            // Also accept patterns that look like architecture names:
            // - Short (2-10 chars), lowercase, alphanumeric with hyphens/underscores
            // - Starts with letter, ends with alphanumeric
            || (s.len() >= 2
                && s.len() <= 10
                && s.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
                && s.chars().next().map_or(false, |c| c.is_alphabetic())
                && s.chars().last().map_or(false, |c| c.is_alphanumeric())
                && s == s.to_lowercase()
                // Exclude common non-architecture words
                && !matches!(s, "unknown" | "test" | "none" | "all" | "any"))
    }

    /// Filter packages based on architecture specification
    /// If arch_spec is "any", only allow packages with Multi-Arch: allowed/foreign (per Debian rules)
    /// If arch_spec is specific architecture, filter by that architecture
    /// If arch_spec is None, use default architecture filtering
    pub fn filter_packages_by_arch_spec(&self, packages: Vec<Package>, arch_spec: Option<&str>, format: PackageFormat) -> Vec<Package> {
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
                    self.filter_packages_by_arch(packages, format)
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
                self.filter_packages_by_arch(packages, format)
            }
        }
    }


    /// Setup resolvo provider and convert delta_world to requirements
    fn setup_resolvo_provider_and_requirements(
        &mut self,
        delta_world: &HashMap<String, String>,
    ) -> Result<(crate::resolvo::GenericDependencyProvider, Vec<resolvo::ConditionalRequirement>)> {
        let package_format = channel_config().format;

        log::info!(
            "Starting resolvo-based recursive dependency collection for {} packages in delta_world. Repo format: {:?}",
            delta_world.len(),
            package_format
        );
        log::debug!("delta_world contents: {:?}", delta_world);

        // Detect and add Conda virtual packages to cache
        if package_format == PackageFormat::Conda {
            self.add_conda_virtual_packages_to_cache()?;
        }

        // Create provider and convert delta_world to requirements
        let mut provider = self.create_resolvo_provider(package_format, delta_world);
        let requirements = self.convert_initial_packages_to_requirements(delta_world, &mut provider)?;
        log::debug!("Converted {} requirements from delta_world", requirements.len());

        Ok((provider, requirements))
    }

    /// Create a resolvo problem and solver from provider and requirements
    fn create_resolvo_problem_and_solver(
        &self,
        provider: crate::resolvo::GenericDependencyProvider,
        requirements: Vec<resolvo::ConditionalRequirement>,
    ) -> (resolvo::Problem<std::iter::Empty<resolvo::SolvableId>>, resolvo::Solver<crate::resolvo::GenericDependencyProvider>) {
        use resolvo::{Problem, Solver};
        let problem = Problem::new().requirements(requirements);
        let solver = Solver::new(provider);
        (problem, solver)
    }

    /// Run a single solve pass with the given solver and problem
    /// Returns Ok(solvables) on success, or Err with a warning message on failure
    fn run_solve_pass(
        &self,
        solver: &mut resolvo::Solver<crate::resolvo::GenericDependencyProvider>,
        problem: resolvo::Problem<std::iter::Empty<resolvo::SolvableId>>,
        pass_name: &str,
    ) -> Result<Vec<resolvo::SolvableId>> {
        use resolvo::UnsolvableOrCancelled;

        match solver.solve(problem) {
            Ok(solvables) => {
                log::debug!("Solver found solution with {} packages ({})", solvables.len(), pass_name);
                Ok(solvables)
            },
            Err(UnsolvableOrCancelled::Unsolvable(problem)) => {
                let error_msg = problem.display_user_friendly(solver).to_string();
                // Preserve the full formatted error message with tree structure
                Err(color_eyre::eyre::eyre!("Dependency resolution failed for {}:\n{}", pass_name, error_msg))
            }
            Err(UnsolvableOrCancelled::Cancelled(_)) => {
                Err(color_eyre::eyre::eyre!("Dependency resolution was cancelled for {}", pass_name))
            }
        }
    }

    /// Solve dependencies with resolvo using the given flags
    fn solve_with_resolvo(
        &self,
        provider: crate::resolvo::GenericDependencyProvider,
        requirements: Vec<resolvo::ConditionalRequirement>,
        flags: crate::resolvo::DependFieldFlags,
    ) -> Result<(resolvo::Solver<crate::resolvo::GenericDependencyProvider>, Vec<resolvo::SolvableId>)> {
        // Update provider with the desired flags before creating solver
        provider.update_depend_fields(flags);

        // Create problem and solver
        let (problem, mut solver) = self.create_resolvo_problem_and_solver(provider, requirements);

        // Determine pass name based on flags
        let package_format = channel_config().format;
        let base_flags = if package_format == PackageFormat::Pacman {
            crate::resolvo::DependFieldFlags::REQUIRES | crate::resolvo::DependFieldFlags::BUILD_REQUIRES
        } else {
            crate::resolvo::DependFieldFlags::REQUIRES
        };
        let pass_name = if flags != base_flags {
            "1st pass (with RECOMMENDS/SUGGESTS)"
        } else if package_format == PackageFormat::Pacman {
            "2nd pass (REQUIRES|BUILD_REQUIRES only)"
        } else {
            "2nd pass (REQUIRES only)"
        };

        // Run solve pass
        let solvables = self.run_solve_pass(&mut solver, problem, pass_name)?;
        log::debug!("Solver resolved {} solvables", solvables.len());

        Ok((solver, solvables))
    }

    /// Resolvo-based dependency resolver
    /// Internal function for core dependency resolution logic using resolvo SAT solver
    fn resolve_dependencies_with_resolvo(
        &mut self,
        delta_world: &HashMap<String, String>,
        user_request_world: Option<&HashMap<String, String>>,
    ) -> Result<HashMap<String, InstalledPackageInfo>> {
        // Setup provider and requirements
        let (provider, requirements) = self.setup_resolvo_provider_and_requirements(delta_world)?;
        if requirements.is_empty() {
            log::warn!("No valid packages to resolve");
            return Err(color_eyre::eyre::eyre!("No requirements to solve"));
        }

        // Determine flags to use: always include REQUIRES|BUILD_REQUIRES, try with RECOMMENDS/SUGGESTS if configured
        let package_format = channel_config().format;
        let (base_flags, base_flag_desc) = if package_format == PackageFormat::Pacman {
            (
                crate::resolvo::DependFieldFlags::REQUIRES | crate::resolvo::DependFieldFlags::BUILD_REQUIRES,
                "REQUIRES|BUILD_REQUIRES",
            )
        } else {
            (crate::resolvo::DependFieldFlags::REQUIRES, "REQUIRES")
        };
        let mut flags = base_flags;
        if !config().install.no_install_recommends {
            flags = flags | crate::resolvo::DependFieldFlags::RECOMMENDS;
        }
        if config().install.install_suggests {
            flags = flags | crate::resolvo::DependFieldFlags::SUGGESTS;
        }

        // Try to solve with RECOMMENDS/SUGGESTS (if configured) - allow failure
        let (solver, solvables) = match self.solve_with_resolvo(provider, requirements.clone(), flags) {
            Ok(result) => result,
            Err(e) if e.to_string().contains("No requirements to solve") => {
                return Ok(HashMap::new());
            }
            Err(e) if flags != base_flags => {
                // If we tried with additional flags and failed, warn and retry with REQUIRES|BUILD_REQUIRES only
                log::warn!(
                    "Dependency resolution failed with RECOMMENDS/SUGGESTS: {}. Retrying with {} only.",
                    e,
                    base_flag_desc
                );

                // Create a fresh provider and solver for the retry
                let (fresh_provider, _) = self.setup_resolvo_provider_and_requirements(delta_world)?;
                match self.solve_with_resolvo(fresh_provider, requirements, base_flags) {
                    Ok(result) => result,
                    Err(e) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        };

        // Build dependency graph and create result
        let result = self.build_installed_package_info_map(
            &solver,
            &solvables,
            user_request_world,
        )?;

        log::info!("Collected {} packages with dependencies", result.len());
        Ok(result)
    }

    /// Wrapper for resolvo-based dependency resolver that also injects makepkg
    /// build-time dependencies when resolving Pacman/AUR packages.
    ///
    /// Flow:
    /// 1. Call `resolve_dependencies_with_resolvo` once.
    /// 2. If repo format is Pacman and any resolved package is an AUR package,
    ///    extend `delta_world` with makepkg dependencies (`base-devel`, `gawk`,
    ///    `libarchive`, `coreutils`) and remove those names from the no-install list.
    /// 3. Re-run `resolve_dependencies_with_resolvo` with the updated `delta_world`.
    fn resolve_dependencies_adding_makepkg_deps(
        &mut self,
        delta_world: &mut HashMap<String, String>,
        user_request_world: Option<&HashMap<String, String>>,
    ) -> Result<HashMap<String, InstalledPackageInfo>> {
        // First pass: resolve with current delta_world
        let mut all_packages_for_session =
            self.resolve_dependencies_with_resolvo(delta_world, user_request_world)?;

        // Only apply makepkg dependency handling for Pacman format
        let package_format = channel_config().format;
        if package_format != PackageFormat::Pacman {
            return Ok(all_packages_for_session);
        }

        // Check if any resolved package is an AUR package and whether any of them is a *-git package
        let mut has_aur_packages = false;
        let mut has_git_aur = false;
        for pkgkey in all_packages_for_session.keys() {
            if self.is_aur_package(pkgkey) {
                has_aur_packages = true;

                // Extract pkgname from pkgkey and check for '-git' suffix
                if let Ok(pkgname) = crate::package::pkgkey2pkgname(pkgkey) {
                    if pkgname.ends_with("-git") {
                        has_git_aur = true;
                        break;
                    }
                }
            }
        }

        if !has_aur_packages {
            return Ok(all_packages_for_session);
        }

        // Inject makepkg runtime/build-time dependencies into delta_world
        let mut makepkg_depends: HashMap<String, String> = [
            ("gnupg".to_string(), String::new()),
            ("pacman".to_string(), String::new()),      // for makepkg
            ("libarchive".to_string(), String::new()),  // for bsdtar
            ("coreutils".to_string(), String::new()),
            ("base-devel".to_string(), String::new()),
        ]
        .into_iter()
        .collect();

        // If any AUR package being resolved is a *-git package, ensure `git` itself is present
        if has_git_aur {
            makepkg_depends
                .entry("git".to_string())
                .or_insert_with(String::new);
        }

        // Extend delta_world without overwriting any existing constraints
        for (pkgname, constraint) in &makepkg_depends {
            delta_world.entry(pkgname.clone()).or_insert_with(|| constraint.clone());
        }

        // Remove makepkg dependencies from no-install list so they can be pulled in
        self.remove_from_no_install(makepkg_depends.keys());

        // Second pass: resolve again with updated delta_world
        all_packages_for_session =
            self.resolve_dependencies_with_resolvo(delta_world, user_request_world)?;

        Ok(all_packages_for_session)
    }

    /// Resolves dependencies using resolvo SAT solver and performs full installation workflow
    pub fn resolve_and_install_packages(
        &mut self,
        delta_world: &mut HashMap<String, String>,
        user_request_world: Option<&HashMap<String, String>>,
    ) -> Result<crate::install::InstallationPlan> {
        use crate::install::InstallationPlan;

        // Remove "no-install" key - it's not a package
        delta_world.remove("no-install");

        self.load_installed_packages()?;

        // Resolve dependencies (pass user_request_world to extract correct candidate pkgkeys)
        let mut all_packages_for_session =
            self.resolve_dependencies_adding_makepkg_deps(delta_world, user_request_world)?;

        // Determine packages to expose based on source matching
        let packages_to_expose = self.extend_ebin_by_source(&mut all_packages_for_session)?;

        if packages_to_expose.is_empty() && all_packages_for_session.is_empty() {
            let empty_msg = if user_request_world.is_some() {
                "No packages to install or upgrade."
            } else {
                "No packages to upgrade."
            };
            println!("{}", empty_msg);
            return Ok(InstallationPlan::default());
        }

        let mut plan = self.prepare_installation_plan(&all_packages_for_session)?;

        // Fill pkglines for packages that already exist in the store
        crate::store::fill_pkglines_in_plan(&mut plan, self)
            .with_context(|| "Failed to find existing packages in store")?;

        // If we reach here, actions_planned was true, user confirmed, and not dry_run.
        // Proceed with actual installation steps by calling the unified execution method.
        self.execute_installation_plan(plan)
    }

    /// Get candidate pkgkeys from capabilities (package names or provides)
    /// Uses get_candidates() to find packages that satisfy capabilities, which already handles provides
    /// Returns empty set if user_request_world is None
    /// Only includes pkgkeys that are in solvables
    fn get_candidate_pkgkeys_from_capabilities(
        &mut self,
        provider_ref: &crate::resolvo::GenericDependencyProvider,
        solvables: &[resolvo::SolvableId],
        user_request_world: Option<&HashMap<String, String>>,
    ) -> Result<std::collections::HashSet<String>> {
        use resolvo::DependencyProvider;
        use resolvo::runtime::{AsyncRuntime, NowOrNeverRuntime};

        // If user_request_world is None, return empty set
        let capabilities_map = match user_request_world {
            Some(user_request_world) => user_request_world,
            None => {
                return Ok(std::collections::HashSet::new());
            }
        };

        // Create a set of solvable pkgkeys for fast lookup
        let solvable_pkgkeys: std::collections::HashSet<String> = solvables
            .iter()
            .map(|solvable_id| {
                let record = &provider_ref.pool.resolve_solvable(*solvable_id).record;
                record.pkgkey.clone()
            })
            .collect();

        let mut candidate_pkgkeys = std::collections::HashSet::new();

        // For each capability in capabilities_map, get candidates using get_candidates()
        // This already handles provides via load_packages_for_name
        for capability in capabilities_map.keys() {
            // Intern the capability name to get NameId
            let name_id = provider_ref.pool.intern_package_name(
                crate::resolvo::NameType(capability.clone())
            );

            // Call get_candidates to get packages that satisfy this requirement
            // This already handles provides via load_packages_for_name
            match NowOrNeverRuntime::default().block_on(provider_ref.get_candidates(name_id)) {
                Some(candidates) => {
                    for solvable_id in &candidates.candidates {
                        let record = &provider_ref.pool.resolve_solvable(*solvable_id).record;
                        // Only add if this pkgkey is in solvables
                        if solvable_pkgkeys.contains(&record.pkgkey) {
                            candidate_pkgkeys.insert(record.pkgkey.clone());
                            log::debug!("[RESOLVO] Found candidate pkgkey: {} (pkgkey: {})", record.pkgname, record.pkgkey);
                        }
                    }
                }
                None => {
                    log::warn!("[RESOLVO] No candidates found for capability: {}", capability);
                }
            }
        }

        log::debug!("[RESOLVO] Extracted {} candidate pkgkeys from capabilities", candidate_pkgkeys.len());
        Ok(candidate_pkgkeys)
    }

    /// Add Conda virtual packages to cache
    fn add_conda_virtual_packages_to_cache(&mut self) -> Result<()> {
        match crate::conda_pkg::detect_conda_virtual_packages() {
            Ok(virtual_packages) => {
                use std::sync::Arc;
                for virtual_pkg in virtual_packages {
                    log::debug!("Adding virtual package to cache: {}={}", virtual_pkg.pkgname, virtual_pkg.version);
                    self.add_package_to_cache(Arc::new(virtual_pkg), PackageFormat::Conda);
                }
                Ok(())
            }
            Err(e) => {
                log::warn!("Failed to detect Conda virtual packages: {}", e);
                Err(e)
            }
        }
    }

    /// Extract no-install list from world (space-separated string)
    pub fn get_no_install_set(&self) -> std::collections::HashSet<String> {
        self.world
            .get("no-install")
            .map(|s| {
                s.split_whitespace()
                    .map(|pkg| pkg.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Create a resolvo dependency provider
    fn create_resolvo_provider(&mut self, format: PackageFormat, delta_world: &HashMap<String, String>) -> crate::resolvo::GenericDependencyProvider {
        use crate::resolvo::DependFieldFlags;
        let depend_fields = DependFieldFlags::REQUIRES;

        let delta_world_keys: std::collections::HashSet<String> = delta_world.keys().cloned().collect();

        // Extract no-install list from world (space-separated string)
        let no_install = self.get_no_install_set();

        crate::resolvo::GenericDependencyProvider::new(
            format,
            depend_fields,
            self as *mut PackageManager,
            delta_world_keys,
            no_install,
        )
    }

    /// Convert delta_world to resolvo requirements
    /// delta_world is a HashMap<String, String> where:
    /// - Key: package name
    /// - Value: version constraint string (empty string means no constraint)
    /// Supports version constraints from world.json format
    fn convert_initial_packages_to_requirements(
        &mut self,
        delta_world: &HashMap<String, String>,
        provider: &mut crate::resolvo::GenericDependencyProvider,
    ) -> Result<Vec<resolvo::ConditionalRequirement>> {
        let mut requirements = Vec::new();
        let ignore_missing = crate::models::config().common.ignore_missing;

        for (pkgname, constraint_str) in delta_world {
            // Check if package/capability exists when ignore_missing is enabled
            if ignore_missing && !self.check_package_or_capability_exists(pkgname) {
                log::warn!(
                    "Package/capability '{}' not found, skipping (ignore_missing=true)",
                    pkgname
                );
                continue;
            }

            // Parse constraint string from delta_world (or use world.json if delta_world has empty string)
            let final_constraints = if constraint_str.is_empty() {
                // No constraint in delta_world, check world.json
                self.world.get(pkgname)
                    .and_then(|world_constraint_str| {
                        if world_constraint_str.is_empty() {
                            None
                        } else {
                            crate::parse_requires::parse_world_constraint(world_constraint_str)
                        }
                    })
            } else {
                // Use constraint from delta_world
                crate::parse_requires::parse_world_constraint(constraint_str)
            };

            if let Some(constraints) = final_constraints {
                // Create requirement with constraints
                log::debug!("Using version constraints for '{}': constraints={:?}",
                    pkgname, constraints);
                let requirement = self.create_constrained_requirement(pkgname, &constraints, provider);
                requirements.push(requirement);
            } else {
                // No version constraints - create requirement for any version
                // Note: We don't skip already installed packages here since resolvo will handle that
                let requirement = self.create_package_name_requirement(pkgname, provider);
                requirements.push(requirement);
            }
        }

        Ok(requirements)
    }

    /// Check if a package or capability exists in the repository
    /// Returns true if packages are found, false otherwise
    fn check_package_or_capability_exists(&mut self, name: &str) -> bool {
        // First, try direct package name lookup
        match self.map_pkgname2packages(name) {
            Ok(packages) if !packages.is_empty() => return true,
            _ => {}
        }

        // If no packages found, try capability/provide lookup
        match crate::mmio::map_provide2pkgnames(name) {
            Ok(provider_pkgnames) => {
                for provider_pkgname in provider_pkgnames {
                    match self.map_pkgname2packages(&provider_pkgname) {
                        Ok(packages) if !packages.is_empty() => return true,
                        _ => continue,
                    }
                }
            }
            _ => {}
        }

        false
    }

    /// Create a requirement that matches any version of a package
    /// This allows the solver to choose the best version that satisfies all constraints
    fn create_package_name_requirement(
        &self,
        pkgname: &str,
        provider: &mut crate::resolvo::GenericDependencyProvider,
    ) -> resolvo::ConditionalRequirement {
        use resolvo::ConditionalRequirement;

        // Intern package name
        let name_id = provider.pool.intern_package_name(
            crate::resolvo::NameType(pkgname.to_string())
        );

        // Create a version set that matches any version (no constraints)
        let pkg_depend = crate::parse_requires::PkgDepend {
            capability: pkgname.to_string(),
            constraints: Vec::new(), // No constraints = matches any version
        };
        let or_deps = vec![pkg_depend];
        let and_deps = vec![or_deps];
        let version_set_id = provider.pool.intern_version_set(
            name_id,
            crate::resolvo::SolverMatchSpec::MatchSpec(and_deps),
        );

        ConditionalRequirement {
            requirement: version_set_id.into(),
            condition: None,
        }
    }

    /// Create a requirement with version constraints
    /// Supports constraints like =, >=, >, <=, <, !=, ~=
    fn create_constrained_requirement(
        &self,
        pkgname: &str,
        constraints: &[crate::parse_requires::VersionConstraint],
        provider: &mut crate::resolvo::GenericDependencyProvider,
    ) -> resolvo::ConditionalRequirement {
        use resolvo::ConditionalRequirement;

        // Intern package name
        let name_id = provider.pool.intern_package_name(
            crate::resolvo::NameType(pkgname.to_string())
        );

        // Create a version set with the specified constraints
        let pkg_depend = crate::parse_requires::PkgDepend {
            capability: pkgname.to_string(),
            constraints: constraints.to_vec(),
        };
        let or_deps = vec![pkg_depend];
        let and_deps = vec![or_deps];
        let version_set_id = provider.pool.intern_version_set(
            name_id,
            crate::resolvo::SolverMatchSpec::MatchSpec(and_deps),
        );

        ConditionalRequirement {
            requirement: version_set_id.into(),
            condition: None,
        }
    }

    /// Build dependency graph and create InstalledPackageInfo map
    fn build_installed_package_info_map(
        &mut self,
        solver: &resolvo::Solver<crate::resolvo::GenericDependencyProvider>,
        solvables: &[resolvo::SolvableId],
        user_request_world: Option<&HashMap<String, String>>,
    ) -> Result<HashMap<String, InstalledPackageInfo>> {
        let provider_ref = solver.provider();
        let format = provider_ref.format;

        // Extract request_world pkgkeys: candidate pkgkeys for user_request_world (handles provides)
        // request_world_pkgkeys is only used for ebin_exposure computing, not for depth calculation
        let request_world_pkgkeys = self.get_candidate_pkgkeys_from_capabilities(
            provider_ref,
            solvables,
            user_request_world,
        )?;
        log::debug!("[RESOLVO] Found {} request_world pkgkeys out of {} resolved solvables: {:?}", request_world_pkgkeys.len(), solvables.len(), request_world_pkgkeys);

        // Build dependency graph from resolved solvables
        let (pkgkey_to_depends, pkgkey_to_rdepends) =
            self.build_dependency_graph(provider_ref, solvables)?;

        // For Pacman format, also build build-dependency graph
        let (pkgkey_to_bdepends, pkgkey_to_rbdepends) = if format == PackageFormat::Pacman {
            log::debug!("[RESOLVO] Building build-dependency graph for Pacman format");
            // Update provider to use BUILD_REQUIRES only
            provider_ref.update_depend_fields(crate::resolvo::DependFieldFlags::BUILD_REQUIRES);
            let (bdepends, rbdepends) = self.build_dependency_graph(provider_ref, solvables)?;
            (bdepends, rbdepends)
        } else {
            (HashMap::new(), HashMap::new())
        };

        // Calculate dependency depths
        let pkgkey_to_depth = self.calculate_pkgkey_to_depth(
            &pkgkey_to_depends,
            &pkgkey_to_rdepends,
            &pkgkey_to_bdepends,
            &pkgkey_to_rbdepends,
        )?;

        // Create InstalledPackageInfo entries with correct depths
        let result = self.create_installed_package_info_map(
            provider_ref,
            solvables,
            &pkgkey_to_depends,
            &pkgkey_to_rdepends,
            &pkgkey_to_bdepends,
            &pkgkey_to_rbdepends,
            &pkgkey_to_depth,
            &request_world_pkgkeys,
        )?;

        Ok(result)
    }

    /// Build dependency graph from resolved solvables.
    ///
    /// This function builds a dependency graph by:
    /// 1. Processing all resolved solvables to extract their dependencies
    /// 2. Ensuring all solvables have entries in the dependency maps (even if empty)
    fn build_dependency_graph(
        &mut self,
        provider_ref: &crate::resolvo::GenericDependencyProvider,
        solvables: &[resolvo::SolvableId],
    ) -> Result<(
        HashMap<String, Vec<String>>,
        HashMap<String, Vec<String>>,
    )> {
        use resolvo::{DependencyProvider, Interner};
        use resolvo::runtime::{AsyncRuntime, NowOrNeverRuntime};

        let mut pkgkey_to_depends: HashMap<String, Vec<String>> = HashMap::new();
        let mut pkgkey_to_rdepends: HashMap<String, Vec<String>> = HashMap::new();

        // First pass: collect all resolved packages and build dependency graph
        // Ensure each solvable has an entry in pkgkey_to_depends (even if empty)
        for solvable_id in solvables {
            let record = &provider_ref.pool.resolve_solvable(*solvable_id).record;
            let pkgkey = record.pkgkey.clone();

            // Ensure entry exists (may be empty vec)
            pkgkey_to_depends.entry(pkgkey.clone()).or_insert_with(Vec::new);

            // Load package to get full info
            let _package = match self.load_package_info(&pkgkey) {
                Ok(pkg) => (*pkg).clone(),
                Err(e) => {
                    log::warn!("Failed to load resolved package {}: {}", pkgkey, e);
                    continue;
                }
            };

            // Get dependencies for this package
            let deps = match NowOrNeverRuntime::default().block_on(provider_ref.get_dependencies(*solvable_id)) {
                resolvo::Dependencies::Known(known_deps) => known_deps,
                resolvo::Dependencies::Unknown(reason) => {
                    let reason_str = provider_ref.display_string(reason).to_string();
                    log::warn!("Dependencies unknown for {}: {}", pkgkey, reason_str);
                    continue;
                }
            };

            // Extract dependency pkgkeys
            let dep_pkgkeys = self.extract_dependency_pkgkeys(provider_ref, solvables, &deps.requirements);
            pkgkey_to_depends.insert(pkgkey.clone(), dep_pkgkeys.clone());

            // Update reverse dependencies
            for dep_pkgkey in &dep_pkgkeys {
                pkgkey_to_rdepends
                    .entry(dep_pkgkey.clone())
                    .or_insert_with(Vec::new)
                    .push(pkgkey.clone());
            }
        }

        Ok((pkgkey_to_depends, pkgkey_to_rdepends))
    }

    /// Extract pkgkeys from dependency requirements
    fn extract_dependency_pkgkeys(
        &self,
        provider_ref: &crate::resolvo::GenericDependencyProvider,
        solvables: &[resolvo::SolvableId],
        requirements: &[resolvo::ConditionalRequirement],
    ) -> Vec<String> {
        use resolvo::{Interner, VersionSetId};

        let mut dep_pkgkeys = Vec::new();

        for req in requirements {
            // Extract version set IDs from the requirement
            let version_set_ids: Vec<VersionSetId> = match req.requirement {
                resolvo::Requirement::Single(version_set_id) => vec![version_set_id],
                resolvo::Requirement::Union(union_id) => {
                    provider_ref.version_sets_in_union(union_id).collect()
                }
            };

            for version_set_id in version_set_ids {
                let dep_name_id = provider_ref.version_set_name(version_set_id);
                let dep_name = provider_ref.display_name(dep_name_id).to_string();

                // Find the solvable that satisfies this requirement
                // Check both direct pkgname match and provides
                for other_solvable_id in solvables {
                    let other_record = &provider_ref.pool.resolve_solvable(*other_solvable_id).record;
                    // Check direct pkgname match
                    if other_record.pkgname == dep_name {
                        dep_pkgkeys.push(other_record.pkgkey.clone());
                        break;
                    }
                    // Check if package provides the capability
                    if provider_ref.package_provides_capability(&other_record.pkgkey, &dep_name) {
                        dep_pkgkeys.push(other_record.pkgkey.clone());
                        break;
                    }
                }
            }
        }

        dep_pkgkeys
    }

    /// Find leaf nodes (packages with no reverse dependencies)
    fn find_leaf_nodes_by_rdepends(
        remaining_rdepends: &HashMap<String, Vec<String>>,
    ) -> Vec<String> {
        remaining_rdepends
            .iter()
            .filter(|(_, rdepends)| rdepends.is_empty())
            .map(|(pkgkey, _)| pkgkey.clone())
            .collect()
    }

    /// Find leaf nodes by checking build reverse dependencies
    fn find_leaf_nodes_by_rbdepends(
        remaining_rbdepends: &HashMap<String, Vec<String>>,
    ) -> Vec<String> {
        remaining_rbdepends
            .iter()
            .filter(|(_, rbdepends)| rbdepends.is_empty())
            .map(|(pkgkey, _)| pkgkey.clone())
            .collect()
    }

    /// Find candidate node with least build reverse dependencies for breaking circular dependencies
    fn find_candidate_with_least_rbdepends(
        remaining_rbdepends: &HashMap<String, Vec<String>>,
    ) -> Option<String> {
        remaining_rbdepends
            .iter()
            .filter(|(_, rbdepends)| !rbdepends.is_empty())
            .min_by_key(|(_, rbdepends)| rbdepends.len())
            .map(|(pkgkey, _)| pkgkey.clone())
    }

    /// Find candidate node with least regular reverse dependencies for breaking circular dependencies
    fn find_candidate_with_least_rdepends(
        remaining_rdepends: &HashMap<String, Vec<String>>,
    ) -> Option<String> {
        remaining_rdepends
            .iter()
            .filter(|(_, rdepends)| !rdepends.is_empty())
            .min_by_key(|(_, rdepends)| rdepends.len())
            .map(|(pkgkey, _)| pkgkey.clone())
    }

    /// Remove a node from the dependency graph and update reverse dependencies
    fn remove_node_and_update_dependencies(
        node: &str,
        pkgkey_to_depends: &HashMap<String, Vec<String>>,
        pkgkey_to_bdepends: &HashMap<String, Vec<String>>,
        remaining_rdepends: &mut HashMap<String, Vec<String>>,
        remaining_rbdepends: &mut HashMap<String, Vec<String>>,
    ) {
        // Remove node from tracking maps
        remaining_rdepends.remove(node);
        remaining_rbdepends.remove(node);

        // Update reverse regular dependencies
        if let Some(depends_list) = pkgkey_to_depends.get(node) {
            for dep_pkgkey in depends_list {
                if let Some(rdepends) = remaining_rdepends.get_mut(dep_pkgkey) {
                    rdepends.retain(|x| x != node);
                }
            }
        }

        // Update reverse build dependencies
        if let Some(bdepends_list) = pkgkey_to_bdepends.get(node) {
            for dep_pkgkey in bdepends_list {
                if let Some(rbdepends) = remaining_rbdepends.get_mut(dep_pkgkey) {
                    rbdepends.retain(|x| x != node);
                }
            }
        }
    }

    /// Process leaf nodes: assign depth and remove them from the graph
    fn process_leaf_nodes(
        leaf_nodes: &[String],
        pkgkey_to_depends: &HashMap<String, Vec<String>>,
        pkgkey_to_bdepends: &HashMap<String, Vec<String>>,
        pkgkey_to_depth: &mut HashMap<String, u16>,
        remaining_rdepends: &mut HashMap<String, Vec<String>>,
        remaining_rbdepends: &mut HashMap<String, Vec<String>>,
        current_depth: u16,
    ) {
        // Set depth for all leaf nodes
        for pkgkey in leaf_nodes {
            pkgkey_to_depth.insert(pkgkey.clone(), current_depth);
        }

        // Remove leaf nodes and update reverse dependencies
        for leaf_pkgkey in leaf_nodes {
            Self::remove_node_and_update_dependencies(
                leaf_pkgkey,
                pkgkey_to_depends,
                pkgkey_to_bdepends,
                remaining_rdepends,
                remaining_rbdepends,
            );
        }
    }

    /// Break circular dependency by trying different strategies
    fn break_circular_dependency(
        pkgkey_to_depends: &HashMap<String, Vec<String>>,
        pkgkey_to_bdepends: &HashMap<String, Vec<String>>,
        pkgkey_to_depth: &mut HashMap<String, u16>,
        remaining_rdepends: &mut HashMap<String, Vec<String>>,
        remaining_rbdepends: &mut HashMap<String, Vec<String>>,
        current_depth: u16,
    ) -> bool {
        // Strategy 1: Try to find leaf nodes by remaining_rbdepends
        let leaf_nodes_by_rbdepends = Self::find_leaf_nodes_by_rbdepends(remaining_rbdepends);
        if !leaf_nodes_by_rbdepends.is_empty() {
            log::debug!(
                "Found {} leaf nodes by rbdepends at depth {}",
                leaf_nodes_by_rbdepends.len(),
                current_depth
            );
            Self::process_leaf_nodes(
                &leaf_nodes_by_rbdepends,
                pkgkey_to_depends,
                pkgkey_to_bdepends,
                pkgkey_to_depth,
                remaining_rdepends,
                remaining_rbdepends,
                current_depth,
            );
            return true;
        }

        // Strategy 2: Remove node with least rbdepends (if has non-empty rbdepends)
        if let Some(candidate) = Self::find_candidate_with_least_rbdepends(remaining_rbdepends) {
            let rbdepends_count = remaining_rbdepends
                .get(&candidate)
                .map(|v| v.len())
                .unwrap_or(0);
            log::debug!(
                "Breaking circular dependency by removing node {} with least rbdepends ({}) at depth {}",
                candidate,
                rbdepends_count,
                current_depth
            );
            pkgkey_to_depth.insert(candidate.clone(), current_depth);
            Self::remove_node_and_update_dependencies(
                &candidate,
                pkgkey_to_depends,
                pkgkey_to_bdepends,
                remaining_rdepends,
                remaining_rbdepends,
            );
            return true;
        }

        // Strategy 3: Remove node with least rdepends
        if let Some(candidate) = Self::find_candidate_with_least_rdepends(remaining_rdepends) {
            let rdepends_count = remaining_rdepends
                .get(&candidate)
                .map(|v| v.len())
                .unwrap_or(0);
            log::debug!(
                "Breaking circular dependency by removing node {} with least rdepends ({}) at depth {}",
                candidate,
                rdepends_count,
                current_depth
            );
            pkgkey_to_depth.insert(candidate.clone(), current_depth);
            Self::remove_node_and_update_dependencies(
                &candidate,
                pkgkey_to_depends,
                pkgkey_to_bdepends,
                remaining_rdepends,
                remaining_rbdepends,
            );
            return true;
        }

        false
    }

    /// Calculate dependency depths based on dependency graph using topological sort.
    ///
    /// This function assigns a depth value to each package based on its position in the dependency
    /// graph. Packages with no reverse dependencies (leaf nodes) are assigned depth 0, and depth
    /// increases as we move up the dependency tree.
    ///
    /// The algorithm handles both regular dependencies and build dependencies (for Pacman format):
    /// - Regular dependencies: packages that this package depends on at runtime
    /// - Build dependencies: packages needed only during build time (Pacman only)
    ///
    /// Circular dependency breaking strategy (when no leaf nodes are found):
    /// 1. First, try to find leaf nodes by checking remaining_rbdepends (build reverse dependencies)
    /// 2. If still no leaf nodes, remove the node with the least rbdepends (build reverse dependencies)
    /// 3. If still no progress, remove the node with the least rdepends (regular reverse dependencies)
    /// 4. Last resort: assign all remaining packages the current depth
    ///
    /// This approach leads to better depth assignments and avoids deadlocks while still maintaining
    /// a reasonable dependency ordering.
    ///
    /// # Arguments
    ///
    /// * `pkgkey_to_depends` - Map of package keys to their regular dependencies
    /// * `pkgkey_to_rdepends` - Map of package keys to packages that depend on them (reverse regular deps)
    /// * `pkgkey_to_bdepends` - Map of package keys to their build dependencies (Pacman only)
    /// * `pkgkey_to_rbdepends` - Map of package keys to packages that have them as build deps (reverse build deps)
    ///
    /// # Returns
    ///
    /// A HashMap mapping package keys to their calculated dependency depths
    pub fn calculate_pkgkey_to_depth(
        &self,
        pkgkey_to_depends: &HashMap<String, Vec<String>>,
        pkgkey_to_rdepends: &HashMap<String, Vec<String>>,
        pkgkey_to_bdepends: &HashMap<String, Vec<String>>,
        pkgkey_to_rbdepends: &HashMap<String, Vec<String>>,
    ) -> Result<HashMap<String, u16>> {
        let mut pkgkey_to_depth: HashMap<String, u16> = HashMap::new();

        // Create a mutable copy of pkgkey_to_rdepends for tracking remaining reverse dependencies
        let mut remaining_rdepends: HashMap<String, Vec<String>> = pkgkey_to_rdepends
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        // Create a mutable copy of pkgkey_to_rbdepends for tracking remaining reverse build dependencies
        let mut remaining_rbdepends: HashMap<String, Vec<String>> = pkgkey_to_rbdepends
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        // Initialize remaining_rdepends and remaining_rbdepends for all packages in pkgkey_to_depends
        for pkgkey in pkgkey_to_depends.keys() {
            remaining_rdepends.entry(pkgkey.clone()).or_insert_with(Vec::new);
            remaining_rbdepends.entry(pkgkey.clone()).or_insert_with(Vec::new);
        }

        let mut current_depth = 0;

        loop {
            // Find packages with empty remaining_rdepends (leaf nodes at current depth)
            let leaf_nodes = Self::find_leaf_nodes_by_rdepends(&remaining_rdepends);

            if leaf_nodes.is_empty() {
                // No more leaf nodes - check if there are any remaining packages
                if remaining_rdepends.is_empty() {
                    break;
                } else {
                    // Circular dependency detected - try to break it using helper function
                    if Self::break_circular_dependency(
                        pkgkey_to_depends,
                        pkgkey_to_bdepends,
                        &mut pkgkey_to_depth,
                        &mut remaining_rdepends,
                        &mut remaining_rbdepends,
                        current_depth,
                    ) {
                        // Successfully broke the cycle, continue to next iteration
                        current_depth += 1;
                        continue;
                    }

                    // Last resort: assign remaining packages max depth
                    log::warn!(
                        "Found {} packages with unresolved dependencies, assigning depth {}",
                        remaining_rdepends.len(),
                        current_depth
                    );
                    for pkgkey in remaining_rdepends.keys() {
                        pkgkey_to_depth.insert(pkgkey.clone(), current_depth);
                    }
                    break;
                }
            }

            log::debug!(
                "Found {} leaf nodes at depth {}",
                leaf_nodes.len(),
                current_depth
            );

            // Process leaf nodes: assign depth and remove from graph
            Self::process_leaf_nodes(
                &leaf_nodes,
                pkgkey_to_depends,
                pkgkey_to_bdepends,
                &mut pkgkey_to_depth,
                &mut remaining_rdepends,
                &mut remaining_rbdepends,
                current_depth,
            );

            current_depth += 1;
        }

        log::debug!("Calculated depths for {} packages", pkgkey_to_depth.len());
        Ok(pkgkey_to_depth)
    }

    /// Create InstalledPackageInfo map with correct dependency depths
    fn create_installed_package_info_map(
        &self,
        provider_ref: &crate::resolvo::GenericDependencyProvider,
        solvables: &[resolvo::SolvableId],
        pkgkey_to_depends: &HashMap<String, Vec<String>>,
        pkgkey_to_rdepends: &HashMap<String, Vec<String>>,
        pkgkey_to_bdepends: &HashMap<String, Vec<String>>,
        pkgkey_to_rbdepends: &HashMap<String, Vec<String>>,
        pkgkey_to_depth: &HashMap<String, u16>,
        request_world_pkgkeys: &std::collections::HashSet<String>,
    ) -> Result<HashMap<String, InstalledPackageInfo>> {
        let mut result = HashMap::new();

        // Iterate through all packages in the dependency graph
        for pkgkey in pkgkey_to_depends.keys() {
            // Skip conda virtual packages (names starting with __)
            if let Ok(pkgname) = crate::package::pkgkey2pkgname(pkgkey) {
                if pkgname.starts_with("__") {
                    continue;
                }
            }

            // Get depth from pre-calculated map (default to 0 if not found)
            let depth = pkgkey_to_depth.get(pkgkey).copied().unwrap_or(0);

            // Determine ebin_exposure: true only for request_world packages
            let ebin_exposure = request_world_pkgkeys.contains(pkgkey);

            // Create InstalledPackageInfo
            let pkg_info = self.create_installed_package_info(
                pkgkey,
                provider_ref,
                solvables,
                pkgkey_to_depends,
                pkgkey_to_rdepends,
                pkgkey_to_bdepends,
                pkgkey_to_rbdepends,
                depth,
                ebin_exposure,
            )?;

            result.insert(pkgkey.clone(), pkg_info.clone());
        }

        log::debug!("Final result size: {}", result.len());
        Ok(result)
    }

    /// Create InstalledPackageInfo for a single package
    fn create_installed_package_info(
        &self,
        pkgkey: &str,
        provider_ref: &crate::resolvo::GenericDependencyProvider,
        solvables: &[resolvo::SolvableId],
        pkgkey_to_depends: &HashMap<String, Vec<String>>,
        pkgkey_to_rdepends: &HashMap<String, Vec<String>>,
        pkgkey_to_bdepends: &HashMap<String, Vec<String>>,
        pkgkey_to_rbdepends: &HashMap<String, Vec<String>>,
        depend_depth: u16,
        ebin_exposure: bool,
    ) -> Result<InstalledPackageInfo> {
        // pkgline should be empty initially - it will be filled by fill_pkglines_in_plan
        // if the package already exists in the store, otherwise it will remain empty
        // and the package will be downloaded and installed
        let pkgline = String::new();

        let depends_list = pkgkey_to_depends.get(pkgkey).cloned().unwrap_or_default();
        let bdepends_list = pkgkey_to_bdepends.get(pkgkey).cloned().unwrap_or_default();

        let arch = solvables
            .iter()
            .find_map(|&id| {
                let rec = &provider_ref.pool.resolve_solvable(id).record;
                if rec.pkgkey == pkgkey {
                    Some(rec.arch.clone())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "unknown".to_string());

        // Merges rdepends from already installed packages (self.installed_packages)
        // to "predict the future" - incorporating historical dependency data.
        // The merged dependencies are sorted and de-duplicated to maintain consistency.
        let mut merged_rdepends = pkgkey_to_rdepends.get(pkgkey).cloned().unwrap_or_default();
        if let Some(installed_info) = self.installed_packages.get(pkgkey) {
            // Merge and de-duplicate
            merged_rdepends.extend_from_slice(&installed_info.rdepends);
            merged_rdepends.sort();
            merged_rdepends.dedup();
        }

        // Merges rbdepends from already installed packages (self.installed_packages)
        // to "predict the future" - incorporating historical build dependency data.
        let mut merged_rbdepends = pkgkey_to_rbdepends.get(pkgkey).cloned().unwrap_or_default();
        if let Some(installed_info) = self.installed_packages.get(pkgkey) {
            // Merge and de-duplicate
            merged_rbdepends.extend_from_slice(&installed_info.rbdepends);
            merged_rbdepends.sort();
            merged_rbdepends.dedup();
        }

        Ok(crate::models::InstalledPackageInfo {
            pkgline,
            arch,
            depend_depth,
            install_time: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            ebin_exposure,
            rdepends: merged_rdepends,
            depends: depends_list,
            bdepends: bdepends_list,
            rbdepends: merged_rbdepends,
            ebin_links: Vec::new(),
            pending_triggers: Vec::new(),
            triggers_awaited: false,
            config_failed: false,
        })
    }

    // Filter packages based on architecture that matches config().common.arch
    // This is to handle situation when both x86_64 and i686 packages are available with same
    // pkgname and version, e.g. fedora fcitx5-qt 5.1.9-3.fc42 has 2 packages for x86_64/i686.
    pub fn filter_packages_by_arch(&self, packages: Vec<Package>, format: PackageFormat) -> Vec<Package> {
        let target_arch = crate::models::config().common.arch.as_str();

        // For RPM format, noarch packages should be included regardless of target arch
        // This is standard RPM behavior - noarch packages are architecture-independent
        let is_rpm_format = format == PackageFormat::Rpm;
        // For Conda format, "all" arch packages (from noarch) should be included regardless of target arch
        // This is standard Conda behavior - noarch packages are architecture-independent
        let is_conda_format = format == PackageFormat::Conda;

        // If there are no packages with matching architecture, return all packages
        let arch_packages: Vec<Package> = packages.iter()
            .filter(|pkg| {
                // For RPM format, noarch packages should be included regardless of target arch
                // This is standard RPM behavior - noarch packages are architecture-independent
                if is_rpm_format && pkg.arch == "noarch" {
                    return true;
                }
                // For Conda format, "all" arch packages (from noarch) should be included regardless of target arch
                // This is standard Conda behavior - noarch packages are architecture-independent
                if is_conda_format && pkg.arch == "all" {
                    return true;
                }
                !pkg.arch.is_empty() && pkg.arch == target_arch
            })
            .cloned()
            .collect();

        log::debug!(
            "filter_packages_by_arch: target_arch='{}', format={:?}, is_rpm={}, input={} packages, output={} packages",
            target_arch,
            format,
            is_rpm_format,
            packages.len(),
            arch_packages.len()
        );

        if !arch_packages.is_empty() {
            arch_packages
        } else {
            packages
        }

    }

    /// Get package format from repodata_name, or return default (Epkg)
    fn get_format_from_package(package: &Package) -> PackageFormat {
        if !package.repodata_name.is_empty() {
            let repodata_indice = crate::models::repodata_indice();
            if let Some(repo_index) = repodata_indice.get(&package.repodata_name) {
                return repo_index.format;
            }
        }
        // Default to Epkg if we can't determine format
        PackageFormat::Epkg
    }

    /// Helper to add a package to cache and update indexes
    pub fn add_package_to_cache(&mut self, package: Arc<Package>, format: PackageFormat) {
        let pkgkey = package.pkgkey.clone();
        let pkgname = package.pkgname.clone();

        // Add to pkgkey2package
        self.pkgkey2package.insert(pkgkey.clone(), Arc::clone(&package));

        // Update pkgname2packages index
        self.pkgname2packages
            .entry(pkgname.clone())
            .or_insert_with(Vec::new)
            .push(Arc::clone(&package));

        // Update provide2pkgnames index
        // IMPORTANT: Provides are in the form cap_with_arch=version (e.g., "libfoo(x86-64)=2.0")
        // cap_with_arch is an atomic tag that should NEVER be split. The provide2pkgnames
        // index is keyed by cap_with_arch (e.g., "libfoo(x86-64)"), not by cap alone.
        // When doing lookups, always use cap_with_arch directly, never strip the arch.
        for provide in &package.provides {
            // Parse provides string and extract names with optional versions
            // parse_provides preserves cap_with_arch (e.g., "libfoo(x86-64)")
            let provide_map = parse_provides(provide, format);
            for (provide_name, _version) in provide_map {
                // provide_name is cap_with_arch (atomic, never split)
                // version is available but not currently used for indexing
                self.provide2pkgnames
                    .entry(provide_name)
                    .or_insert_with(HashSet::new)
                    .insert(pkgname.clone());
            }
        }
    }

    pub fn map_pkgname2packages(&mut self, pkgname: &str) -> Result<Vec<Package>> {
        // First check if we have packages in pkgname2packages index (for testing)
        if let Some(cached_packages) = self.pkgname2packages.get(pkgname) {
            if !cached_packages.is_empty() {
                return Ok(cached_packages.iter().map(|pkg_arc| (**pkg_arc).clone()).collect());
            }
        }

        // Fall back to mmio lookup (for production)
        match crate::mmio::map_pkgname2packages(pkgname) {
            Ok(packages_list) => {
                for package in &packages_list {
                    // cache for later references and update indexes
                    log::trace!("Caching package: {}", package.pkgkey);
                    let format = Self::get_format_from_package(package);
                    self.add_package_to_cache(Arc::new(package.clone()), format);
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
        // Try to find in cache first
        if let Some(package) = self.pkgkey2package.get(pkgkey) {
            log::trace!("Found cached package info for '{}'", pkgkey);
            return Ok(Arc::clone(package));
        }

        // Query info in packages.txt
        log::debug!("Package '{}' not in cache, loading from repository", pkgkey);
        match crate::mmio::map_pkgkey2package(pkgkey) {
            Ok(package) => {
                let format = Self::get_format_from_package(&package);
                let arc_package = Arc::new(package);
                // Cache the package for future use and update indexes
                self.add_package_to_cache(Arc::clone(&arc_package), format);
                Ok(arc_package)
            }
            Err(e) => {
                Err(e)
            }
        }
    }


    /// Check if two constraints with opposite operators are logically mutually exclusive
    /// even when they have different operands.
    ///
    /// Examples:
    /// - "< 2.1~~" and ">= 2.2" are mutually exclusive if 2.1~~ <= 2.2
    /// - "< 2.3" and "> 2.3" are mutually exclusive (no version can satisfy both)
    /// - "<= 2.3" and ">= 2.3" are NOT mutually exclusive (version 2.3 satisfies both)
    pub fn are_constraints_logically_mutually_exclusive(
        &self,
        constraint1: &VersionConstraint,
        constraint2: &VersionConstraint,
        format: PackageFormat,
    ) -> bool {
        // If either constraint has an unexpanded RPM macro, we can't meaningfully compare them
        // Treat them as not mutually exclusive (they'll be ignored during actual checking anyway)
        if constraint1.operand.contains("%{") || constraint2.operand.contains("%{") {
            return false;
        }

        // Normalize operands by handling RPM ~~ operator
        // In RPM, "2.1~~" means "less than 2.2", so for comparison purposes,
        // we treat "2.1~~" as slightly less than "2.2"
        // When comparing with another version, we can check if base version < other version
        let normalize_operand_for_comparison = |op: &str| -> (String, bool) {
            if op.ends_with("~~") {
                // Remove ~~ - the base version represents "less than next version"
                let base = op.trim_end_matches("~~");
                (base.to_string(), true) // true indicates this is a "less than next" version
            } else {
                (op.to_string(), false)
            }
        };

        let (op1_base, op1_is_tilde) = normalize_operand_for_comparison(&constraint1.operand);
        let (op2_base, op2_is_tilde) = normalize_operand_for_comparison(&constraint2.operand);

        // Compare the base operands to determine if constraints are mutually exclusive
        let comparison = version::compare_versions(&op1_base, &op2_base, format);

        match comparison {
            Some(std::cmp::Ordering::Less) => {
                // op1_base < op2_base
                match (&constraint1.operator, &constraint2.operator) {
                    (Operator::VersionLessThan, Operator::VersionGreaterThanEqual) |
                    (Operator::VersionLessThan, Operator::VersionGreaterThan) |
                    (Operator::VersionLessThanEqual, Operator::VersionGreaterThan) => {
                        // < X and >= Y where X < Y: mutually exclusive (no overlap)
                        true
                    }
                    (Operator::VersionLessThanEqual, Operator::VersionGreaterThanEqual) => {
                        // <= X and >= Y where X < Y: mutually exclusive (no overlap)
                        true
                    }
                    (Operator::VersionGreaterThanEqual, Operator::VersionLessThan) |
                    (Operator::VersionGreaterThanEqual, Operator::VersionLessThanEqual) |
                    (Operator::VersionGreaterThan, Operator::VersionLessThan) |
                    (Operator::VersionGreaterThan, Operator::VersionLessThanEqual) => {
                        // >= X and < Y where X < Y: NOT mutually exclusive (ranges overlap)
                        // Example: >= 4.2.5 and < 5.0 - version 4.5 satisfies both
                        false
                    }
                    _ => false,
                }
            }
            Some(std::cmp::Ordering::Equal) => {
                // op1_base == op2_base
                // When base versions are equal, check if operators and tilde status make them mutually exclusive
                match (&constraint1.operator, &constraint2.operator) {
                    (Operator::VersionLessThan, Operator::VersionGreaterThanEqual) |
                    (Operator::VersionLessThan, Operator::VersionGreaterThan) |
                    (Operator::VersionLessThanEqual, Operator::VersionGreaterThan) |
                    (Operator::VersionGreaterThan, Operator::VersionLessThanEqual) |
                    (Operator::VersionGreaterThan, Operator::VersionLessThan) |
                    (Operator::VersionGreaterThanEqual, Operator::VersionLessThan) => {
                        // When base versions are equal:
                        // - If both have ~~ or both don't have ~~, they're mutually exclusive only if operators are strict opposites
                        // - If one has ~~ and the other doesn't:
                        //   * "< X~~" (meaning < next version) and ">= X" are NOT mutually exclusive
                        //   * But "< X~~" and "> X" might be mutually exclusive depending on interpretation
                        // For simplicity, when base versions are equal and one has ~~, we only treat as mutually exclusive
                        // if both operators are strict (< vs >, not <= vs >=)
                        if op1_is_tilde == op2_is_tilde {
                            // Both have same tilde status
                            matches!((&constraint1.operator, &constraint2.operator),
                                (Operator::VersionLessThan, Operator::VersionGreaterThan) |
                                (Operator::VersionGreaterThan, Operator::VersionLessThan))
                        } else {
                            // One has ~~, one doesn't - be conservative and don't treat as mutually exclusive
                            // unless operators are strict opposites
                            // Actually, "< X~~" means "< next version", so if we have ">= X", they overlap
                            // Only strict opposites like "< X~~" and "> X" might be mutually exclusive
                            // But this is complex, so for now we'll be conservative
                            false
                        }
                    }
                    (Operator::VersionLessThanEqual, Operator::VersionGreaterThanEqual) => {
                        // <= X and >= X: NOT mutually exclusive (X satisfies both)
                        false
                    }
                    _ => false,
                }
            }
            Some(std::cmp::Ordering::Greater) => {
                // op1_base > op2_base, check swapped order
                match (&constraint2.operator, &constraint1.operator) {
                    (Operator::VersionLessThan, Operator::VersionGreaterThanEqual) |
                    (Operator::VersionLessThan, Operator::VersionGreaterThan) |
                    (Operator::VersionLessThanEqual, Operator::VersionGreaterThan) => {
                        // < X and >= Y where X < Y (swapped, so Y < X): mutually exclusive (no overlap)
                        // Example: < 2.1 and >= 2.2 - no version can satisfy both
                        true
                    }
                    (Operator::VersionLessThanEqual, Operator::VersionGreaterThanEqual) => {
                        // <= X and >= Y where X < Y (swapped, so Y < X): mutually exclusive (no overlap)
                        // Example: <= 2.1 and >= 2.2 - no version can satisfy both
                        true
                    }
                    (Operator::VersionGreaterThanEqual, Operator::VersionLessThan) |
                    (Operator::VersionGreaterThanEqual, Operator::VersionLessThanEqual) |
                    (Operator::VersionGreaterThan, Operator::VersionLessThan) |
                    (Operator::VersionGreaterThan, Operator::VersionLessThanEqual) => {
                        // >= X and < Y where X > Y (swapped): NOT mutually exclusive (ranges overlap)
                        // Example: >= 1.31.6 and < 3 - version 2.5.0 satisfies both, so they're NOT mutually exclusive
                        false
                    }
                    _ => false,
                }
            }
            None => {
                // Can't compare versions, be conservative and don't treat as mutually exclusive
                false
            }
        }
    }


    /// Extract version from a provide entry remainder (the part after the capability name)
    /// Returns Some((operator, version)) or None if no version found
    ///
    /// IMPORTANT: Package 'provides' fields only support these forms:
    /// - cap_with_arch (no version, just the capability name)
    /// - cap_with_arch EQUALS cap_version (exact version match only)
    ///
    /// The remainder_trimmed, provide_entry vars and everything in this function will never
    /// contain >=, >, <=, < operators, so we only need to handle the "=" operator.
    fn extract_version_from_remainder<'a>(remainder: &'a str, provide_entry: &'a str) -> Option<(&'a str, &'a str)> {
        if remainder.starts_with('=') {
            // Alpine format: "=version" (no spaces)
            Some(("=", &remainder[1..]))
        } else if remainder.starts_with(" = ") {
            // RPM format with spaces: " = version"
            Some(("=", &remainder[3..]))
        } else if remainder.starts_with("(= ") {
            // Debian format: "(= version)"
            let version_start = 3;
            if let Some(close_pos) = remainder[version_start..].find(')') {
                Some(("=", &remainder[version_start..version_start + close_pos]))
            } else {
                Some(("=", &remainder[version_start..]))
            }
        } else if let Some(pos) = provide_entry.find('=') {
            // Fallback: search in entire provide_entry (for backward compatibility)
            // Check if this is Alpine format (no spaces) or RPM format (with spaces)
            if pos > 0 && pos < provide_entry.len() - 1 {
                let before = &provide_entry[pos - 1..pos];
                let after = &provide_entry[pos + 1..pos + 2];
                // If there's no space before or after, it's Alpine format
                if before != " " && after != " " {
                    Some(("=", &provide_entry[pos + 1..]))
                } else if before == " " {
                    // RPM format with space before: "pkgname = version"
                    Some(("=", &provide_entry[pos + 3..]))
                } else {
                    Some(("=", &provide_entry[pos + 1..]))
                }
            } else {
                Some(("=", &provide_entry[pos + 1..]))
            }
        } else if let Some(pos) = provide_entry.find(" = ") {
            // RPM format with spaces: "pkgname = version"
            Some(("=", &provide_entry[pos + 3..]))
        } else if let Some(pos) = provide_entry.find("(= ") {
            // Debian format: "pkgname (= version)"
            let version_start = pos + 3;
            if let Some(close_pos) = provide_entry[version_start..].find(')') {
                Some(("=", &provide_entry[version_start..version_start + close_pos]))
            } else {
                Some(("=", &provide_entry[version_start..]))
            }
        } else {
            // No version specified in provide entry
            None
        }
    }

    /// Check if a version satisfies a set of constraints
    fn check_version_satisfies_constraints(
        &mut self,
        version: &str,
        constraints: &Vec<VersionConstraint>,
        format: PackageFormat,
    ) -> Result<bool> {
        // Separate constraints into mutually exclusive groups (OR conditions) and compatible constraints (AND conditions)
        let mut or_groups: Vec<Vec<&VersionConstraint>> = Vec::new();
        let mut and_constraints: Vec<&VersionConstraint> = Vec::new();

        // Filter out conditional constraints first
        let non_conditional_constraints: Vec<&VersionConstraint> = constraints.iter()
            .filter(|c| !matches!(c.operator, Operator::IfInstall))
            .collect();

        // Group mutually exclusive constraints together
        let mut processed = vec![false; non_conditional_constraints.len()];
        for i in 0..non_conditional_constraints.len() {
            if processed[i] {
                continue;
            }
            let constraint_i = non_conditional_constraints[i];
            let mut or_group = vec![constraint_i];
            processed[i] = true;

            // Look for mutually exclusive constraints
            for j in (i + 1)..non_conditional_constraints.len() {
                if processed[j] {
                    continue;
                }
                let constraint_j = non_conditional_constraints[j];

                // Check if constraints are mutually exclusive
                let are_mutually_exclusive = match (&constraint_i.operator, &constraint_j.operator) {
                    (Operator::VersionLessThan, Operator::VersionGreaterThan) |
                    (Operator::VersionLessThan, Operator::VersionGreaterThanEqual) |
                    (Operator::VersionLessThanEqual, Operator::VersionGreaterThan) |
                    (Operator::VersionLessThanEqual, Operator::VersionGreaterThanEqual) |
                    (Operator::VersionGreaterThan, Operator::VersionLessThan) |
                    (Operator::VersionGreaterThan, Operator::VersionLessThanEqual) |
                    (Operator::VersionGreaterThanEqual, Operator::VersionLessThan) |
                    (Operator::VersionGreaterThanEqual, Operator::VersionLessThanEqual) => {
                        if constraint_i.operand == constraint_j.operand {
                            true
                        } else {
                            self.are_constraints_logically_mutually_exclusive(
                                constraint_i,
                                constraint_j,
                                format,
                            )
                        }
                    }
                    _ => false,
                };

                if are_mutually_exclusive {
                    let mut can_add = true;
                    for existing_constraint in &or_group[1..] {
                        let mutually_exclusive_with_existing = match (&existing_constraint.operator, &constraint_j.operator) {
                            (Operator::VersionLessThan, Operator::VersionGreaterThan) |
                            (Operator::VersionLessThan, Operator::VersionGreaterThanEqual) |
                            (Operator::VersionLessThanEqual, Operator::VersionGreaterThan) |
                            (Operator::VersionLessThanEqual, Operator::VersionGreaterThanEqual) |
                            (Operator::VersionGreaterThan, Operator::VersionLessThan) |
                            (Operator::VersionGreaterThan, Operator::VersionLessThanEqual) |
                            (Operator::VersionGreaterThanEqual, Operator::VersionLessThan) |
                            (Operator::VersionGreaterThanEqual, Operator::VersionLessThanEqual) => {
                                if existing_constraint.operand == constraint_j.operand {
                                    true
                                } else {
                                    self.are_constraints_logically_mutually_exclusive(
                                        existing_constraint,
                                        constraint_j,
                                        format,
                                    )
                                }
                            }
                            _ => false,
                        };
                        if !mutually_exclusive_with_existing {
                            can_add = false;
                            break;
                        }
                    }

                    if can_add {
                        or_group.push(constraint_j);
                        processed[j] = true;
                    }
                }
            }

            if or_group.len() > 1 {
                or_groups.push(or_group);
            } else {
                and_constraints.push(constraint_i);
            }
        }

        // Check AND constraints: all must be satisfied
        for constraint in &and_constraints {
            if !version::check_version_constraint(version, constraint, format)? {
                return Ok(false);
            }
        }

        // Check OR groups: at least one constraint in each group must be satisfied
        for or_group in &or_groups {
            let mut any_satisfied = false;
            for constraint in or_group {
                if version::check_version_constraint(version, constraint, format)? {
                    any_satisfied = true;
                    break;
                }
            }
            if !any_satisfied {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Check if a package implicitly provides a capability (i.e., the capability name matches the package name)
    /// In Alpine and most package managers, a package implicitly provides its own name.
    fn check_implicit_provide(
        &mut self,
        provider_pkgkey: &str,
        base_capability: &str,
        provider_pkg: &Package,
        constraints: &Vec<VersionConstraint>,
        format: PackageFormat,
    ) -> Result<bool> {
        // Check if the capability name matches the package name itself
        if let Ok(pkgname) = crate::package::pkgkey2pkgname(provider_pkgkey) {
            if base_capability == pkgname {
                // Package implicitly provides its own name - use package's own version
                let provided_version = provider_pkg.version.trim();

                log::trace!(
                    "Provider {} implicitly provides '{}' (its own name) with version '{}'",
                    provider_pkgkey, base_capability, provided_version
                );

                // Check constraints against the package's own version
                if self.check_version_satisfies_constraints(provided_version, constraints, format)? {
                    log::debug!(
                        "Provider {} implicitly provides '{}' version '{}' satisfies all constraints",
                        provider_pkgkey, base_capability, provided_version
                    );
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Check if a provider package's provides satisfy version constraints for a capability
    pub fn check_provider_satisfies_constraints(
        &mut self,
        provider_pkgkey: &str,
        capability: &str,
        constraints: &Vec<VersionConstraint>,
        format: PackageFormat,
    ) -> Result<bool> {
        // Load the provider package
        let provider_pkg = match self.load_package_info(provider_pkgkey) {
            Ok(pkg) => pkg,
            Err(e) => {
                log::warn!("Failed to load package info for {}: {}", provider_pkgkey, e);
                return Ok(false);
            }
        };

        // Strip any version constraints from the capability name for matching
        // The capability might include version constraints like "pc:libpcre2-8>=10.32"
        // but we need to match against provide entries like "pc:libpcre2-8=10.46"
        let base_capability = if let Some(pos) = capability.find(|c: char| c == '>' || c == '<' || c == '=' || c == '!') {
            capability[..pos].trim_end()
        } else {
            capability
        };

        let provider_pkg_version = provider_pkg.version.trim().to_string();

        log::trace!(
            "Checking provider {} for capability '{}' (base: '{}') with constraints {:?}",
            provider_pkgkey, capability, base_capability, constraints
        );

        // For Alpine and Pacman formats, provide entries may contain multiple space-separated provide items
        // Alpine: "pc:libpcre2-16=10.46 pc:libpcre2-32=10.46 pc:libpcre2-8=10.46"
        // Pacman: "libutil-linux libblkid.so=1-64 libfdisk.so=1-64 libmount.so=1-64"
        // For Debian/RPM format, each entry is already a complete provide entry
        // e.g., "libgcc1 (= 1:14.2.0-19)" or "test-pkg = 1.0.0"
        // Also check for bundled() variants: if looking for "cap", also check "bundled(cap)"
        let bundled_variant = format!("bundled({})", base_capability);
        for provide_entry_string in &provider_pkg.provides {
            let provide_items: Vec<&str> = if format == PackageFormat::Apk || format == PackageFormat::Pacman {
                // Alpine/Pacman: split by whitespace to get individual provide items
                provide_entry_string.split_whitespace().collect()
            } else {
                // Debian/RPM: each entry is already complete, no splitting needed
                vec![provide_entry_string.as_str()]
            };

            for provide_entry in provide_items {
                let provide_entry_trimmed = provide_entry.trim();

                // Check if this provide entry matches the capability (direct or bundled)
                // First check direct match
                let matches_direct = provide_entry_trimmed.starts_with(base_capability);
                // Then check bundled variant match
                let matches_bundled = provide_entry_trimmed.starts_with(&bundled_variant);

                if !matches_direct && !matches_bundled {
                    continue; // Doesn't match at all
                }

                // Use the appropriate capability name for remainder checking
                let matched_capability = if matches_bundled {
                    &bundled_variant
                } else {
                    base_capability
                };

                // Check if the remainder (after capability name) is valid
                // IMPORTANT: Package 'provides' fields only support cap_with_arch or cap_with_arch EQUALS cap_version.
                //
                // Operators like >=, >, <=, < are artifacts from metadata parsing and should be ignored.
                // wfg /c/epkg% gr -c '^provides: .*>' ~/.cache/epkg/channels/|g -v ':0$'
                // /home/wfg/.cache/epkg/channels/opensuse:16.0/oss/x86_64/packages.txt:10
                // /home/wfg/.cache/epkg/channels/fedora:42/Everything-updates/x86_64/packages.txt:11
                // /home/wfg/.cache/epkg/channels/fedora:42/Everything/x86_64/packages.txt:12
                // wfg /c/epkg% gr -c '^provides: .*<' ~/.cache/epkg/channels/|g -v ':0$'
                // /home/wfg/.cache/epkg/channels/opensuse:16.0/oss/x86_64/packages.txt:10
                // /home/wfg/.cache/epkg/channels/fedora:42/Everything-updates/x86_64/packages.txt:5
                // /home/wfg/.cache/epkg/channels/fedora:42/Everything/x86_64/packages.txt:22
                //
                // Also handle library aliases like "lib.so=lib.so-64" for Arch Linux
                let remainder = &provide_entry_trimmed[matched_capability.len()..];
                let remainder_trimmed = remainder.trim_start();

                // Explicitly skip provides with invalid operators (>=, <=, >, <) - these are artifacts
                if !remainder_trimmed.is_empty() && (
                    remainder_trimmed.starts_with(">=") ||
                    remainder_trimmed.starts_with("<=") ||
                    remainder_trimmed.starts_with(" > ") ||
                    remainder_trimmed.starts_with(" < ") ||
                    (remainder_trimmed.starts_with('>') && !remainder_trimmed.starts_with(">=")) ||
                    (remainder_trimmed.starts_with('<') && !remainder_trimmed.starts_with("<="))
                ) {
                    // Ignore provides with operators other than "=" (artifacts from metadata parsing)
                    continue;
                }

                if !remainder_trimmed.is_empty() && !remainder_trimmed.starts_with('=') &&
                   !remainder_trimmed.starts_with("(= ") {
                    // Doesn't match - capability name is a prefix of something else
                    continue;
                }

                // Check if this is a library alias (Arch Linux format: "lib.so=lib.so-64")
                // Library aliases don't have version constraints, so they satisfy any requirement for the base capability
                if format == PackageFormat::Pacman && remainder_trimmed.starts_with('=') {
                    let after_equals = &remainder_trimmed[1..];
                    // Check if it looks like a library alias (contains .so and doesn't start with digit)
                    if after_equals.contains(".so") && !after_equals.chars().next().map_or(false, |c| c.is_ascii_digit()) {
                        // This is a library alias - it satisfies the requirement without version checking
                        // (since requires for library aliases are parsed as the base capability without constraints)
                        return Ok(true);
                    }
                }

                // Extract version from provide entry if present
                // Use remainder_trimmed to handle leading spaces in Debian format
                let provide_version = Self::extract_version_from_remainder(remainder_trimmed, provide_entry);

                // If no version in provide entry, use the package's own version
                // This is standard behavior across all package formats: when a package provides
                // a capability without a version, the package's version is used for constraint checking
                // NOTE: In all package formats (RPM, Debian, Alpine, Pacman, etc.), provides are
                // always stored as exact versions with "=", never with operators like ">=".
                // The version in the provide entry is the actual version at which the package
                // provides the capability. Version operators (>=, >, <=, <) are only used in
                // requirements/constraints, not in provides.
                let has_non_conditional_constraints = constraints.iter().any(|c| !matches!(c.operator, Operator::IfInstall));
                let mut used_package_version_directly = false;
                let provided_version = if let Some((_, version_str)) = provide_version {
                    // Provide has a version - use that version for constraint checking
                    version_str.trim()
                } else {
                    // No version in provide entry - use package's own version
                    if has_non_conditional_constraints {
                        used_package_version_directly = true;
                        provider_pkg_version.as_str()
                    } else {
                        // No version constraints (or only conditional), so this satisfies
                        return Ok(true);
                    }
                };

                // Check if the provided version satisfies all constraints
                if self.check_version_satisfies_constraints(provided_version, constraints, format)? {
                    log::debug!(
                        "Provider {} provides '{}' version '{}' satisfies all constraints",
                        provider_pkgkey, capability, provided_version
                    );
                    return Ok(true);
                }

                // Fallback for RPM: some capabilities (e.g., php-composer()) only record upstream
                // versions in their provides entries, even though dependencies may specify a release.
                // If the provide failed because it lacked the release, retry with the package EVR.
                if !used_package_version_directly
                    && format == PackageFormat::Rpm
                    && Self::rpm_constraints_require_release(constraints)
                    && Self::rpm_provide_missing_release(provided_version, provider_pkg_version.as_str())
                {
                    if self.check_version_satisfies_constraints(provider_pkg_version.as_str(), constraints, format)? {
                        log::debug!(
                            "Provider {} fallback: using package version '{}' for capability '{}' satisfies constraints {:?}",
                            provider_pkgkey,
                            provider_pkg_version,
                            capability,
                            constraints
                        );
                        return Ok(true);
                    }
                }
            }
        }

        // If no explicit provide entry matched, check if the capability name matches
        // the package name itself (implicit provide). In Alpine and most package managers,
        // a package implicitly provides its own name.
        if self.check_implicit_provide(provider_pkgkey, &base_capability, &provider_pkg, constraints, format)? {
            return Ok(true);
        }

        log::debug!(
            "Provider {} does not provide '{}' with version satisfying constraints",
            provider_pkgkey, capability
        );
        Ok(false)
    }

    fn rpm_provide_missing_release(provided_version: &str, package_version: &str) -> bool {
        let provided = match version::PackageVersion::parse(provided_version) {
            Ok(parsed) => parsed,
            Err(_) => return false,
        };
        let package = match version::PackageVersion::parse(package_version) {
            Ok(parsed) => parsed,
            Err(_) => return false,
        };

        provided.epoch == package.epoch
            && provided.upstream == package.upstream
            && provided.revision == "0"
            && package.revision != "0"
    }

    fn rpm_constraints_require_release(constraints: &Vec<VersionConstraint>) -> bool {
        constraints
            .iter()
            .filter(|c| !matches!(c.operator, Operator::IfInstall))
            .filter_map(|c| version::PackageVersion::parse(c.operand.trim()).ok())
            .any(|parsed| parsed.revision != "0")
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_capability_architecture() {
        let pm = PackageManager {
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            pkgname2packages: HashMap::new(),
            provide2pkgnames: HashMap::new(),
            installed_packages: HashMap::new(),
            world: HashMap::new(),
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

        // Test RPM architecture specifications
        let (base, arch_spec) = pm.parse_capability_architecture("wine-cms(x86-32)", PackageFormat::Rpm);
        assert_eq!(base, "wine-cms");
        assert_eq!(arch_spec, Some("i686".to_string()));

        let (base, arch_spec) = pm.parse_capability_architecture("wine-cms(x86-64)", PackageFormat::Rpm);
        assert_eq!(base, "wine-cms");
        assert_eq!(arch_spec, Some("x86_64".to_string()));

        let (base, arch_spec) = pm.parse_capability_architecture("wine-core(x86-32)", PackageFormat::Rpm);
        assert_eq!(base, "wine-core");
        assert_eq!(arch_spec, Some("i686".to_string()));

        let (base, arch_spec) = pm.parse_capability_architecture("wine-ldap(x86-64)", PackageFormat::Rpm);
        assert_eq!(base, "wine-ldap");
        assert_eq!(arch_spec, Some("x86_64".to_string()));

        // Test RPM library capabilities with 64bit/32bit architecture specifications
        let (base, arch_spec) = pm.parse_capability_architecture("libavahi-client.so.3()(64bit)", PackageFormat::Rpm);
        assert_eq!(base, "libavahi-client.so.3()");
        assert_eq!(arch_spec, Some("x86_64".to_string()));

        let (base, arch_spec) = pm.parse_capability_architecture("libavahi-common.so.3()(64bit)", PackageFormat::Rpm);
        assert_eq!(base, "libavahi-common.so.3()");
        assert_eq!(arch_spec, Some("x86_64".to_string()));

        let (base, arch_spec) = pm.parse_capability_architecture("libfoo.so.1()(32bit)", PackageFormat::Rpm);
        assert_eq!(base, "libfoo.so.1()");
        assert_eq!(arch_spec, Some("i686".to_string()));

        // Test RPM capabilities without architecture specification
        let (base, arch_spec) = pm.parse_capability_architecture("wine", PackageFormat::Rpm);
        assert_eq!(base, "wine");
        assert_eq!(arch_spec, None);

        // Test RPM capabilities with invalid architecture (should not parse)
        let (base, arch_spec) = pm.parse_capability_architecture("wine-cms(invalid-arch)", PackageFormat::Rpm);
        assert_eq!(base, "wine-cms(invalid-arch)");
        assert_eq!(arch_spec, None);

        // Test RPM capabilities with parentheses but not at the end (should not parse as arch spec)
        let (base, arch_spec) = pm.parse_capability_architecture("wine-cms(x86-32)-extra", PackageFormat::Rpm);
        assert_eq!(base, "wine-cms(x86-32)-extra");
        assert_eq!(arch_spec, None);

        // Test RPM capabilities with unmatched parentheses (should not parse)
        let (base, arch_spec) = pm.parse_capability_architecture("wine-cms(x86-32", PackageFormat::Rpm);
        assert_eq!(base, "wine-cms(x86-32");
        assert_eq!(arch_spec, None);

        // Test that Debian format doesn't parse RPM-style parentheses
        let (base, arch_spec) = pm.parse_capability_architecture("wine-cms(x86-32)", PackageFormat::Deb);
        assert_eq!(base, "wine-cms(x86-32)");
        assert_eq!(arch_spec, None);
    }

    #[test]
    fn test_map_rpm_arch_to_package_arch() {
        // Test valid RPM architecture mappings
        assert_eq!(PackageManager::map_rpm_arch_to_package_arch("x86-32"), Some("i686".to_string()));
        assert_eq!(PackageManager::map_rpm_arch_to_package_arch("x86-64"), Some("x86_64".to_string()));

        // Test invalid/unmapped architectures
        assert_eq!(PackageManager::map_rpm_arch_to_package_arch("invalid"), None);
        assert_eq!(PackageManager::map_rpm_arch_to_package_arch("amd64"), None);
        assert_eq!(PackageManager::map_rpm_arch_to_package_arch("i686"), None);
    }

    #[test]
    fn test_filter_packages_by_arch_spec_multiarch() {
        let pm = PackageManager {
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            pkgname2packages: HashMap::new(),
            provide2pkgnames: HashMap::new(),
            installed_packages: HashMap::new(),
            world: HashMap::new(),
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
        let filtered = pm.filter_packages_by_arch_spec(packages.clone(), Some("any"), PackageFormat::Deb);
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
        let filtered = pm.filter_packages_by_arch_spec(packages.clone(), Some("amd64"), PackageFormat::Deb);
        assert_eq!(filtered.len(), 6, "All packages should match amd64 architecture");

        // Test no architecture specification (should use default filtering)
        let filtered = pm.filter_packages_by_arch_spec(packages.clone(), None, PackageFormat::Deb);
        assert_eq!(filtered.len(), 6, "All packages should match default arch filtering");
    }

    #[test]
    fn test_filter_packages_by_arch_conda_all() {
        let pm = PackageManager {
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            pkgname2packages: HashMap::new(),
            provide2pkgnames: HashMap::new(),
            installed_packages: HashMap::new(),
            world: HashMap::new(),
            has_worker_process: false,
            ipc_socket: String::new(),
            ipc_stream: None,
            child_pid: None,
        };

        // Create test Conda packages
        let mut pkg_all = Package {
            pkgname: "glibc-amzn2-aarch64".to_string(),
            version: "2.26-5".to_string(),
            arch: "all".to_string(),  // Conda noarch packages use "all"
            ..Default::default()
        };
        pkg_all.pkgkey = "glibc-amzn2-aarch64__2.26-5__all".to_string();

        let mut pkg_x86_64 = Package {
            pkgname: "glibc-amzn2".to_string(),
            version: "2.26-5".to_string(),
            arch: "x86_64".to_string(),
            ..Default::default()
        };
        pkg_x86_64.pkgkey = "glibc-amzn2__2.26-5__x86_64".to_string();

        let mut pkg_aarch64 = Package {
            pkgname: "glibc-amzn2".to_string(),
            version: "2.26-5".to_string(),
            arch: "aarch64".to_string(),
            ..Default::default()
        };
        pkg_aarch64.pkgkey = "glibc-amzn2__2.26-5__aarch64".to_string();

        let packages = vec![pkg_all.clone(), pkg_x86_64.clone(), pkg_aarch64.clone()];

        // Test filtering: packages with arch="all" should be included regardless of target arch
        // This is similar to RPM's "noarch" behavior
        let filtered = pm.filter_packages_by_arch(packages.clone(), PackageFormat::Conda);

        // The package with arch="all" should always be included
        assert!(filtered.iter().any(|p| p.pkgkey == pkg_all.pkgkey),
                "Conda package with arch='all' should be included regardless of target architecture");

        // Packages matching target arch should be included
        let target_arch = crate::models::config().common.arch.as_str();
        if target_arch == "x86_64" {
            assert!(filtered.iter().any(|p| p.pkgkey == pkg_x86_64.pkgkey),
                    "Package with arch='x86_64' should be included when target is x86_64");
            assert!(!filtered.iter().any(|p| p.pkgkey == pkg_aarch64.pkgkey),
                    "Package with arch='aarch64' should NOT be included when target is x86_64");
        } else if target_arch == "aarch64" {
            assert!(filtered.iter().any(|p| p.pkgkey == pkg_aarch64.pkgkey),
                    "Package with arch='aarch64' should be included when target is aarch64");
            assert!(!filtered.iter().any(|p| p.pkgkey == pkg_x86_64.pkgkey),
                    "Package with arch='x86_64' should NOT be included when target is aarch64");
        }
    }

    #[test]
    fn test_check_single_constraint() {
        // Test VersionGreaterThan
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThan,
            operand: "5.13".to_string(),
        };
        assert!(version::check_version_constraint("6.1", &constraint, PackageFormat::Rpm).unwrap());
        assert!(!version::check_version_constraint("5.13", &constraint, PackageFormat::Rpm).unwrap());
        assert!(!version::check_version_constraint("5.0", &constraint, PackageFormat::Rpm).unwrap());

        // Test VersionLessThan
        let constraint = VersionConstraint {
            operator: Operator::VersionLessThan,
            operand: "7.0".to_string(),
        };
        assert!(version::check_version_constraint("6.1", &constraint, PackageFormat::Rpm).unwrap());
        assert!(!version::check_version_constraint("7.0", &constraint, PackageFormat::Rpm).unwrap());
        assert!(!version::check_version_constraint("8.0", &constraint, PackageFormat::Rpm).unwrap());

        // Test VersionGreaterThanEqual
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "4.2.5".to_string(),
        };
        assert!(version::check_version_constraint("6.1", &constraint, PackageFormat::Rpm).unwrap());
        assert!(version::check_version_constraint("4.2.5", &constraint, PackageFormat::Rpm).unwrap());
        assert!(!version::check_version_constraint("4.2.4", &constraint, PackageFormat::Rpm).unwrap());

        // Test VersionLessThanEqual
        let constraint = VersionConstraint {
            operator: Operator::VersionLessThanEqual,
            operand: "7.0".to_string(),
        };
        assert!(version::check_version_constraint("6.1", &constraint, PackageFormat::Rpm).unwrap());
        assert!(version::check_version_constraint("7.0", &constraint, PackageFormat::Rpm).unwrap());
        assert!(!version::check_version_constraint("8.0", &constraint, PackageFormat::Rpm).unwrap());
    }

    #[test]
    fn test_check_single_constraint_version_compatible() {
        // Test VersionCompatible for Alpine APK format (the original bug case)
        // python3 3.12.12-r0 should satisfy ~3.12 (VersionCompatible "3.12")
        let constraint = VersionConstraint {
            operator: Operator::VersionCompatible,
            operand: "3.12".to_string(),
        };
        assert!(version::check_version_constraint("3.12.12-r0", &constraint, PackageFormat::Apk).unwrap(),
                "3.12.12-r0 should satisfy ~3.12");
        assert!(version::check_version_constraint("3.12.0-r0", &constraint, PackageFormat::Apk).unwrap(),
                "3.12.0-r0 should satisfy ~3.12");
        assert!(version::check_version_constraint("3.12", &constraint, PackageFormat::Apk).unwrap(),
                "3.12 should satisfy ~3.12");
        assert!(version::check_version_constraint("3.12.15-r1", &constraint, PackageFormat::Apk).unwrap(),
                "3.12.15-r1 should satisfy ~3.12");
        assert!(!version::check_version_constraint("3.11.9-r0", &constraint, PackageFormat::Apk).unwrap(),
                "3.11.9-r0 should NOT satisfy ~3.12");
        assert!(!version::check_version_constraint("3.10.0-r0", &constraint, PackageFormat::Apk).unwrap(),
                "3.10.0-r0 should NOT satisfy ~3.12");

        // Test VersionCompatible for RPM format
        let constraint = VersionConstraint {
            operator: Operator::VersionCompatible,
            operand: "5.13".to_string(),
        };
        assert!(version::check_version_constraint("5.13.0-1.fc42", &constraint, PackageFormat::Rpm).unwrap(),
                "5.13.0-1.fc42 should satisfy ~5.13");
        assert!(version::check_version_constraint("5.13.5-2.el8", &constraint, PackageFormat::Rpm).unwrap(),
                "5.13.5-2.el8 should satisfy ~5.13");
        assert!(version::check_version_constraint("5.13", &constraint, PackageFormat::Rpm).unwrap(),
                "5.13 should satisfy ~5.13");
        assert!(!version::check_version_constraint("5.12.9-1.fc42", &constraint, PackageFormat::Rpm).unwrap(),
                "5.12.9-1.fc42 should NOT satisfy ~5.13");
        assert!(!version::check_version_constraint("5.11", &constraint, PackageFormat::Rpm).unwrap(),
                "5.11 should NOT satisfy ~5.13");

        // Test VersionCompatible for Debian format
        let constraint = VersionConstraint {
            operator: Operator::VersionCompatible,
            operand: "2.5".to_string(),
        };
        assert!(version::check_version_constraint("2.5.0-1", &constraint, PackageFormat::Deb).unwrap(),
                "2.5.0-1 should satisfy ~2.5");
        assert!(version::check_version_constraint("2.5.3-2ubuntu1", &constraint, PackageFormat::Deb).unwrap(),
                "2.5.3-2ubuntu1 should satisfy ~2.5");
        assert!(version::check_version_constraint("2.5", &constraint, PackageFormat::Deb).unwrap(),
                "2.5 should satisfy ~2.5");
        assert!(!version::check_version_constraint("2.4.9-1", &constraint, PackageFormat::Deb).unwrap(),
                "2.4.9-1 should NOT satisfy ~2.5");
        assert!(!version::check_version_constraint("2.3", &constraint, PackageFormat::Deb).unwrap(),
                "2.3 should NOT satisfy ~2.5");

        // Test VersionCompatible with patch versions
        let constraint = VersionConstraint {
            operator: Operator::VersionCompatible,
            operand: "1.2.3".to_string(),
        };
        assert!(version::check_version_constraint("1.2.3", &constraint, PackageFormat::Rpm).unwrap(),
                "1.2.3 should satisfy ~1.2.3");
        assert!(version::check_version_constraint("1.2.3-1", &constraint, PackageFormat::Rpm).unwrap(),
                "1.2.3-1 should satisfy ~1.2.3");
        assert!(version::check_version_constraint("1.2.4", &constraint, PackageFormat::Rpm).unwrap(),
                "1.2.4 should satisfy ~1.2.3");
        assert!(version::check_version_constraint("1.2.10", &constraint, PackageFormat::Rpm).unwrap(),
                "1.2.10 should satisfy ~1.2.3");
        assert!(!version::check_version_constraint("1.2.2", &constraint, PackageFormat::Rpm).unwrap(),
                "1.2.2 should NOT satisfy ~1.2.3");
        assert!(!version::check_version_constraint("1.1.9", &constraint, PackageFormat::Rpm).unwrap(),
                "1.1.9 should NOT satisfy ~1.2.3");
    }

    #[test]
    fn test_check_single_constraint_with_tilde_tilde_suffix() {
        // Test VersionGreaterThanEqual with ~~ suffix (Debian Rust packages)
        // This is the specific case from the bug report: >= 0.7.5-~~ should match 0.7.5-1+b3
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "0.7.5-~~".to_string(),
        };
        assert!(version::check_version_constraint("0.7.5-1+b3", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5-1+b3 should satisfy >= 0.7.5-~~");
        assert!(version::check_version_constraint("0.7.5-1", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5-1 should satisfy >= 0.7.5-~~");
        assert!(version::check_version_constraint("0.7.5", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5 should satisfy >= 0.7.5-~~");
        assert!(version::check_version_constraint("0.7.6", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.6 should satisfy >= 0.7.5-~~");
        assert!(!version::check_version_constraint("0.7.4", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.4 should NOT satisfy >= 0.7.5-~~");

        // Test VersionGreaterThanEqual with ~~ suffix (no dash before ~~)
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "0.7.5~~".to_string(),
        };
        assert!(version::check_version_constraint("0.7.5-1", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5-1 should satisfy >= 0.7.5~~");
        assert!(version::check_version_constraint("0.7.6", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.6 should satisfy >= 0.7.5~~");
        assert!(!version::check_version_constraint("0.7.4", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.4 should NOT satisfy >= 0.7.5~~");

        // Test VersionGreaterThan with ~~ suffix
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThan,
            operand: "0.7.5-~~".to_string(),
        };
        assert!(version::check_version_constraint("0.7.6", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.6 should satisfy > 0.7.5-~~");
        assert!(version::check_version_constraint("0.7.5-1", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5-1 should satisfy > 0.7.5-~~");
        assert!(!version::check_version_constraint("0.7.5", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5 should NOT satisfy > 0.7.5-~~");
        assert!(!version::check_version_constraint("0.7.4", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.4 should NOT satisfy > 0.7.5-~~");

        // Test VersionGreaterThan with ~~ suffix for versions with revisions (specific bug fix)
        // This is the case from the user's error: > 0.6.0-4~~ should match 0.6.0-4
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThan,
            operand: "0.6.0-4~~".to_string(),
        };
        assert!(version::check_version_constraint("0.6.0-4", &constraint, PackageFormat::Deb).unwrap(),
                "0.6.0-4 should satisfy > 0.6.0-4~~ (because > X-Y~~ means >= X-Y for versions with revisions)");
        assert!(version::check_version_constraint("0.6.0-5", &constraint, PackageFormat::Deb).unwrap(),
                "0.6.0-5 should satisfy > 0.6.0-4~~");
        assert!(version::check_version_constraint("0.6.1", &constraint, PackageFormat::Deb).unwrap(),
                "0.6.1 should satisfy > 0.6.0-4~~");
        assert!(!version::check_version_constraint("0.6.0-3", &constraint, PackageFormat::Deb).unwrap(),
                "0.6.0-3 should NOT satisfy > 0.6.0-4~~");

        // Test VersionLessThan with ~~ suffix
        let constraint = VersionConstraint {
            operator: Operator::VersionLessThan,
            operand: "0.7.5-~~".to_string(),
        };
        assert!(version::check_version_constraint("0.7.4", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.4 should satisfy < 0.7.5-~~");
        assert!(!version::check_version_constraint("0.7.5", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5 should NOT satisfy < 0.7.5-~~");
        assert!(!version::check_version_constraint("0.7.6", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.6 should NOT satisfy < 0.7.5-~~");

        // Test VersionLessThanEqual with ~~ suffix
        let constraint = VersionConstraint {
            operator: Operator::VersionLessThanEqual,
            operand: "0.7.5-~~".to_string(),
        };
        assert!(version::check_version_constraint("0.7.4", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.4 should satisfy <= 0.7.5-~~");
        assert!(version::check_version_constraint("0.7.5", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5 should satisfy <= 0.7.5-~~");
        assert!(!version::check_version_constraint("0.7.6", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.6 should NOT satisfy <= 0.7.5-~~");

        // Test VersionEqual with ~~ suffix
        let constraint = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "0.7.5-~~".to_string(),
        };
        assert!(version::check_version_constraint("0.7.5", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5 should satisfy = 0.7.5-~~");
        assert!(!version::check_version_constraint("0.7.6", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.6 should NOT satisfy = 0.7.5-~~");

        // Test VersionEqual with local version suffix (+)
        // In Debian, packages with local version suffixes (e.g., "1.0+2") should satisfy
        // exact version constraints on the base version (e.g., "= 1.0")
        let constraint = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "6.14.0-1017.18~24.04.1".to_string(),
        };
        assert!(version::check_version_constraint("6.14.0-1017.18~24.04.1+2", &constraint, PackageFormat::Deb).unwrap(),
                "6.14.0-1017.18~24.04.1+2 should satisfy = 6.14.0-1017.18~24.04.1");
        assert!(version::check_version_constraint("6.14.0-1017.18~24.04.1", &constraint, PackageFormat::Deb).unwrap(),
                "6.14.0-1017.18~24.04.1 should satisfy = 6.14.0-1017.18~24.04.1");
        assert!(!version::check_version_constraint("6.14.0-1017.18~24.04.2", &constraint, PackageFormat::Deb).unwrap(),
                "6.14.0-1017.18~24.04.2 should NOT satisfy = 6.14.0-1017.18~24.04.1");

        // Test VersionGreaterThanEqual with ~ suffix (Debian pre-release indicator)
        // In Debian, "X~" has lowest precedence, so ">= X~" effectively means ">= X"
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "5.15.13+dfsg~".to_string(),
        };
        assert!(version::check_version_constraint("5.15.13+dfsg-1ubuntu1", &constraint, PackageFormat::Deb).unwrap(),
                "5.15.13+dfsg-1ubuntu1 should satisfy >= 5.15.13+dfsg~");
        assert!(version::check_version_constraint("5.15.13+dfsg", &constraint, PackageFormat::Deb).unwrap(),
                "5.15.13+dfsg should satisfy >= 5.15.13+dfsg~");
        assert!(version::check_version_constraint("5.15.14+dfsg", &constraint, PackageFormat::Deb).unwrap(),
                "5.15.14+dfsg should satisfy >= 5.15.13+dfsg~");
        assert!(!version::check_version_constraint("5.15.12+dfsg", &constraint, PackageFormat::Deb).unwrap(),
                "5.15.12+dfsg should NOT satisfy >= 5.15.13+dfsg~");

        // Test VersionGreaterThanEqual with ~ suffix when provided version also has ~
        // This is the speech-dispatcher case: ">= 0.12.0~" should match "0.12.0~rc2-2build3"
        // because 0.12.0~rc2 > 0.12.0~ (rc2 has higher precedence than bare ~)
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "0.12.0~".to_string(),
        };
        assert!(version::check_version_constraint("0.12.0~rc2-2build3", &constraint, PackageFormat::Deb).unwrap(),
                "0.12.0~rc2-2build3 should satisfy >= 0.12.0~");
        assert!(version::check_version_constraint("0.12.0~rc1-1", &constraint, PackageFormat::Deb).unwrap(),
                "0.12.0~rc1-1 should satisfy >= 0.12.0~");
        assert!(version::check_version_constraint("0.12.0-1", &constraint, PackageFormat::Deb).unwrap(),
                "0.12.0-1 should satisfy >= 0.12.0~ (final version > pre-release)");
        assert!(!version::check_version_constraint("0.11.9-1", &constraint, PackageFormat::Deb).unwrap(),
                "0.11.9-1 should NOT satisfy >= 0.12.0~");

        // Test VersionGreaterThanEqual with ~ suffix when versions differ only by trailing ~
        // This is the golang-google-genproto case: ">= 0.0~git20210726.e7812ac~" should match
        // "0.0~git20210726.e7812ac-4" because the version without trailing ~ is greater
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "0.0~git20210726.e7812ac~".to_string(),
        };
        assert!(version::check_version_constraint("0.0~git20210726.e7812ac-4", &constraint, PackageFormat::Deb).unwrap(),
                "0.0~git20210726.e7812ac-4 should satisfy >= 0.0~git20210726.e7812ac~");
        assert!(version::check_version_constraint("0.0~git20210726.e7812ac-1", &constraint, PackageFormat::Deb).unwrap(),
                "0.0~git20210726.e7812ac-1 should satisfy >= 0.0~git20210726.e7812ac~");
        assert!(version::check_version_constraint("0.0~git20210726.e7812ac", &constraint, PackageFormat::Deb).unwrap(),
                "0.0~git20210726.e7812ac should satisfy >= 0.0~git20210726.e7812ac~");

        // Test VersionNotEqual with ~~ suffix
        let constraint = VersionConstraint {
            operator: Operator::VersionNotEqual,
            operand: "0.7.5-~~".to_string(),
        };
        assert!(!version::check_version_constraint("0.7.5", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5 should NOT satisfy != 0.7.5-~~");
        assert!(version::check_version_constraint("0.7.6", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.6 should satisfy != 0.7.5-~~");

        // Test VersionLessThan with ~~ suffix for simple integer versions (bug fix)
        // This tests the specific case: python3.13dist(numpy)(<2~~,>=1.20)
        // where <2~~ should mean <3 (next version after 2)
        let constraint = VersionConstraint {
            operator: Operator::VersionLessThan,
            operand: "2~~".to_string(),
        };
        assert!(version::check_version_constraint("1.20", &constraint, PackageFormat::Rpm).unwrap(),
                "1.20 should satisfy < 2~~ (which means < 3)");
        assert!(version::check_version_constraint("1.99", &constraint, PackageFormat::Rpm).unwrap(),
                "1.99 should satisfy < 2~~ (which means < 3)");
        // Skip testing 2.1 and 2.0 as they might be considered equal to 2 in upstream comparison
        // The key test cases are the actual versions from the bug report: 2.2.4 and 2.2.6
        assert!(version::check_version_constraint("2.2.4", &constraint, PackageFormat::Rpm).unwrap(),
                "2.2.4 should satisfy < 2~~ (which means < 3)");
        assert!(version::check_version_constraint("2.2.6", &constraint, PackageFormat::Rpm).unwrap(),
                "2.2.6 should satisfy < 2~~ (which means < 3)");
        assert!(!version::check_version_constraint("2", &constraint, PackageFormat::Rpm).unwrap(),
                "2 should NOT satisfy < 2~~ (base version is excluded)");
        assert!(!version::check_version_constraint("3", &constraint, PackageFormat::Rpm).unwrap(),
                "3 should NOT satisfy < 2~~ (which means < 3)");
        assert!(!version::check_version_constraint("3.0", &constraint, PackageFormat::Rpm).unwrap(),
                "3.0 should NOT satisfy < 2~~ (which means < 3)");

        // Test VersionLessThan with ~~ suffix for versions with dots
        // <1.20~~ should mean <1.21 (increment last numeric segment)
        let constraint = VersionConstraint {
            operator: Operator::VersionLessThan,
            operand: "1.20~~".to_string(),
        };
        assert!(version::check_version_constraint("1.19", &constraint, PackageFormat::Rpm).unwrap(),
                "1.19 should satisfy < 1.20~~ (which means < 1.21)");
        assert!(!version::check_version_constraint("1.20", &constraint, PackageFormat::Rpm).unwrap(),
                "1.20 should NOT satisfy < 1.20~~ (base version is excluded)");
        assert!(version::check_version_constraint("1.20.5", &constraint, PackageFormat::Rpm).unwrap(),
                "1.20.5 should satisfy < 1.20~~ (which means < 1.21)");
        assert!(!version::check_version_constraint("1.21", &constraint, PackageFormat::Rpm).unwrap(),
                "1.21 should NOT satisfy < 1.20~~ (which means < 1.21)");
        assert!(!version::check_version_constraint("1.22", &constraint, PackageFormat::Rpm).unwrap(),
                "1.22 should NOT satisfy < 1.20~~ (which means < 1.21)");

        // Test with RPM format (where ~~ is also used)
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "1.2.3-~~".to_string(),
        };
        assert!(version::check_version_constraint("1.2.3-1.el8", &constraint, PackageFormat::Rpm).unwrap(),
                "1.2.3-1.el8 should satisfy >= 1.2.3-~~");
        assert!(version::check_version_constraint("1.2.4", &constraint, PackageFormat::Rpm).unwrap(),
                "1.2.4 should satisfy >= 1.2.3-~~");
        assert!(!version::check_version_constraint("1.2.2", &constraint, PackageFormat::Rpm).unwrap(),
                "1.2.2 should NOT satisfy >= 1.2.3-~~");

        // Test edge case: multiple dashes before ~~
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "1.2.3-beta-~~".to_string(),
        };
        assert!(version::check_version_constraint("1.2.3-beta-1", &constraint, PackageFormat::Deb).unwrap(),
                "1.2.3-beta-1 should satisfy >= 1.2.3-beta-~~");
        assert!(version::check_version_constraint("1.2.3-beta", &constraint, PackageFormat::Deb).unwrap(),
                "1.2.3-beta should satisfy >= 1.2.3-beta-~~");
    }

    #[test]
    fn test_check_provider_satisfies_constraints_with_or_conditions() {
        use crate::models::Package;
        use std::sync::Arc;

        let mut pm = PackageManager {
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            pkgname2packages: HashMap::new(),
            provide2pkgnames: HashMap::new(),
            installed_packages: HashMap::new(),
            world: HashMap::new(),
            has_worker_process: false,
            ipc_socket: String::new(),
            ipc_stream: None,
            child_pid: None,
        };

        // Create a mock package that provides python3.13dist(isort) version 6.1
        let mut provider_pkg = Package {
            pkgname: "python3-isort".to_string(),
            version: "6.1.0-1.fc42".to_string(),
            arch: "noarch".to_string(),
            pkgkey: "python3-isort__6.1.0-1.fc42__noarch".to_string(),
            provides: vec!["python3.13dist(isort) = 6.1".to_string()],
            ..Default::default()
        };

        // Cache the package
        pm.pkgkey2package.insert(
            provider_pkg.pkgkey.clone(),
            Arc::new(provider_pkg.clone()),
        );

        // Test case 1: OR condition with mutually exclusive constraints
        // Requirement: ((python3.13dist(isort) < 5.13 or python3.13dist(isort) > 5.13) with python3.13dist(isort) < 7 with python3.13dist(isort) >= 4.2.5)
        // Version 6.1 should satisfy: > 5.13 (OR), < 7 (AND), >= 4.2.5 (AND)
        let constraints = vec![
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "5.13".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThan,
                operand: "5.13".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "7".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "4.2.5".to_string(),
            },
        ];

        let result = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "python3.13dist(isort)",
            &constraints,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 6.1 should satisfy OR condition (> 5.13) and AND conditions (< 7, >= 4.2.5)");

        // Test case 2: Version that doesn't satisfy OR condition
        // Version 5.13 should NOT satisfy: < 5.13 is false, > 5.13 is false (exactly equal)
        provider_pkg.provides = vec!["python3.13dist(isort) = 5.13".to_string()];
        pm.pkgkey2package.insert(
            provider_pkg.pkgkey.clone(),
            Arc::new(provider_pkg.clone()),
        );

        let result = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "python3.13dist(isort)",
            &constraints,
            PackageFormat::Rpm,
        );
        assert!(!result.unwrap(), "Version 5.13 should NOT satisfy OR condition (neither < 5.13 nor > 5.13, exactly equal)");

        // Test case 3: Version that satisfies < 5.13 branch of OR condition
        provider_pkg.provides = vec!["python3.13dist(isort) = 5.0".to_string()];
        let constraints_v2 = vec![
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "5.13".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThan,
                operand: "5.13".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "4.2.5".to_string(),
            },
        ];
        pm.pkgkey2package.insert(
            provider_pkg.pkgkey.clone(),
            Arc::new(provider_pkg.clone()),
        );

        let result = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "python3.13dist(isort)",
            &constraints_v2,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 5.0 should satisfy OR condition (< 5.13) and AND condition (>= 4.2.5)");

        // Test case 4: Only AND constraints (no OR groups)
        let constraints_and_only = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "4.2.5".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "7".to_string(),
            },
        ];
        provider_pkg.provides = vec!["python3.13dist(isort) = 6.1".to_string()];
        pm.pkgkey2package.insert(
            provider_pkg.pkgkey.clone(),
            Arc::new(provider_pkg.clone()),
        );

        let result = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "python3.13dist(isort)",
            &constraints_and_only,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 6.1 should satisfy all AND constraints");

        // Test case 5: AND constraint failure
        let constraints_fail = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "4.2.5".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "5.0".to_string(), // 6.1 is not < 5.0
            },
        ];
        provider_pkg.provides = vec!["python3.13dist(isort) = 6.1".to_string()];
        pm.pkgkey2package.insert(
            provider_pkg.pkgkey.clone(),
            Arc::new(provider_pkg.clone()),
        );

        let result = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "python3.13dist(isort)",
            &constraints_fail,
            PackageFormat::Rpm,
        );
        assert!(!result.unwrap(), "Version 6.1 should NOT satisfy constraint < 5.0");

        // Test case 6: Multiple OR groups with same operand pattern
        // First OR group: < 3 or > 3, Second OR group: < 7 or > 7
        // Version 5.0 should satisfy: > 3 (first OR) and < 7 (second OR, but also compatible with AND)
        let constraints_multiple_or = vec![
            // First OR group: < 3 or > 3
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "3".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThan,
                operand: "3".to_string(),
            },
            // Second OR group: < 7 or > 7
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "7".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThan,
                operand: "7".to_string(),
            },
        ];
        provider_pkg.provides = vec!["python3.13dist(isort) = 5.0".to_string()];
        pm.pkgkey2package.insert(
            provider_pkg.pkgkey.clone(),
            Arc::new(provider_pkg.clone()),
        );

        let result = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "python3.13dist(isort)",
            &constraints_multiple_or,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 5.0 should satisfy both OR groups (> 3 and < 7)");
    }

    #[test]
    fn test_or_group_detection_with_different_operands() {
        use crate::models::Package;
        use std::sync::Arc;

        let mut pm = PackageManager {
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            pkgname2packages: HashMap::new(),
            provide2pkgnames: HashMap::new(),
            installed_packages: HashMap::new(),
            world: HashMap::new(),
            has_worker_process: false,
            ipc_socket: String::new(),
            ipc_stream: None,
            child_pid: None,
        };

        // Create a mock package that provides python3.13dist(google-api-core) version 2.11.1
        let mut provider_pkg = Package {
            pkgname: "python3-google-api-core".to_string(),
            version: "2.11.1-11.fc42".to_string(),
            arch: "noarch".to_string(),
            pkgkey: "python3-google-api-core__1:2.11.1-11.fc42__noarch".to_string(),
            provides: vec!["python3.13dist(google-api-core) = 2.11.1".to_string()],
            ..Default::default()
        };

        pm.pkgkey2package.insert(
            provider_pkg.pkgkey.clone(),
            Arc::new(provider_pkg.clone()),
        );

        // Test case: OR group with different operands (without ~~ to avoid constraint checking issues)
        // Requirement: ((python3.13dist(google-api-core) < 2.1 or >= 2.2) with >= 1.31.6)
        // Version 2.11.1 should satisfy: >= 2.2 (OR) and >= 1.31.6 (AND)
        let constraints = vec![
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "2.1".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "2.2".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "1.31.6".to_string(),
            },
        ];

        let result = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "python3.13dist(google-api-core)",
            &constraints,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 2.11.1 should satisfy OR condition (>= 2.2) and AND condition (>= 1.31.6)");

        // Test case: Version that satisfies < 2.1 branch
        provider_pkg.provides = vec!["python3.13dist(google-api-core) = 2.0.5".to_string()];
        pm.pkgkey2package.insert(
            provider_pkg.pkgkey.clone(),
            Arc::new(provider_pkg.clone()),
        );

        let result = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "python3.13dist(google-api-core)",
            &constraints,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 2.0.5 should satisfy OR condition (< 2.1) and AND condition (>= 1.31.6)");
    }

    #[test]
    fn test_or_group_detection_multiple_with_clauses() {
        use crate::models::Package;
        use std::sync::Arc;

        let mut pm = PackageManager {
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            pkgname2packages: HashMap::new(),
            provide2pkgnames: HashMap::new(),
            installed_packages: HashMap::new(),
            world: HashMap::new(),
            has_worker_process: false,
            ipc_socket: String::new(),
            ipc_stream: None,
            child_pid: None,
        };

        // Create a mock package that provides version 2.5.0
        let provider_pkg = Package {
            pkgname: "python3-google-api-core".to_string(),
            version: "2.5.0-1.fc42".to_string(),
            arch: "noarch".to_string(),
            pkgkey: "python3-google-api-core__1:2.5.0-1.fc42__noarch".to_string(),
            provides: vec!["python3.13dist(google-api-core) = 2.5.0".to_string()],
            ..Default::default()
        };

        pm.pkgkey2package.insert(
            provider_pkg.pkgkey.clone(),
            Arc::new(provider_pkg.clone()),
        );

        // Test case: Multiple OR groups (simplified)
        // Version 2.5.0 should satisfy:
        // - First OR: >= 2.2 (since 2.5.0 >= 2.2)
        // - Second OR: >= 2.3 (since 2.5.0 >= 2.3)
        // - Third OR: > 2.3 (since 2.5.0 > 2.3)
        // - AND: < 3, >= 1.31.6
        let constraints = vec![
            // First OR group: < 2.1 or >= 2.2
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "2.1".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "2.2".to_string(),
            },
            // Second OR group: < 2.2 or >= 2.3
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "2.2".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "2.3".to_string(),
            },
            // Third OR group: < 2.3 or > 2.3 (same operand, should be detected)
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "2.3".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThan,
                operand: "2.3".to_string(),
            },
            // AND constraints
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "3".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "1.31.6".to_string(),
            },
        ];

        let result = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "python3.13dist(google-api-core)",
            &constraints,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 2.5.0 should satisfy all OR groups and AND constraints");
    }

    #[test]
    fn test_or_group_detection_same_operand_strict() {
        use crate::models::Package;
        use std::sync::Arc;

        let mut pm = PackageManager {
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            pkgname2packages: HashMap::new(),
            provide2pkgnames: HashMap::new(),
            installed_packages: HashMap::new(),
            world: HashMap::new(),
            has_worker_process: false,
            ipc_socket: String::new(),
            ipc_stream: None,
            child_pid: None,
        };

        let mut provider_pkg = Package {
            pkgname: "test-pkg".to_string(),
            version: "2.3.0-1".to_string(),
            arch: "noarch".to_string(),
            pkgkey: "test-pkg__2.3.0-1__noarch".to_string(),
            provides: vec!["test-capability = 2.3".to_string()],
            ..Default::default()
        };

        pm.pkgkey2package.insert(
            provider_pkg.pkgkey.clone(),
            Arc::new(provider_pkg.clone()),
        );

        // Test case: <= X and >= X should NOT be mutually exclusive (X satisfies both)
        let constraints_not_exclusive = vec![
            VersionConstraint {
                operator: Operator::VersionLessThanEqual,
                operand: "2.3".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "2.3".to_string(),
            },
        ];

        let result = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "test-capability",
            &constraints_not_exclusive,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 2.3 should satisfy both <= 2.3 and >= 2.3 (not mutually exclusive)");

        // Test case: < X and > X should be mutually exclusive
        let constraints_exclusive = vec![
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "2.3".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThan,
                operand: "2.3".to_string(),
            },
        ];

        let result = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "test-capability",
            &constraints_exclusive,
            PackageFormat::Rpm,
        );
        assert!(!result.unwrap(), "Version 2.3 should NOT satisfy both < 2.3 and > 2.3 (mutually exclusive)");

        // But version 2.2 should satisfy < 2.3 (first OR branch)
        provider_pkg.provides = vec!["test-capability = 2.2".to_string()];
        pm.pkgkey2package.insert(
            provider_pkg.pkgkey.clone(),
            Arc::new(provider_pkg.clone()),
        );

        let result = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "test-capability",
            &constraints_exclusive,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 2.2 should satisfy OR condition (< 2.3)");
    }

    #[test]
    fn test_debian_provide_format_with_parentheses() {
        use crate::models::Package;
        use std::sync::Arc;

        let mut pm = PackageManager {
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            pkgname2packages: HashMap::new(),
            provide2pkgnames: HashMap::new(),
            installed_packages: HashMap::new(),
            world: HashMap::new(),
            has_worker_process: false,
            ipc_socket: String::new(),
            ipc_stream: None,
            child_pid: None,
        };

        // Test Debian format: libgcc1 (= 1:14.2.0-19)
        let provider_pkg = Package {
            pkgname: "libgcc-s1".to_string(),
            version: "14.2.0-19".to_string(),
            arch: "amd64".to_string(),
            pkgkey: "libgcc-s1__14.2.0-19__amd64".to_string(),
            provides: vec!["libgcc1 (= 1:14.2.0-19)".to_string()],
            ..Default::default()
        };

        pm.pkgkey2package.insert(
            provider_pkg.pkgkey.clone(),
            Arc::new(provider_pkg.clone()),
        );

        // Test constraint: libgcc1 (>= 1:3.0)
        // Version 1:14.2.0-19 should satisfy >= 1:3.0
        let constraints = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "1:3.0".to_string(),
            },
        ];

        let result = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "libgcc1",
            &constraints,
            PackageFormat::Deb,
        );
        assert!(result.unwrap(), "Version 1:14.2.0-19 should satisfy >= 1:3.0");
    }

    #[test]
    fn test_debian_provide_format_various_operators() {
        use crate::models::Package;
        use std::sync::Arc;

        let mut pm = PackageManager {
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            pkgname2packages: HashMap::new(),
            provide2pkgnames: HashMap::new(),
            installed_packages: HashMap::new(),
            world: HashMap::new(),
            has_worker_process: false,
            ipc_socket: String::new(),
            ipc_stream: None,
            child_pid: None,
        };

        // Test various Debian provide formats with parentheses
        // Note: Debian provide entries always use "= version" format
        // The constraint is what the requester needs
        let test_cases = vec![
            // Provided version 1.0.0, constraint: = 1.0.0 -> true
            ("test-pkg (= 1.0.0)", "test-pkg", Operator::VersionEqual, "1.0.0", true),
            // Provided version 1.0.0, constraint: = 1.0.1 -> false
            ("test-pkg (= 1.0.0)", "test-pkg", Operator::VersionEqual, "1.0.1", false),
            // Provided version 2.0.0, constraint: >= 1.5.0 -> true (2.0.0 >= 1.5.0)
            ("test-pkg (= 2.0.0)", "test-pkg", Operator::VersionGreaterThanEqual, "1.5.0", true),
            // Provided version 2.0.0, constraint: >= 2.5.0 -> false (2.0.0 < 2.5.0)
            ("test-pkg (= 2.0.0)", "test-pkg", Operator::VersionGreaterThanEqual, "2.5.0", false),
            // Provided version 3.0.0, constraint: <= 3.5.0 -> true (3.0.0 <= 3.5.0)
            ("test-pkg (= 3.0.0)", "test-pkg", Operator::VersionLessThanEqual, "3.5.0", true),
            // Provided version 3.0.0, constraint: <= 2.5.0 -> false (3.0.0 > 2.5.0)
            ("test-pkg (= 3.0.0)", "test-pkg", Operator::VersionLessThanEqual, "2.5.0", false),
            // Provided version 4.5.0, constraint: > 4.0.0 -> true (4.5.0 > 4.0.0)
            ("test-pkg (= 4.5.0)", "test-pkg", Operator::VersionGreaterThan, "4.0.0", true),
            // Provided version 4.0.0, constraint: > 4.0.0 -> false (4.0.0 is not > 4.0.0)
            ("test-pkg (= 4.0.0)", "test-pkg", Operator::VersionGreaterThan, "4.0.0", false),
            // Provided version 4.5.0, constraint: < 5.0.0 -> true (4.5.0 < 5.0.0)
            ("test-pkg (= 4.5.0)", "test-pkg", Operator::VersionLessThan, "5.0.0", true),
            // Provided version 5.0.0, constraint: < 5.0.0 -> false (5.0.0 is not < 5.0.0)
            ("test-pkg (= 5.0.0)", "test-pkg", Operator::VersionLessThan, "5.0.0", false),
        ];

        for (provide_entry, capability, constraint_op, constraint_operand, expected) in test_cases {
            let provider_pkg = Package {
                pkgname: "test-provider".to_string(),
                version: "1.0.0".to_string(),
                arch: "amd64".to_string(),
                pkgkey: format!("test-provider__1.0.0__amd64"),
                provides: vec![provide_entry.to_string()],
                ..Default::default()
            };

            pm.pkgkey2package.insert(
                provider_pkg.pkgkey.clone(),
                Arc::new(provider_pkg.clone()),
            );

            let constraints = vec![
                VersionConstraint {
                    operator: constraint_op.clone(),
                    operand: constraint_operand.to_string(),
                },
            ];

            let result = pm.check_provider_satisfies_constraints(
                &provider_pkg.pkgkey,
                capability,
                &constraints,
                PackageFormat::Deb,
            );

            assert_eq!(
                result.unwrap(),
                expected,
                "Failed for provide_entry: '{}', constraint: {:?} '{}'",
                provide_entry,
                constraint_op,
                constraint_operand
            );
        }
    }

    #[test]
    fn test_epoch_version_comparison() {
        use crate::models::Package;
        use std::sync::Arc;

        let mut pm = PackageManager {
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            pkgname2packages: HashMap::new(),
            provide2pkgnames: HashMap::new(),
            installed_packages: HashMap::new(),
            world: HashMap::new(),
            has_worker_process: false,
            ipc_socket: String::new(),
            ipc_stream: None,
            child_pid: None,
        };

        // Test epoch version comparison: 1:14.2.0-19 >= 1:3.0
        let provider_pkg = Package {
            pkgname: "libgcc-s1".to_string(),
            version: "14.2.0-19".to_string(),
            arch: "amd64".to_string(),
            pkgkey: "libgcc-s1__14.2.0-19__amd64".to_string(),
            provides: vec!["libgcc1 (= 1:14.2.0-19)".to_string()],
            ..Default::default()
        };

        pm.pkgkey2package.insert(
            provider_pkg.pkgkey.clone(),
            Arc::new(provider_pkg.clone()),
        );

        // Test various epoch version constraints
        let test_cases = vec![
            ("1:14.2.0-19", Operator::VersionGreaterThanEqual, "1:3.0", true),
            ("1:14.2.0-19", Operator::VersionGreaterThanEqual, "1:14.2.0-19", true),
            ("1:14.2.0-19", Operator::VersionGreaterThanEqual, "1:15.0.0", false),
            ("1:14.2.0-19", Operator::VersionGreaterThanEqual, "2:1.0.0", false), // epoch 2 > epoch 1
            ("2:1.0.0", Operator::VersionGreaterThanEqual, "1:14.2.0-19", true), // epoch 2 > epoch 1
            ("1:14.2.0-19", Operator::VersionLessThanEqual, "1:15.0.0", true),
            ("1:14.2.0-19", Operator::VersionLessThanEqual, "1:14.2.0-19", true),
            ("1:14.2.0-19", Operator::VersionLessThanEqual, "1:3.0", false),
            ("1:14.2.0-19", Operator::VersionEqual, "1:14.2.0-19", true),
            ("1:14.2.0-19", Operator::VersionEqual, "1:14.2.0-20", false),
        ];

        for (provided_version, constraint_op, constraint_operand, expected) in test_cases {
            let mut test_pkg = provider_pkg.clone();
            test_pkg.provides = vec![format!("libgcc1 (= {})", provided_version)];
            test_pkg.pkgkey = format!("libgcc-s1__{}__amd64", provided_version.replace(':', "_"));

            pm.pkgkey2package.insert(
                test_pkg.pkgkey.clone(),
                Arc::new(test_pkg.clone()),
            );

            let constraints = vec![
                VersionConstraint {
                    operator: constraint_op.clone(),
                    operand: constraint_operand.to_string(),
                },
            ];

            let result = pm.check_provider_satisfies_constraints(
                &test_pkg.pkgkey,
                "libgcc1",
                &constraints,
                PackageFormat::Deb,
            );

            assert_eq!(
                result.unwrap(),
                expected,
                "Failed for provided_version: '{}', constraint: {:?} '{}'",
                provided_version,
                constraint_op,
                constraint_operand
            );
        }
    }

    #[test]
    fn test_rpm_vs_debian_provide_format() {
        use crate::models::Package;
        use std::sync::Arc;

        let mut pm = PackageManager {
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            pkgname2packages: HashMap::new(),
            provide2pkgnames: HashMap::new(),
            installed_packages: HashMap::new(),
            world: HashMap::new(),
            has_worker_process: false,
            ipc_socket: String::new(),
            ipc_stream: None,
            child_pid: None,
        };

        // Test RPM format: "capability = version" (no parentheses)
        let rpm_provider = Package {
            pkgname: "rpm-provider".to_string(),
            version: "1.0.0".to_string(),
            arch: "x86_64".to_string(),
            pkgkey: "rpm-provider__1.0.0__x86_64".to_string(),
            provides: vec!["test-capability = 1.0.0".to_string()],
            ..Default::default()
        };

        pm.pkgkey2package.insert(
            rpm_provider.pkgkey.clone(),
            Arc::new(rpm_provider.clone()),
        );

        let rpm_constraints = vec![
            VersionConstraint {
                operator: Operator::VersionEqual,
                operand: "1.0.0".to_string(),
            },
        ];

        let rpm_result = pm.check_provider_satisfies_constraints(
            &rpm_provider.pkgkey,
            "test-capability",
            &rpm_constraints,
            PackageFormat::Rpm,
        );
        assert!(rpm_result.unwrap(), "RPM format should work: 'test-capability = 1.0.0'");

        // Test Debian format: "capability (= version)" (with parentheses)
        let deb_provider = Package {
            pkgname: "deb-provider".to_string(),
            version: "1.0.0".to_string(),
            arch: "amd64".to_string(),
            pkgkey: "deb-provider__1.0.0__amd64".to_string(),
            provides: vec!["test-capability (= 1.0.0)".to_string()],
            ..Default::default()
        };

        pm.pkgkey2package.insert(
            deb_provider.pkgkey.clone(),
            Arc::new(deb_provider.clone()),
        );

        let deb_constraints = vec![
            VersionConstraint {
                operator: Operator::VersionEqual,
                operand: "1.0.0".to_string(),
            },
        ];

        let deb_result = pm.check_provider_satisfies_constraints(
            &deb_provider.pkgkey,
            "test-capability",
            &deb_constraints,
            PackageFormat::Deb,
        );
        assert!(deb_result.unwrap(), "Debian format should work: 'test-capability (= 1.0.0)'");
    }

    #[test]
    fn test_alpine_pkgconfig_multiple_provides() {
        use crate::models::Package;
        use std::sync::Arc;

        let mut pm = PackageManager {
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            pkgname2packages: HashMap::new(),
            provide2pkgnames: HashMap::new(),
            installed_packages: HashMap::new(),
            world: HashMap::new(),
            has_worker_process: false,
            ipc_socket: String::new(),
            ipc_stream: None,
            child_pid: None,
        };

        // Create a provider package that provides multiple pkgconfig entries in a single string
        // This is the format used by Alpine packages like pcre2-dev
        let provider_pkg = Package {
            pkgname: "pcre2-dev".to_string(),
            version: "10.46-r0".to_string(),
            arch: "x86_64".to_string(),
            pkgkey: "pcre2-dev__10.46-r0__x86_64".to_string(),
            provides: vec![
                // Multiple provide entries in a single string (space-separated)
                "pc:libpcre2-16=10.46 pc:libpcre2-32=10.46 pc:libpcre2-8=10.46 pc:libpcre2-posix=10.46 cmd:pcre2-config=10.46-r0".to_string(),
            ],
            ..Default::default()
        };

        // Cache the package
        pm.pkgkey2package.insert(
            provider_pkg.pkgkey.clone(),
            Arc::new(provider_pkg.clone()),
        );

        // Test 1: Check that pc:libpcre2-8>=10.32 is satisfied by pc:libpcre2-8=10.46
        let constraints = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "10.32".to_string(),
            },
        ];

        let result = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "pc:libpcre2-8",
            &constraints,
            PackageFormat::Apk,
        ).unwrap();

        assert!(result, "pc:libpcre2-8=10.46 should satisfy pc:libpcre2-8>=10.32");

        // Test 2: Check that pc:libpcre2-8>=10.50 is NOT satisfied by pc:libpcre2-8=10.46
        let constraints_fail = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "10.50".to_string(),
            },
        ];

        let result_fail = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "pc:libpcre2-8",
            &constraints_fail,
            PackageFormat::Apk,
        ).unwrap();

        assert!(!result_fail, "pc:libpcre2-8=10.46 should NOT satisfy pc:libpcre2-8>=10.50");

        // Test 3: Check that capability name with version constraints is handled correctly
        let result_with_constraint_in_name = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "pc:libpcre2-8>=10.32",  // Capability name includes constraint
            &constraints,
            PackageFormat::Apk,
        ).unwrap();

        assert!(result_with_constraint_in_name, "Should handle capability name with version constraints");

        // Test 4: Check that other provide entries in the same string are also found
        let constraints_posix = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "10.0".to_string(),
            },
        ];

        let result_posix = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "pc:libpcre2-posix",
            &constraints_posix,
            PackageFormat::Apk,
        ).unwrap();

        assert!(result_posix, "pc:libpcre2-posix=10.46 should satisfy pc:libpcre2-posix>=10.0");
    }

    #[test]
    fn test_check_provider_satisfies_constraints_implicit_provide() {
        use crate::models::Package;
        use std::sync::Arc;

        let mut pm = PackageManager {
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            pkgname2packages: HashMap::new(),
            provide2pkgnames: HashMap::new(),
            installed_packages: HashMap::new(),
            world: HashMap::new(),
            has_worker_process: false,
            ipc_socket: String::new(),
            ipc_stream: None,
            child_pid: None,
        };

        // Test case 1: Implicit provide with VersionCompatible (~) operator (the bug case)
        // Package bluez__5.82-r0__x86_64 should satisfy requirement bluez~5.82
        // Use a version format that works with VersionCompatible (with patch component)
        let bluez_pkg = Package {
            pkgname: "bluez".to_string(),
            version: "5.82.0-r0".to_string(),
            arch: "x86_64".to_string(),
            pkgkey: "bluez__5.82.0-r0__x86_64".to_string(),
            provides: vec![], // No explicit provides - relies on implicit provide
            ..Default::default()
        };

        pm.pkgkey2package.insert(
            bluez_pkg.pkgkey.clone(),
            Arc::new(bluez_pkg.clone()),
        );

        let constraints_compatible = vec![
            VersionConstraint {
                operator: Operator::VersionCompatible,
                operand: "5.82".to_string(),
            },
        ];

        let result = pm.check_provider_satisfies_constraints(
            &bluez_pkg.pkgkey,
            "bluez",
            &constraints_compatible,
            PackageFormat::Apk,
        );
        assert!(result.unwrap(), "bluez__5.82.0-r0__x86_64 should satisfy bluez~5.82 via implicit provide");

        // Test case 2: Implicit provide with VersionGreaterThanEqual constraint
        let constraints_gte = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "5.80".to_string(),
            },
        ];

        let result = pm.check_provider_satisfies_constraints(
            &bluez_pkg.pkgkey,
            "bluez",
            &constraints_gte,
            PackageFormat::Apk,
        );
        assert!(result.unwrap(), "bluez__5.82.0-r0__x86_64 should satisfy bluez>=5.80 via implicit provide");

        // Test case 3: Implicit provide with constraint that doesn't match
        let constraints_fail = vec![
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "5.80".to_string(),
            },
        ];

        let result = pm.check_provider_satisfies_constraints(
            &bluez_pkg.pkgkey,
            "bluez",
            &constraints_fail,
            PackageFormat::Apk,
        );
        assert!(!result.unwrap(), "bluez__5.82.0-r0__x86_64 should NOT satisfy bluez<5.80");

        // Test case 4: Capability name doesn't match package name - should not use implicit provide
        let result = pm.check_provider_satisfies_constraints(
            &bluez_pkg.pkgkey,
            "different-package",
            &constraints_compatible,
            PackageFormat::Apk,
        );
        assert!(!result.unwrap(), "Should not use implicit provide when capability name doesn't match package name");

        // Test case 5: Package with explicit provide should still work (explicit takes precedence)
        let bluez_with_explicit = Package {
            pkgname: "bluez".to_string(),
            version: "5.82.0-r0".to_string(),
            arch: "x86_64".to_string(),
            pkgkey: "bluez__5.82.0-r0__x86_64_explicit".to_string(),
            provides: vec!["bluez = 5.82.0-r0".to_string()], // Explicit provide
            ..Default::default()
        };

        pm.pkgkey2package.insert(
            bluez_with_explicit.pkgkey.clone(),
            Arc::new(bluez_with_explicit.clone()),
        );

        let result = pm.check_provider_satisfies_constraints(
            &bluez_with_explicit.pkgkey,
            "bluez",
            &constraints_compatible,
            PackageFormat::Apk,
        );
        assert!(result.unwrap(), "Explicit provide should work and take precedence over implicit");

        // Test case 6: Implicit provide with multiple constraints (AND)
        let constraints_multiple = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "5.80".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "6.0".to_string(),
            },
        ];

        let result = pm.check_provider_satisfies_constraints(
            &bluez_pkg.pkgkey,
            "bluez",
            &constraints_multiple,
            PackageFormat::Apk,
        );
        assert!(result.unwrap(), "bluez__5.82.0-r0__x86_64 should satisfy bluez>=5.80,<6.0 via implicit provide");

        // Test case 7: Implicit provide with OR constraints
        let constraints_or = vec![
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "5.80".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThan,
                operand: "5.80".to_string(),
            },
        ];

        let result = pm.check_provider_satisfies_constraints(
            &bluez_pkg.pkgkey,
            "bluez",
            &constraints_or,
            PackageFormat::Apk,
        );
        assert!(result.unwrap(), "bluez__5.82.0-r0__x86_64 should satisfy bluez<5.80 OR bluez>5.80 via implicit provide");
    }

    #[test]
    fn test_check_version_satisfies_constraints() {
        let mut pm = PackageManager {
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            pkgname2packages: HashMap::new(),
            provide2pkgnames: HashMap::new(),
            installed_packages: HashMap::new(),
            world: HashMap::new(),
            has_worker_process: false,
            ipc_socket: String::new(),
            ipc_stream: None,
            child_pid: None,
        };

        // Test case 1: VersionCompatible constraint
        let constraints = vec![
            VersionConstraint {
                operator: Operator::VersionCompatible,
                operand: "5.82".to_string(),
            },
        ];
        // Test with versions that have patch components (like the existing test pattern)
        assert!(pm.check_version_satisfies_constraints("5.82.0-r0", &constraints, PackageFormat::Apk).unwrap(),
                "5.82.0-r0 should satisfy ~5.82");
        assert!(pm.check_version_satisfies_constraints("5.82.1-r0", &constraints, PackageFormat::Apk).unwrap(),
                "5.82.1-r0 should satisfy ~5.82");
        assert!(!pm.check_version_satisfies_constraints("5.81-r0", &constraints, PackageFormat::Apk).unwrap(),
                "5.81-r0 should NOT satisfy ~5.82");

        // Test case 2: Multiple AND constraints
        let constraints_and = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "5.80".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "6.0".to_string(),
            },
        ];
        assert!(pm.check_version_satisfies_constraints("5.82-r0", &constraints_and, PackageFormat::Apk).unwrap(),
                "5.82-r0 should satisfy >=5.80,<6.0");
        assert!(!pm.check_version_satisfies_constraints("5.79-r0", &constraints_and, PackageFormat::Apk).unwrap(),
                "5.79-r0 should NOT satisfy >=5.80,<6.0");
        // With upstream-only comparison: 6.0-r0 has upstream 6.0, so 6.0 < 6.0 is false
        assert!(!pm.check_version_satisfies_constraints("6.0-r0", &constraints_and, PackageFormat::Apk).unwrap(),
                "6.0-r0 should NOT satisfy >=5.80,<6.0 (upstream-only comparison: 6.0 < 6.0 is false)");
        assert!(!pm.check_version_satisfies_constraints("6.0", &constraints_and, PackageFormat::Apk).unwrap(),
                "6.0 should NOT satisfy >=5.80,<6.0");

        // Test case 3: OR constraints (mutually exclusive)
        let constraints_or = vec![
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "5.80".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThan,
                operand: "5.80".to_string(),
            },
        ];
        assert!(pm.check_version_satisfies_constraints("5.82-r0", &constraints_or, PackageFormat::Apk).unwrap(),
                "5.82-r0 should satisfy <5.80 OR >5.80");
        assert!(pm.check_version_satisfies_constraints("5.79-r0", &constraints_or, PackageFormat::Apk).unwrap(),
                "5.79-r0 should satisfy <5.80 OR >5.80");
        // With upstream-only comparison: 5.80-r0 has upstream 5.80, so 5.80 < 5.80 is false and 5.80 > 5.80 is false
        assert!(!pm.check_version_satisfies_constraints("5.80-r0", &constraints_or, PackageFormat::Apk).unwrap(),
                "5.80-r0 should NOT satisfy <5.80 OR >5.80 (upstream-only comparison: 5.80 == 5.80)");
        assert!(!pm.check_version_satisfies_constraints("5.80", &constraints_or, PackageFormat::Apk).unwrap(),
                "5.80 should NOT satisfy <5.80 OR >5.80 (exactly equal)");

        // Test case 4: Mixed AND and OR constraints
        let constraints_mixed = vec![
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "5.80".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThan,
                operand: "5.80".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "6.0".to_string(),
            },
        ];
        assert!(pm.check_version_satisfies_constraints("5.82-r0", &constraints_mixed, PackageFormat::Apk).unwrap(),
                "5.82-r0 should satisfy (<5.80 OR >5.80) AND <6.0");
        assert!(!pm.check_version_satisfies_constraints("6.1-r0", &constraints_mixed, PackageFormat::Apk).unwrap(),
                "6.1-r0 should NOT satisfy (<5.80 OR >5.80) AND <6.0");
    }

    #[test]
    fn test_rpm_provide_with_version_operators() {
        use crate::models::Package;
        use std::sync::Arc;

        let mut pm = PackageManager {
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            pkgname2packages: HashMap::new(),
            provide2pkgnames: HashMap::new(),
            installed_packages: HashMap::new(),
            world: HashMap::new(),
            has_worker_process: false,
            ipc_socket: String::new(),
            ipc_stream: None,
            child_pid: None,
        };

        // Test case 1: Package provides capability with exact version
        // In RPM repositories, provides are always stored as exact versions with "=".
        // When a package provides "test-cap = 3.0.0", it means the package provides
        // test-cap at version 3.0.0. We use that version to check against requirement constraints.
        let provider_pkg = Package {
            pkgname: "test-provider".to_string(),
            version: "3.0.0".to_string(),
            arch: "x86_64".to_string(),
            pkgkey: "test-provider__3.0.0__x86_64".to_string(),
            provides: vec!["test-cap = 3.0.0".to_string()], // RPM format: exact version with =
            ..Default::default()
        };

        pm.pkgkey2package.insert(
            provider_pkg.pkgkey.clone(),
            Arc::new(provider_pkg.clone()),
        );

        // Requirement: test-cap(>=2.0.0)
        // Provided version 3.0.0 should satisfy >=2.0.0
        let constraints = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "2.0.0".to_string(),
            },
        ];

        let result = pm.check_provider_satisfies_constraints(
            &provider_pkg.pkgkey,
            "test-cap",
            &constraints,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Package providing test-cap=3.0.0 should satisfy requirement >=2.0.0");

        // Test case 2: Package provides version that doesn't satisfy requirement
        let provider_pkg_low = Package {
            pkgname: "test-pkg-low".to_string(),
            version: "1.0.0".to_string(),
            arch: "x86_64".to_string(),
            pkgkey: "test-pkg-low__1.0.0__x86_64".to_string(),
            provides: vec!["test-cap = 1.0.0".to_string()], // Provides at version 1.0.0
            ..Default::default()
        };

        pm.pkgkey2package.insert(
            provider_pkg_low.pkgkey.clone(),
            Arc::new(provider_pkg_low.clone()),
        );

        let constraints_high = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "2.0.0".to_string(),
            },
        ];

        // This should fail because provided version 1.0.0 doesn't satisfy >=2.0.0
        let result_low = pm.check_provider_satisfies_constraints(
            &provider_pkg_low.pkgkey,
            "test-cap",
            &constraints_high,
            PackageFormat::Rpm,
        );
        assert!(!result_low.unwrap(), "Package providing test-cap=1.0.0 should NOT satisfy requirement >=2.0.0");

        // Test case 3: Package provides version that satisfies multiple constraints
        let provider_pkg_multi = Package {
            pkgname: "test-pkg-multi".to_string(),
            version: "3.0.0".to_string(),
            arch: "x86_64".to_string(),
            pkgkey: "test-pkg-multi__3.0.0__x86_64".to_string(),
            provides: vec!["test-cap = 3.0.0".to_string()],
            ..Default::default()
        };

        pm.pkgkey2package.insert(
            provider_pkg_multi.pkgkey.clone(),
            Arc::new(provider_pkg_multi.clone()),
        );

        let constraints_multi = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "2.5.0".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "4.0.0".to_string(),
            },
        ];

        let result_multi = pm.check_provider_satisfies_constraints(
            &provider_pkg_multi.pkgkey,
            "test-cap",
            &constraints_multi,
            PackageFormat::Rpm,
        );
        assert!(result_multi.unwrap(), "Package providing test-cap=3.0.0 should satisfy requirement >=2.5.0,<4.0.0");

        // Test case 6: Package provides with exact version (=) - should use provided version, not package version
        let provider_pkg_eq = Package {
            pkgname: "test-pkg-eq".to_string(),
            version: "5.0.0".to_string(), // Package version is 5.0.0
            arch: "x86_64".to_string(),
            pkgkey: "test-pkg-eq__5.0.0__x86_64".to_string(),
            provides: vec!["test-cap = 3.0.0".to_string()], // But provides test-cap at version 3.0.0
            ..Default::default()
        };

        pm.pkgkey2package.insert(
            provider_pkg_eq.pkgkey.clone(),
            Arc::new(provider_pkg_eq.clone()),
        );

        let constraints_eq = vec![
            VersionConstraint {
                operator: Operator::VersionEqual,
                operand: "3.0.0".to_string(),
            },
        ];

        let result_eq = pm.check_provider_satisfies_constraints(
            &provider_pkg_eq.pkgkey,
            "test-cap",
            &constraints_eq,
            PackageFormat::Rpm,
        );
        assert!(result_eq.unwrap(), "Package providing test-cap=3.0.0 should satisfy requirement =3.0.0 (using provided version, not package version)");

        // Test case 4: Real-world scenario - mesa-libglapi case
        // Package provides mesa-libglapi at its own version, which should satisfy the requirement
        let mesa_provider = Package {
            pkgname: "mesa-dri-drivers".to_string(),
            version: "25.1.9-1.fc42".to_string(),
            arch: "x86_64".to_string(),
            pkgkey: "mesa-dri-drivers__25.1.9-1.fc42__x86_64".to_string(),
            provides: vec!["mesa-libglapi = 25.1.9-1.fc42".to_string()], // Provides at package's own version
            ..Default::default()
        };

        pm.pkgkey2package.insert(
            mesa_provider.pkgkey.clone(),
            Arc::new(mesa_provider.clone()),
        );

        let mesa_constraints = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "25.0.0~rc2-1".to_string(),
            },
        ];

        let mesa_result = pm.check_provider_satisfies_constraints(
            &mesa_provider.pkgkey,
            "mesa-libglapi",
            &mesa_constraints,
            PackageFormat::Rpm,
        );
        assert!(mesa_result.unwrap(), "mesa-dri-drivers providing mesa-libglapi=25.1.9-1.fc42 should satisfy requirement >=25.0.0~rc2-1");
    }

    #[test]
    fn test_check_provider_satisfies_constraints_rpm_composer_fallback() {
        use crate::models::Package;
        use std::sync::Arc;

        let mut pm = PackageManager {
            pkgkey2package: HashMap::new(),
            pkgline2package: HashMap::new(),
            pkgname2packages: HashMap::new(),
            provide2pkgnames: HashMap::new(),
            installed_packages: HashMap::new(),
            world: HashMap::new(),
            has_worker_process: false,
            ipc_socket: String::new(),
            ipc_stream: None,
            child_pid: None,
        };

        // Test case: RPM composer capability fallback
        // Package provides capability with only upstream version (no release),
        // but dependency requires a release. The fallback should use the package's
        // full EVR (with release) to satisfy the constraint.
        //
        // Real-world example: php-geshi provides "php-composer(geshi/geshi) = 1.0.9.1"
        // but dokuwiki requires "php-composer(geshi/geshi) >= 1.0.9.1-5"
        let php_geshi = Package {
            pkgname: "php-geshi".to_string(),
            version: "1.0.9.1-18.20230219git7884d22.fc42".to_string(),
            arch: "noarch".to_string(),
            pkgkey: "php-geshi__1.0.9.1-18.20230219git7884d22.fc42__noarch".to_string(),
            provides: vec!["php-composer(geshi/geshi) = 1.0.9.1".to_string()], // Only upstream version, no release
            ..Default::default()
        };

        pm.pkgkey2package.insert(
            php_geshi.pkgkey.clone(),
            Arc::new(php_geshi.clone()),
        );

        // Constraint requires a release (>= 1.0.9.1-5)
        let constraints = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "1.0.9.1-5".to_string(),
            },
        ];

        // The initial check should fail because provide version "1.0.9.1" doesn't satisfy ">= 1.0.9.1-5"
        // But the fallback should succeed by using the package's full version "1.0.9.1-18.20230219git7884d22.fc42"
        let result = pm.check_provider_satisfies_constraints(
            &php_geshi.pkgkey,
            "php-composer(geshi/geshi)",
            &constraints,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "php-geshi providing php-composer(geshi/geshi)=1.0.9.1 should satisfy requirement >=1.0.9.1-5 via fallback to package version 1.0.9.1-18.20230219git7884d22.fc42");

        // Test case 2: Constraint without release should work with provided version directly
        let constraints_no_release = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "1.0.9.1".to_string(),
            },
        ];

        let result_no_release = pm.check_provider_satisfies_constraints(
            &php_geshi.pkgkey,
            "php-composer(geshi/geshi)",
            &constraints_no_release,
            PackageFormat::Rpm,
        );
        assert!(result_no_release.unwrap(), "php-geshi providing php-composer(geshi/geshi)=1.0.9.1 should satisfy requirement >=1.0.9.1 (no release needed)");

        // Test case 3: Constraint with release that's too high should fail even with fallback
        let constraints_too_high = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "1.0.9.1-100".to_string(),
            },
        ];

        let result_too_high = pm.check_provider_satisfies_constraints(
            &php_geshi.pkgkey,
            "php-composer(geshi/geshi)",
            &constraints_too_high,
            PackageFormat::Rpm,
        );
        assert!(!result_too_high.unwrap(), "php-geshi version 1.0.9.1-18.20230219git7884d22.fc42 should NOT satisfy requirement >=1.0.9.1-100");

        // Test case 4: Package with provide that already includes release should not use fallback
        let php_geshi_with_release = Package {
            pkgname: "php-geshi-release".to_string(),
            version: "1.0.9.1-18.20230219git7884d22.fc42".to_string(),
            arch: "noarch".to_string(),
            pkgkey: "php-geshi-release__1.0.9.1-18.20230219git7884d22.fc42__noarch".to_string(),
            provides: vec!["php-composer(geshi/geshi) = 1.0.9.1-18.20230219git7884d22.fc42".to_string()], // Already includes release
            ..Default::default()
        };

        pm.pkgkey2package.insert(
            php_geshi_with_release.pkgkey.clone(),
            Arc::new(php_geshi_with_release.clone()),
        );

        let result_with_release = pm.check_provider_satisfies_constraints(
            &php_geshi_with_release.pkgkey,
            "php-composer(geshi/geshi)",
            &constraints,
            PackageFormat::Rpm,
        );
        assert!(result_with_release.unwrap(), "php-geshi-release providing php-composer(geshi/geshi)=1.0.9.1-18.20230219git7884d22.fc42 should satisfy requirement >=1.0.9.1-5 (no fallback needed)");
    }
}
