use std::fs;
use std::path::Path;
use std::collections::HashMap;
use anyhow::Result;
use anyhow::anyhow;
use crate::dirs;
use crate::utils::*;
use crate::models::*;

impl PackageManager {

    pub fn del_package(&self, fs_dir: &str, symlink_dir: &str) -> Result<()> {
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
                let rfs_file_ebin = rfs_file.to_string_lossy().replace("/bin", "/ebin");
                let appbin_target_path = Path::new(&symlink_dir).join(&rfs_file_ebin);
                if fs::symlink_metadata(&appbin_target_path).is_ok() {
                    fs::remove_file(&appbin_target_path).unwrap();
                }
            }
        }

        Ok(())
    }

    pub fn remove_packages(&mut self, package_specs: Vec<String>) -> Result<()> {
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
            .filter(|(pkgline, info)| (info.depend_depth == 0 && 
                !input_package_info.contains_key(*pkgline)) || 
                self.essential_pkgnames.contains(self.pkghash2spec.get(&pkgline[0..32]).unwrap().name.as_str()))
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
            if !self.options.assume_yes {
                println!("Do you want to continue with uninstallation? (y/n):");
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).expect("Failed to read input");
                if input.trim().to_lowercase() != "y" {
                    println!("Aborted removal.");
                    return Ok(());
                }
            }
        } else {
            println!("No packages to remove.");
        }

        // Step 6: Remove package files
        let symlink_dir = self.create_new_generation()?;
        for pkgline in &installed_to_remove {
            // remove files
            let store_root = self.dirs.epkg_store;
            let fs_dir = format!("{}/{}/fs", store_root.display(), pkgline);
            self.del_package(&fs_dir, &symlink_dir)?;
        }

        //  Step 7: Save installed packages
        for package_name in &installed_to_remove {
            self.installed_packages.remove(package_name);
        } 
        self.save_installed_packages()?;
        self.record_history("remove", vec![], installed_to_remove.clone())?;
        println!("Remove successful - Total packages: {}", installed_to_remove.len());

        Ok(())
    }

}
