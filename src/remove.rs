use std::fs;
use std::path::PathBuf;
use std::collections::HashMap;
use color_eyre::eyre::{self, Result, WrapErr};
use color_eyre::eyre::eyre;
use crate::utils::*;
use crate::models::*;
use crate::scriptlets::{run_scriptlets, ScriptletType};

impl PackageManager {

    pub fn unlink_package(&self, fs_dir: &PathBuf, env_root: &PathBuf) -> Result<()> {
        let fs_files = list_package_files(fs_dir.to_str().unwrap())?;
        log::debug!("Unlinking package from {} to {} ({} files)", fs_dir.display(), env_root.display(), fs_files.len());
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
                fs::remove_file(&target_path)?;
            }
            // Remove appbin-file
            if fhs_file.starts_with("usr/bin/") || fhs_file.starts_with("usr/sbin/") {
                let ebin_file = fhs_file.to_string_lossy()
                    .replace("/bin", "/ebin")
                    .replace("/sbin", "/ebin");
                let appbin_target_path = env_root.join(&ebin_file);
                if fs::symlink_metadata(&appbin_target_path).is_ok() {
                    log::debug!("Removing appbin file: {}", appbin_target_path.display());
                    fs::remove_file(&appbin_target_path)?;
                }
            }
        }

        Ok(())
    }

    pub fn remove_packages(&mut self, package_specs: Vec<String>) -> Result<()> {
        self.load_installed_packages()?;
        let channel_config = self.get_channel_config(config().common.env.clone())?;
        let repo_format = channel_config.format;
        let mut input_package_info = self.resolve_package_info(package_specs.clone(), repo_format);
        log::debug!(
            "Loaded {} installed packages; Input specs: {:?}, resolved to {} packages",
            self.installed_packages.len(),
            package_specs,
            input_package_info.len()
        );

        // Step 1: Find duplicates between installed_packages and input_package_info
        let duplicates: Vec<String> = input_package_info
            .keys()
            .filter(|name| self.installed_packages.contains_key(*name))
            .cloned()
            .collect();
        if duplicates.is_empty() {
            println!("Packages are not installed:");
            for package_name in package_specs.clone() {
                println!("- {}", package_name);
            }
            return Ok(());
        }
        log::debug!("Found duplicate packages: {:?}", duplicates);

        // Step 2: Check if packages is being depended on by installed packages
        let mut duplicates_depended: Vec<String> = duplicates
            .iter()
            .filter(|name| self.installed_packages.get(*name)
                .map(|info| info.depend_depth > 0)
                .unwrap_or(false))
            .cloned()
            .collect();

        for pkg_name in input_package_info.keys() {
            duplicates_depended.retain(|x| x != pkg_name);
        }

        if !duplicates_depended.is_empty() {
            eprintln!("Warning: The following packages are depended on by others and cannot be removed:");
            for package_name in &duplicates_depended {
                eprintln!("- {}", package_name);
            }
            return Err(eyre::eyre!("Cannot remove packages that are depended on by others"));
        }

        // Step 3: Find non-duplicates (packages not installed), Remove non-duplicates from input_package_info
        let non_duplicates: Vec<String> = input_package_info
            .keys()
            .filter(|name| !self.installed_packages.contains_key(*name))
            .cloned()
            .collect();
        if !non_duplicates.is_empty() {
            eprintln!("Warning: The following packages are not installed and cannot be removed:");
            for package_name in &non_duplicates {
                eprintln!("- {}", package_name);
            }
        }
        for package_name in &non_duplicates {
            input_package_info.remove(package_name);
        }

        // Step 4: Collect recursive dependencies that should be kept (Parse the packages that are only depended on by input_package_info)
        let mut packages_to_keep: HashMap<String, InstalledPackageInfo> = self
            .installed_packages
            .iter()
            .filter(|(pkgkey, info)| {
                if info.depend_depth == 0 && !input_package_info.contains_key(*pkgkey) {
                    log::debug!("Keeping independent package: {}", pkgkey);
                    return true;
                }
                // Now pkgkey is the actual key, so we can look it up directly
                if let Some(package) = self.pkgkey2package.get(*pkgkey) {
                    let is_essential = crate::mmio::is_essential_pkgname(&package.pkgname);
                    if is_essential {
                        log::debug!("Keeping essential package: {} ({})", pkgkey, package.pkgname);
                    }
                    is_essential
                } else {
                    false
                }
            })
            .map(|(key, value)| (key.clone(), (*value).clone()))
            .collect();
        let channel_config = self.channels_config.get(&config().common.env)
            .ok_or_else(|| eyre!(
                "Channel configuration not found for environment '{}'. Ensure environment is initialized and linked to a channel.",
                config().common.env
            ))?;
        // Collect dependencies
        let dependencies = self.collect_recursive_depends(&packages_to_keep, channel_config.format)?;

        // Create a complete map of packages to keep including dependencies
        let mut all_packages_to_keep = packages_to_keep.clone();
        all_packages_to_keep.extend(dependencies);
        log::debug!("Packages to keep: {:?}", all_packages_to_keep.keys());

        let installed_to_remove: Vec<String> = self.installed_packages
            .keys()
            .filter(|name| !all_packages_to_keep.contains_key(*name))
            .cloned()
            .collect();

        // Step 5: Show packages to remove
        if !installed_to_remove.is_empty() {
            println!("Packages to remove:");
            for pkgkey in &installed_to_remove {
                println!("- {}", pkgkey);
            }
            if !config().common.assume_yes {
                println!("Do you want to continue with uninstallation? (y/n):");
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)
                    .with_context(|| "Failed to read user input")?;
                if input.trim().to_lowercase() != "y" {
                    println!("Aborted removal.");
                    return Ok(());
                }
            }
        } else {
            println!("No packages to remove.");
        }

        // Exit early if in simulate mode, but only after all computations are done
        if config().common.simulate {
            return Ok(());
        }

        // Step 6: Remove package files
        let new_generation = self.create_new_generation()?;
        let env_root = self.get_default_env_root()?;
        let store_root = dirs().epkg_store.clone();
        
        // Create packages_to_remove map for scriptlets
        let packages_to_remove: HashMap<String, InstalledPackageInfo> = installed_to_remove
            .iter()
            .filter_map(|pkgkey| {
                self.installed_packages.get(pkgkey)
                    .map(|info| (pkgkey.clone(), info.clone()))
            })
            .collect();

        // Run pre-remove scriptlets for all packages to be removed
        let current_env_name_ref = &config().common.env;
        let channel_config = self.channels_config.get(current_env_name_ref)
            .ok_or_else(|| eyre::eyre!(
                "Channel configuration not found for environment '{}'. Ensure environment is initialized and linked to a channel.",
                current_env_name_ref
            ))?;

        // Step 1: Pre-remove scriptlets
        run_scriptlets(
            &packages_to_remove,
            &store_root,
            &env_root,
            channel_config.format,
            ScriptletType::PreRemove,
            false, // is_upgrade
        )?;

        for pkgkey in &installed_to_remove {
            // remove link files
            let pkgline = self.installed_packages.get(pkgkey)
                .ok_or_else(|| eyre!("Package not found: {}", pkgkey))?
                .pkgline.clone();
            log::debug!("Removing files for package {} from {:?}", pkgkey, store_root.join(&pkgline).join("fs"));
            self.unlink_package(&store_root.join(pkgline).join("fs"), &env_root)?;
        }

        // Step 3: Post-remove scriptlets
        run_scriptlets(
            &packages_to_remove,
            &store_root,
            &env_root,
            channel_config.format,
            ScriptletType::PostRemove,
            false, // is_upgrade
        )?;

        // Step 7: Save installed packages
        for pkgkey in &installed_to_remove {
            self.installed_packages.remove(pkgkey);
        }
        self.save_installed_packages(&new_generation)?;
        self.record_history(&new_generation, "remove", vec![], installed_to_remove.clone())?;

        // Last step: update current symlink to point to the new generation
        self.update_current_generation_symlink(new_generation)?;

        println!("Remove successful - Total packages: {}", installed_to_remove.len());

        Ok(())
    }

}
