use crate::models::*;
use color_eyre::{eyre::eyre, Result};
use std::collections::HashMap;

impl PackageManager {
    pub fn upgrade_packages(&mut self, package_names: Vec<String>) -> Result<()> {
        let original_installed_packages = self.installed_packages.clone();
        let mut initial_packages_to_process: HashMap<String, InstalledPackageInfo> = HashMap::new();
        let mut missing_items_log: Vec<String> = Vec::new();

        let mut packages_to_expose_from_args = HashMap::new();

        if package_names.is_empty() {
            // If no packages are specified, upgrade all top-level installed packages.
            log::info!("No packages specified; proceeding with full system upgrade of top-level packages.");
            initial_packages_to_process = self.installed_packages
                .iter()
                .filter(|(_, info)| info.depend_depth == 0)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
        } else {
            // If specific packages are named, resolve them to their latest versions.
            log::info!("Upgrading specified packages: {:?}", package_names);
            for pkg_name in &package_names {
                // apt-get behavior: if pkg_name not already installed, will install it
                self.add_one_package_installing_with_arch_spec(
                    pkg_name,
                    None, // Let the resolver pick the best arch
                    0,    // Top-level packages are depth 0
                    true, // Explicitly requested packages are exposed
                    &mut initial_packages_to_process,
                    &mut missing_items_log,
                    false, // Not in OR group context for upgrade
                );
            }

            packages_to_expose_from_args = initial_packages_to_process.clone();

            // For packages that are already installed, copy their pkgline from the existing installation
            for (pkgkey, package_info) in &mut packages_to_expose_from_args {
                if let Some(existing_info) = self.installed_packages.get(pkgkey) {
                    package_info.pkgline = existing_info.pkgline.clone();
                }
            }
        }

        if !missing_items_log.is_empty() {
            return Err(eyre!(
                "The following packages could not be found: {}",
                missing_items_log.join(", ")
            ));
        }

        if initial_packages_to_process.is_empty() {
            println!("No packages to upgrade.");
            return Ok(());
        }

        // Collect all dependencies for the initial set of packages.
        let all_packages_for_session = self.collect_recursive_depends(&initial_packages_to_process, channel_config().format)?;

        // Call the main installation function, which will handle planning and execution.
        self.install_pkgkeys(
            all_packages_for_session,
            packages_to_expose_from_args,
            &original_installed_packages,
        )
    }
}
