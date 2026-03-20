//! Auto load 3rd party RPM repos in $env_root/etc/yum.repos.d/*.repo
//! - official repos are skipped there, since we only use $env_root/etc/epkg/channel.yaml for official sources
//! - metalink repos are skipped, since they are merely useful for official repos
//! - mirrorlist is not supported for the same reason

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use color_eyre::eyre::{Result, WrapErr};
use crate::models::{RepoConfig, ChannelConfig, PackageFormat};
use glob;
use crate::lfs;

/// Parse a single RPM repository configuration section
/// Format:
/// [repo-name]
/// name=Repository Name
/// baseurl=http://example.com/repo/$releasever/$basearch/
/// enabled=1
/// gpgcheck=1
fn parse_repo_section(content: &str) -> Result<HashMap<String, RepoConfig>> {
    let mut repos = HashMap::new();
    let mut current_repo: Option<(String, HashMap<String, String>)> = None;

    for line in content.lines() {
        let line = line.trim();

        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        // Check if this is a section header
        if line.starts_with('[') && line.ends_with(']') {
            // Save previous repo if exists
            if let Some((name, config)) = current_repo.take() {
                if let Some(repo_config) = create_repo_config_from_section(&config) {
                    repos.insert(name, repo_config);
                }
            }

            // Start new section
            let section_name = line[1..line.len()-1].to_string();
            current_repo = Some((section_name, HashMap::new()));
            continue;
        }

        // Parse key=value pairs
        if let Some((key, value)) = line.split_once('=') {
            if let Some((_, ref mut config)) = current_repo.as_mut() {
                config.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
    }

    // Save the last repo if exists
    if let Some((name, config)) = current_repo.take() {
        if let Some(repo_config) = create_repo_config_from_section(&config) {
            repos.insert(name, repo_config);
        }
    }

    Ok(repos)
}

fn create_repo_config_from_section(config: &HashMap<String, String>) -> Option<RepoConfig> {
    // Check if repo is enabled
    let enabled = match config.get("enabled") {
        Some(val) => {
            let val_lower = val.to_lowercase();
            // DNF5 considers these as false: "0", "no", "false", "off"
            // Everything else is considered true
            !(val_lower == "0" || val_lower == "no" || val_lower == "false" || val_lower == "off")
        },
        None => true, // Default to enabled if not specified
    };

    // Get baseurl - this is the most important field
    let baseurl = config.get("baseurl")?;
    if baseurl.is_empty() {
        return None;
    }

    // For RPM repos, the index_url typically points to repodata/repomd.xml
    let package_baseurl = baseurl.trim_end_matches('/').to_string();
    let index_url = format!("{}/repodata/repomd.xml", package_baseurl);

    Some(RepoConfig {
        enabled,
        index_url,
        package_baseurl,
        ..RepoConfig::default()
    })
}

fn parse_repo_file(path: &Path) -> Result<ChannelConfig> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read repo file: {}", path.display()))?;

    let repo_configs = parse_repo_section(&content)?;

    let mut repos = HashMap::new();
    for (section_name, repo_config) in repo_configs {
        repos.insert(section_name, repo_config);
    }

    let channel_config = ChannelConfig {
        format: PackageFormat::Rpm,
        repos,
        file_path: path.to_string_lossy().to_string(),
        ..ChannelConfig::default()
    };

    Ok(channel_config)
}

/// Load repository configurations from env_root/etc/yum.repos.d/*.repo files as ChannelConfig instances
pub fn load_rpm_system_repos(env_root: &Path) -> Result<Vec<ChannelConfig>> {
    let mut all_channel_configs = Vec::new();
    let repos_dir = crate::dirs::path_join(env_root, &["etc", "yum.repos.d"]);
    let pattern = repos_dir.join("*.repo");

    if !lfs::exists_on_host(&repos_dir) {
        return Ok(all_channel_configs);
    }

    for entry in glob::glob(&pattern.to_string_lossy())? {
        match entry {
            Ok(path) => {

                // Fedora official repos need no special handling, since they all uses only
                // 'metalink' which will be auto skipped:
                // % ls /home/wfg/.epkg/envs/fedora/etc/yum.repos.d/
                // fedora-cisco-openh264.repo  fedora.repo  fedora-updates.repo  fedora-updates-testing.repo
                let filename = path.file_name().and_then(|n| n.to_str());
                if matches!(filename, Some("openEuler.repo")) {
                    continue;
                }

                match parse_repo_file(&path) {
                    Ok(channel_config) => {
                        all_channel_configs.push(channel_config);
                    }
                    Err(e) => {
                        eprintln!("Warning: Failed to parse {}: {}", path.display(), e);
                    }
                }
            }
            Err(e) => {
                eprintln!("Warning: Failed to read glob entry: {}", e);
            }
        }
    }

    Ok(all_channel_configs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_repo_section() {
        let content = r#"[fedora]
name=Fedora $releasever - $basearch
baseurl=http://download.fedoraproject.org/pub/fedora/linux/releases/$releasever/Everything/$basearch/os/
enabled=1
gpgcheck=1

[updates]
name=Fedora $releasever - $basearch - Updates
baseurl=http://download.fedoraproject.org/pub/fedora/linux/updates/$releasever/Everything/$basearch/
enabled=1
gpgcheck=1

[disabled-repo]
name=Disabled Repository
baseurl=http://example.com/repo/
enabled=0
"#;

        let result = parse_repo_section(content);
        assert!(result.is_ok());
        let repos = result.unwrap();

        // Should have 3 repos (including disabled)
        assert_eq!(repos.len(), 3);

        // Check fedora repo
        let fedora_config = repos.get("fedora").unwrap();
        assert!(fedora_config.enabled);
        assert_eq!(fedora_config.package_baseurl, "http://download.fedoraproject.org/pub/fedora/linux/releases/$releasever/Everything/$basearch/os");
        assert_eq!(fedora_config.index_url, "http://download.fedoraproject.org/pub/fedora/linux/releases/$releasever/Everything/$basearch/os/repodata/repomd.xml".to_string());

        // Check updates repo
        let updates_config = repos.get("updates").unwrap();
        assert!(updates_config.enabled);
        assert_eq!(updates_config.package_baseurl, "http://download.fedoraproject.org/pub/fedora/linux/updates/$releasever/Everything/$basearch");

        // Check disabled repo
        let disabled_config = repos.get("disabled-repo").unwrap();
        assert!(!disabled_config.enabled);
        assert_eq!(disabled_config.package_baseurl, "http://example.com/repo");
        assert_eq!(disabled_config.index_url, "http://example.com/repo/repodata/repomd.xml".to_string());
    }

    #[test]
    fn test_create_repo_config_from_section() {
        let mut config = HashMap::new();
        config.insert("baseurl".to_string(), "http://example.com/repo/".to_string());
        config.insert("enabled".to_string(), "1".to_string());
        config.insert("name".to_string(), "Test Repo".to_string());

        let result = create_repo_config_from_section(&config);
        assert!(result.is_some());
        let repo_config = result.unwrap();
        assert!(repo_config.enabled);
        assert_eq!(repo_config.package_baseurl, "http://example.com/repo");
        assert_eq!(repo_config.index_url, "http://example.com/repo/repodata/repomd.xml".to_string());

        // Test disabled repo
        config.insert("enabled".to_string(), "0".to_string());
        let result = create_repo_config_from_section(&config);
        assert!(result.is_some());
        let disabled_repo_config = result.unwrap();
        assert!(!disabled_repo_config.enabled);
        assert_eq!(disabled_repo_config.package_baseurl, "http://example.com/repo");
        assert_eq!(disabled_repo_config.index_url, "http://example.com/repo/repodata/repomd.xml".to_string());

        // Test missing baseurl
        let mut config_no_baseurl = HashMap::new();
        config_no_baseurl.insert("enabled".to_string(), "1".to_string());
        let result = create_repo_config_from_section(&config_no_baseurl);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_repo_section_with_semicolon_comments() {
        let content = r#"; This is a comment
[fedora]
name=Fedora $releasever - $basearch
; Another comment
baseurl=http://download.fedoraproject.org/pub/fedora/linux/releases/$releasever/Everything/$basearch/os/
enabled=1
gpgcheck=1

[updates]
name=Fedora $releasever - $basearch - Updates
baseurl=http://download.fedoraproject.org/pub/fedora/linux/updates/$releasever/Everything/$basearch/
enabled=1
"#;

        let result = parse_repo_section(content);
        assert!(result.is_ok());
        let repos = result.unwrap();
        assert_eq!(repos.len(), 2);
        assert!(repos.contains_key("fedora"));
        assert!(repos.contains_key("updates"));
    }

    #[test]
    fn test_boolean_values_for_enabled() {
        // Test all false values
        let false_values = vec!["0", "no", "false", "off", "NO", "FALSE", "OFF"];

        for false_val in false_values {
            let mut config = HashMap::new();
            config.insert("baseurl".to_string(), "http://example.com/repo/".to_string());
            config.insert("enabled".to_string(), false_val.to_string());

            let result = create_repo_config_from_section(&config);
            assert!(result.is_some(), "Expected repo config to be created for value: {}", false_val);
            let repo_config = result.unwrap();
            assert!(!repo_config.enabled, "Expected repo to be disabled for value: {}", false_val);
            assert_eq!(repo_config.package_baseurl, "http://example.com/repo");
        }

        // Test true values (anything not in false list)
        let true_values = vec!["1", "yes", "true", "on", "YES", "TRUE", "ON", "enabled", "2"];

        for true_val in true_values {
            let mut config = HashMap::new();
            config.insert("baseurl".to_string(), "http://example.com/repo/".to_string());
            config.insert("enabled".to_string(), true_val.to_string());

            let result = create_repo_config_from_section(&config);
            assert!(result.is_some(), "Expected repo to be enabled for value: {}", true_val);
            assert!(result.unwrap().enabled);
        }

        // Test default (no enabled key)
        let mut config = HashMap::new();
        config.insert("baseurl".to_string(), "http://example.com/repo/".to_string());

        let result = create_repo_config_from_section(&config);
        assert!(result.is_some());
        assert!(result.unwrap().enabled);
    }

    #[test]
    fn test_malformed_input() {
        // Test empty baseurl
        let mut config = HashMap::new();
        config.insert("baseurl".to_string(), "".to_string());
        config.insert("enabled".to_string(), "1".to_string());

        let result = create_repo_config_from_section(&config);
        assert!(result.is_none());

        // Test missing equals sign (should be handled gracefully)
        let content = r#"[repo]
baseurl http://example.com
enabled=1
"#;
        let result = parse_repo_section(content);
        assert!(result.is_ok());
        // Should only parse the enabled line, ignore malformed baseurl
        let repos = result.unwrap();
        assert_eq!(repos.len(), 0); // No valid repos due to missing baseurl
    }
}
