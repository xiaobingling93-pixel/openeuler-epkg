use anyhow::{Context, Result};
use grep::regex::RegexMatcher;
use grep::searcher::Searcher;
use grep::searcher::sinks::UTF8;
use std::fs;
use crate::models::*;

macro_rules! LIST_OUTPUT_FORMAT {
    () => {
        "{:<24} {:<16} {:<24} {:<20} {}"
    };
}

impl PackageManager {

    pub fn list_packages(&mut self, glob_pattern: &str) -> Result<()> {
        self.load_store_paths()?;

        // Convert the pattern to a regex based on the rules
        let regex_pattern = match glob_pattern {
            p if p.starts_with('*') => format!("{}__", &p[1..]), // '*xxx' => 'xxx__'
            p if p.contains('*') => format!("__{}__", p.replace('*', ".*")), // 'xx*x' => '__xx.*x__'
            _ => format!("__{}__", glob_pattern), // 'xxx' => '__xxx__'
        };

        // Compile the regex matcher
        let matcher = RegexMatcher::new(&regex_pattern)?;
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

                // Create a Searcher
                let mut searcher = Searcher::new();

                // Use UTF8 sink to handle the search results
                let mut matches = Vec::new();
                let sink = UTF8(|_line_num, line| {
                    matches.push(line.to_string());
                    Ok(true)
                });

                // Perform the search
                searcher.search_slice(&matcher, contents.as_bytes(), sink)?;

                // Print the matches in the desired format
                for line in matches {
                    let parts: Vec<&str> = line.split("__").collect();
                    if parts.len() >= 4 {
                        let hash = parts[0];
                        let pkgname = parts[1];
                        let mut version_release = parts[2..].join("__");
                        if version_release.ends_with('\n') { version_release.pop(); }

                        // Print the header
                        if !header_printed {
                            header_printed = true;
                            println!(
                                LIST_OUTPUT_FORMAT!(),
                                "Channel", "Repo", "Package", "Version-Release", "Hash"
                            );
                            println!("{}", "-".repeat(120)); // Separator line
                        }

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

        Ok(())
    }
}
