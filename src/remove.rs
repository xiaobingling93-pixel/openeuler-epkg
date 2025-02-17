use std::fs;
use std::path::Path;
use std::collections::HashMap;
use clap::parser::ValuesRef;
use anyhow::Result;
use anyhow::anyhow;
use crate::paths;
use crate::models::*;
use crate::utils::list_package_files;

impl PackageManager {

    pub fn remove_package_files(&self, fs_dir: &str, symlink_dir: &str) -> Result<()> {
        let fs_files = list_package_files(&fs_dir)?;
        for fs_file in fs_files {
            let rfs_file = fs_file.strip_prefix(&fs_dir).unwrap();
            let target_path = Path::new(&symlink_dir).join(rfs_file);
            // println!("fs_file: {:?}\nrfs_file: {:?}\ntarget_path: {:?}", fs_file, rfs_file, target_path);

            // Skip dir
            if target_path.is_dir() {
                continue;
            }

            // Remove file (include symlink)
            if fs::symlink_metadata(&target_path).is_ok() {
                fs::remove_file(&target_path).unwrap();
            }

            // Remove appbin-file
            if rfs_file.starts_with("usr/bin/") {
                let rfs_file_appbin = rfs_file.to_string_lossy().replace("/bin", "/app-bin");
                let appbin_target_path = Path::new(&symlink_dir).join(&rfs_file_appbin);
                if fs::symlink_metadata(&appbin_target_path).is_ok() {
                    fs::remove_file(&appbin_target_path).unwrap();
                }
            }
        }

        Ok(())
    }

    pub fn remove_packages(&mut self, package_specs: ValuesRef<String>) -> Result<()> {
        let origin_pkg_names: Vec<String> = package_specs.clone().map(|s| s.clone()).collect();

        self.load_store_paths()?;
        self.load_installed_packages()?;
        let mut input_package_info = self.resolve_package_info(package_specs.clone());

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
            return Err(anyhow!("Error: Unable to find packages"));
        }

        // Step 2: Check if packages is being depended on by installed packages
        let duplicates_depended: Vec<String> = duplicates
            .iter()
            .filter(|name| self.installed_packages[*name].depend_depth > 0)
            .cloned()
            .collect();
        if !duplicates_depended.is_empty() {
            eprintln!("Warning: The following packages are depended on by others and cannot be removed:");
            for package_name in &duplicates_depended {
                eprintln!("- {}", package_name);
            }
            return Err(anyhow!("Cannot remove packages that are depended on by others"));
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
            .filter(|(pkgline, info)| info.depend_depth == 0 && !input_package_info.contains_key(*pkgline))
            .map(|(key, value)| (key.clone(), (*value).clone()))
            .collect();
        self.collect_recursive_depends(&mut packages_to_keep)?;
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
            println!("Do you want to continue with uninstallation? (y/n):");
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).expect("Failed to read input");
            if input.trim().to_lowercase() != "y" {
                println!("Aborted removal.");
                return Ok(());
            }
        } else {
            println!("No packages to remove.");
        }

        // Step 6: Remove package in epkg_envs_root/$cur_env/profile-current/ files
        let symlink_dir = format!("{}/{}/profile-current", paths::instance.epkg_envs_root.display(), self.options.env);
        for pkgline in &installed_to_remove {
            // remove files
            let fs_dir = format!("{}/{}/fs", paths::instance.epkg_store_root.display(), pkgline);
            self.remove_package_files(&fs_dir, &symlink_dir)?;
        }

        // Step 7: Save the updated installed_packages
        for package_name in &installed_to_remove {
            self.installed_packages.remove(package_name);
        } 
        self.save_installed_packages()?;

        //  Step 8: Save History
        self.record_history("remove", origin_pkg_names)?;

        Ok(())
    }

}