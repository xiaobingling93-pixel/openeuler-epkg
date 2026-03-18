#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::PathBuf;
use std::collections::HashMap;
use color_eyre::Result;
#[cfg(unix)]
use color_eyre::eyre;
#[cfg(unix)]
use crate::lfs;
use crate::plan::InstallationPlan;
use crate::models::PACKAGE_CACHE;
use crate::install::execute_installation_plan;
use crate::io::{load_installed_packages, load_world};

/// Unlink package files from the environment.
///
/// Validates the pkgline, constructs the store path, and removes all package files
/// from the environment root.
///
/// # Arguments
/// * `pkgkey` - Package key for logging
/// * `pkgline` - Package line (relative path in store)
/// * `store_root` - Root of the package store
/// * `env_root` - Root of the environment
#[cfg(unix)]
pub fn unlink_package(
    pkgkey: &str,
    pkgline: &str,
    store_root: &PathBuf,
    env_root: &PathBuf,
) -> Result<()> {
    // Validate pkgline
    if pkgline.is_empty() || pkgline.contains("/") || pkgline.contains("..") {
        log::error!("Invalid pkgline for {}: '{}'. Skipping unlink.", pkgkey, pkgline);
        return Err(eyre::eyre!("Invalid pkgline for {}: '{}'", pkgkey, pkgline));
    }

    let pkg_store_path = store_root.join(pkgline);
    log::info!("Unlinking files for package: {} from store path {}", pkgkey, pkg_store_path.display());

    let fs_dir = pkg_store_path.join("fs");
    if !fs_dir.exists() {
        // If the 'fs' directory doesn't exist, it might mean the package was corrupted
        // or never fully extracted. It's safer to log this and return Ok, treating it as if
        // there are no files to unlink, rather than panicking.
        // This can happen if a previous removal was interrupted or if the store is manually altered.
        log::warn!("Package FS root {} does not exist for package directory {}. Assuming no files to unlink.", fs_dir.display(), pkg_store_path.display());
        return Ok(());
    }
    let fs_dir_str = fs_dir.to_str()
        .ok_or_else(|| eyre::eyre!("Invalid path for fs_dir: {}", fs_dir.display()))?;
    let fs_files = crate::utils::list_package_files_with_info(fs_dir_str)?;
    log::debug!("Unlinking package from {} to {} ({} files)", pkg_store_path.display(), env_root.display(), fs_files.len());
    for fs_file_info in fs_files {
        // fs_file_info.path is already relative
        let target_path = env_root.join(&fs_file_info.path);

        // Skip symlinks for top-level directories, some are manually created in create_environment_dirs_early()
        if matches!(fs_file_info.path.as_str(), "sbin" | "bin" | "lib" | "lib64" | "lib32" | "share" | "include" | "usr/sbin" | "usr/lib64" | "usr/libexec") {
            continue;
        }

        // Skip dir in source
        if fs_file_info.is_dir() {
            continue;
        }

        // Check if target exists and get its metadata
        if let Ok(target_metadata) = fs::symlink_metadata(&target_path) {
            // Skip if target is a directory (directories are typically shared and shouldn't be removed)
            if target_metadata.is_dir() {
                log::trace!("Skipping directory at target path: {}", target_path.display());
                continue;
            }

            // Remove file (include symlink)
            lfs::remove_file(&target_path)?;
        }
    }

    Ok(())
}

/// Removes specified packages and their orphaned dependencies.
///
/// This function operates solely on the information within `installed_packages`
/// and does not consult external repositories or perform new dependency resolution.
/// This ensures that removal decisions are based purely on the currently recorded
/// state of installed packages.
///
/// The process is as follows:
/// 1. Loads the current set of installed packages.
/// 2. Resolves the input `package_specs` (user-provided package names or keys)
///    against the loaded `installed_packages`. Packages not found are reported.
/// 3. Initializes a `final_removal_set` with packages explicitly matched from `package_specs`.
/// 4. A `processing_queue` is used, seeded with these explicitly requested packages.
/// 5. Iteratively, for each package (`pkg_A`) taken from the queue:
///    a. Its direct dependencies (`pkg_A.depends`) are examined.
///    b. For each dependency (`pkg_B`):
///       i. If `pkg_B` is already in `final_removal_set`, it's skipped.
///       ii. `pkg_B` is NOT automatically removed if it's marked as user-installed
///           (`ebin_exposure` is true or `depend_depth == 0`) or if it's an essential package.
///       iii. Otherwise, `pkg_B` is considered an orphan (and added to `final_removal_set`
///            and the `processing_queue`) if ALL of its recorded reverse dependencies
///            (`pkg_B.rdepends`) are already present in the `final_removal_set`.
///            This means all packages that depend on `pkg_B` are themselves being removed.
/// 6. After the queue is empty, `final_removal_set` contains all packages to be uninstalled.
/// 7. If not a dry run, pre-remove scriptlets are run for these packages.
/// 8. Files for each package in `final_removal_set` are unlinked from the environment.
/// 9. The corresponding entries are removed from `installed_packages`.
/// 10. Post-remove scriptlets are run.
/// 11. A new generation is created, `installed-packages.json` is saved, history recorded,
///     and the 'current' generation symlink is updated.
///
/// This method aims for a safer and more predictable removal process, especially
/// ensuring that shared dependencies are not removed if other installed packages
/// (not part of the current removal set) still rely on them according to the
/// `rdepends` information.
pub fn remove_packages(package_specs: Vec<String>) -> Result<InstallationPlan> {
    load_installed_packages()?;
    load_world()?;

    // Remove packages from world.json based on the specs
    remove_from_world(&package_specs)?;

    let plan = prepare_removal_plan(package_specs)?;

    if plan.ordered_operations.is_empty() {
        return Ok(InstallationPlan::default());
    }

    execute_installation_plan(plan)
}

/// Creates an InstallationPlan for package removal operations.
/// This function handles the dependency resolution and orphan detection logic
/// that was previously embedded in remove_packages().
pub fn prepare_removal_plan(package_specs: Vec<String>) -> Result<InstallationPlan> {
    let mut old_removes = HashMap::new();

    // Step 1: Resolve package specs to package keys
    let (explicitly_requested_keys, _not_found_specs) = resolve_removal_specs(&package_specs);

    // Step 2: Check for blocking dependencies and build old_removes map
    let _prevented_removals = add_top_removable(&mut old_removes, &explicitly_requested_keys);

    // Step 3: Process orphaned dependencies
    add_orphaned_dependencies(&mut old_removes);

    // Step 4: Nothing to install/upgrade
    let new_pkgs = crate::models::InstalledPackagesMap::new();

    // Step 5: Use prepare_installation_plan() with old_removes
    crate::plan::prepare_installation_plan(&new_pkgs, Some(old_removes))
}

/// Remove packages from world.json based on package specs
fn remove_from_world(package_specs: &[String]) -> Result<()> {
    use crate::parse_requires::parse_package_spec_with_version;
    use crate::models::PackageFormat;

    for spec in package_specs {
        // Parse the spec to get package name
        // Use Rpm as default format since most package specs are RPM-based
        // and the format is not available in this context
        let (pkgname, _) = parse_package_spec_with_version(spec, PackageFormat::Rpm);

        // Remove from world.json
        PACKAGE_CACHE.world.write().unwrap().remove(&pkgname);
    }

    Ok(())
}

/// Resolve package specs to package keys for removal
fn resolve_removal_specs(package_specs: &[String]) -> (std::collections::HashSet<String>, Vec<String>) {
    let mut explicitly_requested_keys = std::collections::HashSet::new();
    let mut not_found_specs = Vec::new();

    for spec in package_specs {
        // Try exact match first
        if PACKAGE_CACHE.installed_packages.read().unwrap().contains_key(spec) {
            explicitly_requested_keys.insert(spec.clone());
            continue;
        }
        // Try prefix match (e.g., spec is 'name' and key is 'name__version__arch')
        let mut found_prefix_match = false;
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        for (installed_key, _) in installed.iter() {
            if installed_key.starts_with(&(spec.clone() + "__")) {
                log::info!("Interpreting spec '{}' as '{}' for removal.", spec, installed_key);
                explicitly_requested_keys.insert(installed_key.clone());
                found_prefix_match = true;
                break;
            }
        }
        if !found_prefix_match {
            not_found_specs.push(spec.clone());
        }
    }

    if !not_found_specs.is_empty() {
        println!("Warning: The following specified packages were not found among installed packages:");
        for spec in &not_found_specs {
            println!("- {}", spec);
        }
    }

    if explicitly_requested_keys.is_empty() {
        if package_specs.is_empty() {
            println!("No packages specified for removal.");
        } else {
            println!("No installed packages match the request to remove.");
        }
    }

    (explicitly_requested_keys, not_found_specs)
}

/// Filter out blocking dependencies and populate initial removal plan.
///
/// This function validates whether explicitly requested packages can be safely removed
/// by checking their reverse dependencies (rdepends). A package can only be removed if
/// all of its reverse dependencies are either:
/// - Not currently installed, or
/// - Also explicitly requested for removal
///
/// Packages that pass this check are added to the removal plan.
/// Packages that are blocked by active reverse dependencies are recorded in the
/// prevented_removals map and a warning is displayed to the user.
///
/// # Arguments
/// * `plan` - Mutable reference to the InstallationPlan to populate with removable packages
/// * `explicitly_requested_keys` - Set of package keys that the user wants to remove
///
/// # Returns
/// Map of blocked package keys to their blocking reverse dependencies
fn add_top_removable(
    old_removes: &mut crate::models::InstalledPackagesMap,
    explicitly_requested_keys: &std::collections::HashSet<String>
) -> HashMap<String, Vec<String>> {
    let mut prevented_removals: HashMap<String, Vec<String>> = HashMap::new();

    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
    for requested_key in explicitly_requested_keys {
        if let Some(requested_pkg_info) = installed.get(requested_key) {
            let mut can_remove_requested_key = true;
            let mut blocking_rdepends = Vec::new();

            for rdep_key in &requested_pkg_info.rdepends {
                // Check if this rdepend is an installed package AND is NOT also being explicitly requested for removal
                if installed.contains_key(rdep_key) && !explicitly_requested_keys.contains(rdep_key) {
                    can_remove_requested_key = false;
                    blocking_rdepends.push(rdep_key.clone());
                }
            }

            if can_remove_requested_key {
                old_removes.insert(requested_key.clone(), requested_pkg_info.clone());
            } else {
                prevented_removals.insert(requested_key.clone(), blocking_rdepends);
            }
        } else {
            // This case should ideally not be hit if explicitly_requested_keys is derived from installed_packages
            // but as a safeguard if spec resolution logic changes.
            log::warn!("Package key '{}' from explicit request not found in installed_packages during removal check.", requested_key);
        }
    }

    if !prevented_removals.is_empty() {
        println!("Warning: The following packages cannot be removed because other installed packages depend on them:");
        for (pkg_to_remove, blockers) in &prevented_removals {
            println!("- '{}' is required by: {}", pkg_to_remove, blockers.join(", "));
        }
        // If ALL explicitly requested packages were blocked, then old_removes will be empty.
        if old_removes.is_empty() {
             println!("No packages will be removed as all specified packages have active dependencies or were not found.");
        }
        // If some were blocked but others were not, old_removes is not empty,
        // and we proceed with the ones that can be removed. The warning above is sufficient.
    }

    if old_removes.is_empty() && !explicitly_requested_keys.is_empty() {
        println!("No packages will be removed.");
    }

    prevented_removals
}

/// Find orphaned dependencies recursively and adds them to the removal plan.
///
/// This function identifies and marks dependencies as orphaned when all packages
/// that depend on them are being removed. It operates on packages already marked
/// for removal in the plan and recursively checks their dependencies.
///
/// The algorithm works as follows:
/// 1. Starts with a queue of packages already marked for removal in `plan.old_removes`.
/// 2. For each package being removed, examines its direct dependencies.
/// 3. For each dependency, determines if it should be considered orphaned:
///    - Skips if already marked for removal
///    - Skips if explicitly installed (`ebin_exposure == true`) or has `depend_depth == 0`
///    - Skips if it's an essential package
///    - Marks as orphaned if ALL of its reverse dependencies (`rdepends`) are
///      already in the removal plan (meaning all packages that depend on it are
///      being removed)
/// 4. When a dependency is marked as orphaned, it's added to the removal plan
///    and to the processing queue to check its own dependencies recursively.
///
/// This ensures that transitive dependencies are properly cleaned up when
/// they're no longer needed by any remaining installed packages.
///
/// # Arguments
/// * `plan` - Mutable reference to the InstallationPlan that contains packages
///            already marked for removal and will be updated with orphaned dependencies
fn add_orphaned_dependencies(old_removes: &mut crate::models::InstalledPackagesMap) {
    // Build initial processing queue from packages already marked for removal
    let mut processing_queue: Vec<String> = old_removes.keys().cloned().collect();
    let mut visited_for_orphan_check = std::collections::HashSet::new();

    while let Some(pkgkey_being_removed) = processing_queue.pop() {
        if !visited_for_orphan_check.insert(pkgkey_being_removed.clone()) {
            continue;
        }

        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        let current_pkg_info = match installed.get(&pkgkey_being_removed) {
            Some(info) => info.clone(),
            None => continue,
        };

        for dep_pkgkey in &current_pkg_info.depends {
            if old_removes.contains_key(dep_pkgkey) {
                continue; // Already marked for removal
            }

            log::debug!("[Orphan Check] Considering dependency '{}' of package '{}' being removed.", dep_pkgkey, pkgkey_being_removed);
            let dep_info = match installed.get(dep_pkgkey) {
                Some(info) => info,
                None => {
                    log::warn!("Dependency '{}' of '{}' not found in installed_packages. Skipping orphan check.", dep_pkgkey, pkgkey_being_removed);
                    continue;
                }
            };

            log::debug!("[Orphan Check] Properties for dep '{}': ebin_exposure={}, depend_depth={}, rdepends={:?}", dep_pkgkey, dep_info.ebin_exposure, dep_info.depend_depth, dep_info.rdepends);

            if dep_info.ebin_exposure || dep_info.depend_depth == 0 {
                log::debug!("Dependency '{}' is explicitly installed or depth 0, not removing automatically.", dep_pkgkey);
                continue;
            }

            let dep_pkgname_parts: Vec<&str> = dep_pkgkey.split("__").collect();
            let dep_pkgname = dep_pkgname_parts.get(0).unwrap_or(&"");
            if crate::mmio::is_essential_pkgname(dep_pkgname) {
                log::debug!("Dependency '{}' ({}) is essential, not removing automatically.", dep_pkgkey, dep_pkgname);
                continue;
            }

            // Check if all reverse dependencies are being removed
            let mut all_rdepends_being_removed = true;
            if dep_info.rdepends.is_empty() {
                // If a non-ebin package has no rdepends, it's an orphan if its direct requirer (pkgkey_being_removed) is removed.
                // However, this state implies its rdepends were not properly recorded or it's a very old package.
                // For safety, we only act if there's at least one rdepend and it's pkgkey_being_removed or also in final_removal_set.
                // If rdepends is truly empty, it means no *other* installed package depends on it.
                // If pkgkey_being_removed was its only dependent, it becomes an orphan.
                // This is implicitly handled: if no other rdepend exists that is *not* being removed, it's an orphan.
                log::debug!("Dependency '{}' has no recorded rdepends. It will be removed if not kept by other means.", dep_pkgkey);
            } else {
                for rdep_pkgkey in &dep_info.rdepends {
                    let rdep_is_in_final_set = old_removes.contains_key(rdep_pkgkey);
                    log::debug!("[Orphan Check]   Checking rdepend '{}' (of dep '{}'): is_in_final_removal_set? {}", rdep_pkgkey, dep_pkgkey, rdep_is_in_final_set);
                    if !rdep_is_in_final_set {
                        all_rdepends_being_removed = false;
                        log::debug!("Dependency '{}' will be kept because its rdepend '{}' is not being removed.", dep_pkgkey, rdep_pkgkey);
                        break;
                    }
                }
            }

            log::debug!("[Orphan Check] Final all_rdepends_being_removed for dep '{}': {}", dep_pkgkey, all_rdepends_being_removed);
            if all_rdepends_being_removed {
                log::info!("Marking orphaned dependency for removal: {}", dep_pkgkey);
                old_removes.insert(dep_pkgkey.clone(), dep_info.clone());
                // Add to queue only if not already processed to check its dependencies for orphaning
                if !visited_for_orphan_check.contains(dep_pkgkey) {
                     processing_queue.push(dep_pkgkey.clone());
                }
            }
        }
    }
}
