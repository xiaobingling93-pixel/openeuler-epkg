use std::process::exit;
use std::collections::HashMap;
use clap::parser::ValuesRef;
use anyhow::Result;
use anyhow::anyhow;
use crate::models::*;

impl PackageManager {

    pub fn remove_packages(&mut self, package_specs: ValuesRef<String>) -> Result<()> {

        self.load_store_paths()?;
        self.load_installed_packages()?;

        // Step 1: Resolve package info for the given specs
        let mut packages_to_remove = self.resolve_package_info(package_specs);

        // Step 2: Find duplicates between installed_packages and packages_to_remove
        let duplicates: Vec<String> = packages_to_remove
            .keys()
            .filter(|name| self.installed_packages.contains_key(*name))
            .cloned()
            .collect();

        // Step 3: Find duplicates with depend_depth > 0
        let duplicates_depended: Vec<String> = duplicates
            .iter()
            .filter(|name| self.installed_packages[*name].depend_depth > 0)
            .cloned()
            .collect();

        // Step 4: If duplicates_depended is not empty, warn and exit
        if !duplicates_depended.is_empty() {
            eprintln!("Warning: The following packages are depended on by others and cannot be removed:");
            for package_name in &duplicates_depended {
                eprintln!("- {}", package_name);
            }
            return Err(anyhow!("Cannot remove packages that are depended on by others"));
        }

        // Step 5: Find non-duplicates (packages not installed)
        let non_duplicates: Vec<String> = packages_to_remove
            .keys()
            .filter(|name| !self.installed_packages.contains_key(*name))
            .cloned()
            .collect();

        // Step 6: Warn about non-duplicates (packages not installed)
        if !non_duplicates.is_empty() {
            eprintln!("Warning: The following packages are not installed and cannot be removed:");
            for package_name in &non_duplicates {
                eprintln!("- {}", package_name);
            }
        }

        // Step 7: Remove non-duplicates from packages_to_remove
        for package_name in &non_duplicates {
            packages_to_remove.remove(package_name);
        }

        // Step 8: Collect recursive dependencies that should be kept
        let mut packages_to_keep: HashMap<String, InstalledPackageInfo> = self
            .installed_packages
            .iter()
            .filter(|(pkgline, info)| info.depend_depth == 0 && !packages_to_remove.contains_key(*pkgline))
            .map(|(key, value)| (key.clone(), (*value).clone())) // Clone the key and value
            .collect(); // Collect into a HashMap
        self.collect_recursive_depends(&mut packages_to_keep);

        // Step 9: Find final duplicates after collecting dependencies
        let installed_to_remove: Vec<String> = self.installed_packages
            .keys()
            .filter(|name| !packages_to_keep.contains_key(*name))
            .cloned()
            .collect();

        // Step 10: Show packages to remove
        if !installed_to_remove.is_empty() {
            println!("Packages to remove:");
            for package_name in &installed_to_remove {
                println!("- {}", package_name);
            }
        } else {
            println!("No packages to remove.");
        }

        // Step 11: Remove from installed_packages
        for package_name in &installed_to_remove {
            self.installed_packages.remove(package_name);
        }

        // Step 12: Save the updated installed_packages
        self.save_installed_packages()?;

        Ok(())
    }

}
