use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use crate::models::*;
use crate::dirs::{*, user_private_envs, user_public_envs};
use crate::deinit;
use crate::io;
use crate::utils;

#[derive(Debug)]
pub struct GcPlan {
    pub old_downloads: Option<Vec<PathBuf>>,
    pub unused_channels: Vec<PathBuf>,
    pub unused_packages: Vec<PathBuf>,
    pub old_unpack_dirs: Vec<PathBuf>,
}

impl GcPlan {
    pub fn new() -> Self {
        Self {
            old_downloads: None,
            unused_channels: Vec::new(),
            unused_packages: Vec::new(),
            old_unpack_dirs: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.old_downloads.as_ref().map_or(true, |v| v.is_empty()) &&
        self.unused_channels.is_empty() &&
        self.unused_packages.is_empty() &&
        self.old_unpack_dirs.is_empty()
    }

    pub fn total_size(&self) -> u64 {
        let mut total = 0u64;

        if let Some(ref downloads) = self.old_downloads {
            for path in downloads {
                if let Ok(metadata) = fs::metadata(path) {
                    total += metadata.len();
                }
            }
        }

        for path in &self.unused_channels {
            total += get_dir_size(path).unwrap_or(0);
        }

        for path in &self.unused_packages {
            total += get_dir_size(path).unwrap_or(0);
        }

        for path in &self.old_unpack_dirs {
            total += get_dir_size(path).unwrap_or(0);
        }

        total
    }
}

pub fn gc_epkg(old_downloads_days: Option<u64>) -> Result<()> {
    let mut plan = GcPlan::new();

    if let Some(days) = old_downloads_days {
        // User explicitly provided --old-downloads flag
        plan.old_downloads = Some(collect_old_downloads(days)?);
    } else {
        // User did not provide --old-downloads flag, do general cleanup
        let (in_use_channels, in_use_packages) = collect_in_use_resources()?;

        plan.unused_channels = collect_unused_channels(&in_use_channels)?;
        plan.unused_packages = collect_unused_packages(&in_use_packages)?;
        plan.old_unpack_dirs = collect_old_unpack_dirs()?;
    }

    if plan.is_empty() {
        println!("No cleanup required.");
        return Ok(());
    }

    // Display plan and confirm
    display_gc_plan(&plan, old_downloads_days.is_some())?;

    if !utils::user_prompt_and_confirm()? {
        println!("Garbage collection cancelled by user.");
        return Ok(());
    }

    // Execute the plan
    execute_gc_plan(&plan)?;

    println!("Garbage collection completed successfully.");
    Ok(())
}

fn collect_old_downloads(days: u64) -> Result<Vec<PathBuf>> {
    let downloads_dir = dirs().epkg_downloads_cache.clone();
    if !downloads_dir.exists() {
        return Ok(Vec::new());
    }

    let cutoff_time = if days == 0 {
        SystemTime::now() // Remove all files
    } else {
        SystemTime::now() - Duration::from_secs(days * 24 * 60 * 60)
    };

    let mut old_files = Vec::new();

    if let Ok(entries) = fs::read_dir(&downloads_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Ok(metadata) = fs::metadata(&path) {
                    if let Ok(modified_time) = metadata.modified() {
                        if modified_time < cutoff_time {
                            old_files.push(path);
                        }
                    }
                }
            }
        }
    }

    Ok(old_files)
}

fn collect_in_use_resources() -> Result<(HashSet<String>, HashSet<String>)> {
    let mut in_use_channels = HashSet::new();
    let mut in_use_packages = HashSet::new();

    if config().init.shared_store {
        // Collect from all users
        let all_users = deinit::get_all_users()?;
        for (username, home_dir) in all_users {
            collect_user_envs_channels(&username, &home_dir.to_string_lossy(), &mut in_use_channels, &mut in_use_packages)?;
        }
    } else {
        // Collect from current user only
        let username = get_username()?;
        let home_dir = get_home()?;
        collect_user_envs_channels(&username, &home_dir, &mut in_use_channels, &mut in_use_packages)?;
    }

    if config().common.verbose {
        println!("In-use channels: {:?}", in_use_channels);
        println!("In-use packages: {:#?}", in_use_packages);
    }

    Ok((in_use_channels, in_use_packages))
}

fn collect_user_envs_channels(
    username: &str,
    home_dir: &str,
    in_use_channels: &mut HashSet<String>,
    in_use_packages: &mut HashSet<String>
) -> Result<()> {
    // Collect from private environments
    let private_envs_root = user_private_envs(home_dir);
    collect_envs_from_directory(&private_envs_root, in_use_channels, in_use_packages)?;

    // Collect from public environments
    let public_envs_root = user_public_envs(username);
    collect_envs_from_directory(&public_envs_root, in_use_channels, in_use_packages)?;

    Ok(())
}

fn collect_envs_from_directory(
    envs_root: &Path,
    in_use_channels: &mut HashSet<String>,
    in_use_packages: &mut HashSet<String>
) -> Result<()> {
    if !envs_root.exists() {
        return Ok(());
    }

    if let Ok(entries) = fs::read_dir(envs_root) {
        for entry in entries.flatten() {
            let env_path = entry.path();
            if env_path.is_dir() {
                collect_env_configs(&env_path, in_use_channels, in_use_packages)?;
            }
        }
    }

    Ok(())
}

fn collect_env_configs(
    env_path: &Path,
    in_use_channels: &mut HashSet<String>,
    in_use_packages: &mut HashSet<String>
) -> Result<()> {
    // Load channel config
    let channel_config_path = env_path.join("etc/epkg/channel.yaml");
    if channel_config_path.exists() {
        if let Ok((mut channel_config, _)) = io::read_yaml_file::<ChannelConfig>(&channel_config_path) {
            // Set defaults to populate channel field from distro:version
            if let Ok(()) = crate::io::set_channel_config_defaults(&mut channel_config) {
                if config().common.verbose {
                    println!("Found channel config: {} -> {}", env_path.display(), channel_config.channel);
                }
                in_use_channels.insert(channel_config.channel);
            }
        }
    }

    // Load env config
    let env_config_path = env_path.join("etc/epkg/env.yaml");
    if env_config_path.exists() {
        if let Ok((_env_config, _)) = io::read_yaml_file::<EnvConfig>(&env_config_path) {
            // Load installed packages
            let packages_path = env_path.join("generations/current/installed-packages.json");
            if packages_path.exists() {
                if let Ok(contents) = fs::read_to_string(&packages_path) {
                    if let Ok(packages) = serde_json::from_str::<HashMap<String, InstalledPackageInfo>>(&contents) {
                        for (_pkgkey, pkg_info) in &packages {
                            in_use_packages.insert(pkg_info.pkgline.clone());
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn collect_unused_channels(in_use_channels: &HashSet<String>) -> Result<Vec<PathBuf>> {
    let channel_cache_dir = dirs().epkg_channel_cache.clone();
    if !channel_cache_dir.exists() {
        return Ok(Vec::new());
    }

    let mut unused_channels = Vec::new();
    let mut all_channels = Vec::new();

    if let Ok(entries) = fs::read_dir(&channel_cache_dir) {
        for entry in entries.flatten() {
            let channel_path = entry.path();
            if channel_path.is_dir() {
                let channel_name = channel_path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("");

                all_channels.push(channel_name.to_string());

                if !in_use_channels.contains(channel_name) {
                    unused_channels.push(channel_path);
                }
            }
        }
    }

    if config().common.verbose {
        println!("All channel directories: {:?}", all_channels);
        println!("Unused channel directories: {}", unused_channels.len());
    }

    Ok(unused_channels)
}

fn collect_unused_packages(in_use_packages: &HashSet<String>) -> Result<Vec<PathBuf>> {
    let store_dir = dirs().epkg_store.clone();
    if !store_dir.exists() {
        return Ok(Vec::new());
    }

    let mut unused_packages = Vec::new();

    if let Ok(entries) = fs::read_dir(&store_dir) {
        for entry in entries.flatten() {
            let package_path = entry.path();
            if package_path.is_dir() {
                let package_name = package_path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("");

                if !in_use_packages.contains(package_name) {
                    unused_packages.push(package_path);
                }
            }
        }
    }

    Ok(unused_packages)
}

fn collect_old_unpack_dirs() -> Result<Vec<PathBuf>> {
    let unpack_dir = dirs().epkg_cache.join("unpack");
    if !unpack_dir.exists() {
        return Ok(Vec::new());
    }

    let mut old_unpack_dirs = Vec::new();

    // Only remove directories older than 1 hour to avoid race conditions
    let cutoff_time = SystemTime::now() - Duration::from_secs(3600); // 1 hour

    if let Ok(entries) = fs::read_dir(&unpack_dir) {
        for entry in entries.flatten() {
            let unpack_path = entry.path();
            if unpack_path.is_dir() {
                // Check if directory is older than 1 hour
                if let Ok(metadata) = fs::metadata(&unpack_path) {
                    if let Ok(modified_time) = metadata.modified() {
                        if modified_time < cutoff_time {
                            old_unpack_dirs.push(unpack_path);
                        }
                    }
                }
            }
        }
    }

    Ok(old_unpack_dirs)
}

fn display_gc_plan(plan: &GcPlan, is_old_downloads: bool) -> Result<()> {
    println!("\n=== Epkg Garbage Collection Plan ===");

    if is_old_downloads {
        if let Some(ref downloads) = plan.old_downloads {
            if !downloads.is_empty() {
                println!("\nOld download files to remove:");
                let total_size = downloads.iter()
                    .map(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0))
                    .sum::<u64>();
                println!("  Directory: {}", dirs().epkg_downloads_cache.display());
                println!("  Files: {}", downloads.len());
                println!("  Total size: {}", utils::format_size(total_size));

                if config().common.verbose {
                    for file in downloads.iter().take(10) {
                        println!("    {}", file.display());
                    }
                    if downloads.len() > 10 {
                        println!("    ... and {} more files", downloads.len() - 10);
                    }
                }
            } else {
                println!("\nNo old download files found.");
            }
        }
    } else {
        if !plan.unused_channels.is_empty() {
            println!("\nUnused directories to remove under: {}", dirs().epkg_channel_cache.display());
            let total_size = plan.unused_channels.iter()
                .map(|p| get_dir_size(p).unwrap_or(0))
                .sum::<u64>();
            let mut unused_channel_names: Vec<String> = plan.unused_channels
                .iter()
                .map(|dir| dir.file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| dir.display().to_string()))
                .collect();
            unused_channel_names.sort();
            println!(
                "  Directories: {} [{}]",
                plan.unused_channels.len(),
                unused_channel_names.join(" ")
            );
            println!("  Total size: {}", utils::format_size(total_size));

            if config().common.verbose {
                for dir in &plan.unused_channels {
                    println!("    {}", dir.display());
                }
            }
        }

        if !plan.unused_packages.is_empty() {
            println!("\nUnused directories to remove under: {}", dirs().epkg_store.display());
            let total_size = plan.unused_packages.iter()
                .map(|p| get_dir_size(p).unwrap_or(0))
                .sum::<u64>();
            println!("  Directories: {}", plan.unused_packages.len());
            println!("  Total size: {}", utils::format_size(total_size));

            if config().common.verbose {
                for dir in &plan.unused_packages {
                    println!("    {}", dir.display());
                }
            }
        }

        if !plan.old_unpack_dirs.is_empty() {
            println!("\nUnused directories to remove under: {}", dirs().epkg_cache.join("unpack").display());
            let total_size = plan.old_unpack_dirs.iter()
                .map(|p| get_dir_size(p).unwrap_or(0))
                .sum::<u64>();
            println!("  Directories: {}", plan.old_unpack_dirs.len());
            println!("  Total size: {}", utils::format_size(total_size));

            if config().common.verbose {
                for dir in &plan.old_unpack_dirs {
                    println!("    {}", dir.display());
                }
            }
        }
    }

    if plan.is_empty() {
        println!("No cleanup required.");
    } else {
        println!("\nTotal space to be freed: {}", utils::format_size(plan.total_size()));
    }

    Ok(())
}

fn execute_gc_plan(plan: &GcPlan) -> Result<()> {
    // Remove old downloads
    if let Some(ref downloads) = plan.old_downloads {
        for file in downloads {
            if file.exists() {
                println!("Removing old download file: {}", file.display());
                fs::remove_file(file)
                    .wrap_err_with(|| format!("Failed to remove file: {}", file.display()))?;
            }
        }
    }

    // Remove unused channels
    for dir in &plan.unused_channels {
        if dir.exists() {
            println!("Removing unused channel directory: {}", dir.display());
            force_remove_dir_all(dir)
                .wrap_err_with(|| format!("Failed to remove directory: {}", dir.display()))?;
        }
    }

    // Remove unused packages
    for dir in &plan.unused_packages {
        if dir.exists() {
            println!("Removing unused package directory: {}", dir.display());
            force_remove_dir_all(dir)
                .wrap_err_with(|| format!("Failed to remove directory: {}", dir.display()))?;
        }
    }

    // Remove unused unpack directories
    for dir in &plan.old_unpack_dirs {
        if dir.exists() {
            println!("Removing unused unpack directory: {}", dir.display());
            force_remove_dir_all(dir)
                .wrap_err_with(|| format!("Failed to remove directory: {}", dir.display()))?;
        }
    }

    Ok(())
}

fn get_dir_size(path: &Path) -> Result<u64> {
    let mut total_size = 0u64;

    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            // Use symlink_metadata to avoid following symlinks
            if let Ok(metadata) = fs::symlink_metadata(&entry_path) {
                if metadata.is_file() {
                    total_size += metadata.len();
                } else if metadata.is_dir() {
                    // Don't follow symlinks to avoid counting the same files multiple times
                    if !metadata.file_type().is_symlink() {
                        total_size += get_dir_size(&entry_path).unwrap_or(0);
                    }
                }
            }
        }
    }

    Ok(total_size)
}



/// Recursively removes a directory, fixing permission issues if needed.
fn force_remove_dir_all<P: AsRef<Path>>(path: P) -> Result<(), std::io::Error> {
    let path = path.as_ref();

    // First, try normal deletion
    let initial_result = fs::remove_dir_all(path);
    if initial_result.is_ok() {
        return Ok(());
    }

    // If failed, collect all parent directories of read-only files
    let parent_dirs = find_readonly_dirs(path)?;
    if parent_dirs.is_empty() {
        if let Err(ref e) = initial_result {
            println!(
                "Initial attempt to remove directory '{}' failed: {}",
                path.display(),
                e
            );
        }
        return initial_result;
    }

    println!("Some directories are read-only and cannot be removed automatically.");
    println!("Making {} directories writable...", parent_dirs.len());

    // Make parent directories writable
    for dir in &parent_dirs {
        let mut perms = fs::metadata(&dir)?.permissions();
        perms.set_readonly(false); // Make writable
        println!("  - {}", &dir.display());
        fs::set_permissions(&dir, perms)?;
    }

    println!("Retrying directory removal after permission fix...");

    // Retry deletion
    match fs::remove_dir_all(path) {
        Ok(_) => {
            println!("Directory successfully removed after permission fix");
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to remove directory even after permission fix: {}", e);
            Err(e)
        }
    }
}

/// Finds all read-only directories within the given path
fn find_readonly_dirs<P: AsRef<Path>>(root: P) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut readonly_dirs = Vec::new();
    let mut dir_stack = vec![root.as_ref().to_path_buf()];

    while let Some(dir) = dir_stack.pop() {
        // Check if current directory is read-only
        if let Ok(metadata) = fs::metadata(&dir) {
            if metadata.permissions().readonly() {
                readonly_dirs.push(dir.clone());
            }
        }

        // Add subdirectories to stack
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    dir_stack.push(entry.path());
                }
            }
        }
    }

    // Remove duplicates and sort for consistent output
    readonly_dirs.sort();
    readonly_dirs.dedup();
    Ok(readonly_dirs)
}
