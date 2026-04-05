use std::fs;
use crate::lfs;
use std::path::{Path, PathBuf};
use color_eyre::eyre::{self, Result, WrapErr, ContextCompat};
use time::OffsetDateTime;
use time::macros::format_description;
use crate::models::*;
use crate::io::read_installed_packages;
use crate::install::execute_installation_plan;

pub fn get_current_generation_id() -> Result<u32> {
    let generations_root = crate::dirs::get_default_generations_root()?;
    let current_link = generations_root.join("current");
    // If the "current" symlink doesn't exist yet, default to generation 0.
    // This represents the initial empty state before any install operations.
    if !lfs::exists_on_host(&current_link) {
        return Ok(0);
    }

    let target = fs::read_link(&current_link)
        .with_context(|| format!("Failed to read symlink: {}", current_link.display()))?;
    // On Windows, read_link returns the full path; extract just the directory name
    let target_name = target.file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("Failed to extract generation name from symlink target '{}'", target.display()))?;
    let generation_id = target_name.parse::<u32>()
        .with_context(|| format!("Failed to parse generation id from '{}'", target_name))?;
    Ok(generation_id)
}

/// Create a new generation directory for install/upgrade/remove/restore operations.
///
/// Always creates generation (current_id + 1):
/// - env create creates generation 0
/// - subsequent operations (install, restore) always increment by 1
/// - restore creates new generation with content matching the restore target
pub fn create_new_generation_with_root(generations_root: &Path) -> Result<PathBuf> {
    // Get current generation info
    let current_link = generations_root.join("current");
    let current_id = if lfs::exists_on_host(&current_link) {
        let target = fs::read_link(&current_link).with_context(|| format!("Failed to read symlink: {}", current_link.display()))?;
        // On Windows, read_link returns the full path; extract just the directory name
        let target_name = target.file_name()
            .and_then(|n| n.to_str())
            .with_context(|| format!("Failed to extract generation name from symlink target '{}'", target.display()))?;
        target_name.parse::<u32>().with_context(||
            format!("Failed to parse generation id from '{}'", target_name))?
    } else {
        // No current generation exists, start with generation 1
        // (generation 0 is created by env create, so if missing, we're in a fresh state)
        0
    };
    let current_generation = generations_root.join(current_id.to_string());

    // Always create new generation for install/upgrade/remove operations
    // This is different from env create which creates generation 0
    let new_id = current_id + 1;
    let new_generation = generations_root.join(new_id.to_string());

    // Create new generation directory
    lfs::create_dir_all(&new_generation)?;

    // Copy command.json from current generation if it exists
    let command_json = current_generation.join("command.json");
    if lfs::exists_on_host(&command_json) {
        lfs::copy(command_json, new_generation.join("command.json"))?;
    }

    Ok(new_generation)
}

pub fn update_current_generation_symlink_with_root(generations_root: &Path, new_generation: PathBuf) -> Result<()> {
    let current_link = generations_root.join("current");

    if lfs::exists_on_host(&current_link) {
        lfs::remove_file(&current_link)?;
    }

    lfs::symlink_dir_for_virtiofs(&new_generation.file_name().unwrap(), &current_link)?;
    Ok(())
}

pub fn record_history(new_generation_path: &PathBuf, plan: Option<&crate::plan::InstallationPlan>) -> Result<()> {
    let command_json = new_generation_path.join("command.json");

    let mut command = if let Some(plan) = plan {
        crate::plan::plan_to_generation_command(plan)
    } else {
        GenerationCommand::default()
    };

    // Set metadata fields
    command.timestamp = OffsetDateTime::now_local()?.format(&format_description!("[year]-[month]-[day] [hour repr:24]:[minute]:[second] [offset_hour sign:mandatory][offset_minute]")).unwrap_or_else(|_| "<time_fmt_err>".to_string());
    command.command_line = config().command_line.to_string();
    command.action = if plan.is_some() {
        format!("{:?}", config().subcommand)
    } else {
        "Create".to_string()
    };

    let json = serde_json::to_string_pretty(&command)?;
    lfs::write(&command_json, json)?;

    Ok(())
}

pub fn print_history() -> Result<()> {
    println!("{}  ENVIRONMENT HISTORY  {}", "_".repeat(33), "_".repeat(33));
    println!("{:<3} | {:<26} | {:<8} | {:<8} | {}", "id", "timestamp", "action", "packages", "command line");
    println!("{:-<3}-+-{:-<26}-+-{:-<8}-+-{:-<8}-+-{:-<32}", "", "", "", "", "");

    let generations_root = crate::dirs::get_default_generations_root()?;
    let mut history_entries: Vec<(u32, GenerationCommand)> = Vec::new();

    // Collect history entries
    for entry in fs::read_dir(&generations_root)? {
        let path = entry?.path();
        let filename = path.file_name().and_then(|s| s.to_str());

        // Skip 'current' symlink
        if let Some(gen_name) = filename {
            if gen_name == "current" {
                continue;
            }

            // Process only directories with numeric names (generations)
            if let Ok(id) = gen_name.parse::<u32>() {
                let command_json = path.join("command.json");
                if lfs::exists_on_host(&command_json) {
                    if let Ok(command) = crate::io::read_json_file(&command_json) {
                        history_entries.push((id, command));
                    }
                }
            }
        }
    }

    history_entries.sort_by_key(|entry| entry.0);

    // Limit number of generations to show if max_generations is set
    if let Some(max) = config().history.max_generations {
        let start = if history_entries.len() > max as usize {
            history_entries.len() - max as usize
        } else {
            0
        };
        history_entries = history_entries[start..].to_vec();
    }

    for (id, command) in history_entries {
        let mut package_changes = Vec::new();

        if command.fresh_installs.len() > 0 {
            package_changes.push(format!("+{}", command.fresh_installs.len()));
        }
        if command.upgrades_new.len() > 0 {
            package_changes.push(format!("^{}", command.upgrades_new.len()));
        }
        if command.old_removes.len() > 0 {
            package_changes.push(format!("-{}", command.old_removes.len()));
        }

        let package_counts = if package_changes.is_empty() {
            "".to_string()
        } else {
            package_changes.join(" ")
        };

        println!("{:<3} | {:<26} | {:<8} | {:<8} | {}",
            id,
            command.timestamp,
            command.action,
            package_counts,
            command.command_line
        );
    }
    Ok(())
}

/// Rollback/restore environment to a specific generation.
///
/// Generation semantics:
/// - Generation 0: created by `env create` (empty initial state)
/// - Each install/restore operation creates a new generation (current_id + 1)
/// - Restore only works on CONTENT (installed packages), not on generation number
/// - After restore to generation N, a new generation (current_id + 1) is created
///   with content matching generation N
/// - `restore 0` restores to empty state (like fresh `env create`)
pub fn rollback_history(rollback_id: i32) -> Result<()> {
    crate::repo::sync_channel_metadata()?;

    let generations_root = crate::dirs::get_default_generations_root()?;
    let current_generation_id = get_current_generation_id()?;

    // Handle negative rollback IDs (relative rollback)
    let target_id = if rollback_id < 0 {
        let abs_rollback : u32 = rollback_id.abs() as u32;
        if abs_rollback > current_generation_id {
            return Err(eyre::eyre!("Cannot rollback beyond generation 0"));
        }
        current_generation_id - abs_rollback
    } else {
        rollback_id as u32
    };

    // Check if target_id exists
    let rollback_generation = generations_root.join(target_id.to_string());
    if !lfs::exists_on_host(&rollback_generation) {
        return Err(eyre::eyre!("No such history record: Generation {} does not exist", target_id));
    }

    // Check if target_id is the current id
    if target_id == current_generation_id {
        return Err(eyre::eyre!("Cannot restore to the current generation"));
    }

    // Load target generation's installed-packages.json
    let target_packages = read_installed_packages(&config().common.env_name, target_id)?;

    // Compute delta_removes: packages in installed but not in target
    crate::io::load_installed_packages()?;
    let delta_removes = {
        let installed = &*crate::models::PACKAGE_CACHE.installed_packages.read().unwrap();
        let mut delta_removes = crate::models::InstalledPackagesMap::new();
        for (pkgkey, pkg_info) in installed.iter() {
            if !target_packages.contains_key(pkgkey) {
                delta_removes.insert(pkgkey.clone(), pkg_info.clone());
            }
        }
        delta_removes
    }; // <- read lock dropped here, avoiding dead-lock

    // Create InstallationPlan by diffing the two generations
    // PACKAGE_CACHE.installed_packages represents current state, target_packages is desired state
    let plan = crate::plan::prepare_installation_plan(&target_packages, Some(delta_removes))?;
    execute_installation_plan(plan)?;
    Ok(())
}
