use std::fs;
use std::path::PathBuf;
use std::collections::HashMap;
use color_eyre::eyre::{self, Result};
use crate::models::*;
use crate::install::InstallationPlan;

impl PackageManager {

    pub fn unlink_package(&self, pkg_store_path: &PathBuf, env_root: &PathBuf) -> Result<()> {
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
            let fs_file = &fs_file_info.path;
            let fhs_file = fs_file.strip_prefix(&fs_dir)
                .map_err(|e| eyre::eyre!("Failed to strip prefix from path: {}", e))?;
            let target_path = env_root.join(fhs_file);

            // Skip dir
            if fs_file_info.is_dir() {
                continue;
            }

            // Remove file (include symlink)
            if fs::symlink_metadata(&target_path).is_ok() {
                log::trace!("Removing package file: {}", target_path.display());
                fs::remove_file(&target_path)?;
            }
        }

        Ok(())
    }

    /// Removes specified packages and their orphaned dependencies.
    ///
    /// This function operates solely on the information within `self.installed_packages`
    /// and does not consult external repositories or perform new dependency resolution
    /// via `collect_recursive_depends`. This ensures that removal decisions are based
    /// purely on the currently recorded state of installed packages.
    ///
    /// The process is as follows:
    /// 1. Loads the current set of installed packages.
    /// 2. Resolves the input `package_specs` (user-provided package names or keys)
    ///    against the loaded `self.installed_packages`. Packages not found are reported.
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
    /// 9. The corresponding entries are removed from `self.installed_packages`.
    /// 10. Post-remove scriptlets are run.
    /// 11. A new generation is created, `installed-packages.json` is saved, history recorded,
    ///     and the 'current' generation symlink is updated.
    ///
    /// This method aims for a safer and more predictable removal process, especially
    /// ensuring that shared dependencies are not removed if other installed packages
    /// (not part of the current removal set) still rely on them according to the
    /// `rdepends` information.
    pub fn remove_packages(&mut self, package_specs: Vec<String>) -> Result<()> {
        self.load_installed_packages()?;

        let plan = self.prepare_removal_plan(package_specs)?;

        if plan.old_removes.is_empty() {
            return Ok(());
        }

        self.execute_installation_plan(plan)
    }

    /// Creates an InstallationPlan for package removal operations.
    /// This function handles the dependency resolution and orphan detection logic
    /// that was previously embedded in remove_packages().
    pub fn prepare_removal_plan(&mut self, package_specs: Vec<String>) -> Result<InstallationPlan> {
        let mut plan = InstallationPlan::default();

        // Step 1: Resolve package specs to package keys
        let (explicitly_requested_keys, _not_found_specs) = self.resolve_removal_specs(&package_specs);

        // Step 2: Check for blocking dependencies
        let (processing_queue, _prevented_removals) = self.check_removal_dependencies(&explicitly_requested_keys, &mut plan);

        // Step 3: Process orphaned dependencies
        self.process_orphaned_dependencies(&mut plan, processing_queue);

        // Step 4: Auto-populate expose plan based on removal actions
        crate::PackageManager::auto_populate_expose_plan(&mut plan);

        Ok(plan)
    }

    /// Resolve package specs to package keys for removal
    fn resolve_removal_specs(&self, package_specs: &[String]) -> (std::collections::HashSet<String>, Vec<String>) {
        let mut explicitly_requested_keys = std::collections::HashSet::new();
        let mut not_found_specs = Vec::new();

        for spec in package_specs {
            // Try exact match first
            if self.installed_packages.contains_key(spec) {
                explicitly_requested_keys.insert(spec.clone());
                continue;
            }
            // Try prefix match (e.g., spec is 'name' and key is 'name__version__arch')
            let mut found_prefix_match = false;
            for installed_key in self.installed_packages.keys() {
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

    /// Check for blocking dependencies and populate initial removal plan
    fn check_removal_dependencies(
        &self,
        explicitly_requested_keys: &std::collections::HashSet<String>,
        plan: &mut InstallationPlan
    ) -> (Vec<String>, HashMap<String, Vec<String>>) {
        let mut processing_queue: Vec<String> = Vec::new();
        let mut prevented_removals: HashMap<String, Vec<String>> = HashMap::new();

        for requested_key in explicitly_requested_keys {
            if let Some(requested_pkg_info) = self.installed_packages.get(requested_key) {
                let mut can_remove_requested_key = true;
                let mut blocking_rdepends = Vec::new();

                for rdep_key in &requested_pkg_info.rdepends {
                    // Check if this rdepend is an installed package AND is NOT also being explicitly requested for removal
                    if self.installed_packages.contains_key(rdep_key) && !explicitly_requested_keys.contains(rdep_key) {
                        can_remove_requested_key = false;
                        blocking_rdepends.push(rdep_key.clone());
                    }
                }

                if can_remove_requested_key {
                    plan.old_removes.insert(requested_key.clone(), requested_pkg_info.clone());
                    processing_queue.push(requested_key.clone());
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
            // If ALL explicitly requested packages were blocked, then plan.old_removes will be empty.
            if plan.old_removes.is_empty() {
                 println!("No packages will be removed as all specified packages have active dependencies or were not found.");
            }
            // If some were blocked but others were not, plan.old_removes is not empty,
            // and we proceed with the ones that can be removed. The warning above is sufficient.
        }

        if plan.old_removes.is_empty() && !explicitly_requested_keys.is_empty() {
            println!("No packages will be removed.");
        }

        (processing_queue, prevented_removals)
    }

    /// Process orphaned dependencies recursively
    fn process_orphaned_dependencies(&mut self, plan: &mut InstallationPlan, mut processing_queue: Vec<String>) {
        // The original processing_queue was populated directly from explicitly_requested_keys.
        // Now it's populated with keys that passed the rdepend check.
        // The `visited_for_orphan_check` set is still appropriate for the subsequent orphan processing loop.
        let mut visited_for_orphan_check = std::collections::HashSet::new();

        while let Some(pkgkey_being_removed) = processing_queue.pop() {
            if !visited_for_orphan_check.insert(pkgkey_being_removed.clone()) {
                continue;
            }

            let current_pkg_info = match self.installed_packages.get(&pkgkey_being_removed) {
                Some(info) => info.clone(),
                None => continue,
            };

            for dep_pkgkey in &current_pkg_info.depends {
                if plan.old_removes.contains_key(dep_pkgkey) {
                    continue; // Already marked for removal
                }

                log::debug!("[Orphan Check] Considering dependency '{}' of package '{}' being removed.", dep_pkgkey, pkgkey_being_removed);
                let dep_info = match self.installed_packages.get(dep_pkgkey) {
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
                    // If a non-appbin package has no rdepends, it's an orphan if its direct requirer (pkgkey_being_removed) is removed.
                    // However, this state implies its rdepends were not properly recorded or it's a very old package.
                    // For safety, we only act if there's at least one rdepend and it's pkgkey_being_removed or also in final_removal_set.
                    // If rdepends is truly empty, it means no *other* installed package depends on it.
                    // If pkgkey_being_removed was its only dependent, it becomes an orphan.
                    // This is implicitly handled: if no other rdepend exists that is *not* being removed, it's an orphan.
                    log::debug!("Dependency '{}' has no recorded rdepends. It will be removed if not kept by other means.", dep_pkgkey);
                } else {
                    for rdep_pkgkey in &dep_info.rdepends {
                        let rdep_is_in_final_set = plan.old_removes.contains_key(rdep_pkgkey);
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
                    plan.old_removes.insert(dep_pkgkey.clone(), dep_info.clone());
                    // Add to queue only if not already processed to check its dependencies for orphaning
                    if !visited_for_orphan_check.contains(dep_pkgkey) {
                         processing_queue.push(dep_pkgkey.clone());
                    }
                }
            }
        }
    }

}
