//! World state management module
//!
//! This module handles the "world" file that tracks user-requested packages and their constraints,
//! providing functions to load, save, and manage package specifications.

use std::collections::HashMap;
use color_eyre::Result;
use crate::models::PackageManager;
use crate::models::PACKAGE_CACHE;

impl PackageManager {

    /// Create a delta_world HashMap from package specs
    /// Parses each spec and returns a map of package name -> version constraint
    pub fn create_delta_world_from_specs(package_specs: &[String]) -> HashMap<String, String> {
        use crate::parse_requires::{parse_package_spec_with_version, format_version_constraint_for_world};

        let mut delta_world = HashMap::new();

        for spec in package_specs {
            let (pkgname, constraints) = parse_package_spec_with_version(spec, crate::models::PackageFormat::Apk);

            // Format the constraint string
            let constraint_str = if let Some(ref constraints) = constraints {
                format_version_constraint_for_world(constraints)
            } else {
                String::new()
            };

            // Add to delta_world
            delta_world.insert(pkgname, constraint_str);
        }

        delta_world
    }

    /// Update self.world from delta_world
    /// Also automatically removes any delta_world keys from world['no-install']
    pub fn apply_delta_world(&mut self, delta_world: &HashMap<String, String>) {
        // Remove delta_world keys from no-install if they exist
        let packages_to_remove: Vec<String> = delta_world.keys().cloned().collect();
        self.remove_from_no_install(packages_to_remove.iter());

        // Add/update packages in world
        let mut world = PACKAGE_CACHE.world.write().unwrap();
        for (pkgname, constraint_str) in delta_world {
            world.insert(pkgname.clone(), constraint_str.clone());
        }
    }

    /// Remove packages from no-install list
    /// Takes an iterator of package names to remove
    pub fn remove_from_no_install<'a, I>(&mut self, packages_to_remove: I)
    where
        I: Iterator<Item = &'a String>,
    {
        let mut no_install_set = self.get_no_install_set();
        let mut changed = false;

        for pkgname in packages_to_remove {
            if no_install_set.remove(pkgname) {
                changed = true;
            }
        }

        if changed {
            self.update_no_install_in_world(no_install_set);
        }
    }

    /// Update world["no-install"] with the given HashSet of package names
    /// Converts to space-separated string and updates or removes the key
    fn update_no_install_in_world(&mut self, no_install_set: std::collections::HashSet<String>) {
        let mut no_install_vec: Vec<String> = no_install_set.into_iter().collect();
        no_install_vec.sort();

        let mut world = PACKAGE_CACHE.world.write().unwrap();
        if no_install_vec.is_empty() {
            // Remove "no-install" key if list is empty
            world.remove("no-install");
        } else {
            // Update world with space-separated string
            world.insert("no-install".to_string(), no_install_vec.join(" "));
        }
    }

    /// Apply no-install changes from CLI to world.json
    /// Parses config.install.no_install cmdline string (e.g., "pkg1,pkg2,-pkg3")
    /// and updates world["no-install"] with space-separated package names
    pub fn apply_no_install_changes(&mut self) -> Result<()> {
        let config = crate::models::config();

        // If no cmdline string provided, nothing to do
        if config.install.no_install.is_empty() {
            return Ok(());
        }

        // Get current no-install list from world
        let mut no_install_set = self.get_no_install_set();

        // Parse cmdline string: "pkg1,pkg2,-pkg3" format
        for item in config.install.no_install.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }

            if item.starts_with('-') {
                // Remove from list: strip the leading -
                let pkg = item[1..].trim().to_string();
                if !pkg.is_empty() {
                    no_install_set.remove(&pkg);
                }
            } else {
                // Add to list
                no_install_set.insert(item.to_string());
            }
        }

        // Update world with the modified set
        self.update_no_install_in_world(no_install_set);

        Ok(())
    }

    /// Add essential packages to delta_world if not already in self.world
    pub fn add_essential_packages_to_delta_world(&mut self, delta_world: &mut HashMap<String, String>) -> Result<()> {
        let essential_pkgnames = crate::mmio::get_essential_pkgnames()?;
        let world = PACKAGE_CACHE.world.read().unwrap();
        for essential_pkgname in &essential_pkgnames {
            // Only add if not already in self.world or delta_world
            if !world.contains_key(essential_pkgname) && !delta_world.contains_key(essential_pkgname) {
                // Add with empty constraint string (no version constraint)
                delta_world.insert(essential_pkgname.clone(), String::new());
            }
        }
        Ok(())
    }

    /// Extract no-install list from world (space-separated string)
    pub fn get_no_install_set(&self) -> std::collections::HashSet<String> {
        PACKAGE_CACHE.world.read().unwrap()
            .get("no-install")
            .map(|s| {
                s.split_whitespace()
                    .map(|pkg| pkg.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

}
