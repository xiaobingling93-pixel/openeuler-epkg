use std::fs;
use std::io::Error;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::exit;
use nix::unistd;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use crate::models::*;
use crate::dirs::*;

#[derive(Debug)]
pub struct DeinitPlan {
    pub dirs_to_remove: Vec<PathBuf>,
    pub shell_rc_files: Vec<ShellRcInfo>,
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
        self.dirs_to_remove.is_empty() && self.shell_rc_files.is_empty() && self.symlinks_to_remove.is_empty()
    }
}

pub fn deinit_epkg(scope: &str) -> Result<()> {
    // Validate scope and permissions
    match scope {
        "personal" => deinit_personal()?,
        "global" => deinit_global()?,
        _ => return Err(eyre::eyre!("Invalid scope: {}. Must be 'personal' or 'global'", scope)),
    }
    Ok(())
}

fn deinit_personal() -> Result<()> {
    let home_dir = get_home()?;
    let mut plan = collect_user_personal_plan(&PathBuf::from(&home_dir))?;

    // Add $HOME/bin/epkg symlink to removal list
    let home_bin_epkg = PathBuf::from(&home_dir).join("bin/epkg");
    if home_bin_epkg.exists() {
        plan.symlinks_to_remove.push(home_bin_epkg);
    }

    execute_deinit_with_plan(plan, "personal", "No epkg installation found for current user.")
}

fn deinit_global() -> Result<()> {
    // We'll deinit every user! So check if running as real root
    if !unistd::getuid().is_root() {
        eprintln!("Global deinitialization requires root user.");
        exit(1);
    }

    let mut plan = collect_global_deinit_plan()?;

    // Add /usr/local/bin/epkg symlink to removal list
    let usr_local_bin_epkg = PathBuf::from("/usr/local/bin/epkg");
    if usr_local_bin_epkg.exists() {
        plan.symlinks_to_remove.push(usr_local_bin_epkg);
    }

    execute_deinit_with_plan(plan, "global", "No global epkg installation found.")
}

fn execute_deinit_with_plan(plan: DeinitPlan, scope: &str, empty_message: &str) -> Result<()> {
    if plan.is_empty() {
        println!("{}", empty_message);
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

fn collect_global_deinit_plan() -> Result<DeinitPlan> {
    let mut plan = DeinitPlan::new();
    let opt_epkg = dirs().opt_epkg.clone();

    if !opt_epkg.exists() {
        println!("Global epkg directory {} does not exist.", opt_epkg.display());
        return Ok(plan);
    }

    // Get all users and clean their personal epkg directories
    let all_users = get_all_users()?;
    for (_username, home_dir) in all_users {
        let user_plan = collect_user_personal_plan(&home_dir)?;
        plan.dirs_to_remove.extend(user_plan.dirs_to_remove);
        plan.shell_rc_files.extend(user_plan.shell_rc_files);
    }

    plan.dirs_to_remove.push(opt_epkg);

    Ok(plan)
}

fn collect_user_personal_plan(home_dir: &Path) -> Result<DeinitPlan> {
    let mut plan = DeinitPlan::new();
    let home_epkg = home_dir.join(".epkg");

    // Add cache directory (but preserve downloads)
    let cache_dir = home_dir.join(".cache/epkg");
    if cache_dir.exists() {
        let channel_dir = cache_dir.join("channel");
        if channel_dir.exists() {
            plan.dirs_to_remove.push(channel_dir);
        }
    }

    if home_epkg.exists() {
        plan.dirs_to_remove.push(home_epkg);
    }

    // Get shell RC files for this user
    let user_shell_rcs = get_user_shell_rc(home_dir)?;
    plan.shell_rc_files.extend(user_shell_rcs);

    Ok(plan)
}

fn get_all_users() -> Result<Vec<(String, PathBuf)>> {
    let mut users = Vec::new();

    // Try to get all users from /etc/passwd
    if let Ok(content) = fs::read_to_string("/etc/passwd") {
        for line in content.lines() {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 6 {
                let username = parts[0];
                let uid: u32 = parts[2].parse().unwrap_or(0);
                let home_dir = parts[5];

                // Skip system users (UID < 1000) and special users
                if uid >= 1000 && username != "nobody" && !home_dir.is_empty() {
                    let home_path = PathBuf::from(home_dir);
                    if home_path.exists() {
                        users.push((username.to_string(), home_path));
                    }
                }
            }
        }
    }

    // Add root user
    users.push(("root".to_string(), PathBuf::from("/root")));

    Ok(users)
}

fn get_user_shell_rc(home_dir: &Path) -> Result<Vec<ShellRcInfo>> {
    let mut shell_rcs = Vec::new();

    // Check common shell RC files
    let rc_files = [
        (".bashrc", "bash"),
        (".zshrc", "zsh"),
        (".kshrc", "ksh"),
        (".cshrc", "csh"),
        (".tcshrc", "tcsh"),
        (".config/fish/config.fish", "fish"),
    ];

    for (rc_file, _shell_name) in rc_files.iter() {
        let rc_path = home_dir.join(rc_file);
        if rc_path.exists() {
            shell_rcs.push(ShellRcInfo {
                rc_file_path: rc_path.to_string_lossy().into_owned(),
                source_script_name: "epkg-rc.sh".to_string(),
            });
        }
    }

    Ok(shell_rcs)
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
            println!("  {}", rc_file.rc_file_path);
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
        if symlink.exists() {
            println!("Removing symlink: {}", symlink.display());
            fs::remove_file(symlink)
                .wrap_err_with(|| format!("Failed to remove symlink: {}", symlink.display()))?;
        }
    }

    // Modify shell RC files
    for rc_file in &plan.shell_rc_files {
        remove_epkg_from_rc_file(&rc_file.rc_file_path)?;
    }

    // Remove directories in the end
    for dir in &plan.dirs_to_remove {
        if dir.exists() {
            println!("Removing directory: {}", dir.display());
            force_remove_dir_all(dir)
                .wrap_err_with(|| format!("Failed to remove directory: {}", dir.display()))?;
        }
    }

    Ok(())
}

fn remove_epkg_from_rc_file(rc_file_path: &str) -> Result<()> {
    let path = Path::new(rc_file_path);
    if !path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(path)
        .wrap_err_with(|| format!("Failed to read RC file: {}", rc_file_path))?;

    // Check if epkg configuration is present
    if !content.contains("# epkg begin") || !content.contains("# epkg end") {
        return Ok(());
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
    fs::write(path, new_content)
        .wrap_err_with(|| format!("Failed to write RC file: {}", rc_file_path))?;

    println!("Modified shell configuration: {}", rc_file_path);
    Ok(())
}

/// Recursively removes a directory, fixing permission issues if needed.
fn force_remove_dir_all<P: AsRef<Path>>(path: P) -> Result<(), Error> {
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
fn find_readonly_dirs<P: AsRef<Path>>(root: P) -> Result<Vec<PathBuf>, Error> {
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
