use std::fs;
use std::path::Path;
use anyhow::{Context, Result};
use crate::models::*;

macro_rules! LIST_OUTPUT_FORMAT {
    () => {
        "{:<24} {:<16} {:<24} {:<20} {}"
    };
}

impl PackageManager {
    pub fn list_packages(&mut self, glob_pattern: &str) -> Result<()> {
        self.load_store_paths()?;

        // Determine the search pattern based on the glob syntax
        let (prefix, suffix) = match glob_pattern {
            p if p.starts_with('*') => (None, Some(&p[1..])), // '*xxx' => search for suffix
            p if p.ends_with('*') => (Some(&p[..p.len() - 1]), None), // 'xxx*' => search for prefix
            _ => (Some(glob_pattern), Some(glob_pattern)), // 'xxx' => search for exact match
        };

        let mut header_printed = false;

        // Iterate over all repositories
        for repodata in &self.repos_data {
            for entry in &repodata.store_paths {
                // Construct the file path
                let file_path = format!(
                    "{}/{}",
                    repodata.dir,
                    entry.filename.strip_suffix(".zst").unwrap_or(&entry.filename)
                );

                // Read the file contents
                let contents = fs::read_to_string(&file_path)
                    .with_context(|| format!("Failed to load store-paths from {}", file_path))?;

                // Process each line in the file
                for line in contents.lines() {
                    let parts: Vec<&str> = line.split("__").collect();
                    if parts.len() >= 4 {
                        let hash = parts[0];
                        let pkgname = parts[1];
                        let version_release = parts[2..].join("__");

                        // Check if the package name matches the pattern
                        let matches = match (prefix, suffix) {
                            (Some(pre), Some(suf)) => pkgname.contains(pre) && pkgname.contains(suf),
                            (Some(pre), None) => pkgname.starts_with(pre),
                            (None, Some(suf)) => pkgname.ends_with(suf),
                            _ => false,
                        };

                        if matches {
                            // Print the header if not already printed
                            if !header_printed {
                                header_printed = true;
                                println!(
                                    LIST_OUTPUT_FORMAT!(),
                                    "Channel", "Repo", "Package", "Version-Release", "Hash"
                                );
                                println!("{}", "-".repeat(120)); // Separator line
                            }

                            // Print the package details
                            println!(
                                LIST_OUTPUT_FORMAT!(),
                                self.env_config.channel.name,
                                repodata.name,
                                pkgname,
                                version_release,
                                hash
                            );
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
