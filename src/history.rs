use std::fs;
use std::path::Path;
use std::collections::HashMap;
use std::os::unix::fs::symlink;
use anyhow::anyhow;
use anyhow::Result;
use anyhow::Context;
use crate::paths;
use crate::models::*;

pub fn move_profile_contents(src_dir: &str, dst_dir: &str) -> Result<()> {
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

pub fn load_installed_packages(env: &str, profile_id: u64) -> Result<HashMap<String, InstalledPackageInfo>> {
    let file_path = format!("{}/{}/profile-{}/installed-packages.json", paths::instance.epkg_envs_root.display(), env, profile_id);
    let contents = fs::read_to_string(&file_path).with_context(|| format!("Failed to read file: {}", file_path))?;
    let packages: HashMap<String, InstalledPackageInfo> = serde_json::from_str(&contents).with_context(|| format!("Failed to parse JSON from file: {}", file_path))?;
    Ok(packages)
}

impl PackageManager {

    pub fn get_current_id(&self) -> Result<u64> {
        let profile_current = format!("{}/{}/profile-current", paths::instance.epkg_envs_root.display(), self.options.env);
        let target = fs::read_link(&profile_current).with_context(|| format!("Failed to read symlink: {}", profile_current))?;
        let parts: Vec<&str> = target.to_str().unwrap().split("-").collect();
        let current_profile_id = parts[1].parse::<u64>().with_context(|| format!("Failed to parse profile id from '{}'", parts[1]))?;
        Ok(current_profile_id)
    }

    pub fn get_profile_dir(&self) -> Result<String> {
        // Get current profile id
        let current_profile_id = self.get_current_id()?;

        // Check profile command json
        let cur_profile = format!("{}/{}/profile-{}", paths::instance.epkg_envs_root.display(), self.options.env, current_profile_id);
        let command_json = format!("{}/{}/profile-current/command.json", paths::instance.epkg_envs_root.display(), self.options.env);
        if !Path::new(&command_json).exists() {
            return Ok(cur_profile);
        }

        // mv profile-{cur}/* -> profile-{new}/*
        let new_profile = format!(
            "{}/{}/profile-{}",
            paths::instance.epkg_envs_root.display(),
            self.options.env,
            current_profile_id + 1
        );
        move_profile_contents(&cur_profile, &new_profile)?;

        // ln -sf profile-current -> cur_profile
        let profile_current = format!("{}/{}/profile-current", paths::instance.epkg_envs_root.display(), self.options.env);
        fs::remove_file(&profile_current)?;
        symlink(&new_profile, &profile_current)?;

        Ok(new_profile)
    }

    pub fn record_history(&mut self, action: &str, packages: Vec<String>, command_line: &str) -> Result<()> {
        let command_json = format!("{}/{}/profile-current/command.json", paths::instance.epkg_envs_root.display(), self.options.env);
        let command = ProfileCommand {
            timestamp: chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z").to_string(),
            action: action.to_string(),
            packages: packages,
            command_line: command_line.to_string(),
        };
        let json = serde_json::to_string_pretty(&command)?;
        fs::write(&command_json, json)?;
    
        Ok(())
    }

    pub fn print_history(&mut self) -> Result<()> {
        println!("{} env history", self.options.env);
        println!("{:<4} | {:<26} | {:<10} | {:<30} | {}", "id", "timestamp", "action", "packages", "command line");
        println!("{:-<4}-+-{:-<26}-+-{:-<10}-+-{:-<30}-+-{:-<}", "", "", "", "", "");

        let profile_dir = format!("{}/{}", paths::instance.epkg_envs_root.display(), self.options.env);
        let mut history_entries: Vec<(u64, ProfileCommand)> = Vec::new();

        // Collect history entries
        for entry in fs::read_dir(&profile_dir)? {
            let path = entry?.path();
            let filename = path.file_name().and_then(|s| s.to_str());
            
            if let Some(profile) = filename {
                if !profile.starts_with("profile-") || profile == "profile-current" {
                    continue;
                }
                
                if let Ok(id) = profile[8..].parse::<u64>() {
                    if let Ok(contents) = fs::read_to_string(path.join("command.json")) {
                        if let Ok(command) = serde_json::from_str(&contents) {
                            history_entries.push((id, command));
                        }
                    }
                }
            }
        }

        history_entries.sort_by_key(|entry| entry.0);
        for (id, command) in history_entries {
            println!("{:<4} | {:<26} | {:<10} | {:<30} | {}", id, command.timestamp, command.action, command.packages.join(" "), command.command_line);
        }
        Ok(())
    }

    pub fn rollback_history(&mut self, rollback_id: u64) -> Result<()> {
        // Check if rollback_id exists
        let rollback_profile = format!("{}/{}/profile-{}", paths::instance.epkg_envs_root.display(), self.options.env, rollback_id);
        if !Path::new(&rollback_profile).exists() {
            return Err(anyhow!("No such history record: Profile {} does not exist", rollback_id));
        }

        // Check if rollback_id is the last id
        let current_profile_id = self.get_current_id()?;
        if rollback_id == current_profile_id {
            return Err(anyhow!("Cannot rollback to the current profile"));
        }

        // Load current_profile_id ~ rollback_id installed-packages.json, filter need new/del packages
        let current_packages = load_installed_packages(&self.options.env, current_profile_id)?;
        let rollback_packages = load_installed_packages(&self.options.env, rollback_id)?;
        let new_packages: Vec<String> = rollback_packages.keys()
            .filter(|name| !current_packages.contains_key(*name))
            .cloned()
            .collect();
        let del_packages: Vec<String> = current_packages.keys()
            .filter(|name| !rollback_packages.contains_key(*name))
            .cloned()
            .collect();

        println!("Rollback informaton:");
        println!("New: {:?}, Del: {:?}", new_packages, del_packages);

        // Remove del_packages
        let symlink_dir = self.get_profile_dir()?;
        for pkgline in new_packages {
            // Todo: appbin_flag need fix
            let fs_dir = format!("{}/{}/fs", paths::instance.epkg_store_root.display(), pkgline);
            self.new_package(&fs_dir, &symlink_dir, false)?;
        }
        for pkgline in del_packages {
            let fs_dir = format!("{}/{}/fs", paths::instance.epkg_store_root.display(), pkgline);
            self.del_package(&fs_dir, &symlink_dir)?;
        }

        // Cp rollback_id installed-packages.json to current profile
        let installed_json = format!("{}/{}/profile-{}/installed-packages.json", paths::instance.epkg_envs_root.display(), self.options.env, rollback_id);
        let current_json = format!("{}/{}/profile-current/installed-packages.json", paths::instance.epkg_envs_root.display(), self.options.env);
        fs::copy(&installed_json, &current_json)?;

        Ok(())
    }
}