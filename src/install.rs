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

impl PackageManager {

    pub fn install_packages(&mut self, package_specs: ValuesRef<String>) -> Result<()> {

        self.load_store_paths()?;
        self.load_installed_packages()?;

        let mut packages_to_install = self.manual_install_packages(package_specs);
        let mut depend_packages: HashMap<String, InstalledPackageInfo> = HashMap::new();
        let mut depth = 1;

        self.collect_depends(&packages_to_install, &mut depend_packages, depth)?;

        while !depend_packages.is_empty() {
            packages_to_install.extend(depend_packages);
            depend_packages = HashMap::new();
            depth += 1;
            self.collect_depends(&packages_to_install, &mut depend_packages, depth)?;
        }

        if self.options.verbose {
            println!("Packages to install:");
            print_packages_by_depend_depth(&packages_to_install);
        }

        self.installed_packages.extend(packages_to_install);
        self.save_installed_packages()?;

        Ok(())
    }

    // convert user provided @pkg_names to pkglines,
    // skipping the ones already in @installed_packages
    fn manual_install_packages(&self, pkg_names: ValuesRef<String>) -> HashMap<String, InstalledPackageInfo> {
        let mut packages_to_install = HashMap::new();
        let mut missing_names = Vec::new();

        for pkgname in pkg_names {
            if let Some(pkglines) = self.pkgname2lines.get(pkgname) {
                for pkgline in pkglines {
                    if !self.installed_packages.contains_key(pkgline) {
                        packages_to_install.insert(
                            pkgline.clone(),
                            InstalledPackageInfo {
                                install_time: Utc::now(),
                                depend_depth: 0,
                            },
                        );
                    }
                }
            } else {
                missing_names.push(pkgname);
            }
        }

        if !missing_names.is_empty() {
            println!("Missing packages: {:#?}", missing_names);
            if !self.options.ignore_missing {
                exit(1);
            }
        }

        packages_to_install
    }
}
