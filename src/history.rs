use std::fs;
use std::path::Path;
use std::os::unix::fs::symlink;
use anyhow::anyhow;
use anyhow::Result;
use anyhow::Context;
use crate::paths;
use crate::utils::*;
use crate::models::*;

impl PackageManager {

    pub fn get_current_id(&self) -> Result<u64> {
        let profile_current = format!("{}/{}/profile-current", paths::instance.epkg_envs_root.display(), self.options.env);
        let target = fs::read_link(&profile_current).with_context(|| format!("Failed to read symlink: {}", profile_current))?;
        let parts: Vec<&str> = target.to_str().unwrap().split("-").collect();
        let current_profile_id = parts[1].parse::<u64>().with_context(|| format!("Failed to parse profile id from '{}'", parts[1]))?;
        Ok(current_profile_id)
    }

    // Create profile directory
    pub fn create_profile_dir(&self) -> Result<String> {
        // Get current profile id
        let current_profile_id = self.get_current_id()?;

        // Check profile command json
        let cur_profile = format!("{}/{}/profile-{}", paths::instance.epkg_envs_root.display(), self.options.env, current_profile_id);
        let command_json = format!("{}/{}/profile-current/command.json", paths::instance.epkg_envs_root.display(), self.options.env);
        if !Path::new(&command_json).exists() {
            return Ok(cur_profile);
        }

        // cp -R profile-last profile-cur
        let new_profile = format!("{}/{}/profile-{}", paths::instance.epkg_envs_root.display(), self.options.env, current_profile_id+1);
        copy_dir_all(&cur_profile, &new_profile)?;

        // ln -sf profile-current -> cur_profile
        let profile_current = format!("{}/{}/profile-current", paths::instance.epkg_envs_root.display(), self.options.env);
        fs::remove_file(&profile_current)?;
        symlink(&new_profile, &profile_current)?;

        Ok(new_profile)
    }

    pub fn record_history(&mut self, action: &str, packages: Vec<String>) -> Result<()> {
        let command_json = format!("{}/{}/profile-current/command.json", paths::instance.epkg_envs_root.display(), self.options.env);
        let command = ProfileCommand {
            timestamp: chrono::Local::now().format("%Y-%m-%d %H:%M:%S  %:z").to_string(),
            action: action.to_string(),
            packages,
        };
        let json = serde_json::to_string_pretty(&command)?;
        // Check if command.json exists
        if Path::new(&command_json).exists() {
            fs::remove_file(&command_json)?;
            fs::write(&command_json, json)?;
        } else {
            fs::write(&command_json, json)?;
        }
        
        Ok(())
    }

    pub fn print_history(&mut self) -> Result<()> {
        println!("{:<4} | {:<30} | {:<15} | {}", "id", "timestamp", "action", "packages");
        println!("{:-<4}-+-{:-<30}-+-{:-<15}-+-{:-<}", "", "", "", "");

        let profile_dir = format!("{}/{}", paths::instance.epkg_envs_root.display(), self.options.env);
        let mut history_entries: Vec<(u64, String, String, String)> = Vec::new();

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
                            history_entries.push((id, command.timestamp, command.action, command.packages.join("")));
                        }
                    }
                }
            }
        }

        // sort in ascending order of id
        history_entries.sort_by_key(|entry| entry.0);
        for (id, timestamp, action, packages) in history_entries {
            println!("{:<4} | {:<30} | {:<15} | {}", id, timestamp, action, packages);
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

        // // symlink profile-current to profile-id
        // let profile_current = format!("{}/{}/profile-current", paths::instance.epkg_envs_root.display(), self.options.env);
        // let profile_history = format!("{}/{}/profile-{}", paths::instance.epkg_envs_root.display(), self.options.env, rollback_id);
        // fs::remove_file(&profile_current)?;
        // symlink(&profile_history, &profile_current)?;

        // // Remove profile dir between id and last id
        // for i in rollback_id+1..self.history.last().unwrap().id+1 {
        //     let profile = format!("{}/{}/profile-{}", paths::instance.epkg_envs_root.display(), self.options.env, i);
        //     fs::remove_dir_all(&profile)?;
        // }

        // // Remove history record between id and last id
        // self.history.retain(|r| r.id <= rollback_id);

        Ok(())
    }
}