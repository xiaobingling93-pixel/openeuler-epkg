use std::env;
use std::path::Path;
use color_eyre::Result;
use color_eyre::eyre;
use crate::models::*;
use crate::dirs::get_env_root;
use crate::environment::registered_env_configs;

// Construct PATH from active and registered environments.
// Order (from left/earliest to right/latest in PATH):
// 1) Active envs (most recently activated first)
// 2) Registered envs with path-order >= 0 (ascending path-order)
// 3) Original/system PATH entries (with existing epkg bits removed)
// 4) Registered envs with path-order < 0 (ascending abs(path-order))
pub fn update_path() -> Result<()> {
    let mut path_components = Vec::new();
    let mut pure = false;

    // Add active environment paths (last activated first)
    if let Ok(active_env) = env::var("EPKG_ACTIVE_ENV") {
        let active_envs: Vec<&str> = active_env.split(':').collect();
        for env_name in active_envs.iter() {
            let (env_name, is_pure) = if env_name.ends_with(PURE_ENV_SUFFIX) {
                (env_name[..env_name.len()-1].to_string(), true)
            } else {
                (env_name.to_string(), false)
            };
            pure = pure && is_pure;
            path_components.extend(get_active_env_paths(&env_name, is_pure)?);
        }

    }

    if !pure {
        // Add registered environment paths in time order
        path_components.extend(get_registered_env_paths()?);
    }

    // Remove duplicates while preserving order
    let mut seen = std::collections::HashSet::new();
    path_components.retain(|item| seen.insert(item.clone()));

    // Validate we have at least one path
    if path_components.is_empty() {
        return Err(eyre::eyre!("No valid paths found to update PATH"));
    }

    // Join paths with colons
    let new_path = path_components.join(":");

    // Update PATH
    env::set_var("PATH", &new_path);
    println!("export PATH=\"{}\"", &new_path);

    Ok(())
}

fn get_active_env_paths(active_env: &str, pure: bool) -> Result<Vec<String>> {
    let mut path_components = Vec::new();

    let env_root = get_env_root(active_env.to_string())?;

    // Validate environment exists
    if !env_root.exists() {
        return Err(eyre::eyre!("Active environment '{}' does not exist", active_env));
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

fn get_registered_env_paths() -> Result<Vec<String>> {
    let mut path_components = Vec::new();

    let mut prepend: Vec<(i32, String, String)> = Vec::new();
    let mut append: Vec<(i32, String, String)> = Vec::new();

    for config in registered_env_configs() {
        let ebin_path = Path::new(&config.env_root).join("usr/ebin");
        if !ebin_path.exists() {
            continue;
        }

        let path_str = ebin_path.display().to_string();
        let entry = (config.register_path_order, config.name.clone(), path_str);

        if config.register_path_order >= 0 {
            prepend.push(entry);
        } else {
            append.push(entry);
        }
    }

    prepend.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    append.sort_by(|a, b| a.0.abs().cmp(&b.0.abs()).then(a.1.cmp(&b.1)));

    path_components.extend(prepend.into_iter().map(|(_, _, path)| path));

    // Get system paths, excluding epkg paths
    path_components.extend(get_system_paths()?);

    path_components.extend(append.into_iter().map(|(_, _, path)| path));

    Ok(path_components)
}

fn get_system_paths() -> Result<Vec<String>> {
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
