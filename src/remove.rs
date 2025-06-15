use std::fs;
use std::path::PathBuf;
use std::collections::HashMap;
use color_eyre::eyre::{self, Result, WrapErr};
use color_eyre::eyre::eyre;
use crate::utils::*;
use crate::models::*;
use crate::scriptlets::{run_scriptlets, ScriptletType};

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
        let fs_files = list_package_files(fs_dir.to_str().unwrap_or_else(|| panic!("Invalid path for fs_dir: {}", fs_dir.display())))?;
        log::debug!("Unlinking package from {} to {} ({} files)", pkg_store_path.display(), env_root.display(), fs_files.len());
        for fs_file in fs_files {
            let fhs_file = fs_file.strip_prefix(&fs_dir)
                .map_err(|e| eyre::eyre!("Failed to strip prefix from path: {}", e))?;
            let target_path = env_root.join(fhs_file);

            // Skip dir
            if target_path.is_dir() {
                continue;
            }

            // Remove file (include symlink)
            if fs::symlink_metadata(&target_path).is_ok() {
                log::trace!("Removing package file: {}", target_path.display());
                fs::remove_file(&target_path)?;
            }
        }

        // Remove ebin wrappers based on stored links
        let pkgline_osstr = pkg_store_path.file_name().ok_or_else(||
            eyre::eyre!("Could not extract file name from path: {}", pkg_store_path.display())
        )?;
        let pkgline = pkgline_osstr.to_string_lossy();
        if pkgline.is_empty() {
            return Err(eyre::eyre!("Extracted pkgline is empty from path: {}", pkg_store_path.display()));
        }

        let pkgkey = crate::package::pkgline2pkgkey(&pkgline)
            .map_err(|e| eyre::eyre!("Failed to get pkgkey from pkgline '{}' (from {}): {}", pkgline, pkg_store_path.display(), e))?;

        if let Some(package_info) = self.installed_packages.get(&pkgkey) {
            if !package_info.ebin_links.is_empty() {
                log::debug!("Removing {} ebin wrappers for package '{}'", package_info.ebin_links.len(), pkgkey);
                for relative_ebin_path_str in &package_info.ebin_links {
                    let ebin_path = env_root.join(relative_ebin_path_str);
                    if fs::symlink_metadata(&ebin_path).is_ok() {
                        log::debug!("Removing ebin wrapper: {}", ebin_path.display());
                        fs::remove_file(&ebin_path)
                            .with_context(|| format!("Failed to remove ebin wrapper {}", ebin_path.display()))?;
                        } else {
                            log::warn!("Ebin wrapper listed in metadata not found for removal: {}", ebin_path.display());
                    }
                }
            }
        } else {
            log::warn!("Package info not found for key '{}' (derived from pkgline '{}' from path '{}') during unlink. Cannot remove ebin wrappers.", pkgkey, pkgline, pkg_store_path.display());
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

        let mut explicitly_requested_keys = std::collections::HashSet::new();
        let mut not_found_specs = Vec::new();

        for spec in &package_specs {
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
            for spec in not_found_specs {
                println!("- {}", spec);
            }
        }

        if explicitly_requested_keys.is_empty() {
            if package_specs.is_empty() {
                println!("No packages specified for removal.");
            } else {
                println!("No installed packages match the request to remove.");
            }
            return Ok(());
        }

        let mut final_removal_set: HashMap<String, InstalledPackageInfo> = HashMap::new();
        let mut processing_queue: Vec<String> = Vec::new();
        let mut prevented_removals: HashMap<String, Vec<String>> = HashMap::new();

        for requested_key in &explicitly_requested_keys {
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
                    final_removal_set.insert(requested_key.clone(), requested_pkg_info.clone());
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
            // If ALL explicitly requested packages were blocked, then final_removal_set will be empty.
            if final_removal_set.is_empty() {
                 println!("No packages will be removed as all specified packages have active dependencies or were not found.");
                 return Ok(());
            }
            // If some were blocked but others were not, final_removal_set is not empty,
            // and we proceed with the ones that can be removed. The warning above is sufficient.
        }

        // If, after all checks, there's nothing to remove (e.g. initial request was valid but all items got filtered out)
        if final_removal_set.is_empty() && !explicitly_requested_keys.is_empty() {
            println!("No packages will be removed.");
            return Ok(());
        }

        // The original processing_queue was populated directly from explicitly_requested_keys.
        // Now it's populated with keys that passed the rdepend check.
        // The `visited_for_orphan_check` set is still appropriate for the subsequent orphan processing loop.
        let mut visited_for_orphan_check = std::collections::HashSet::new();

        while let Some(pkgkey_being_removed) = processing_queue.pop() {
            if !visited_for_orphan_check.insert(pkgkey_being_removed.clone()) {
                continue;
            }

            let current_pkg_info = match self.installed_packages.get(&pkgkey_being_removed) {
                Some(info) => info.clone(), // Clone to satisfy borrow checker for later map insertions
                None => continue, // Should not happen as we populate from installed_packages
            };

            for dep_pkgkey in &current_pkg_info.depends {
                if final_removal_set.contains_key(dep_pkgkey) {
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

                let mut all_rdepends_being_removed = true;
                log::debug!("[Orphan Check] Initial all_rdepends_being_removed for dep '{}': {}", dep_pkgkey, all_rdepends_being_removed);
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
                        let rdep_is_in_final_set = final_removal_set.contains_key(rdep_pkgkey);
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
                    final_removal_set.insert(dep_pkgkey.clone(), dep_info.clone());
                    // Add to queue only if not already processed to check its dependencies for orphaning
                    if !visited_for_orphan_check.contains(dep_pkgkey) {
                         processing_queue.push(dep_pkgkey.clone());
                    }
                }
            }
        }

        if final_removal_set.is_empty() {
            println!("No packages to remove after considering dependencies.");
            return Ok(());
        }

        println!("Packages to remove:");
        let mut sorted_removal_keys: Vec<String> = final_removal_set.keys().cloned().collect();
        sorted_removal_keys.sort(); // For consistent output
        for pkgkey in &sorted_removal_keys {
            println!("- {}", pkgkey);
        }

        if config().common.dry_run {
            log::info!("Dry run: No changes will be made.");
            return Ok(());
        }

        // Update rdepends of packages that depended on the removed packages
        // This is done after the dry_run check, as it modifies the state of self.installed_packages.
        for (removed_pkg_key, _removed_pkg_info_clone) in &final_removal_set {
            // The `final_removal_set` contains clones of `InstalledPackageInfo` as they were when added.
            // So, `_removed_pkg_info_clone.depends` is the correct list of dependencies for `removed_pkg_key`.
            if let Some(info_of_removed_pkg) = final_removal_set.get(removed_pkg_key) { // Use the info from final_removal_set
                for dep_on_key in &info_of_removed_pkg.depends {
                    // If the dependency itself is NOT being removed
                    if !final_removal_set.contains_key(dep_on_key) {
                        // Get the mutable info of this dependency from the main installed_packages map
                        if let Some(dep_pkg_info_mut) = self.installed_packages.get_mut(dep_on_key) {
                            let initial_rdep_count = dep_pkg_info_mut.rdepends.len();
                            dep_pkg_info_mut.rdepends.retain(|r| r != removed_pkg_key);
                            if dep_pkg_info_mut.rdepends.len() < initial_rdep_count {
                                log::debug!("Updated rdepends for '{}': removed '{}' (was one of its rdepends)", dep_on_key, removed_pkg_key);
                            } else {
                                log::trace!("Checked rdepends for '{}': '{}' was not found as an rdepend (or already removed)", dep_on_key, removed_pkg_key);
                            }
                        }
                    }
                }
            }
        }

        let new_generation = self.create_new_generation()?;
        let env_root = self.get_default_env_root()?;
        let store_root = dirs().epkg_store.clone();
        let repo_format = {
            let channel_config = self.get_channel_config(config().common.env.clone())?;
            channel_config.format
        };

        run_scriptlets(
            &final_removal_set,
            &store_root,
            &env_root,
            repo_format,
            ScriptletType::PreRemove,
            false, // is_upgrade
        )?;

        for pkgkey in &sorted_removal_keys {
            if let Some(info_to_remove) = final_removal_set.get(pkgkey) {
                // Ensure pkgline is valid for path construction
                if info_to_remove.pkgline.is_empty() || info_to_remove.pkgline.contains("/") || info_to_remove.pkgline.contains("..") {
                    log::error!("Invalid pkgline for {}: '{}'. Skipping unlink.", pkgkey, info_to_remove.pkgline);
                    return Err(eyre!("Invalid pkgline for {}: '{}'", pkgkey, info_to_remove.pkgline));
                }
                let pkg_store_path = store_root.join(&info_to_remove.pkgline);
                log::info!("Unlinking files for package: {} from store path {}", pkgkey, pkg_store_path.display());
                self.unlink_package(&pkg_store_path, &env_root)
                    .with_context(|| format!("Failed to unlink package {} (store path: {})", pkgkey, pkg_store_path.display()))?;
                self.installed_packages.remove(pkgkey);
            } else {
                 log::warn!("Package {} was in final_removal_set keys but not in map value during unlink phase.", pkgkey);
            }
        }

        run_scriptlets(
            &final_removal_set,
            &store_root,
            &env_root,
            repo_format,
            ScriptletType::PostRemove,
            false, // is_upgrade
        )?;

        self.save_installed_packages(&new_generation)?;
        self.record_history(&new_generation, "remove", sorted_removal_keys, vec![])?;
        self.update_current_generation_symlink(new_generation)?;

        println!("Removal successful. {} packages removed.", final_removal_set.len());
        Ok(())
    }

}
