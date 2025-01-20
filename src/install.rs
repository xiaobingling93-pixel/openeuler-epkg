use std::process::exit;
use std::collections::HashMap;
use chrono::Utc;
use clap::parser::ValuesRef;
use anyhow::Result;
use crate::models::*;

fn print_packages_by_depend_depth(packages: &HashMap<String, InstalledPackageInfo>) {
    // Convert HashMap to a Vec of tuples (pkgline, info)
    let mut packages_vec: Vec<(&String, &InstalledPackageInfo)> = packages.iter().collect();

    // Sort by depend_depth
    packages_vec.sort_by(|a, b| a.1.depend_depth.cmp(&b.1.depend_depth));

    // Print the header
    println!("{:<12} {:<10}", "depend_depth", "package");

    // Print each package
    for (pkgline, info) in packages_vec {
        println!("{:<12} {:<10}", info.depend_depth, pkgline);
    }
}

/// Finds duplicates between `a` and `b`,
/// shows a warning about the duplicates, and removes them from `b`.
pub fn remove_duplicates(
    a: &HashMap<String, InstalledPackageInfo>,
    b: &mut HashMap<String, InstalledPackageInfo>,
    warn: &str) {

    let duplicates: Vec<_> = b
        .keys()
        .filter(|&package_name| a.contains_key(package_name))
        .cloned()
        .collect();

    if !duplicates.is_empty() {
        if !warn.is_empty() {
            eprintln!("{}", warn);
            for package_name in &duplicates {
                eprintln!("- {}", package_name);
            }
        }

        // Remove duplicates from `b`
        for package_name in duplicates {
            b.remove(&package_name);
        }
    }
}

impl PackageManager {

    pub fn install_packages(&mut self, package_specs: ValuesRef<String>) -> Result<()> {

        self.load_store_paths()?;
        self.load_installed_packages()?;

        let mut packages_to_install = self.resolve_package_info(package_specs);
        remove_duplicates(&self.installed_packages, &mut packages_to_install, "Warning: The following packages are already installed and will be skipped:");

        self.collect_recursive_depends(&mut packages_to_install);
        remove_duplicates(&self.installed_packages, &mut packages_to_install, "");

        if self.options.verbose {
            println!("Packages to install:");
            print_packages_by_depend_depth(&packages_to_install);
        }

        self.installed_packages.extend(packages_to_install);
        self.save_installed_packages()?;

        Ok(())
    }

}
