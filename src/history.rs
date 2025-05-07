use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::collections::HashMap;
use std::os::unix::fs::symlink;
use anyhow::anyhow;
use anyhow::Result;
use anyhow::Context;
use crate::models::*;

pub fn move_generation_contents(src_dir: &str, dst_dir: &str) -> Result<()> {
    let src_path = Path::new(src_dir);
    let dst_path = Path::new(dst_dir);

    if !src_path.exists() {
        return Err(anyhow!("Source directory '{}' does not exist", src_path.display()));
    }

    // Create destination directory
    fs::create_dir_all(dst_path).with_context(|| format!("Failed to create destination directory '{}'", dst_path.display()))?;

    // Exclude installed-packages.json and command.json
    let excluded = ["installed-packages.json", "command.json"];

    for entry in fs::read_dir(src_path)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let file_name_str = file_name.to_string_lossy();

        if excluded.contains(&file_name_str.as_ref()) {
            continue;
        }

        let src_entry_path = entry.path();
        let dst_entry_path = dst_path.join(&file_name);

        fs::rename(&src_entry_path, &dst_entry_path)
            .with_context(|| format!("Failed to move '{}' to '{}'", src_entry_path.display(), dst_entry_path.display()))?;
    }
    Ok(())
}

impl PackageManager {
    pub fn load_installed_packages(&mut self, env: &str, generation_id: u64) -> Result<HashMap<String, InstalledPackageInfo>> {
        let generations_root = self.get_generations_root(env)?;
        let file_path = generations_root.join(generation_id.to_string()).join("installed-packages.json");
        let contents = fs::read_to_string(&file_path).with_context(|| format!("Failed to read file: {}", file_path.display()))?;
        let packages: HashMap<String, InstalledPackageInfo> = serde_json::from_str(&contents).with_context(|| format!("Failed to parse JSON from file: {}", file_path.display()))?;
        Ok(packages)
    }

    pub fn get_current_generation_id(&self) -> Result<u64> {
        let generations_root = self.get_default_generations_root()?;
        let current_link = generations_root.join("current");
        let target = fs::read_link(&current_link).with_context(|| format!("Failed to read symlink: {}", current_link.display()))?;
        let generation_id = target.to_str().unwrap().parse::<u64>().with_context(||
            format!("Failed to parse generation id from '{}'", target.to_str().unwrap()))?;
        Ok(generation_id)
    }

    pub fn get_generation_path(&self, generation_id: u64) -> Result<PathBuf> {
        let generations_root = self.get_default_generations_root()?;
        Ok(generations_root.join(generation_id.to_string()))
    }

    pub fn get_current_generation_path(&self) -> Result<PathBuf> {
        let current_id = self.get_current_generation_id()?;
        self.get_generation_path(current_id)
    }

    pub fn create_new_generation(&self) -> Result<PathBuf> {
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

        // Move contents from current to new generation
        move_generation_contents(current_generation.to_str().unwrap(), new_generation.to_str().unwrap())?;

        // Update current symlink to point to the new generation
        self.update_current_generation_symlink(new_id)?;

        Ok(new_generation)
    }

    pub fn update_current_generation_symlink(&self, generation_id: u64) -> Result<()> {
        let generations_root = self.get_default_generations_root()?;
        let current_link = generations_root.join("current");

        if current_link.exists() {
            fs::remove_file(&current_link)?;
        }

        symlink(&generation_id.to_string(), &current_link)?;
        Ok(())
    }

    pub fn record_history(&mut self, action: &str, new_packages: Vec<String>, del_packages: Vec<String>, command_line: &str) -> Result<()> {
        let current_gen_id = self.get_current_generation_id()?;
        let generations_root = self.get_default_generations_root()?;
        let command_json = generations_root.join(current_gen_id.to_string()).join("command.json");

        let command = GenerationCommand {
            timestamp: chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z").to_string(),
            action: action.to_string(),
            new_packages,
            del_packages,
            command_line: command_line.to_string(),
        };

        let json = serde_json::to_string_pretty(&command)?;
        fs::write(&command_json, json)?;

        Ok(())
    }

    pub fn print_history(&mut self) -> Result<()> {
        println!("{}  {} env history  {}", "-".repeat(50), self.options.env, "-".repeat(50));
        println!("{:<3} | {:<26} | {:<10} | {:<12} | {:<12} | {}", "id", "timestamp", "action", "new_packages", "del_packages", "command line");
        println!("{:-<3}-+-{:-<26}-+-{:-<10}-+-{:-<12}-+-{:-<12}-+-{:-<40}", "", "", "", "", "", "");

        let generations_root = self.get_default_generations_root()?;
        let mut history_entries: Vec<(u64, GenerationCommand)> = Vec::new();

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
                if let Ok(id) = gen_name.parse::<u64>() {
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
        if let Some(max) = self.options.max_generations {
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

    pub fn rollback_history(&mut self, rollback_id: u64, command_line: &str) -> Result<()> {
        // Check if rollback_id exists
        let generations_root = self.get_default_generations_root()?;
        let rollback_generation = generations_root.join(rollback_id.to_string());

        if !rollback_generation.exists() {
            return Err(anyhow!("No such history record: Generation {} does not exist", rollback_id));
        }

        // Check if rollback_id is the last id
        let current_generation_id = self.get_current_generation_id()?;
        if rollback_id == current_generation_id {
            return Err(anyhow!("Cannot rollback to the current generation"));
        }

        // Load current and rollback installed-packages.json
        let current_packages = self.load_installed_packages(&self.options.env, current_generation_id)?;
        let rollback_packages = self.load_installed_packages(&self.options.env, rollback_id)?;

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
        let generation_path = self.create_new_generation()?;
        let store_root = self.dirs.epkg_store;

        // Apply package changes
        for (pkgline, appbin_flag) in &new_packages {
            let fs_dir = format!("{}/{}/fs", store_root.display(), pkgline);
            self.new_package(&fs_dir, &generation_path.to_str().unwrap(), *appbin_flag)?;
        }
        for pkgline in &del_packages {
            let fs_dir = format!("{}/{}/fs", store_root.display(), pkgline);
            self.del_package(&fs_dir, &generation_path.to_str().unwrap())?;
        }

        // Copy rollback generation's installed-packages.json to current generation
        let rollback_json = rollback_generation.join("installed-packages.json");
        fs::copy(&rollback_json, generation_path.join("installed-packages.json"))?;

        // Record history
        self.record_history("rollback", new_packages.iter().map(|(name, _)| name.clone()).collect(), del_packages, command_line)?;
        println!("Rollback success!");

        Ok(())
    }
}
