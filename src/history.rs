use std::fs;
use anyhow::Result;
use anyhow::Context;
use crate::models::*;
use crate::paths;

impl PackageManager {
    pub fn print_history(&mut self) -> Result<()> {
        self.load_history()?;
        for record in &self.history {
            println!("{}|{}|{}|{}", record.id, record.timestamp, record.action, record.packages.join(" "));
        }

        Ok(())
    }

    pub fn load_history(&mut self) -> Result<()> {
        let file_path = format!("{}/{}/.history", paths::instance.epkg_envs_root.display(), self.options.env,);
        let contents = fs::read_to_string(&file_path).with_context(|| format!("Failed to read file: {}", file_path))?;

        self.history = contents
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    return None;
                }
                let parts: Vec<&str> = trimmed.split('|').collect();
                if parts.len() != 4 {
                    return None;
                }
                Some(HistoryRecord {
                    id: parts[0].parse::<u64>().unwrap(),
                    timestamp: parts[1].to_string(),
                    action: parts[2].to_string(),
                    packages: parts[3].split_whitespace().map(|s| s.to_string()).collect(),
                })
            })
            .collect();

        Ok(())
    }

    pub fn save_history(&self) -> Result<()> {
        let file_path = format!("{}/{}/.history", paths::instance.epkg_envs_root.display(), self.options.env,);
        let contents = self.history.iter().map(|record| {
            format!("{}|{}|{}|{}", record.id, record.timestamp, record.action, record.packages.join(" "))
        }).collect::<Vec<String>>().join("\n");
        fs::write(&file_path, contents).with_context(|| format!("Failed to write file: {}", file_path))?;

        Ok(())
    }

    pub fn record_history(&mut self, action: &str, packages: Vec<String>) -> Result<()> {
        self.load_history()?;
        // if history is empty, set id to 1, otherwise set id to the last id + 1
        let id = if self.history.is_empty() {
            1
        } else {
            self.history.last().unwrap().id + 1
        };
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
}