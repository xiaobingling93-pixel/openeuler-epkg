use std::fs;
use std::path::Path;
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

impl PackageManager {

    pub fn get_current_id(&self) -> Result<u64> {
        let profile_current = format!("{}/{}/profile-current", paths::instance.epkg_envs_root.display(), self.options.env);
        let target = fs::read_link(&profile_current).with_context(|| format!("Failed to read symlink: {}", profile_current))?;
        let parts: Vec<&str> = target.to_str().unwrap().split("-").collect();
        let current_profile_id = parts[1].parse::<u64>().with_context(|| format!("Failed to parse profile id from '{}'", parts[1]))?;
        Ok(current_profile_id)
    }

    pub fn get_profile_dir(&self, rollback: bool) -> Result<String> {
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
            if rollback { current_profile_id - 1 } else { current_profile_id + 1 }
        );
        move_profile_contents(&cur_profile, &new_profile)?;
        if rollback {
            fs::remove_dir_all(&cur_profile)?;
        }

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
        println!("{:<4} | {:<26} | {:<10} | {:<30} | {}", "id", "timestamp", "action", "packages", "command line");
        println!("{:-<4}-+-{:-<26}-+-{:-<10}-+-{:-<30}-+-{:-<}", "", "", "", "", "");

        let profile_dir = format!("{}/{}", paths::instance.epkg_envs_root.display(), self.options.env);
        let mut history_entries: Vec<(u64, String, String, String, String)> = Vec::new();

        for entry in fs::read_dir(&profile_dir)?.filter_map(Result::ok) {
            let path = entry.path();
            if !path.is_dir() || path.ends_with("profile-current") {
                continue;
            }
            if let Some(profile) = path.file_name().and_then(|s| s.to_str()) {
                if profile.starts_with("profile-") {
                    if let Ok(id) = profile[8..].parse::<u64>() {
                        let command_json = path.join("command.json");
                        if command_json.exists() {
                            let contents = fs::read_to_string(&command_json)?;
                            let command: ProfileCommand = serde_json::from_str(&contents)?;
                            history_entries.push((id, command.timestamp, command.action, command.packages.join(" "), command.command_line));
                        }
                    }
                }
            }
        }

        // sort in ascending order of id
        history_entries.sort_by_key(|entry| entry.0);
        for (id, timestamp, action, packages, command_line) in history_entries {
            println!("{:<4} | {:<26} | {:<10} | {:<30} | {}", id, timestamp, action, packages, command_line);
        }
        Ok(())
    }

    pub fn rollback_history(&mut self, rollback_id: u64) -> Result<()> {
        // Check if rollback_id exists
        let profile_dir = format!("{}/{}", paths::instance.epkg_envs_root.display(), self.options.env);
        let rollback_profile = format!("{}/profile-{}", profile_dir, rollback_id);
        if !Path::new(&rollback_profile).exists() {
            return Err(anyhow!("No such history record: Profile {} does not exist", rollback_id));
        }

        // Check if rollback_id is the last id
        let current_profile_id = self.get_current_id()?;
        if rollback_id == current_profile_id {
            return Err(anyhow!("Cannot rollback to the current profile"));
        }

        // load current_profile_id ~ rollback_id command.json
        let mut history_entries: Vec<(u64, String, String, Vec<String>)> = Vec::new();
        for i in rollback_id+1..current_profile_id+1 {
            let profile = format!("{}/profile-{}", profile_dir, i);
            let command_json = format!("{}/command.json", profile);
            if Path::new(&command_json).exists() {
                let contents = fs::read_to_string(&command_json)?;
                let command: ProfileCommand = serde_json::from_str(&contents)?;
                history_entries.push((i, command.timestamp, command.action, command.packages));
            }
        }
        history_entries.sort_by(|a, b| b.0.cmp(&a.0));

        // traversal history_entries
        for (profile_id, _timestamp, action, packages) in history_entries {
            println!("Rollback ...: {} {}, profile {} -> {}", action, packages.join(" "), profile_id, profile_id-1);
            let mut rollback_action = String::new();
            if action.trim() == "install" {
                rollback_action = "remove".to_string();
                self.remove_packages(packages.clone(), true, false)?;
            } else if action.trim() == "remove" {
                rollback_action = "install".to_string();
                self.install_packages(packages.clone(), true)?;
            } else if action.trim() == "upgrade" {
                // Todo upgrade & downgrade
            }
            println!("Rollback done: {} {}", rollback_action, packages.join(" "));
        }

        Ok(())
    }
}