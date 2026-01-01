use std::fs;
use std::path::{Path, PathBuf};
use std::os::unix::fs::symlink;
use color_eyre::eyre::{self, Result, WrapErr};
use time::OffsetDateTime;
use time::macros::format_description;
use crate::models::*;
use crate::io::read_installed_packages;
use crate::install::execute_installation_plan;

pub fn get_current_generation_id() -> Result<u32> {
    let generations_root = crate::dirs::get_default_generations_root()?;
    let current_link = generations_root.join("current");
    // If the "current" symlink doesn't exist yet, default to generation 1.
    if !current_link.exists() {
        return Ok(1);
    }

    let target = fs::read_link(&current_link)
        .with_context(|| format!("Failed to read symlink: {}", current_link.display()))?;
    let generation_id = target
        .to_str()
        .unwrap()
        .parse::<u32>()
        .with_context(|| format!("Failed to parse generation id from '{}'", target.to_str().unwrap()))?;
    Ok(generation_id)
}

pub fn create_new_generation_with_root(generations_root: &Path) -> Result<PathBuf> {
    // Get current generation info
    let current_link = generations_root.join("current");
    let current_id = if current_link.exists() {
        let target = fs::read_link(&current_link).with_context(|| format!("Failed to read symlink: {}", current_link.display()))?;
        target.to_str().unwrap().parse::<u32>().with_context(||
            format!("Failed to parse generation id from '{}'", target.to_str().unwrap()))?
    } else {
        // No current generation exists, start with generation 1
        1
    };
    let current_generation = generations_root.join(current_id.to_string());

    // Check if we need to create a new generation
    let command_json = current_generation.join("command.json");
    if !command_json.exists() {
        // Current generation has no command history, just return it
        return Ok(current_generation);
    }

    // Create new generation
    let new_id = current_id + 1;
    let new_generation = generations_root.join(new_id.to_string());

    // Create new generation directory
    fs::create_dir_all(&new_generation)?;

    // FHS directories are now at root level
    // So only copy metadata files from current to new generation.
    // No need copy installed-packages.json since its JSON data will be
    // loaded from old generation dir and saved to new generation dir.
    fs::copy(command_json, new_generation.join("command.json"))?;

    Ok(new_generation)
}

pub fn update_current_generation_symlink_with_root(generations_root: &Path, new_generation: PathBuf) -> Result<()> {
    let current_link = generations_root.join("current");

    if current_link.exists() {
        fs::remove_file(&current_link)?;
    }

    symlink(&new_generation.file_name().unwrap(), &current_link)?;
    Ok(())
}

pub fn record_history(new_generation_path: &PathBuf, plan: Option<&crate::plan::InstallationPlan>) -> Result<()> {
    let command_json = new_generation_path.join("command.json");

    let mut command = GenerationCommand {
        timestamp: OffsetDateTime::now_local()?.format(&format_description!("[year]-[month]-[day] [hour repr:24]:[minute]:[second] [offset_hour sign:mandatory][offset_minute]")).unwrap_or_else(|_| "<time_fmt_err>".to_string()),
        action: "Create".to_string(),
        command_line: config().command_line.to_string(),
        fresh_installs: Vec::new(),
        upgrades_new: Vec::new(),
        upgrades_old: Vec::new(),
        old_removes: Vec::new(),
        new_exposes: Vec::new(),
        del_exposes: Vec::new(),
    };

    // If an InstallationPlan is provided, populate the command with its members
    if let Some(plan) = plan {
        command.action = format!("{:?}", config().subcommand);
        // Extract operations from ordered_operations
        let mut new_exposes = Vec::new();
        let mut del_exposes = Vec::new();

        for op in &plan.ordered_operations {
            if op.should_expose() {
                if let Some((pkgkey, _)) = &op.new_pkg {
                    new_exposes.push(pkgkey.clone());
                }
            }
            if op.should_unexpose() {
                if let Some((old_pkgkey, _)) = &op.old_pkg {
                    del_exposes.push(old_pkgkey.clone());
                }
            }
        }

        command.fresh_installs = plan.fresh_installs.keys().cloned().collect();
        command.upgrades_new = plan.upgrades_new.keys().cloned().collect();
        command.upgrades_old = plan.upgrades_old.keys().cloned().collect();
        command.old_removes = plan.old_removes.keys().cloned().collect();
        command.new_exposes = new_exposes;
        command.del_exposes = del_exposes;
    }

    let json = serde_json::to_string_pretty(&command)?;
    fs::write(&command_json, json)?;

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
                if command_json.exists() {
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

pub fn rollback_history(rollback_id: i32) -> Result<()> {
    let generations_root = crate::dirs::get_default_generations_root()?;
    let current_generation_id = get_current_generation_id()?;

    // Handle negative rollback IDs (relative rollback)
    let target_id = if rollback_id < 0 {
        let abs_rollback : u32 = rollback_id.abs() as u32;
        if abs_rollback >= current_generation_id {
            return Err(eyre::eyre!("Cannot rollback beyond generation 1"));
        }
        current_generation_id - abs_rollback
    } else {
        rollback_id as u32
    };

    // Check if target_id exists
    let rollback_generation = generations_root.join(target_id.to_string());
    if !rollback_generation.exists() {
        return Err(eyre::eyre!("No such history record: Generation {} does not exist", target_id));
    }

    // Check if target_id is the current id
    if target_id == current_generation_id {
        return Err(eyre::eyre!("Cannot restore to the current generation"));
    }

    // Load target generation's installed-packages.json
    let target_packages = read_installed_packages(&config().common.env, target_id)?;

    // Compute delta_removes: packages in installed but not in target
    crate::io::load_installed_packages()?;
    let installed = &*crate::models::PACKAGE_CACHE.installed_packages.read().unwrap();
    let mut delta_removes = crate::models::InstalledPackagesMap::new();
    for (pkgkey, pkg_info) in installed.iter() {
        if !target_packages.contains_key(pkgkey) {
            delta_removes.insert(pkgkey.clone(), pkg_info.clone());
        }
    }

    // Create InstallationPlan by diffing the two generations
    // PACKAGE_CACHE.installed_packages represents current state, target_packages is desired state
    let plan = crate::plan::prepare_installation_plan(&target_packages, Some(delta_removes))?;
    execute_installation_plan(plan)?;
    Ok(())
}
