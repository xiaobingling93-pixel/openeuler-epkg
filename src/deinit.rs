use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::exit;

use color_eyre::eyre::{self, WrapErr};
use color_eyre::Result;
#[cfg(unix)]
use nix::unistd;

use crate::dirs::*;
use crate::models::*;
#[cfg(unix)]
use crate::utils;
use crate::lfs;

#[derive(Debug)]
pub struct DeinitPlan {
    pub dirs_to_remove: Vec<PathBuf>,
    pub shell_rc_files: Vec<String>,
    pub symlinks_to_remove: Vec<PathBuf>,
}

impl DeinitPlan {
    pub fn new() -> Self {
        Self {
            dirs_to_remove: Vec::new(),
            shell_rc_files: Vec::new(),
            symlinks_to_remove: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.dirs_to_remove.is_empty() &&
        self.shell_rc_files.is_empty() &&
        self.symlinks_to_remove.is_empty()
    }
}

pub fn deinit_epkg(scope: &str) -> Result<()> {
    #[cfg(target_os = "linux")]
    crate::apparmor::remove_apparmor_profile()?;

    let plan = match scope {
        "personal" => collect_user_personal_plan()?,
        #[cfg(unix)]
        "global" => collect_global_deinit_plan()?,
        #[cfg(not(unix))]
        "global" => return Err(eyre::eyre!("Global deinitialization is not supported on this platform")),
        _ => return Err(eyre::eyre!("Invalid scope: {}. Must be 'personal' or 'global'", scope)),
    };

    execute_deinit_with_plan(plan, scope)
}

fn execute_deinit_with_plan(plan: DeinitPlan, scope: &str) -> Result<()> {
    if plan.is_empty() {
        return Ok(());
    }

    // Display plan and confirm
    display_deinit_plan(&plan, scope)?;
    if !confirm_deinit()? {
        println!("Deinitialization cancelled by user.");
        return Ok(());
    }

    // Execute the plan
    execute_deinit_plan(&plan)?;

    println!("Epkg deinitialization completed successfully.");
    println!("For changes to take effect, close and re-open your current shell.");
    Ok(())
}

#[cfg(unix)]
fn collect_global_deinit_plan() -> Result<DeinitPlan> {
    // We'll deinit every user! So check if running by root (effective UID)
    if !unistd::geteuid().is_root() {
        eprintln!("Global deinitialization requires root user.");
        exit(1);
    }

    let mut plan = DeinitPlan::new();
    let opt_epkg = dirs().opt_epkg.clone();

    if !lfs::exists_on_host(&opt_epkg) {
        println!("Global epkg directory {} does not exist.", opt_epkg.display());
        exit(1);
    }

    // Remove global /opt/epkg/
    plan.dirs_to_remove.push(opt_epkg);

    // Remove /usr/local/bin/epkg symlink
    let usr_local_bin_epkg = PathBuf::from("/usr/local/bin/epkg");
    if lfs::exists_on_host(&usr_local_bin_epkg) {
        plan.symlinks_to_remove.push(usr_local_bin_epkg);
    }

    // Update global shell rc files
    let global_shell_rcs = crate::dirs::get_global_shell_rc()?;
    plan.shell_rc_files.extend(global_shell_rcs);

    Ok(plan)
}

fn collect_user_personal_plan() -> Result<DeinitPlan> {
    let mut plan = DeinitPlan::new();

    if config().init.shared_store {
        // Remove /opt/epkg/envs/$USER/
        let user_public_envs_path = dirs().user_envs.clone();
        if lfs::exists_on_host(&user_public_envs_path) {
            plan.dirs_to_remove.push(user_public_envs_path);
        }

        // Remove /opt/epkg/cache/aur_builds/$USER/
        let user_aur_builds_path = dirs().user_aur_builds.clone();
        if lfs::exists_on_host(&user_aur_builds_path) {
            plan.dirs_to_remove.push(user_aur_builds_path);
        }
    } else {
        // Remove .epkg/
        let home_epkg = dirs().home_epkg.clone();
        if lfs::exists_on_host(&home_epkg) {
            plan.dirs_to_remove.push(home_epkg);
        }

        // Remove .cache/epkg/channels/
        let channels_cache_dir = dirs().epkg_channels_cache.clone();
        if lfs::exists_on_host(&channels_cache_dir) {
            plan.dirs_to_remove.push(channels_cache_dir);
        }

        // Preserve downloads cache, handy for development test cycles

        // Remove $HOME/bin/epkg symlink
        let home_dir = get_home()?;
        let home_bin_epkg =
            crate::dirs::path_join(&PathBuf::from(&home_dir), &["bin", "epkg"]);
        if lfs::exists_on_host(&home_bin_epkg) {
            plan.symlinks_to_remove.push(home_bin_epkg);
        }

        // Update user shell rc files
        let user_shell_rcs = crate::dirs::get_user_shell_rc(&PathBuf::from(&home_dir))?;
        plan.shell_rc_files.extend(user_shell_rcs);
    }

    Ok(plan)
}

fn display_deinit_plan(plan: &DeinitPlan, scope: &str) -> Result<()> {
    println!("\n=== Epkg Deinitialization Plan ({}) ===", scope);

    if !plan.dirs_to_remove.is_empty() {
        println!("\nDirectories to remove:");
        for dir in &plan.dirs_to_remove {
            println!("  {}", dir.display());
        }
    }

    if !plan.symlinks_to_remove.is_empty() {
        println!("\nSymlinks to remove:");
        for symlink in &plan.symlinks_to_remove {
            println!("  {}", symlink.display());
        }
    }

    if !plan.shell_rc_files.is_empty() {
        println!("\nShell configuration files to modify:");
        for rc_file in &plan.shell_rc_files {
            println!("  {}", rc_file);
        }
    }

    if plan.is_empty() {
        println!("No changes required.");
    }

    Ok(())
}

fn confirm_deinit() -> Result<bool> {
    if config().common.dry_run {
        println!("Dry run mode: No changes will be made to the system.");
        return Ok(false);
    }

    if config().common.assume_no {
        return Ok(false);
    }

    if config().common.assume_yes {
        return Ok(true);
    }

    print!("\nDo you want to continue with deinitialization? [y/N] ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let trimmed = input.trim().to_lowercase();
    Ok(trimmed == "y" || trimmed == "yes")
}

fn execute_deinit_plan(plan: &DeinitPlan) -> Result<()> {
    // Remove symlinks
    for symlink in &plan.symlinks_to_remove {
        if lfs::exists_on_host(symlink) {
            println!("Removing symlink: {}", symlink.display());
            lfs::remove_file(symlink)?;
        }
    }

    // Modify shell RC files
    for rc_file in &plan.shell_rc_files {
        remove_epkg_from_rc_file(rc_file)?;
    }

    // Remove directories in the end
    for dir in &plan.dirs_to_remove {
        if lfs::exists_on_host(dir) {
            println!("Removing directory: {}", dir.display());
            force_remove_dir_all(dir)
                .wrap_err_with(|| format!("Failed to remove directory: {}", dir.display()))?;
        }
    }

    Ok(())
}

pub fn remove_epkg_from_rc_file(rc_file_path: &str) -> Result<String> {
    let path = Path::new(rc_file_path);
    if !lfs::exists_on_host(path) {
        return Ok(String::new());
    }

    let content = fs::read_to_string(path)
        .wrap_err_with(|| format!("Failed to read RC file: {}", rc_file_path))?;

    // Check if epkg configuration is present
    if !content.contains("# epkg begin") || !content.contains("# epkg end") {
        return Ok(content);
    }

    // Remove epkg configuration block
    let lines: Vec<&str> = content.lines().collect();
    let mut new_lines = Vec::new();
    let mut in_epkg_block = false;

    for line in lines {
        if line.contains("# epkg begin") {
            in_epkg_block = true;
            continue;
        }
        if line.contains("# epkg end") {
            in_epkg_block = false;
            continue;
        }
        if !in_epkg_block {
            new_lines.push(line);
        }
    }

    let new_content = new_lines.join("\n");

    // Write back the modified content
    lfs::write(path, &new_content)?;

    println!("Removed epkg from shell RC file: {}", rc_file_path);
    Ok(new_content)
}

/// Recursively removes a directory, fixing permission issues if needed.
///
/// Uses eprintln! for informational messages to avoid interfering with shell eval
/// when called from commands like `epkg env remove`.
#[cfg(unix)]
pub fn force_remove_dir_all<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();

    // First, try normal deletion
    let initial_result = lfs::remove_dir_all(path);
    if initial_result.is_ok() {
        return Ok(());
    }

    // If failed, collect all parent directories of read-only files
    let parent_dirs = find_readonly_dirs(path)?;
    if parent_dirs.is_empty() {
        if let Err(ref e) = initial_result {
            eprintln!(
                "Initial attempt to remove directory '{}' failed: {}",
                path.display(),
                e
            );
        }
        return initial_result.map_err(|e| eyre::eyre!("{}", e));
    }

    eprintln!("Some directories are read-only and cannot be removed automatically.");
    eprintln!("Making {} directories writable...", parent_dirs.len());

    // Make parent directories writable
    for dir in &parent_dirs {
        eprintln!("  - {}", &dir.display());
        utils::fixup_file_permissions(&dir);
    }

    eprintln!("Retrying directory removal after permission fix...");

    // Retry deletion
    match lfs::remove_dir_all(path) {
        Ok(_) => {
            eprintln!("Directory successfully removed after permission fix");
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to remove directory even after permission fix: {}", e);
            Err(eyre::eyre!("{}", e))
        }
    }
}

/// Simple version for non-Unix platforms (Windows)
#[cfg(not(unix))]
pub fn force_remove_dir_all<P: AsRef<Path>>(path: P) -> Result<()> {
    lfs::remove_dir_all(path.as_ref())
        .map_err(|e| eyre::eyre!("Failed to remove directory: {}", e))
}

/// Finds all read-only directories within the given path
#[cfg(unix)]
pub fn find_readonly_dirs<P: AsRef<Path>>(root: P) -> Result<Vec<PathBuf>> {
    let mut readonly_dirs = Vec::new();
    let mut dir_stack = vec![root.as_ref().to_path_buf()];

    while let Some(dir) = dir_stack.pop() {
        // Check if current directory is read-only
        if let Ok(metadata) = lfs::metadata_on_host(&dir) {
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
