use std::process::exit;
use std::collections::HashMap;
use chrono::Utc;
use clap::parser::ValuesRef;
use anyhow::Result;
use crate::models::*;

impl PackageManager {

    pub fn install_packages(&mut self, package_specs: ValuesRef<String>) -> Result<()> {

        self.load_store_paths()?;
        self.load_installed_packages()?;

        let mut packages_to_install = self.manual_install_packages(package_specs);
        let mut depend_packages: HashMap<String, InstalledPackageInfo> = HashMap::new();

        self.collect_depends(&packages_to_install, &mut depend_packages)?;

        while !depend_packages.is_empty() {
            packages_to_install.extend(depend_packages);
            depend_packages = HashMap::new();
            self.collect_depends(&packages_to_install, &mut depend_packages)?;
        }

        if self.options.verbose {
            println!("Installing packages: {:#?}", packages_to_install.keys());
        }

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
                                manual_install: true,
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
