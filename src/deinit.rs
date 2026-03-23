use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::exit;
use std::time::{Duration, Instant};

use color_eyre::eyre::{self, WrapErr};
use color_eyre::Result;
use indicatif::{ProgressBar, ProgressStyle};
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

    for ps in crate::dirs::powershell_profile_paths() {
        if lfs::exists_on_host(&ps) {
            plan.shell_rc_files.push(ps.to_string_lossy().into_owned());
        }
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
            force_remove_dir_all_with_progress(dir)
                .wrap_err_with(|| format!("Failed to remove directory: {}", dir.display()))?;
        }
    }

    Ok(())
}

fn format_removal_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{:.1}s", d.as_secs_f64())
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    }
}

struct RemovalProgress {
    pb: Option<ProgressBar>,
    last_update: Instant,
    start: Instant,
    removed_count: u64,
    plain_mode: bool,
    verbose: bool,
}

impl RemovalProgress {
    fn new() -> Self {
        let start = Instant::now();
        let verbose = config().common.verbose;
        if config().common.quiet {
            return Self {
                pb: None,
                last_update: start,
                start,
                removed_count: 0,
                plain_mode: false,
                verbose,
            };
        }

        let stderr = std::io::stderr();
        if stderr.is_terminal() {
            let pb = ProgressBar::new_spinner();
            pb.enable_steady_tick(Duration::from_millis(100));
            let style = ProgressStyle::with_template("{spinner:.green} {wide_msg}")
                .expect("hard-coded progress template");
            pb.set_style(style);
            pb.set_message("Removing … 0 entries · 0.0s");
            Self {
                pb: Some(pb),
                last_update: start,
                start,
                removed_count: 0,
                plain_mode: false,
                verbose,
            }
        } else {
            Self {
                pb: None,
                last_update: start,
                start,
                removed_count: 0,
                plain_mode: true,
                verbose,
            }
        }
    }

    fn tick(&mut self) {
        if config().common.quiet {
            return;
        }

        self.removed_count += 1;
        const THROTTLE_OPS_DEFAULT: u64 = 500;
        const THROTTLE_OPS_VERBOSE: u64 = 50;
        let throttle_ops = if self.verbose {
            THROTTLE_OPS_VERBOSE
        } else {
            THROTTLE_OPS_DEFAULT
        };
        let throttle_time = if self.verbose {
            Duration::from_millis(200)
        } else {
            Duration::from_secs(1)
        };
        let now = Instant::now();
        if self.removed_count != 1
            && self.removed_count % throttle_ops != 0
            && now.duration_since(self.last_update) < throttle_time
        {
            return;
        }
        self.last_update = now;

        let elapsed = format_removal_elapsed(self.start.elapsed());
        let msg = format!(
            "Removing … {} entries · {}",
            self.removed_count, elapsed
        );
        if let Some(pb) = &self.pb {
            pb.set_message(msg);
        } else if self.plain_mode {
            eprintln!("{msg}");
        }
    }

    fn finish(&mut self) {
        if let Some(pb) = self.pb.take() {
            pb.finish_and_clear();
        }
    }

    fn abandon(&mut self) {
        if let Some(pb) = self.pb.take() {
            pb.abandon();
        }
    }
}

fn remove_dir_all_recursive_inner(path: &Path, progress: &mut RemovalProgress) -> Result<()> {
    let entries = fs::read_dir(path)
        .wrap_err_with(|| format!("Failed to read directory: {}", path.display()))?;
    for entry in entries {
        let entry = entry.wrap_err_with(|| format!("Failed to read entry in {}", path.display()))?;
        let p = entry.path();
        let meta = lfs::symlink_metadata(&p)
            .wrap_err_with(|| format!("Failed to stat: {}", p.display()))?;
        if meta.file_type().is_symlink() {
            lfs::remove_file(&p)?;
        } else if meta.is_dir() {
            remove_dir_all_recursive_inner(&p, progress)?;
            lfs::remove_dir(&p)?;
        } else {
            lfs::remove_file(&p)?;
        }
        progress.tick();
    }
    Ok(())
}

fn remove_dir_all_recursive_with_progress(path: &Path) -> Result<()> {
    let mut progress = RemovalProgress::new();
    let result = (|| -> Result<()> {
        remove_dir_all_recursive_inner(path, &mut progress)?;
        lfs::remove_dir(path)?;
        progress.tick();
        Ok(())
    })();

    match &result {
        Ok(()) => progress.finish(),
        Err(_) => progress.abandon(),
    }
    result
}

/// Like [`force_remove_dir_all`], but shows indeterminate progress (spinner + entry count and
/// elapsed time) on a TTY unless `--quiet` is set. Updates are throttled (every 500 entries or
/// every second by default; every 50 entries or 200ms with `--verbose`) so terminal I/O stays cheap.
/// With no TTY (e.g. script), defaults to the same fast removal as quiet; pass `--verbose` to force
/// throttled lines on stderr.
pub(crate) fn force_remove_dir_all_with_progress<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    if config().common.quiet {
        return force_remove_dir_all(path);
    }
    if !std::io::stderr().is_terminal() && !config().common.verbose {
        return force_remove_dir_all(path);
    }

    match remove_dir_all_recursive_with_progress(path) {
        Ok(()) => Ok(()),
        Err(_e) => force_remove_dir_all(path),
    }
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
