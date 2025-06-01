use std::fs;
use std::path::PathBuf;
use std::collections::HashMap;
use color_eyre::eyre::{self, Result, WrapErr};
use crate::utils::*;
use crate::models::*;

impl PackageManager {

    pub fn unlink_package(&self, fs_dir: &PathBuf, env_root: &PathBuf) -> Result<()> {
        let fs_files = list_package_files(fs_dir.to_str().unwrap())?;
        for fs_file in fs_files {
            let fhs_file = fs_file.strip_prefix(&fs_dir)
                .map_err(|e| eyre::eyre!("Failed to strip prefix from path: {}", e))?;
            let target_path = env_root.join(fhs_file);
            // println!("fs_file: {:?}\nrfs_file: {:?}\ntarget_path: {:?}", fs_file, fhs_file, target_path);

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
                    fs::remove_file(&appbin_target_path)?;
                }
            }
        }

        Ok(())
    }

    pub fn remove_packages(&mut self, package_specs: Vec<String>) -> Result<()> {
        self.load_installed_packages()?;
        let mut input_package_info = self.resolve_package_info(package_specs.clone());
        log::debug!("Input package specs: {:?}", package_specs);

        // Step 1: Find duplicates between installed_packages and input_package_info
        let duplicates: Vec<String> = input_package_info
            .keys()
            .filter(|name| self.installed_packages.contains_key(*name))
            .cloned()
            .collect();
        if duplicates.is_empty() {
            eprintln!("Warning: No match for installed packages:");
            for package_name in package_specs.clone() {
                eprintln!("- {}", package_name);
            }
            return Err(eyre::eyre!("Error: Unable to find packages"));
        }
        log::debug!("Found duplicate packages: {:?}", duplicates);

        // Step 2: Check if packages is being depended on by installed packages
        let duplicates_depended: Vec<String> = duplicates
            .iter()
            .filter(|name| self.installed_packages.get(*name)
                .map(|info| info.depend_depth > 0)
                .unwrap_or(false))
            .cloned()
            .collect();
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
            .filter(|(pkgline, info)| {
                if info.depend_depth == 0 && !input_package_info.contains_key(*pkgline) {
                    log::debug!("Keeping independent package: {}", pkgline);
                    return true;
                }
                if let Some(spec) = self.pkghash2spec.get(&pkgline[0..32]) {
                    let is_essential = self.essential_pkgnames.contains(spec.name.as_str());
                    if is_essential {
                        log::debug!("Keeping essential package: {} ({})", pkgline, spec.name);
                    }
                    is_essential
                } else {
                    false
                }
            })
            .map(|(key, value)| (key.clone(), (*value).clone()))
            .collect();
        self.collect_recursive_depends(&mut packages_to_keep)?;
        log::debug!("Packages to keep: {:?}", packages_to_keep.keys());

        let installed_to_remove: Vec<String> = self.installed_packages
            .keys()
            .filter(|name| !packages_to_keep.contains_key(*name))
            .cloned()
            .collect();

        // Step 5: Show packages to remove
        if !installed_to_remove.is_empty() {
            println!("Packages to remove:");
            for package_name in &installed_to_remove {
                println!("- {}", package_name);
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

        // Step 6: Remove package files
        let new_generation = self.create_new_generation()?;
        let env_root = self.get_default_env_root()?;
        let store_root = dirs().epkg_store.clone();
        for pkgline in &installed_to_remove {
            // remove link files
            log::debug!("Removing files for package {} from {:?}", pkgline, store_root.join(pkgline).join("fs"));
            self.unlink_package(&store_root.join(pkgline).join("fs"), &env_root)?;
        }

        // Step 7: Save installed packages
        for package_name in &installed_to_remove {
            self.installed_packages.remove(package_name);
        }
        self.save_installed_packages(&new_generation)?;
        self.record_history("remove", vec![], installed_to_remove.clone())?;

        // Last step: update current symlink to point to the new generation
        self.update_current_generation_symlink(new_generation)?;

        println!("Remove successful - Total packages: {}", installed_to_remove.len());

        Ok(())
    }

}
