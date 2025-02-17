use std::fs;
use anyhow::Result;
use anyhow::Context;
use crate::models::*;
use crate::paths;

impl PackageManager {
    pub fn print_history(&self) {
        self.load_history()?;
        for record in &self.history {
            println!("{}|{}|{}|{}", record.id, record.timestamp, record.action, record.packages.join(" "));
        }
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

    pub fn record_history(&mut self, action: &str) -> Result<()> {
        self.load_history()?;
        Ok(())
    }
}