use std::fs;
use anyhow::{Context, Result};
use crate::models::*;

// ======================================================================================
// `epkg list` - Search and List Packages
// ======================================================================================
//
// DESCRIPTION:
//   The `epkg list` command searches for packages in the configured repositories and
//   displays their details in a formatted table. It supports simple glob-like patterns
//   for filtering package names.
//
// USAGE:
//   epkg list [PATTERN]
//
// ARGUMENTS:
//   PATTERN:
//     A pattern to filter package names. The pattern can be in one of the following forms:
//
//     1. `xxx`:
//        - Matches packages whose names contain the substring "xxx".
//        - Example: `epkg list podman` matches "podman", "podman-gvproxy", etc.
//
//     2. `*xxx`:
//        - Matches packages whose names end with "xxx".
//        - Example: `epkg list *selinux` matches "pcp-selinux", "audit-selinux", etc.
//
//     3. `xxx*`:
//        - Matches packages whose names start with "xxx".
//        - Example: `epkg list texlive*` matches "texlive-xdvi", "texlive-meetingmins", etc.
//
// OUTPUT FORMAT:
//   The command outputs a table with the following columns:
//
//   - Channel: The channel name (e.g., "openEuler-24.09").
//   - Repo: The repository name (e.g., "everything").
//   - Package: The package name (e.g., "texlive-xdvi").
//   - Version-Release: The package version and release (e.g., "20210325-8").
//   - Hash: The unique hash of the package (e.g., "ZHNXZNVU2HAGMX4FBAFGF4JVH3LGZB2J").
//
//   Example Output:
//   ```
//   Channel                Repo             Package               Version-Release      Hash
//   ============================================================================================
//   openEuler-24.09        everything       texlive-xdvi          20210325-8           ZHNXZNVU2HAGMX4FBAFGF4JVH3LGZB2J
//   openEuler-24.09        everything       pcp-selinux           6.2.2-2              ZHBBEFHR6TO7BWWH7JFAXAKNKKTFJX67
//   openEuler-24.09        everything       podman-gvproxy        4.9.4-8              ZHCF7QQHK2H35B6ME65EPIARWNCZS7LL
//   ```
//
// EXAMPLES:
//   1. List all packages containing "podman":
//      ```
//      epkg list podman
//      ```
//
//   2. List all packages ending with "selinux":
//      ```
//      epkg list *selinux
//      ```
//
//   3. List all packages starting with "texlive":
//      ```
//      epkg list texlive*
//      ```
//
//   4. List all packages (no filter):
//      ```
//      epkg list
//      ```
//
// NOTES:
//   - The command reads package data from the `store-paths` files in the configured
//     repositories.
//   - The pattern matching is case-sensitive.
//   - If no pattern is provided, all packages are listed.
//
// ERROR HANDLING:
//   - If a `store-paths` file cannot be read, an error message is displayed, and the
//     command continues processing the remaining files.
//   - If no packages match the pattern, no output is displayed.
//
// SEE ALSO:
//   - `epkg install`: Install a package.
//   - `epkg remove`: Remove a package.
//   - `epkg update`: Update the package database.
//
// ======================================================================================

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

        let channel_name = self.get_channel_config(self.options.env.clone())?.name.clone();
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
                                channel_name,
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
        
        if !header_printed {
            eprintln!("No packages found matching the pattern: {},  in the repo: {}", glob_pattern, channel_name);
        }

        Ok(())
    }
}
