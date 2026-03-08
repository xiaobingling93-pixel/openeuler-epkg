use std::collections::{HashMap, HashSet};
use std::fs;
use crate::lfs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use crate::models::*;
use crate::dirs::*;
use crate::io;
use crate::utils;
use crate::deinit::force_remove_dir_all;

#[derive(Debug)]
pub struct GcPlan {
    pub old_downloads: Option<Vec<PathBuf>>,
    pub unused_channels: Vec<PathBuf>,
    pub unused_packages: Vec<PathBuf>,
    pub old_unpack_dirs: Vec<PathBuf>,
    pub old_aur_build_dirs: Vec<PathBuf>,
}

impl GcPlan {
    pub fn new() -> Self {
        Self {
            old_downloads: None,
            unused_channels: Vec::new(),
            unused_packages: Vec::new(),
            old_unpack_dirs: Vec::new(),
            old_aur_build_dirs: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.old_downloads.as_ref().map_or(true, |v| v.is_empty()) &&
        self.unused_channels.is_empty() &&
        self.unused_packages.is_empty() &&
        self.old_unpack_dirs.is_empty() &&
        self.old_aur_build_dirs.is_empty()
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

        for path in &self.old_aur_build_dirs {
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
        // Control scope based on whether running as root
        let (in_use_channels, in_use_packages) = collect_in_use_resources()?;

        plan.unused_channels = collect_unused_channels(&in_use_channels)?;
        plan.unused_packages = collect_unused_packages(&in_use_packages)?;
        plan.old_unpack_dirs = collect_old_unpack_dirs()?;
        plan.old_aur_build_dirs = collect_old_aur_build_dirs()?;
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
    if !lfs::exists_on_host(&downloads_dir) {
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

    // Walk environments based on shared_store setting
    walk_environments(|env_path, _owner| {
        collect_env_configs(env_path, &mut in_use_channels, &mut in_use_packages)
    })?;

    if config().common.verbose {
        println!("In-use channels: {:?}", in_use_channels);
        println!("In-use packages: {:#?}", in_use_packages);
    }

    Ok((in_use_channels, in_use_packages))
}

fn collect_env_configs(
    env_path: &Path,
    in_use_channels: &mut HashSet<String>,
    in_use_packages: &mut HashSet<String>
) -> Result<()> {
    // Load channel config
    let channel_config_path = env_path.join("etc/epkg/channel.yaml");
    if lfs::exists_in_env(&channel_config_path) {
        if let Ok(mut channel_config) = io::read_yaml_file::<ChannelConfig>(&channel_config_path) {
            // Set defaults to populate channel field from distro:version
            if let Ok(()) = crate::io::set_channel_config_defaults(&mut channel_config, None) {
                if config().common.verbose {
                    println!("Found channel config: {} -> {}", env_path.display(), channel_config.channel);
                }
                in_use_channels.insert(channel_config.channel);
            }
        }
    }

    // Load env config
    let env_config_path = env_path.join("etc/epkg/env.yaml");
    if lfs::exists_in_env(&env_config_path) {
        if io::read_yaml_file::<EnvConfig>(&env_config_path).is_ok() {
            // Load installed packages
            let packages_path = env_path.join("generations/current/installed-packages.json");
            if lfs::exists_in_env(&packages_path) {
                if let Ok(packages_raw) = crate::io::read_json_file::<HashMap<String, InstalledPackageInfo>>(&packages_path) {
                    let packages: InstalledPackagesMap = packages_raw.into_iter().map(|(k, v)| (k, std::sync::Arc::new(v))).collect();
                    for (_pkgkey, pkg_info) in &packages {
                        in_use_packages.insert(pkg_info.pkgline.clone());
                    }
                }
            }
        }
    }

    Ok(())
}

fn collect_unused_channels(in_use_channels: &HashSet<String>) -> Result<Vec<PathBuf>> {
    let channels_cache_dir = dirs().epkg_channels_cache.clone();
    if !channels_cache_dir.exists() {
        return Ok(Vec::new());
    }

    let mut unused_channels = Vec::new();
    let mut all_channels = Vec::new();

    // Walk the bottom directory level using the shared helper so we have
    // consistent behavior with other directory walkers.
    walk_bottom_dir(&channels_cache_dir, None, &mut |channel_path, _owner| {
        let channel_name = channel_path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");

        all_channels.push(channel_name.to_string());

        if !in_use_channels.contains(channel_name) {
            unused_channels.push(channel_path.to_path_buf());
        }

        Ok(())
    })?;

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

    // Reuse the generic bottom-directory walker for consistency.
    walk_bottom_dir(&store_dir, None, &mut |package_path, _owner| {
        let package_name = package_path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");

        if !in_use_packages.contains(package_name) {
            unused_packages.push(package_path.to_path_buf());
        }

        Ok(())
    })?;

    Ok(unused_packages)
}

/// Common helper to collect old files/directories from a given directory
/// that are older than the specified age threshold
fn collect_old_items_in_dir(dir: &Path, min_age_seconds: u64) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut old_items = Vec::new();
    let cutoff_time = SystemTime::now() - Duration::from_secs(min_age_seconds);

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let item_path = entry.path();
            // Check both files and directories
            if let Ok(metadata) = fs::metadata(&item_path) {
                if let Ok(modified_time) = metadata.modified() {
                    if modified_time < cutoff_time {
                        old_items.push(item_path);
                    }
                }
            }
        }
    }

    Ok(old_items)
}

fn collect_old_unpack_dirs() -> Result<Vec<PathBuf>> {
    let unpack_dir = crate::dirs::unpack_basedir();
    // Only remove directories older than 1 hour to avoid race conditions
    collect_old_items_in_dir(&unpack_dir, 3600)
}

fn collect_old_aur_build_dirs() -> Result<Vec<PathBuf>> {
    let aur_builds_dir = dirs().user_aur_builds.clone();
    // Only remove directories/files older than 1 day
    collect_old_items_in_dir(&aur_builds_dir, 3600 * 24)
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
            println!("\nUnused directories to remove under: {}", dirs().epkg_channels_cache.display());
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
            println!("\nUnused directories to remove under: {}", crate::dirs::unpack_basedir().display());
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

        if !plan.old_aur_build_dirs.is_empty() {
            println!("\nStale files/directories to remove under: {}", dirs().user_aur_builds.display());
            let total_size = plan.old_aur_build_dirs.iter()
                .map(|p| get_dir_size(p).unwrap_or(0))
                .sum::<u64>();
            println!("  Items: {}", plan.old_aur_build_dirs.len());
            println!("  Total size: {}", utils::format_size(total_size));

            if config().common.verbose {
                for item in &plan.old_aur_build_dirs {
                    println!("    {}", item.display());
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
                lfs::remove_file(file)?;
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

    // Remove stale AUR build files/directories
    for item in &plan.old_aur_build_dirs {
        if item.exists() {
            if item.is_dir() {
                println!("Removing stale AUR build directory: {}", item.display());
                force_remove_dir_all(item)
                    .wrap_err_with(|| format!("Failed to remove directory: {}", item.display()))?;
            } else {
                println!("Removing stale AUR build log: {}", item.display());
                lfs::remove_file(item)?;
            }
        }
    }

    Ok(())
}

fn get_dir_size(path: &Path) -> Result<u64> {
    // First, handle the case where `path` itself is a file (or symlink to a file)
    if let Ok(metadata) = fs::symlink_metadata(path) {
        // If it's a file, return its size directly
        if metadata.is_file() {
            return Ok(metadata.len());
        }

        // If it's a symlink to a directory or something else, don't follow it; treat as size 0
        if metadata.file_type().is_symlink() && !metadata.is_dir() {
            return Ok(0);
        }
    }

    // Otherwise, treat `path` as a directory and sum sizes of contained regular files.
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

