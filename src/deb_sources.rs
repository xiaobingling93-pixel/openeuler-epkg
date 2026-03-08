//! Auto load 3rd party DEB repos in $env_root/etc/apt/sources.list.d/
//! - official repos are skipped there, since we only use $env_root/etc/epkg/channel.yaml for official sources
//! - deb-src type sections are auto skipped

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use color_eyre::eyre::{Result, eyre};
use crate::models::{RepoConfig, ChannelConfig, PackageFormat};
use glob;
use crate::lfs;

/// Substitute variables in a string (similar to APT's SubstVar)
/// Currently supports $(ARCH) substitution
fn subst_var(input: &str, arch: &str) -> String {
    input.replace("$(ARCH)", arch)
}

/// Parse traditional sources.list format
/// Format: deb [options] uri suite [component1] [component2] [...]
fn parse_sources_list_line(line: &str, arch: &str) -> Result<Option<RepoConfig>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }

    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 3 || parts[0] != "deb" {
        return Err(eyre!("Malformed line: expected 'deb' type"));
    }

    let mut options = std::collections::HashMap::new();
    let mut start_idx = 1;

    // Parse options if present (they start with '[' and end with ']')
    if parts.len() > 1 && parts[1].starts_with('[') {
        let mut bracket_end = 1;
        let mut option_str = String::new();

        // Collect all parts that are part of the options block
        while bracket_end < parts.len() {
            option_str.push_str(parts[bracket_end]);
            option_str.push(' ');

            if parts[bracket_end].ends_with(']') {
                break;
            }
            bracket_end += 1;
        }

        if bracket_end >= parts.len() || !option_str.trim().ends_with(']') {
            return Err(eyre!("Malformed options: missing closing bracket"));
        }

        // Remove the brackets and parse individual options
        let option_content = &option_str.trim()[1..option_str.trim().len()-1]; // Remove [ and ]
        let option_parts: Vec<&str> = option_content.split_whitespace().collect();

        for option in option_parts {
            if let Some(eq_pos) = option.find('=') {
                let key = option[..eq_pos].to_string();
                let value = option[eq_pos+1..].to_string();
                if key.is_empty() {
                    return Err(eyre!("Malformed option: empty key"));
                }
                if value.is_empty() {
                    return Err(eyre!("Malformed option: empty value"));
                }
                options.insert(key, value);
            } else {
                return Err(eyre!("Malformed option: missing '='"));
            }
        }

        start_idx = bracket_end + 1;
    }

    if start_idx + 1 >= parts.len() {
        return Err(eyre!("Malformed line: missing URI or suite"));
    }

    let uri = subst_var(parts[start_idx], arch);
    let suite = subst_var(parts[start_idx + 1], arch);

    // Check for absolute suite specification (ending with '/')
    let components = if suite.ends_with('/') {
        // For absolute suites, no components should be specified
        if start_idx + 2 < parts.len() {
            return Err(eyre!("Malformed absolute suite: components not allowed"));
        }
        Vec::new()
    } else {
        parts[start_idx + 2..].iter().map(|s| s.to_string()).collect()
    };

    // Construct the index URL similar to deb repo format
    let package_baseurl = uri.trim_end_matches('/').to_string();
    let index_url = if suite.ends_with('/') {
        // Absolute suite specification
        format!("{}/{}Release", package_baseurl, suite)
    } else {
        // Regular suite with dists/
        format!("{}/dists/{}/Release", package_baseurl, suite)
    };

    Ok(Some(RepoConfig {
        index_url,
        package_baseurl,
        components,
        ..RepoConfig::default()
    }))
}

/// Validate and process a deb822 stanza if it has all required fields
fn try_add_stanza(current_repo: &mut HashMap<String, String>, repos: &mut Vec<RepoConfig>, arch: &str, path: &Path) {
    let has_uris = current_repo.contains_key("URIs");
    let has_suites = current_repo.contains_key("Suites");

    if !has_uris || !has_suites {
        // Assume it's an accidental empty line in one section - skip silently
        return;
    }

    repos.extend(extract_repos_from_deb822(current_repo, arch, path));
    current_repo.clear();
}

/// Parse deb822 format (.sources files)
/// Format:
/// Types: deb
/// URIs: http://example.com/debian/
/// Suites: stable
/// Components: main contrib
fn parse_deb822_content(content: &str, arch: &str, path: &Path) -> Result<Vec<RepoConfig>> {
    let mut repos = Vec::new();
    let mut current_repo: HashMap<String, String> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();

        if line.starts_with('#') {
            continue;
        }

        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();

            if key.is_empty() {
                // Continuation of previous line
                if let Some(last_value) = current_repo.values_mut().last() {
                    last_value.push(' ');
                    last_value.push_str(value);
                }
                continue;
            }

            // Check for conflicting keys in the same stanza
            if current_repo.contains_key(key) {
                eprintln!("Warning: {}: duplicate key '{}', overwriting old value", path.display(), key);
            }
            current_repo.insert(key.to_string(), value.to_string());
        } else if line.is_empty() {
            try_add_stanza(&mut current_repo, &mut repos, arch, path);
        }
    }

    // Handle the last stanza
    try_add_stanza(&mut current_repo, &mut repos, arch, path);

    Ok(repos)
}

/// Parse Types field and validate: accept "deb" types, skip single "deb-src" item
fn validate_deb822_types(types: &str) -> bool {
    let type_list: Vec<&str> = types.split_whitespace().collect();

    // Accept if contains "deb", skip if single "deb-src", skip other types
    type_list.contains(&"deb") && type_list != ["deb-src"]
}

/// Extract and validate basic fields from deb822 repo data
fn parse_deb822_fields(repo_data: &HashMap<String, String>) -> Option<(String, String, Vec<String>)> {
    let _types = repo_data.get("Types")?;
    let uris = repo_data.get("URIs")?;
    let suites = repo_data.get("Suites")?;
    let empty_string = String::new();
    let components_str = repo_data.get("Components").unwrap_or(&empty_string);

    let component_list: Vec<String> = if components_str.is_empty() {
        Vec::new()
    } else {
        components_str.split_whitespace().map(|s| s.to_string()).collect()
    };

    Some((uris.clone(), suites.clone(), component_list))
}

/// Extract options from deb822 format
fn extract_deb822_options(repo_data: &HashMap<String, String>) -> std::collections::HashMap<String, String> {
    let mut options = std::collections::HashMap::new();

    // List of options that can have +/- modifiers (like APT_PLUSMINUS in C++ code)
    let plusminus_options = ["Architectures", "Languages", "Targets"];
    for &opt in &plusminus_options {
        if let Some(value) = repo_data.get(opt) {
            options.insert(opt.to_lowercase(), value.clone());
        }
        // Handle +/- variants
        if let Some(value) = repo_data.get(&format!("{}-Add", opt)) {
            options.insert(format!("{}_add", opt.to_lowercase()), value.clone());
        }
        if let Some(value) = repo_data.get(&format!("{}-Remove", opt)) {
            options.insert(format!("{}_remove", opt.to_lowercase()), value.clone());
        }
    }

    // Simple options without modifiers
    let simple_options = [
        "Trusted", "Check-Valid-Until", "Valid-Until-Min", "Valid-Until-Max",
        "Check-Date", "Date-Max-Future", "Snapshot", "Signed-By",
        "PDiffs", "By-Hash", "Include", "Exclude", "Enabled"
    ];
    for &opt in &simple_options {
        if let Some(value) = repo_data.get(opt) {
            options.insert(opt.to_lowercase().replace("-", "_"), value.clone());
        }
    }

    options
}

/// Generate repository configurations from parsed deb822 data
fn generate_repos_from_deb822_data(
    uris: &str,
    suites: &str,
    component_list: &[String],
    options: &std::collections::HashMap<String, String>,
    arch: &str,
    path: &Path
) -> Vec<RepoConfig> {
    // Parse enabled option: defaults to true, set to false only if explicitly "no"
    let enabled = options.get("enabled")
        .map(|v| v.to_lowercase() == "yes")
        .unwrap_or(true);

    // Collect all URIs (treating them as mirrors/alternatives)
    let all_uris: Vec<String> = uris.split_whitespace()
        .map(|uri_str| subst_var(uri_str, arch))
        .collect();

    if all_uris.is_empty() {
        eprintln!("Warning: {}: No URIs specified in repository configuration", path.display());
        return Vec::new();
    }

    // Generate combinations of Suite × Component, with all URIs as alternatives
    let mut repos = Vec::new();

    for suite_str in suites.split_whitespace() {
        let suite = subst_var(suite_str, arch);

        // Always use first URI as primary, rest as alternatives
        let (primary_uri, alternatives) = all_uris.split_first().unwrap();

        let package_baseurl = primary_uri.trim_end_matches('/').to_string();

        let index_url = if suite.ends_with('/') {
            // Absolute suite - no components allowed
            if !component_list.is_empty() {
                // This would be an error in APT, but we'll skip it for now
                eprintln!("Warning: {}: Components specified for absolute suite '{}', skipping", path.display(), suite);
                continue;
            }
            format!("{}/{}Release", package_baseurl, suite)
        } else {
            // Regular suite - include all components
            format!("{}/dists/{}/Release", package_baseurl, suite)
        };

        repos.push(RepoConfig {
            enabled,
            index_url,
            package_baseurl,
            alternative_baseurls: alternatives.to_vec(),
            components: component_list.to_vec(),
            ..RepoConfig::default()
        });
    }

    repos
}

fn extract_repos_from_deb822(repo_data: &HashMap<String, String>, arch: &str, path: &Path) -> Vec<RepoConfig> {
    // Check if this stanza contains deb types
    if !repo_data.get("Types").is_some_and(|types| validate_deb822_types(types)) {
        return Vec::new();
    }

    // Parse basic fields
    let (uris, suites, component_list) = match parse_deb822_fields(repo_data) {
        Some(fields) => fields,
        None => return Vec::new(),
    };

    // Extract options
    let options = extract_deb822_options(repo_data);

    // Generate repository configurations
    generate_repos_from_deb822_data(&uris, &suites, &component_list, &options, arch, path)
}

/// Process a single source file and return channel configurations
fn process_single_source_file(path: &Path, parser: fn(&Path, &str) -> Result<Vec<RepoConfig>>, arch: &str) -> Result<Vec<ChannelConfig>> {
    // Ignore official debian sources as they conflict with our official config in
    // $env_root/etc/epkg/channel.yaml
    let filename = path.file_name().and_then(|n| n.to_str());
    if matches!(filename, Some("debian.sources") | Some("ubuntu.sources")) {
        return Ok(Vec::new());
    }

    let mut channel_configs = Vec::new();

    if let Some(filename) = path.file_stem() {
        let repo_name = filename.to_string_lossy().to_string();
        match parser(path, arch) {
            Ok(repo_configs) => {
                let repo_count = repo_configs.len();
                for (i, repo_config) in repo_configs.into_iter().enumerate() {
                    let repo_name_key = if repo_count == 1 {
                        repo_name.clone()
                    } else {
                        format!("{}-{}", repo_name, i)
                    };
                    let mut repos = HashMap::new();
                    repos.insert(repo_name_key, repo_config);

                    let channel_config = ChannelConfig {
                        format: PackageFormat::Deb,
                        distro: "debian".to_string(), // Default, will be overridden
                        repos,
                        file_path: path.to_string_lossy().to_string(),
                        ..ChannelConfig::default()
                    };
                    channel_configs.push(channel_config);
                }
            }
            Err(e) => {
                eprintln!("Warning: Failed to parse {}: {}", path.display(), e);
            }
        }
    }

    Ok(channel_configs)
}

/// Load repository configurations from apt sources files using glob patterns
///
/// Creates one ChannelConfig per repository instead of grouping multiple repos per file.
/// This avoids channel_config.repos[] hash key conflicts.
///
/// For 3rd party files under sources.list.d/, always use file basename as repo_name.
/// When the sources file contains several suites or components, they'll share the same
/// RepoReleaseItem::RepoRevise::repo_name, but distinguish by different repodata_name.
fn load_apt_sources_with_glob(env_root: &Path, glob_pattern: &str, parser: fn(&Path, &str) -> Result<Vec<RepoConfig>>, arch: &str) -> Result<Vec<ChannelConfig>> {
    let mut channel_configs = Vec::new();
    let sources_dir = env_root.join("etc/apt/sources.list.d");
    let full_pattern = sources_dir.join(glob_pattern);

    if !lfs::exists_on_host(&sources_dir) {
        return Ok(channel_configs);
    }

    for entry in glob::glob(&full_pattern.to_string_lossy())? {
        match entry {
            Ok(path) => {
                match process_single_source_file(&path, parser, arch) {
                    Ok(mut configs) => channel_configs.append(&mut configs),
                    Err(e) => eprintln!("Warning: Failed to process {}: {}", path.display(), e),
                }
            }
            Err(e) => {
                eprintln!("Warning: Failed to read glob entry: {}", e);
            }
        }
    }

    Ok(channel_configs)
}

/// Load repository configurations from env_root/etc/apt/sources.list.d/*.list files
pub fn load_sources_list_configs(env_root: &Path, arch: &str) -> Result<Vec<ChannelConfig>> {
    load_apt_sources_with_glob(env_root, "*.list", parse_sources_list_file, arch)
}

/// Load repository configurations from env_root/etc/apt/sources.list.d/*.sources files (deb822 format)
pub fn load_sources_configs(env_root: &Path, arch: &str) -> Result<Vec<ChannelConfig>> {
    load_apt_sources_with_glob(env_root, "*.sources", parse_sources_file, arch)
}


fn parse_sources_list_file(path: &Path, arch: &str) -> Result<Vec<RepoConfig>> {
    let content = fs::read_to_string(path)?;
    let mut repos = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        match parse_sources_list_line(line, arch) {
            Ok(Some(repo)) => repos.push(repo),
            Ok(None) => {} // Comment or empty line
            Err(e) => {
                eprintln!("Warning: {}:{}: {}", path.display(), line_num + 1, e);
            }
        }
    }

    Ok(repos)
}

fn parse_sources_file(path: &Path, arch: &str) -> Result<Vec<RepoConfig>> {
    let content = fs::read_to_string(path)?;
    parse_deb822_content(&content, arch, path)
}

/// Load all deb repository configurations from system sources as ChannelConfig instances
pub fn load_deb_system_repos(env_root: &Path, arch: &str) -> Result<Vec<ChannelConfig>> {
    // Load from .list and .sources files with error handling
    let loaders = [
        (load_sources_list_configs as fn(&Path, &str) -> Result<Vec<ChannelConfig>>, "Failed to load sources.list configs"),
        (load_sources_configs, "Failed to load sources configs"),
    ];

    let mut all_channel_configs = Vec::new();
    for (loader, error_msg) in loaders {
        match loader(env_root, arch) {
            Ok(channel_configs) => all_channel_configs.extend(channel_configs),
            Err(e) => eprintln!("Warning: {}: {}", error_msg, e),
        }
    }

    Ok(all_channel_configs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sources_list_line() {
        let arch = "amd64";

        // Test valid deb line
        let line = "deb http://deb.debian.org/debian/ stable main contrib";
        let result = parse_sources_list_line(line, arch).unwrap();
        assert!(result.is_some());
        let repo = result.unwrap();
        assert_eq!(repo.package_baseurl, "http://deb.debian.org/debian");
        assert_eq!(repo.index_url, "http://deb.debian.org/debian/dists/stable/Release".to_string());
        assert_eq!(repo.components, vec!["main", "contrib"]);
        assert!(repo.alternative_baseurls.is_empty());

        // Test line with options (options are currently ignored in RepoConfig)
        let line_with_options = "deb [arch=amd64] http://deb.debian.org/debian/ testing main";
        let result = parse_sources_list_line(line_with_options, arch).unwrap();
        assert!(result.is_some());
        let repo = result.unwrap();
        assert_eq!(repo.package_baseurl, "http://deb.debian.org/debian");
        assert_eq!(repo.index_url, "http://deb.debian.org/debian/dists/testing/Release".to_string());
        assert_eq!(repo.components, vec!["main"]);
        assert!(repo.alternative_baseurls.is_empty());

        // Test variable substitution
        let line_with_vars = "deb http://deb.debian.org/debian/ $(ARCH) main";
        let result = parse_sources_list_line(line_with_vars, arch).unwrap();
        assert!(result.is_some());
        let repo = result.unwrap();
        assert_eq!(repo.package_baseurl, "http://deb.debian.org/debian");
        assert_eq!(repo.index_url, "http://deb.debian.org/debian/dists/amd64/Release".to_string());
        assert_eq!(repo.components, vec!["main"]);
        assert!(repo.alternative_baseurls.is_empty());

        // Test absolute suite specification
        let absolute_suite_line = "deb http://deb.debian.org/debian/ stable/updates/";
        let result = parse_sources_list_line(absolute_suite_line, arch).unwrap();
        assert!(result.is_some());
        let repo = result.unwrap();
        assert_eq!(repo.package_baseurl, "http://deb.debian.org/debian");
        assert_eq!(repo.index_url, "http://deb.debian.org/debian/stable/updates/Release".to_string());
        assert!(repo.components.is_empty());
        assert!(repo.alternative_baseurls.is_empty());

        // Test comment line
        let comment = "# deb http://example.com/debian/ stable main";
        assert!(parse_sources_list_line(comment, arch).unwrap().is_none());

        // Test empty line
        assert!(parse_sources_list_line("", arch).unwrap().is_none());

        // Test non-deb line
        let rpm_line = "rpm http://example.com/repo/ stable main";
        assert!(parse_sources_list_line(rpm_line, arch).is_err());
    }

    #[test]
    fn test_parse_deb822_content() {
        let arch = "amd64";
        let content = r#"Types: deb
URIs: http://deb.debian.org/debian/
Suites: stable
Components: main contrib non-free

Types: deb-src
URIs: http://deb.debian.org/debian/
Suites: stable
Components: main contrib
"#;

        let result = parse_deb822_content(content, arch, Path::new("test"));
        assert!(result.is_ok());
        let repos = result.unwrap();
        assert_eq!(repos.len(), 1); // deb type with all components grouped together

        // Check the repo is for the deb type
        let repo = &repos[0];
        assert_eq!(repo.package_baseurl, "http://deb.debian.org/debian");
        assert_eq!(repo.index_url, "http://deb.debian.org/debian/dists/stable/Release".to_string());
        assert!(repo.alternative_baseurls.is_empty()); // No alternatives in this test

        // Check all components are grouped together
        assert_eq!(repo.components, vec!["main", "contrib", "non-free"]);

        // Test variable substitution in deb822
        let content_with_vars = r#"Types: deb
URIs: http://deb.debian.org/debian/
Suites: $(ARCH)
Components: main
"#;
        let result = parse_deb822_content(content_with_vars, "arm64", Path::new("test"));
        assert!(result.is_ok());
        let repos = result.unwrap();
        assert_eq!(repos.len(), 1);
        let repo = &repos[0];
        assert_eq!(repo.package_baseurl, "http://deb.debian.org/debian");
        assert_eq!(repo.index_url, "http://deb.debian.org/debian/dists/arm64/Release".to_string());
        assert_eq!(repo.components, vec!["main"]);

        // Test deb822 with options (options are currently ignored in RepoConfig)
        let content_with_options = r#"Types: deb
URIs: http://deb.debian.org/debian/
Suites: stable
Components: main
Architectures: amd64 arm64
Trusted: yes
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg
"#;
        let result = parse_deb822_content(content_with_options, "amd64", Path::new("test"));
        assert!(result.is_ok());
        let repos = result.unwrap();
        assert_eq!(repos.len(), 1);
        let repo = &repos[0];
        assert_eq!(repo.package_baseurl, "http://deb.debian.org/debian");
        assert_eq!(repo.index_url, "http://deb.debian.org/debian/dists/stable/Release".to_string());
        assert_eq!(repo.components, vec!["main"]);
        assert!(repo.alternative_baseurls.is_empty());
    }

    #[test]
    fn test_repos_to_configs() {
        // Test regular suite URL construction via parse_sources_list_line
        let line = "deb http://deb.debian.org/debian/ stable main";
        let result = parse_sources_list_line(line, "amd64").unwrap().unwrap();
        assert_eq!(result.index_url, "http://deb.debian.org/debian/dists/stable/Release".to_string());
        assert!(result.alternative_baseurls.is_empty());

        // Test absolute suite URL construction via parse_sources_list_line
        let line = "deb http://deb.debian.org/debian/ stable/updates/";
        let result = parse_sources_list_line(line, "amd64").unwrap().unwrap();
        assert_eq!(result.index_url, "http://deb.debian.org/debian/stable/updates/Release".to_string());
        assert!(result.alternative_baseurls.is_empty());

        // Test with alternatives via deb822 parsing
        let content = r#"Types: deb
URIs: http://deb.debian.org/debian/ http://mirror.com/
Suites: stable
Components: main
"#;
        let result = parse_deb822_content(content, "amd64", Path::new("test")).unwrap();
        assert_eq!(result[0].alternative_baseurls, vec!["http://mirror.com/".to_string()]);
    }

    #[test]
    fn test_parse_sources_list_error_cases() {
        let arch = "amd64";

        // Test malformed options
        let malformed_options = "deb [arch=amd64 http://example.com/ stable main";
        assert!(parse_sources_list_line(malformed_options, arch).is_err());

        // Test empty option key
        let empty_key = "deb [=value] http://example.com/ stable main";
        assert!(parse_sources_list_line(empty_key, arch).is_err());

        // Test empty option value
        let empty_value = "deb [key=] http://example.com/ stable main";
        assert!(parse_sources_list_line(empty_value, arch).is_err());

        // Test option without equals
        let no_equals = "deb [key] http://example.com/ stable main";
        assert!(parse_sources_list_line(no_equals, arch).is_err());

        // Test absolute suite with components (should error)
        let absolute_with_components = "deb http://example.com/ stable/ main";
        assert!(parse_sources_list_line(absolute_with_components, arch).is_err());

        // Test missing suite
        let missing_suite = "deb http://example.com/";
        assert!(parse_sources_list_line(missing_suite, arch).is_err());
    }

    #[test]
    fn test_deb822_multiple_combinations() {
        let arch = "amd64";
        let content = r#"Types: deb
URIs: http://deb.debian.org/debian/ http://archive.ubuntu.com/ubuntu/
Suites: focal stable
Components: main contrib
"#;

        let result = parse_deb822_content(content, arch, Path::new("test"));
        assert!(result.is_ok());
        let repos = result.unwrap();

        // Should generate: 2 Suites, each with 2 Components = 2 repos (URIs are now alternatives)
        assert_eq!(repos.len(), 2);

        // Check that all repos use the first URI as primary and have the second as alternative
        for repo in &repos {
            assert_eq!(repo.package_baseurl, "http://deb.debian.org/debian"); // First URI is primary
            assert_eq!(repo.alternative_baseurls, vec!["http://archive.ubuntu.com/ubuntu/".to_string()]); // Second URI is alternative
        }

        // Check the suite/component combinations
        let expected_combinations = vec![
            ("focal", vec!["main", "contrib"]),
            ("stable", vec!["main", "contrib"]),
        ];

        for (i, (expected_suite, expected_components)) in expected_combinations.iter().enumerate() {
            let repo = &repos[i];
            assert_eq!(repo.index_url, format!("http://deb.debian.org/debian/dists/{}/Release", expected_suite));
            assert_eq!(repo.components, expected_components.iter().map(|s| s.to_string()).collect::<Vec<_>>());
            assert_eq!(repo.alternative_baseurls, vec!["http://archive.ubuntu.com/ubuntu/".to_string()]);
        }
    }

    #[test]
    fn test_deb822_type_filtering() {
        let arch = "amd64";
        let content = r#"Types: deb deb-src rpm
URIs: http://example.com/
Suites: stable
Components: main
"#;

        let result = parse_deb822_content(content, arch, Path::new("test"));
        assert!(result.is_ok());
        let repos = result.unwrap();

        // Should process stanza containing 'deb' even with 'deb-src' and 'rpm'
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].package_baseurl, "http://example.com"); // URI
        assert_eq!(repos[0].index_url, "http://example.com/dists/stable/Release".to_string()); // Suite
        assert_eq!(repos[0].components, vec!["main"]); // Components

        // Test that single 'deb-src' is skipped
        let content_deb_src_only = r#"Types: deb-src
URIs: http://example.com/
Suites: stable
Components: main
"#;
        let result = parse_deb822_content(content_deb_src_only, arch, Path::new("test"));
        assert!(result.is_ok());
        let repos = result.unwrap();
        assert_eq!(repos.len(), 0);
    }

    #[test]
    fn test_deb822_enabled_field() {
        let arch = "amd64";

        // Test default enabled (should be true when not specified)
        let content_default = r#"Types: deb
URIs: http://deb.debian.org/debian/
Suites: stable
Components: main
"#;
        let result = parse_deb822_content(content_default, arch, Path::new("test"));
        assert!(result.is_ok());
        let repos = result.unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].enabled, true); // Default should be true

        // Test explicit enabled: yes
        let content_yes = r#"Types: deb
URIs: http://deb.debian.org/debian/
Suites: stable
Components: main
Enabled: yes
"#;
        let result = parse_deb822_content(content_yes, arch, Path::new("test"));
        assert!(result.is_ok());
        let repos = result.unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].enabled, true);

        // Test explicit enabled: no
        let content_no = r#"Types: deb
URIs: http://deb.debian.org/debian/
Suites: stable
Components: main
Enabled: no
"#;
        let result = parse_deb822_content(content_no, arch, Path::new("test"));
        assert!(result.is_ok());
        let repos = result.unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].enabled, false);

        // Test case insensitive (YES should be treated as yes)
        let content_yes_upper = r#"Types: deb
URIs: http://deb.debian.org/debian/
Suites: stable
Components: main
Enabled: YES
"#;
        let result = parse_deb822_content(content_yes_upper, arch, Path::new("test"));
        assert!(result.is_ok());
        let repos = result.unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].enabled, true);

        // Test case insensitive (NO should be treated as no)
        let content_no_upper = r#"Types: deb
URIs: http://deb.debian.org/debian/
Suites: stable
Components: main
Enabled: NO
"#;
        let result = parse_deb822_content(content_no_upper, arch, Path::new("test"));
        assert!(result.is_ok());
        let repos = result.unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].enabled, false);
    }
}

