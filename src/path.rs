use std::fs;
use std::env;
use anyhow::Result;
use crate::paths::instance;
use crate::models::*;

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

            // Add system paths, excluding epkg paths
            path_components.extend(self.get_system_paths()?);
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
        let paths = &*instance;
        let mut path_components = Vec::new();

        let env_root = paths.epkg_envs_root.join(active_env).join("profile-current");

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
        let paths = &*instance;
        let mut path_components = Vec::new();

        let registered_envs_dir = paths.epkg_config_dir.join("registered-envs");
        if let Ok(entries) = fs::read_dir(&registered_envs_dir) {
            // Collect entries with their modification times
            let mut env_entries: Vec<(String, std::time::SystemTime)> = entries
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    let env_name = entry.file_name().into_string().ok()?;
                    let metadata = entry.metadata().ok()?;
                    let modified = metadata.modified().ok()?;
                    Some((env_name, modified))
                })
                .collect();

            // Sort by modification time, newest first
            env_entries.sort_by(|a, b| b.1.cmp(&a.1));

            // Add paths in time order
            for (env_name, _) in env_entries {
                let env_path = paths.epkg_envs_root.join(&env_name)
                    .join("profile-current/usr/ebin");

                if env_path.exists() {
                    path_components.push(env_path.display().to_string());
                }
            }
        }

        Ok(path_components)
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
