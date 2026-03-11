use std::collections::{HashMap, BTreeSet, HashSet};
use std::sync::Arc;
use color_eyre::Result;
use color_eyre::eyre;
use crate::models::*;
use crate::models::PACKAGE_CACHE;
use crate::resolve::provider::GenericDependencyProvider;
use crate::resolve::types::{DependFieldFlags, NameType, SolverMatchSpec};
#[cfg(unix)]
use crate::aur::is_aur_package;
use crate::world::{remove_from_no_install, get_no_install_set};
use crate::io::load_installed_packages;
use crate::plan::prepare_installation_plan;
use crate::install::execute_installation_plan;
use crate::repo::sync_channel_metadata;
use crate::parse_provides::parse_provides;

/// Package pairs where circular dependencies need to be broken.
/// Each pair (pkg_a, pkg_b) means: remove pkg_a from pkg_b's reverse dependencies
/// i.e. pretend pkg_a NOT depend on pkg_b
/// Problem: given ("glibc", "glibc-common"), glibc-common will become depth=0
const CIRCULAR_DEPENDENCY_FILTER_PAIRS: &[(&str, &str)] = &[];

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

/// Update ebin_exposure to true for user-requested packages that are already installed
/// This ensures that when a user explicitly requests a package (e.g., `epkg install g++`),
/// it gets ebin_exposure=true even if it was previously installed as a dependency
fn update_ebin_exposure_for_user_requested(
    packages: &mut InstalledPackagesMap,
    user_request_world: Option<&HashMap<String, String>>,
) -> Result<()> {
    let Some(user_request_world) = user_request_world else {
        return Ok(());
    };

    // For each user-requested package name, find matching packages and set ebin_exposure=true
    for requested_name in user_request_world.keys() {
        // Find packages that match this request (handles provides via get_candidates)
        // We need to match by package name since user_request_world uses names
        for (pkgkey, info_arc) in packages.iter_mut() {
            // Check if this package matches the requested name
            if let Ok(pkgname) = crate::package::pkgkey2pkgname(pkgkey) {
                if &pkgname == requested_name {
                    Arc::make_mut(info_arc).ebin_exposure = true;
                    log::debug!("Setting ebin_exposure=true for user-requested package: {} ({})", pkgkey, requested_name);
                }
            }
        }
    }
    Ok(())
}

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

/// Determine packages to expose based on source matching
fn extend_ebin_by_source(packages: &mut InstalledPackagesMap) -> Result<InstalledPackagesMap> {
    log::debug!("Setting ebin_exposure for {} packages based on source matching.", packages.len());

    let mut user_requested_sources = std::collections::HashSet::new();
    let mut packages_to_expose: InstalledPackagesMap = HashMap::new();

    // First, collect all source package names from user-requested packages
    // (packages with ebin_exposure == true are user-requested in THIS session)
    for (pkgkey, info) in packages.iter() {
        if info.ebin_exposure == true {
            match crate::package_cache::load_package_info(pkgkey) {
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
    for (pkgkey, info) in packages.iter() {
        if info.ebin_exposure == false {
            // For dependencies, check if their source matches any user-requested source
            match crate::package_cache::load_package_info(pkgkey) {
                Ok(pkg_details) => {
                    if let Some(source_name) = &pkg_details.source {
                        if !source_name.is_empty() && user_requested_sources.contains(source_name) {
                            let mut new_info_arc = Arc::clone(info);
                            Arc::make_mut(&mut new_info_arc).ebin_exposure = true;
                            packages_to_expose.insert(pkgkey.clone(), new_info_arc);
                        }
                    }
                }
                Err(e) => {
                    log::warn!("Failed to load package info for {}: {} during ebin_exposure setting. Defaulting ebin_exposure to false.", pkgkey, e);
                    // No need to set false since it's already false
                }
            }
        }
    }
    Ok(packages_to_expose)
}

/// Check if a package has any binaries to expose.
/// Uses the package filelist to check for files in bin/ or sbin/ directories.
/// Returns true if the package has files in bin/ or sbin/ directories.
fn package_has_binaries(store_root: &std::path::Path, pkgline: &str) -> bool {
    // Get filelist for the package
    let filelist = match crate::package_cache::map_pkgline2filelist(store_root, pkgline) {
        Ok(list) => list,
        Err(_) => return false,
    };

    // Check if any file is in a bin directory
    for file in &filelist {
        let file_lower = file.to_lowercase();
        // Check for files in bin/ directories
        if file_lower.starts_with("bin/") ||
           file_lower.starts_with("sbin/") ||
           file_lower.contains("/bin/") ||
           file_lower.contains("/sbin/") {
            return true;
        }
    }

    false
}

/// Extend ebin_exposure to all dependencies (transitive) of meta-packages.
///
/// This handles the case where a user requests a meta-package (like default-jdk)
/// which depends on other packages that provide the actual executables (like
/// openjdk-21-jdk-headless which contains javac). Without this, the meta-package
/// would get ebin_exposure=true but its transitive dependencies would not,
/// resulting in no executables being exposed.
///
/// NOTE: This only propagates ebin_exposure for meta-packages (packages without
/// their own binaries). Regular packages with binaries do not propagate ebin_exposure
/// to their dependencies, as the user only requested the package itself.
fn extend_ebin_to_dependencies(packages: &mut InstalledPackagesMap) -> Result<()> {
    // Collect pkgkeys that have ebin_exposure=true (user-requested packages)
    let user_requested_pkgkeys: Vec<String> = packages.iter()
        .filter(|(_, info)| info.ebin_exposure)
        .map(|(pkgkey, _)| pkgkey.clone())
        .collect();

    if user_requested_pkgkeys.is_empty() {
        return Ok(());
    }

    // Get store root for filelist lookups
    let store_root = crate::models::dirs().epkg_store.clone();

    // Use a worklist algorithm to propagate ebin_exposure to transitive dependencies
    // BUT only for meta-packages (packages without their own binaries)
    let mut worklist: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let mut processed: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Initialize worklist with meta-packages only
    for pkgkey in &user_requested_pkgkeys {
        if let Some(info) = packages.get(pkgkey) {
            // Only propagate for meta-packages (packages without binaries)
            if !package_has_binaries(&store_root, &info.pkgline) {
                log::debug!("Package {} is a meta-package (no binaries), will propagate ebin_exposure", pkgkey);
                worklist.push_back(pkgkey.clone());
            } else {
                log::debug!("Package {} has binaries, not propagating ebin_exposure to dependencies", pkgkey);
            }
        }
    }

    while let Some(pkgkey) = worklist.pop_front() {
        if processed.contains(&pkgkey) {
            continue;
        }
        processed.insert(pkgkey.clone());

        // Get dependencies of this package
        let deps_to_add: Vec<String> = if let Some(info) = packages.get(&pkgkey) {
            info.depends.iter()
                .filter(|dep_pkgkey| {
                    if let Some(dep_info) = packages.get(*dep_pkgkey) {
                        !dep_info.ebin_exposure
                    } else {
                        false
                    }
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        // Set ebin_exposure for dependencies and add them to worklist
        for dep_pkgkey in deps_to_add {
            if let Some(dep_info) = packages.get_mut(&dep_pkgkey) {
                Arc::make_mut(dep_info).ebin_exposure = true;
                log::debug!("Setting ebin_exposure=true for dependency {}", dep_pkgkey);
                // Continue propagating only if this dependency is also a meta-package
                if !package_has_binaries(&store_root, &dep_info.pkgline) {
                    worklist.push_back(dep_pkgkey);
                }
            }
        }
    }

    Ok(())
}


/// Setup resolvo provider and convert delta_world to requirements
fn setup_resolvo_provider_and_requirements(
    delta_world: &HashMap<String, String>,
) -> Result<(GenericDependencyProvider, Vec<resolvo::ConditionalRequirement>)> {
    let package_format = channel_config().format;

    log::info!(
        "Starting resolvo-based recursive dependency collection for {} packages in delta_world. Repo format: {:?}",
        delta_world.len(),
        package_format
    );
    log::debug!("delta_world contents: {:?}", delta_world);

    // Detect and add Conda virtual packages to cache
    if package_format == PackageFormat::Conda {
        crate::package_cache::add_conda_virtual_packages_to_cache()?;
    }

    // Add Debian virtual packages to cache
    if package_format == PackageFormat::Deb {
        crate::package_cache::add_deb_virtual_packages_to_cache()?;
    }

    // Create provider and convert delta_world to requirements
    let mut provider = create_resolvo_provider(package_format, delta_world);
    let requirements = convert_initial_packages_to_requirements(delta_world, &mut provider)?;
    log::debug!("Converted {} requirements from delta_world", requirements.len());

    Ok((provider, requirements))
}

/// Create a resolvo problem and solver from provider and requirements
fn create_resolvo_problem_and_solver(
    provider: GenericDependencyProvider,
    requirements: Vec<resolvo::ConditionalRequirement>,
) -> (resolvo::Problem<std::iter::Empty<resolvo::SolvableId>>, resolvo::Solver<GenericDependencyProvider>) {
    use resolvo::{Problem, Solver};
    let problem = Problem::new().requirements(requirements);
    let solver = Solver::new(provider);
    (problem, solver)
}

/// Run a single solve pass with the given solver and problem
/// Returns Ok(solvables) on success, or Err with a warning message on failure
fn run_solve_pass(
    solver: &mut resolvo::Solver<GenericDependencyProvider>,
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
    provider: GenericDependencyProvider,
    requirements: Vec<resolvo::ConditionalRequirement>,
    flags: DependFieldFlags,
) -> Result<(resolvo::Solver<GenericDependencyProvider>, Vec<resolvo::SolvableId>)> {
    // Update provider with the desired flags before creating solver
    provider.update_depend_fields(flags);

    // Create problem and solver
    let (problem, mut solver) = create_resolvo_problem_and_solver(provider, requirements);

    // Determine pass name based on flags
    let package_format = channel_config().format;
    let base_flags = if package_format == PackageFormat::Pacman {
        DependFieldFlags::REQUIRES | DependFieldFlags::BUILD_REQUIRES
    } else {
        DependFieldFlags::REQUIRES
    };
    let pass_name = if flags != base_flags {
        "1st pass (with RECOMMENDS/SUGGESTS)"
    } else if package_format == PackageFormat::Pacman {
        "2nd pass (REQUIRES|BUILD_REQUIRES only)"
    } else {
        "2nd pass (REQUIRES only)"
    };

    // Run solve pass
    let solvables = run_solve_pass(&mut solver, problem, pass_name)?;
    log::debug!("Solver resolved {} solvables", solvables.len());

    Ok((solver, solvables))
}

/// Resolvo-based dependency resolver
/// Internal function for core dependency resolution logic using resolvo SAT solver
fn resolve_dependencies_with_resolvo(
    delta_world: &HashMap<String, String>,
    user_request_world: Option<&HashMap<String, String>>,
) -> Result<InstalledPackagesMap> {
    // Setup provider and requirements
    let (provider, requirements) = setup_resolvo_provider_and_requirements(delta_world)?;
    if requirements.is_empty() {
        log::info!("No valid packages to resolve");
        // When ignore_missing is enabled and all packages are missing, gracefully return empty result
        return Ok(HashMap::new());
    }

    // Determine flags to use: always include REQUIRES|BUILD_REQUIRES, try with RECOMMENDS/SUGGESTS if configured
    let package_format = channel_config().format;
    let (base_flags, base_flag_desc) = if package_format == PackageFormat::Pacman {
        (
            DependFieldFlags::REQUIRES | DependFieldFlags::BUILD_REQUIRES,
            "REQUIRES|BUILD_REQUIRES",
        )
    } else {
        (DependFieldFlags::REQUIRES, "REQUIRES")
    };
    let mut flags = base_flags;
    if !config().install.no_install_recommends {
        flags = flags | DependFieldFlags::RECOMMENDS;
    }
    if config().install.install_suggests {
        flags = flags | DependFieldFlags::SUGGESTS;
    }

    // Try to solve with RECOMMENDS/SUGGESTS (if configured) - allow failure
    let (solver, solvables) = match solve_with_resolvo(provider, requirements.clone(), flags) {
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
            let (fresh_provider, _) = setup_resolvo_provider_and_requirements(delta_world)?;
            match solve_with_resolvo(fresh_provider, requirements, base_flags) {
                Ok(result) => result,
                Err(e) => return Err(e),
            }
        }
        Err(e) => return Err(e),
    };

    // Build dependency graph and create result
    let result = build_installed_package_info_map(
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
    delta_world: &mut HashMap<String, String>,
    user_request_world: Option<&HashMap<String, String>>,
) -> Result<InstalledPackagesMap> {
    // First pass: resolve with current delta_world
    let mut all_packages_for_session =
        resolve_dependencies_with_resolvo(delta_world, user_request_world)?;

    // Only apply makepkg dependency handling for Pacman format
    let package_format = channel_config().format;
    if package_format != PackageFormat::Pacman {
        return Ok(all_packages_for_session);
    }

    // Check if any resolved package is an AUR package and whether any of them is a *-git package
    let mut has_aur_packages = false;
    let mut has_git_aur = false;
    #[cfg(unix)]
    {
        for pkgkey in all_packages_for_session.keys() {
            if is_aur_package(pkgkey) {
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
    remove_from_no_install(makepkg_depends.keys());

    // Second pass: resolve again with updated delta_world
    all_packages_for_session =
        resolve_dependencies_with_resolvo(delta_world, user_request_world)?;

    Ok(all_packages_for_session)
}

/// Resolves dependencies using resolvo SAT solver and performs full installation workflow
pub fn resolve_and_install_packages(
    delta_world: &mut HashMap<String, String>,
    user_request_world: Option<&HashMap<String, String>>,
) -> Result<crate::plan::InstallationPlan> {
    use crate::plan::InstallationPlan;

    // Remove "no-install" key - it's not a package
    delta_world.remove("no-install");

    sync_channel_metadata()?;
    load_installed_packages()?;

    // Resolve dependencies (pass user_request_world to extract correct candidate pkgkeys)
    let mut all_packages_for_session =
        resolve_dependencies_adding_makepkg_deps(delta_world, user_request_world)?;

    // Update ebin_exposure for user-requested packages (handles skipped reinstall case)
    // This ensures packages explicitly requested by user get ebin_exposure=true even if
    // they were previously installed as dependencies
    update_ebin_exposure_for_user_requested(&mut all_packages_for_session, user_request_world)?;

    // Determine packages to expose based on source matching
    let packages_to_expose = extend_ebin_by_source(&mut all_packages_for_session)?;

    // Also expose direct dependencies of user-requested packages
    // This handles meta-packages like default-jdk that depend on packages providing executables
    extend_ebin_to_dependencies(&mut all_packages_for_session)?;

    if packages_to_expose.is_empty() && all_packages_for_session.is_empty() {
        let empty_msg = if user_request_world.is_some() {
            "No packages to install or upgrade."
        } else {
            "No packages to upgrade."
        };
        println!("{}", empty_msg);
        return Ok(InstallationPlan::default());
    }

    // If all_packages_for_session is not empty but all packages are already installed,
    // we might still need to expose some packages with ebin_exposure=true
    // In this case, packages_to_expose will not be empty after extend_ebin_by_source
    // but all_packages_for_session might contain only packages that are already installed
    // (skipped_reinstalls case)
    if all_packages_for_session.is_empty() && !packages_to_expose.is_empty() {
        // This should not normally happen since packages_to_expose comes from all_packages_for_session
        // But let's handle it for robustness
        let msg = format!("Error: {} packages need exposure but no packages resolved", packages_to_expose.len());
        return Err(eyre::eyre!(msg));
    }

    let plan = prepare_installation_plan(&all_packages_for_session, None)?;

    // If we reach here, actions_planned was true, user confirmed, and not dry_run.
    // Proceed with actual installation steps by calling the unified execution method.
    execute_installation_plan(plan)
}

/// Get candidate pkgkeys from capabilities (package names or provides)
/// Uses get_candidates() to find packages that satisfy capabilities, which already handles provides
/// Returns empty set if user_request_world is None
/// Only includes pkgkeys that are in solvables
fn get_candidate_pkgkeys_from_capabilities(
    provider_ref: &GenericDependencyProvider,
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
            NameType(capability.clone())
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

/// Expand no_install set with provides of packages in the set.
/// For each package name in no_install, look up its packages and parse their provides.
/// Adds all provided capabilities to the no_install set.
fn expand_no_install_with_provides(format: PackageFormat, no_install: HashSet<String>) -> HashSet<String> {
    let mut expanded = no_install.clone();
    for pkgname in &no_install {
        match crate::package_cache::map_pkgname2packages(pkgname) {
            Ok(packages) => {
                for package in packages {
                    for provide_str in &package.provides {
                        let provide_map = parse_provides(provide_str, format);
                        for (provide_name, _version) in provide_map {
                            expanded.insert(provide_name);
                        }
                    }
                    for provide_str in &package.files {
                        let provide_map = parse_provides(provide_str, format);
                        for (provide_name, _version) in provide_map {
                            expanded.insert(provide_name);
                        }
                    }
                }
            }
            Err(e) => {
                log::debug!("[RESOLVO] Could not lookup packages for no_install package '{}': {}", pkgname, e);
            }
        }
    }
    let original_len = no_install.len();
    let expanded_len = expanded.len();
    if expanded_len > original_len {
        let added: Vec<&String> = expanded.difference(&no_install).collect();
        log::debug!("[RESOLVO] Expanded no_install set with {} capabilities ({} -> {}): {:?}",
                   expanded_len - original_len, original_len, expanded_len, added);
    }
    expanded
}

/// Create a resolvo dependency provider
fn create_resolvo_provider(format: PackageFormat, delta_world: &HashMap<String, String>) -> GenericDependencyProvider {
    use crate::resolve::types::DependFieldFlags;
    let depend_fields = DependFieldFlags::REQUIRES;

    let delta_world_keys: std::collections::HashSet<String> = delta_world.keys().cloned().collect();

    // Extract no-install list from world (space-separated string)
    let no_install = get_no_install_set();
    let no_install = expand_no_install_with_provides(format, no_install);

    GenericDependencyProvider::new(
        format,
        depend_fields,
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
    delta_world: &HashMap<String, String>,
    provider: &mut GenericDependencyProvider,
) -> Result<Vec<resolvo::ConditionalRequirement>> {
    let mut requirements = Vec::new();
    let ignore_missing = crate::models::config().common.ignore_missing;

    for (pkgname, constraint_str) in delta_world {
        // Check if package/capability exists when ignore_missing is enabled
        if ignore_missing && !check_package_or_capability_exists(pkgname) {
            log::info!(
                "Package/capability '{}' not found, skipping (ignore_missing=true)",
                pkgname
            );
            continue;
        }

        // Parse constraint string from delta_world (or use world.json if delta_world has empty string)
        let final_constraints = if constraint_str.is_empty() {
            // No constraint in delta_world, check world.json
            PACKAGE_CACHE.world.read().unwrap().get(pkgname)
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
            let requirement = create_constrained_requirement(pkgname, &constraints, provider);
            requirements.push(requirement);
        } else {
            // No version constraints - create requirement for any version
            // Note: We don't skip already installed packages here since resolvo will handle that
            let requirement = create_package_name_requirement(pkgname, provider);
            requirements.push(requirement);
        }
    }

    Ok(requirements)
}

/// Check if a package or capability exists in the repository
/// Returns true if packages are found, false otherwise
fn check_package_or_capability_exists(name: &str) -> bool {
    // First, try direct package name lookup
    match crate::package_cache::map_pkgname2packages(name) {
        Ok(packages) if !packages.is_empty() => return true,
        _ => {}
    }

    // If no packages found, try capability/provide lookup
    match crate::mmio::map_provide2pkgnames(name) {
        Ok(provider_pkgnames) => {
            for provider_pkgname in provider_pkgnames {
                match crate::package_cache::map_pkgname2packages(&provider_pkgname) {
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
    pkgname: &str,
    provider: &mut GenericDependencyProvider,
) -> resolvo::ConditionalRequirement {
    use resolvo::ConditionalRequirement;

    // Intern package name
    let name_id = provider.pool.intern_package_name(
        NameType(pkgname.to_string())
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
        SolverMatchSpec::MatchSpec(and_deps),
    );

    ConditionalRequirement {
        requirement: version_set_id.into(),
        condition: None,
    }
}

/// Create a requirement with version constraints
/// Supports constraints like =, >=, >, <=, <, !=, ~=
fn create_constrained_requirement(
    pkgname: &str,
    constraints: &[crate::parse_requires::VersionConstraint],
    provider: &mut GenericDependencyProvider,
) -> resolvo::ConditionalRequirement {
    use resolvo::ConditionalRequirement;

    // Intern package name
    let name_id = provider.pool.intern_package_name(
        NameType(pkgname.to_string())
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
        SolverMatchSpec::MatchSpec(and_deps),
    );

    ConditionalRequirement {
        requirement: version_set_id.into(),
        condition: None,
    }
}

/// Build dependency graph and create InstalledPackageInfo map
fn build_installed_package_info_map(
    solver: &resolvo::Solver<GenericDependencyProvider>,
    solvables: &[resolvo::SolvableId],
    user_request_world: Option<&HashMap<String, String>>,
) -> Result<InstalledPackagesMap> {
    let provider_ref = solver.provider();
    let format = provider_ref.format;

    // Extract request_world pkgkeys: candidate pkgkeys for user_request_world (handles provides)
    // request_world_pkgkeys is only used for ebin_exposure computing, not for depth calculation
    let request_world_pkgkeys = get_candidate_pkgkeys_from_capabilities(
        provider_ref,
        solvables,
        user_request_world,
    )?;
    log::debug!("[RESOLVO] Found {} request_world pkgkeys out of {} resolved solvables: {:?}", request_world_pkgkeys.len(), solvables.len(), request_world_pkgkeys);

    // Build dependency graph from resolved solvables
    let (pkgkey_to_depends, pkgkey_to_rdepends) =
        build_dependency_graph(provider_ref, solvables)?;

    // For Pacman format, also build build-dependency graph
    let (pkgkey_to_bdepends, pkgkey_to_rbdepends) = if format == PackageFormat::Pacman {
        log::debug!("[RESOLVO] Building build-dependency graph for Pacman format");
        // Update provider to use BUILD_REQUIRES only
        provider_ref.update_depend_fields(DependFieldFlags::BUILD_REQUIRES);
        let (bdepends, rbdepends) = build_dependency_graph(provider_ref, solvables)?;
        (bdepends, rbdepends)
    } else {
        (HashMap::new(), HashMap::new())
    };

    // Calculate dependency depths
    let pkgkey_to_depth = calculate_pkgkey_to_depth(
        &pkgkey_to_depends,
        &pkgkey_to_rdepends,
        &pkgkey_to_bdepends,
        &pkgkey_to_rbdepends,
        &request_world_pkgkeys,
    )?;

    // Create InstalledPackageInfo entries with correct depths
    let result = create_installed_package_info_map(
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
    provider_ref: &GenericDependencyProvider,
    solvables: &[resolvo::SolvableId],
) -> Result<(
    HashMap<String, BTreeSet<String>>,
    HashMap<String, BTreeSet<String>>,
)> {
    use resolvo::{DependencyProvider, Interner};
    use resolvo::runtime::{AsyncRuntime, NowOrNeverRuntime};

    let mut pkgkey_to_depends: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut pkgkey_to_rdepends: HashMap<String, BTreeSet<String>> = HashMap::new();

    // First pass: collect all resolved packages and build dependency graph
    // Ensure each solvable has an entry in pkgkey_to_depends (even if empty)
    for solvable_id in solvables {
        let record = &provider_ref.pool.resolve_solvable(*solvable_id).record;
        let pkgkey = record.pkgkey.clone();

        // Ensure entry exists (may be empty vec)
        pkgkey_to_depends.entry(pkgkey.clone()).or_insert_with(BTreeSet::new);

        // Load package to get full info
        let _package = match crate::package_cache::load_package_info(&pkgkey) {
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
        let dep_pkgkeys = extract_dependency_pkgkeys(provider_ref, solvables, &deps.requirements);
        pkgkey_to_depends.insert(pkgkey.clone(), dep_pkgkeys.clone());

        // Update reverse dependencies
        for dep_pkgkey in &dep_pkgkeys {
            pkgkey_to_rdepends
                .entry(dep_pkgkey.clone())
                .or_insert_with(BTreeSet::new)
                .insert(pkgkey.clone());
        }
    }


    Ok((pkgkey_to_depends, pkgkey_to_rdepends))
}

/// Extract pkgkeys from dependency requirements
fn extract_dependency_pkgkeys(
    provider_ref: &GenericDependencyProvider,
    solvables: &[resolvo::SolvableId],
    requirements: &[resolvo::ConditionalRequirement],
) -> BTreeSet<String> {
    use resolvo::{Interner, VersionSetId};

    let mut dep_pkgkeys = BTreeSet::new();

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
                    dep_pkgkeys.insert(other_record.pkgkey.clone());
                    break;
                }
                // Check if package provides the capability
                if provider_ref.package_provides_capability(&other_record.pkgkey, &dep_name) {
                    dep_pkgkeys.insert(other_record.pkgkey.clone());
                    break;
                }
            }
        }
    }

    // De-duplicate to avoid storing duplicate dependencies
    dep_pkgkeys
}

/// Find leaf nodes (packages with no reverse dependencies)
fn find_leaf_nodes_by_rdepends(
    remaining_rdepends: &HashMap<String, BTreeSet<String>>,
) -> Vec<String> {
    remaining_rdepends
        .iter()
        .filter(|(_, rdepends)| rdepends.is_empty())
        .map(|(pkgkey, _)| pkgkey.clone())
        .collect()
}

/// Find candidate node with least weighted reverse dependencies for breaking circular dependencies
fn find_candidate_with_least_weighted_rdepends(
    remaining_rdepends: &HashMap<String, BTreeSet<String>>,
    request_world_pkgkeys: &std::collections::HashSet<String>,
) -> Option<(String, usize)> {
    remaining_rdepends
        .iter()
        .filter(|(_, rdepends)| !rdepends.is_empty())
        .map(|(pkgkey, rdepends)| {
            let mut weight = 0;
            for rdepend in rdepends {
                if rdepend.contains("lib") {
                    weight += 10;
                } else {
                    weight += 1;
                }
            }
            if pkgkey.contains("lib") {
                weight = weight * 2;
            }
            if request_world_pkgkeys.contains(pkgkey) {
                weight = weight / 2;
            }
            (pkgkey.clone(), weight)
        })
        .min_by_key(|(_, weight)| *weight)
}

/// Remove a node from the dependency graph and update reverse dependencies
fn remove_node_and_update_dependencies(
    node: &str,
    pkgkey_to_depends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_bdepends: &HashMap<String, BTreeSet<String>>,
    remaining_rdepends: &mut HashMap<String, BTreeSet<String>>,
    pkgkey_to_depth: &mut HashMap<String, u16>,
    current_depth: u16,
) {
    // Record depth before removal
    pkgkey_to_depth.insert(node.to_string(), current_depth);

    // Remove node from tracking map
    remaining_rdepends.remove(node);

    // Update reverse regular dependencies
    if let Some(depends_list) = pkgkey_to_depends.get(node) {
        for dep_pkgkey in depends_list {
            if let Some(rdepends) = remaining_rdepends.get_mut(dep_pkgkey) {
                rdepends.remove(node);
            }
        }
    }

    // Update reverse build dependencies
    if let Some(bdepends_list) = pkgkey_to_bdepends.get(node) {
        for dep_pkgkey in bdepends_list {
            if let Some(rdepends) = remaining_rdepends.get_mut(dep_pkgkey) {
                rdepends.remove(node);
            }
        }
    }
}

/// Process leaf nodes: assign depth and remove them from the graph
fn process_leaf_nodes(
    leaf_nodes: &[String],
    pkgkey_to_depends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_bdepends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_depth: &mut HashMap<String, u16>,
    remaining_rdepends: &mut HashMap<String, BTreeSet<String>>,
    current_depth: u16,
) {
    // Remove leaf nodes and update reverse dependencies
    for leaf_pkgkey in leaf_nodes {
        remove_node_and_update_dependencies(
            leaf_pkgkey,
            pkgkey_to_depends,
            pkgkey_to_bdepends,
            remaining_rdepends,
            pkgkey_to_depth,
            current_depth,
        );
    }
}

/// Break circular dependency by trying different strategies
fn break_circular_dependency(
    pkgkey_to_depends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_bdepends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_depth: &mut HashMap<String, u16>,
    remaining_rdepends: &mut HashMap<String, BTreeSet<String>>,
    request_world_pkgkeys: &std::collections::HashSet<String>,
    current_depth: u16,
) -> bool {
    // Strategy: Remove node with least weighted reverse dependencies
    if let Some((candidate, weight)) = find_candidate_with_least_weighted_rdepends(remaining_rdepends, request_world_pkgkeys) {
        log::debug!(
            "Breaking circular dependency by removing node {} with weighted rdepends ({}) at depth {}",
            candidate,
            weight,
            current_depth
        );
        remove_node_and_update_dependencies(
            &candidate,
            pkgkey_to_depends,
            pkgkey_to_bdepends,
            remaining_rdepends,
            pkgkey_to_depth,
            current_depth,
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
/// - Build dependencies: packages needed only during build time (Pacman only, merged with regular dependencies)
///
/// Circular dependency breaking strategy (when no leaf nodes are found):
/// Remove node with least weighted reverse dependencies (considering user-requested packages)
/// Last resort: assign all remaining packages the current depth
///
/// This approach leads to better depth assignments and avoids deadlocks while still maintaining
/// a reasonable dependency ordering. User-requested packages are prioritized when breaking cycles.
///
/// # Arguments
///
/// * `pkgkey_to_depends` - Map of package keys to their regular dependencies
/// * `pkgkey_to_rdepends` - Map of package keys to packages that depend on them (reverse regular deps)
/// * `pkgkey_to_bdepends` - Map of package keys to their build dependencies (Pacman only)
/// * `pkgkey_to_rbdepends` - Map of package keys to packages that have them as build deps (reverse build deps, merged with regular dependencies)
/// * `request_world_pkgkeys` - Set of package keys that the user explicitly requested
///
/// # Returns
///
/// A HashMap mapping package keys to their calculated dependency depths

// Helper function to update remaining_rdepends with circular dependency filtering
fn update_remaining_rdepends_with_filter(
    remaining_rdepends: &mut HashMap<String, BTreeSet<String>>,
    pkgkey: &str,
    rdeps: &BTreeSet<String>,
) {
    use crate::package::pkgkey2pkgname;

    let entry = remaining_rdepends.entry(pkgkey.to_string()).or_insert_with(BTreeSet::new);

    for rdep in rdeps {
        let mut should_filter = false;
        // Convert pkgkey and rdep to pkgname for filtering
        if let Ok(pkgname) = pkgkey2pkgname(pkgkey) {
            if let Ok(rdep_pkgname) = pkgkey2pkgname(rdep) {
                for (pkg_a_name, pkg_b_name) in CIRCULAR_DEPENDENCY_FILTER_PAIRS {
                    if rdep_pkgname == *pkg_a_name && pkgname == *pkg_b_name {
                        should_filter = true;
                        break;
                    }
                }
            }
        }
        if !should_filter {
            entry.insert(rdep.clone());
        }
    }
}


fn combine_and_filter_reverse_dependencies(
    pkgkey_to_rdepends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_rbdepends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_depends: &HashMap<String, BTreeSet<String>>,
) -> HashMap<String, BTreeSet<String>> {
    let mut remaining_rdepends: HashMap<String, BTreeSet<String>> = HashMap::new();

    // First, add all regular reverse dependencies
    for (pkgkey, rdepends) in pkgkey_to_rdepends {
        update_remaining_rdepends_with_filter(&mut remaining_rdepends, pkgkey, rdepends);
    }

    // Then add build reverse dependencies (union with existing)
    for (pkgkey, rbdepends) in pkgkey_to_rbdepends {
        update_remaining_rdepends_with_filter(&mut remaining_rdepends, pkgkey, rbdepends);
    }

    // Ensure all packages in pkgkey_to_depends have an entry (even if empty)
    let empty_deps = BTreeSet::new();
    for pkgkey in pkgkey_to_depends.keys() {
        update_remaining_rdepends_with_filter(&mut remaining_rdepends, pkgkey, &empty_deps);
    }

    remaining_rdepends
}

fn calculate_depths_from_graph(
    pkgkey_to_depends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_bdepends: &HashMap<String, BTreeSet<String>>,
    mut remaining_rdepends: HashMap<String, BTreeSet<String>>,
    request_world_pkgkeys: &std::collections::HashSet<String>,
) -> HashMap<String, u16> {
    let mut pkgkey_to_depth: HashMap<String, u16> = HashMap::new();
    let mut current_depth = 0;

    // Debug: Show initial state of remaining_rdepends
    log::debug!("[INITIAL] remaining_rdepends ({} entries):", remaining_rdepends.len());
    let mut leaf_count = 0;
    let mut non_leaf_count = 0;
    for (pkgkey, rdepends) in remaining_rdepends.iter().take(20) {
        let is_leaf = rdepends.is_empty();
        if is_leaf {
            leaf_count += 1;
        } else {
            non_leaf_count += 1;
        }
        let rdepends_str = if rdepends.is_empty() { "[]".to_string() } else { format!("{:?}", rdepends) };
        log::debug!("    {} -> {} {} (len={})",
            pkgkey,
            if is_leaf { "[LEAF]" } else { "" },
            rdepends_str,
            rdepends.len()
        );
    }
    if remaining_rdepends.len() > 20 {
        log::debug!("    ... (and {} more)", remaining_rdepends.len() - 20);
    }
    log::debug!("[INITIAL] Total leaf nodes (empty rdepends): {}", leaf_count);
    log::debug!("[INITIAL] Total non-leaf nodes: {}", non_leaf_count);

    loop {
        if remaining_rdepends.is_empty() {
            break;
        }
        // Find packages with empty remaining_rdepends (leaf nodes at current depth)
        let leaf_nodes = find_leaf_nodes_by_rdepends(&remaining_rdepends);
        log::debug!("[LOOP depth={}] find_leaf_nodes_by_rdepends returned {} nodes",
            current_depth, leaf_nodes.len());

        if leaf_nodes.is_empty() {
            // Circular dependency detected - try to break it using helper function
            if break_circular_dependency(
                pkgkey_to_depends,
                pkgkey_to_bdepends,
                &mut pkgkey_to_depth,
                &mut remaining_rdepends,
                request_world_pkgkeys,
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

        log::debug!(
            "Found {} leaf nodes at depth {}",
            leaf_nodes.len(),
            current_depth
        );

        // Process leaf nodes: assign depth and remove from graph
        process_leaf_nodes(
            &leaf_nodes,
            pkgkey_to_depends,
            pkgkey_to_bdepends,
            &mut pkgkey_to_depth,
            &mut remaining_rdepends,
            current_depth,
        );

        current_depth += 1;
    }

    log::debug!("Calculated depths for {} packages", pkgkey_to_depth.len());
    pkgkey_to_depth
}

pub fn calculate_pkgkey_to_depth(
    pkgkey_to_depends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_rdepends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_bdepends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_rbdepends: &HashMap<String, BTreeSet<String>>,
    request_world_pkgkeys: &std::collections::HashSet<String>,
) -> Result<HashMap<String, u16>> {
    let remaining_rdepends = combine_and_filter_reverse_dependencies(
        pkgkey_to_rdepends,
        pkgkey_to_rbdepends,
        pkgkey_to_depends,
    );

    let pkgkey_to_depth = calculate_depths_from_graph(
        pkgkey_to_depends,
        pkgkey_to_bdepends,
        remaining_rdepends,
        request_world_pkgkeys,
    );
    Ok(pkgkey_to_depth)
}

/// Create InstalledPackageInfo map with correct dependency depths
fn create_installed_package_info_map(
    provider_ref: &GenericDependencyProvider,
    solvables: &[resolvo::SolvableId],
    pkgkey_to_depends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_rdepends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_bdepends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_rbdepends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_depth: &HashMap<String, u16>,
    request_world_pkgkeys: &std::collections::HashSet<String>,
) -> Result<InstalledPackagesMap> {
    let mut result = HashMap::new();

    // Iterate through all packages in the dependency graph
    for pkgkey in pkgkey_to_depends.keys() {
        // Skip conda virtual packages (names starting with __)
        if let Ok(pkgname) = crate::package::pkgkey2pkgname(pkgkey) {
            if pkgname.starts_with("__") {
                continue;
            }
        }

        // Skip virtual packages (repodata_name == "virtual")
        if let Ok(package) = crate::package_cache::load_package_info(pkgkey) {
            if package.repodata_name == "virtual" {
                log::debug!("Skipping virtual package: {}", pkgkey);
                continue;
            }
        }

        // Get depth from pre-calculated map (default to 0 if not found)
        let depth = pkgkey_to_depth.get(pkgkey).copied().unwrap_or(0);

        // Determine ebin_exposure: true only for request_world packages
        let ebin_exposure = request_world_pkgkeys.contains(pkgkey);

        // Create InstalledPackageInfo
        let pkg_info = create_installed_package_info(
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

        result.insert(pkgkey.clone(), Arc::new(pkg_info));
    }

    log::debug!("Final result size: {}", result.len());
    Ok(result)
}

/// Create InstalledPackageInfo for a single package
fn create_installed_package_info(
    pkgkey: &str,
    provider_ref: &GenericDependencyProvider,
    solvables: &[resolvo::SolvableId],
    pkgkey_to_depends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_rdepends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_bdepends: &HashMap<String, BTreeSet<String>>,
    pkgkey_to_rbdepends: &HashMap<String, BTreeSet<String>>,
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

    // Merges rdepends from already installed packages (installed_packages)
    // to "predict the future" - incorporating historical dependency data.
    // The merged dependencies are sorted and de-duplicated to maintain consistency.
    let mut merged_rdepends = pkgkey_to_rdepends.get(pkgkey).cloned().unwrap_or_default();
    if let Some(installed_info) = PACKAGE_CACHE.installed_packages.read().unwrap().get(pkgkey) {
        // Merge and de-duplicate
        merged_rdepends.extend(installed_info.rdepends.iter().cloned());
    }

    // Merges rbdepends from already installed packages (installed_packages)
    // to "predict the future" - incorporating historical build dependency data.
    let mut merged_rbdepends = pkgkey_to_rbdepends.get(pkgkey).cloned().unwrap_or_default();
    if let Some(installed_info) = PACKAGE_CACHE.installed_packages.read().unwrap().get(pkgkey) {
        // Merge and de-duplicate
        merged_rbdepends.extend(installed_info.rbdepends.iter().cloned());
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
        #[cfg(unix)]
        xdesktop_links: Vec::new(),
        pending_triggers: Vec::new(),
        triggers_awaited: false,
        config_failed: false,
    })
}
