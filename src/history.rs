use std::fs;
use std::path::PathBuf;
use std::os::unix::fs::symlink;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use color_eyre::Result;
use time::OffsetDateTime;
use time::macros::format_description;

use crate::models::*;

impl PackageManager {

    pub fn get_current_generation_id(&mut self) -> Result<u32> {
        let generations_root = self.get_default_generations_root()?;
        let current_link = generations_root.join("current");
        let target = fs::read_link(&current_link).with_context(|| format!("Failed to read symlink: {}", current_link.display()))?;
        let generation_id = target.to_str().unwrap().parse::<u32>().with_context(||
            format!("Failed to parse generation id from '{}'", target.to_str().unwrap()))?;
        Ok(generation_id)
    }

    pub fn get_generation_path(&mut self, generation_id: u32) -> Result<PathBuf> {
        let generations_root = self.get_default_generations_root()?;
        Ok(generations_root.join(generation_id.to_string()))
    }

    #[allow(dead_code)]
    pub fn get_current_generation_path(&mut self) -> Result<PathBuf> {
        let current_id = self.get_current_generation_id()?;
        self.get_generation_path(current_id)
    }

    pub fn create_new_generation(&mut self) -> Result<PathBuf> {
        // Get current generation info
        let current_id = self.get_current_generation_id()?;
        let current_generation = self.get_generation_path(current_id)?;

        // Check if we need to create a new generation
        let command_json = current_generation.join("command.json");
        if !command_json.exists() {
            // Current generation has no command history, just return it
            return Ok(current_generation);
        }

        // Create new generation
        let new_id = current_id + 1;
        let new_generation = self.get_generation_path(new_id)?;

        // Create new generation directory
        fs::create_dir_all(&new_generation)?;

        // FHS directories are now at root level
        // So only copy metadata files from current to new generation.
        // No need copy installed-packages.json since its JSON data will be
        // loaded from old generation dir and saved to new generation dir.
        fs::copy(command_json, new_generation.join("command.json"))?;

        Ok(new_generation)
    }

    pub fn update_current_generation_symlink(&mut self, new_generation: PathBuf) -> Result<()> {
        let generations_root = self.get_default_generations_root()?;
        let current_link = generations_root.join("current");

        if current_link.exists() {
            fs::remove_file(&current_link)?;
        }

        symlink(&new_generation.file_name().unwrap(), &current_link)?;
        Ok(())
    }

    pub fn record_history(&mut self, new_generation_path: &PathBuf, action: &str, new_packages: Vec<String>, del_packages: Vec<String>) -> Result<()> {
        let command_json = new_generation_path.join("command.json");

        let command = GenerationCommand {
            timestamp: OffsetDateTime::now_local()?.format(&format_description!("[year]-[month]-[day] [hour repr:24]:[minute]:[second] [offset_hour sign:mandatory][offset_minute]")).unwrap_or_else(|_| "<time_fmt_err>".to_string()),
            action: action.to_string(),
            new_packages,
            del_packages,
            command_line: config().command_line.to_string(),
        };

        let json = serde_json::to_string_pretty(&command)?;
        fs::write(&command_json, json)?;

        Ok(())
    }

    pub fn print_history(&mut self) -> Result<()> {
        println!("{}  {} env history  {}", "-".repeat(50), config().common.env, "-".repeat(50));
        println!("{:<3} | {:<26} | {:<10} | {:<12} | {:<12} | {}", "id", "timestamp", "action", "new_packages", "del_packages", "command line");
        println!("{:-<3}-+-{:-<26}-+-{:-<10}-+-{:-<12}-+-{:-<12}-+-{:-<40}", "", "", "", "", "", "");

        let generations_root = self.get_default_generations_root()?;
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
                        if let Ok(contents) = fs::read_to_string(command_json) {
                            if let Ok(command) = serde_json::from_str(&contents) {
                                history_entries.push((id, command));
                            }
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
            println!("{:<3} | {:<26} | {:<10} | {:<12} | {:<12} | {}",
                id,
                command.timestamp,
                command.action,
                command.new_packages.len(),
                command.del_packages.len(),
                command.command_line
            );
        }
        Ok(())
    }

    pub fn rollback_history(&mut self, rollback_id: i32) -> Result<()> {
        let generations_root = self.get_default_generations_root()?;
        let current_generation_id = self.get_current_generation_id()?;

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

        // Load current and rollback installed-packages.json
        let current_packages = self.read_installed_packages(&config().common.env, current_generation_id)?;
        let rollback_packages = self.read_installed_packages(&config().common.env, target_id)?;

        // Calculate packages to add/remove
        let new_packages: Vec<(String, bool)> = rollback_packages.keys()
            .filter(|name| !current_packages.contains_key(*name))
            .map(|name| (name.clone(), rollback_packages[name].appbin_flag))
            .collect();
        let del_packages: Vec<String> = current_packages.keys()
            .filter(|name| !rollback_packages.contains_key(*name))
            .cloned()
            .collect();

        // Print rollback information
        println!("{:-^100}", "  Rollback informaton  ");
        println!("{:<6} | {:<32} | {:<20} | {:<10} | {:<7} | {}", "action", "hash", "pkg", "version", "release", "dist");
        println!("{:-<6}-+-{:-<32}-+-{:-<20}-+-{:-<10}-+-{:-<7}-+-{:-<11}", "", "", "", "", "", "");
        for pkg in &del_packages {
            let parts: Vec<&str> = pkg.split("__").collect();
            if parts.len() >= 4 {
                let dist = parts[3].split('.').next().unwrap_or("");
                println!("{:<6} | {:<32} | {:<20} | {:<10} | {:<7} | {}",
                    "del", parts[0], parts[1], parts[2], dist, parts[3]);
            }
        }
        for (pkg, _) in &new_packages {
            let parts: Vec<&str> = pkg.split("__").collect();
            if parts.len() >= 4 {
                let dist = parts[3].split('.').next().unwrap_or("");
                println!("{:<6} | {:<32} | {:<20} | {:<10} | {:<7} | {}",
                    "new", parts[0], parts[1], parts[2], dist, parts[3]);
            }
        }

        // Create a new generation for this rollback operation
        let new_generation = self.create_new_generation()?;
        let store_root = dirs().epkg_store.clone();
        let env_root = self.get_default_env_root()?;

        // Apply package changes directly to FHS directories at root level
        // First phase: Link all packages
        for (pkgline, _) in &new_packages {
            let fs_dir = store_root.join(pkgline).join("fs");
            self.link_package(&fs_dir, &env_root)?;
        }

        // Second phase: Expose packages and handle appbin flags
        for (pkgline, appbin_flag) in &new_packages {
            let fs_dir = store_root.join(pkgline).join("fs");
            if *appbin_flag {
                self.expose_package(&fs_dir, &env_root)?;
            }
        }

        for pkgline in &del_packages {
            let fs_dir = store_root.join(pkgline).join("fs");
            self.unlink_package(&fs_dir, &env_root)?;
        }

        // Copy rollback generation's installed-packages.json to current generation
        let rollback_json = rollback_generation.join("installed-packages.json");
        fs::copy(&rollback_json, new_generation.join("installed-packages.json"))?;

        // Record history
        self.record_history(&new_generation, "rollback", new_packages.iter().map(|(name, _)| name.clone()).collect(), del_packages)?;

        // Last step: update current symlink to point to the new generation
        self.update_current_generation_symlink(new_generation)?;

        println!("Rollback success!");

        Ok(())
    }
}
