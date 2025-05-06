use std::fs;
use std::env;
use anyhow::Result;
use crate::models::*;
use std::path::Path;

impl PackageManager {

    pub fn update_path(&self, pure: bool) -> Result<()> {
        let mut path_components = Vec::new();

        // Add active environment paths
        if let Ok(active_env) = env::var("EPKG_ACTIVE_ENV") {
            path_components.extend(self.get_active_env_paths(&active_env, pure)?);
        }

        if !pure {
            // Add registered environment paths in time order
            path_components.extend(self.get_registered_env_paths()?);
        }

        // Remove duplicates while preserving order
        let mut seen = std::collections::HashSet::new();
        path_components.retain(|item| seen.insert(item.clone()));

        // Validate we have at least one path
        if path_components.is_empty() {
            return Err(anyhow::anyhow!("No valid paths found to update PATH"));
        }

        // Join paths with colons
        let new_path = path_components.join(":");

        // Update PATH
        env::set_var("PATH", &new_path);
        println!("export PATH={}", &new_path);

        Ok(())
    }

    fn get_active_env_paths(&self, active_env: &str, pure: bool) -> Result<Vec<String>> {
        let mut path_components = Vec::new();

        // Use get_env_root instead of directly accessing private_envs
        let env_root = self.get_env_root(active_env.to_string())?;

        // Validate environment exists
        if !env_root.exists() {
            return Err(anyhow::anyhow!("Active environment '{}' does not exist", active_env));
        }

        // Add ebin path
        let ebin_path = env_root.join("usr/ebin");
        if ebin_path.exists() {
            path_components.push(ebin_path.display().to_string());
        }

        // In pure mode, add bin and sbin paths
        if pure {
            let bin_path = env_root.join("usr/bin");
            let sbin_path = env_root.join("usr/sbin");

            if bin_path.exists() {
                path_components.push(bin_path.display().to_string());
            }
            if sbin_path.exists() {
                path_components.push(sbin_path.display().to_string());
            }
        }

        Ok(path_components)
    }

    fn get_registered_env_paths(&self) -> Result<Vec<String>> {
        let mut path_components = Vec::new();

        // Get paths from prepend directory (main environment)
        let prepend_dir = self.dirs.home_config.join("path.d/prepend");
        path_components.extend(self.get_priority_sorted_paths(&prepend_dir)?);

        // Get system paths, excluding epkg paths
        path_components.extend(self.get_system_paths()?);

        // Get paths from append directory (other environments)
        let append_dir = self.dirs.home_config.join("path.d/append");
        path_components.extend(self.get_priority_sorted_paths(&append_dir)?);

        Ok(path_components)
    }

    fn get_priority_sorted_paths(&self, dir: &std::path::Path) -> Result<Vec<String>> {
        let mut entries = Vec::new();

        if let Ok(read_dir) = fs::read_dir(dir) {
            // Collect entries with their priorities
            let mut priority_entries: Vec<(i32, String)> = read_dir
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    let name = entry.file_name().into_string().ok()?;
                    // Extract priority from filename (e.g. "10-main" -> 10)
                    let priority = name.split('-').next()?.parse::<i32>().ok()?;
                    if let Ok(target) = fs::read_link(entry.path()) {
                        Some((priority, target.display().to_string()))
                    } else {
                        None
                    }
                })
                .collect();

            // Sort by priority in ascending order (lower numbers first)
            priority_entries.sort_by_key(|&(priority, _)| priority);
            entries.extend(priority_entries.into_iter().map(|(_, path)| path));
        }

        Ok(entries)
    }

    fn get_system_paths(&self) -> Result<Vec<String>> {
        let mut path_components = Vec::new();

        if let Ok(path) = env::var("PATH") {
            path_components.extend(
                path.split(':')
                    .filter(|dir| !dir.contains("epkg"))
                    .map(String::from)
            );
        }

        Ok(path_components)
    }

}
