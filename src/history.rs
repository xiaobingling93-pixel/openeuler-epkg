use std::fs;
use std::os::unix::fs::symlink;
use anyhow::anyhow;
use anyhow::Result;
use crate::paths;
use crate::utils::*;
use crate::models::*;

impl PackageManager {
    pub fn print_history(&mut self) -> Result<()> {
        self.load_history()?;
        println!("{:<4} | {:<30} | {:<15} | {}", "id", "timestamp", "action", "packages");
        println!("{:-<4}-+-{:-<30}-+-{:-<15}-+-{:-<}", "", "", "", "");
        for record in &self.history {
            println!(
                "{:<4} | {:<30} | {:<15} | {}",
                record.id,
                record.timestamp,
                record.action,
                record.packages.join(" ")
            );
        }
        Ok(())
    }

    pub fn record_history(&mut self, action: &str, packages: Vec<String>) -> Result<()> {
        // if history is empty, set id to 1, otherwise set id to the last id + 1
        let id = self.history.last().map_or(1, |h| h.id + 1);
        // get current timestamp
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S  %:z").to_string();
        // create a new history record
        let record = HistoryRecord {
            id,
            timestamp,
            action: action.to_string(),
            packages,
        };
        // write the history to file
        self.history.push(record);
        self.save_history()?;

        Ok(())
    }

    // Create profile directory
    pub fn create_profile_dir(&self) -> Result<String> {
        let history_id = self.history.last().map_or(1, |h| h.id + 1);
        let cur_profile = format!("{}/{}/profile-{}", paths::instance.epkg_envs_root.display(), self.options.env, history_id);
        if history_id == 1 {
            return Ok(cur_profile);
        }

        // cp -R profile-last profile-cur
        let last_profile = format!("{}/{}/profile-{}", paths::instance.epkg_envs_root.display(), self.options.env, history_id-1);
        copy_dir_all(&last_profile, &cur_profile)?;

        // ln -sf profile-current -> cur_profile
        let profile_current = format!("{}/{}/profile-current", paths::instance.epkg_envs_root.display(), self.options.env);
        fs::remove_file(&profile_current)?;
        symlink(&cur_profile, &profile_current)?;

        Ok(cur_profile)
    }

    pub fn rollback_history(&mut self, rollback_id: u64) -> Result<()> {
        // Check if id is valid
        self.load_history()?;
        let _record = self.history.iter().find(|r| r.id == rollback_id).ok_or_else(|| anyhow!("No such history record"))?;

        // symlink profile-current to profile-id
        let profile_current = format!("{}/{}/profile-current", paths::instance.epkg_envs_root.display(), self.options.env);
        let profile_history = format!("{}/{}/profile-{}", paths::instance.epkg_envs_root.display(), self.options.env, rollback_id);
        fs::remove_file(&profile_current)?;
        symlink(&profile_history, &profile_current)?;

        // Remove profile dir between id and last id
        for i in rollback_id+1..self.history.last().unwrap().id+1 {
            let profile = format!("{}/{}/profile-{}", paths::instance.epkg_envs_root.display(), self.options.env, i);
            fs::remove_dir_all(&profile)?;
        }

        // Remove history record between id and last id
        self.history.retain(|r| r.id <= rollback_id);
        self.save_history()?;

        Ok(())
    }
}