//! # URL Manipulation and Path Resolution
//!
//! This module handles URL processing, path resolution, and mirror URL formatting
//! for the epkg mirror system. It provides utilities for converting between different
//! URL formats and resolving them to local cache paths.
//!
//! ## Key Functionality
//!
//! - **URL to Site Mapping**: Extract site names from full URLs for mirror identification
//! - **Path Resolution**: Convert remote URLs and special patterns to local cache paths
//! - **Mirror URL Formatting**: Generate properly formatted mirror URLs based on configuration
//! - **Distro Directory Resolution**: Find appropriate distribution directories for mirrors
//! - **Security Validation**: Path traversal and security checks for resolved paths
//!
//! ## Supported URL Patterns
//!
//! - **HTTP/HTTPS URLs**: Standard web URLs resolved to cache paths
//! - **Special Patterns**: `$mirror/` and `///` syntax for mirror-relative paths
//! - **Local Paths**: `file://` URLs and absolute/relative filesystem paths
//! - **DNS Patterns**: Automatic HTTPS prefixing for bare domain names
//!
//! ## Global State
//!
//! - **REPODATA_NAME2DISTRO_DIRS**: Maps repository names to distribution directory lists
//!   for efficient distro directory lookup across different repositories

use std::sync::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use color_eyre::eyre::{Result, eyre};
use crate::models::channel_config;
use crate::models::ChannelConfig;
use crate::mirror::{Mirrors, UrlProtocol};

/// Global hashmap for repodata_name to distro_dirs mapping
static REPODATA_NAME2DISTRO_DIRS: std::sync::LazyLock<Mutex<HashMap<String, Vec<String>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Extend the global repodata_name2distro_dirs hashmap with repos from a specific channel config
pub fn extend_repodata_name2distro_dirs(channel_config: &ChannelConfig, repos: &[crate::repo::RepoRevise]) -> Result<()> {
    let mut hashmap = REPODATA_NAME2DISTRO_DIRS.lock()
        .map_err(|e| color_eyre::eyre::eyre!("Failed to lock repodata_name2distro_dirs: {}", e))?;

    // Example output for assets/repos/openeuler.yaml
    // repodata_name2distro_dirs[update] = ["openEuler", "openeuler.org", "openeuler"]
    // repodata_name2distro_dirs[everything] = ["openEuler", "openeuler.org", "openeuler"]
    // repodata_name2distro_dirs[EPOL/main] = ["openEuler", "openeuler.org", "openeuler"]
    // repodata_name2distro_dirs[EPOL/update/main] = ["openEuler", "openeuler.org", "openeuler"]
    for repo in repos {
        hashmap.insert(repo.repodata_name.clone(), channel_config.distro_dirs.clone());
        log::debug!("repodata_name2distro_dirs[{}] = {:?}", repo.repodata_name.clone(), channel_config.distro_dirs.clone());
    }

    Ok(())
}

/// Get distro_dirs for a specific repodata_name
pub(crate) fn get_distro_dirs_for_repodata_name(repodata_name: &str) -> Vec<String> {
    if let Ok(hashmap) = REPODATA_NAME2DISTRO_DIRS.lock() {
        if let Some(distro_dirs) = hashmap.get(repodata_name) {
            return distro_dirs.clone();
        }
    }

    // Fallback to channel_config().distro_dirs if not found
    channel_config().distro_dirs.clone()
}


/// Extract the site from a full download URL
pub fn url2site(url: &str) -> String {
    // Normalise to host(:port) -- ignore everything after the first single '/'
    if let Some(scheme_end) = url.find("://") {
        let after_scheme = &url[scheme_end + 3..]; // skip scheme://

        // Take up to the first '/' **ignoring** any extra slashes that may be part of the
        // epkg "///" placeholder syntax. This ensures that URLs such as
        // "https://mirror.example.com/ubuntu///dists/..." are mapped back to the base
        // "https://mirror.example.com" instead of "https://mirror.example.com/ubuntu".
        let host_end = after_scheme.find('/').unwrap_or(after_scheme.len());
        return after_scheme[..host_end].to_string(); // Return just the site without scheme
    }

    // Fallback – return unchanged
    url.to_string()
}

impl Mirrors {
    /// Find the best matching distro directory for a mirror
    pub fn find_distro_dir(
        mirror: &crate::mirror::types::Mirror,
        distro: &str,
        arch: &str,
        repodata_name: &str,
    ) -> String {
        // Use distro_dirs from the global hashmap for the specific repodata_name
        let sorted_dirs = get_distro_dirs_for_repodata_name(repodata_name);

        log::trace!("find_distro_dir for mirror {}: distro={}, arch={}, repodata_name={}, sorted_dirs.len()={}, mirror.distro_dirs={:?}",
                   mirror.url, distro, arch, repodata_name, sorted_dirs.len(), mirror.distro_dirs);

        let mut found_dir = String::new();
        let mut skipped_reasons = Vec::new();

        for item in &sorted_dirs {
            let item_lower = item.to_lowercase();
            let mut skip_reason = None;

            if distro == "fedora" {
                if item_lower.contains("alt") {
                    skip_reason = Some("contains 'alt'");
                } else if item_lower.contains("archive") {
                    skip_reason = Some("contains 'archive'");
                } else if arch == "x86_64" || arch == "aarch64" {
                    if item_lower.contains("secondary") {
                        skip_reason = Some("contains 'secondary' (x86_64/aarch64)");
                    }
                } else {
                    if !item_lower.contains("secondary") {
                        skip_reason = Some("missing 'secondary' (non-x86_64/aarch64)");
                    }
                }
            }
            if distro == "ubuntu" {
                if arch == "x86_64" {
                    if item_lower.contains("ports") {
                        skip_reason = Some("contains 'ports' (x86_64)");
                    }
                } else {
                    if !item_lower.contains("ports") {
                        skip_reason = Some("missing 'ports' (non-x86_64)");
                    }
                }
            }

            if let Some(reason) = skip_reason {
                skipped_reasons.push(format!("{}: {}", item, reason));
                continue;
            }

            if let Some(orig_dir) = mirror.distro_dirs.get(item)
            {
                // Use the original casing from the mirror itself to avoid wrong capitalisation
                found_dir = orig_dir.clone();
                log::trace!(
                    "find_distro_dir for mirror {}: matched item '{}' -> orig_dir '{}'",
                    mirror.url,
                    item,
                    orig_dir
                );
                break;
            } else {
                skipped_reasons.push(format!("{}: not in mirror.distro_dirs", item));
            }
        }

        if found_dir.is_empty() && !skipped_reasons.is_empty() {
            log::trace!(
                "find_distro_dir for mirror {}: no match found. Skipped: {}",
                mirror.url,
                skipped_reasons.join(", ")
            );
        }

        found_dir
    }

    /// Format mirror URL based on mirror configuration and package format
    ///
    /// Parameters provided directly to avoid dependency on Mirror struct
    pub fn format_mirror_url(&self, mirror_url: &str, top_level: bool, distro_dir: &str) -> Result<String> {
        let distro = &channel_config().distro;

        // Debian's index_url has explicit "$mirror/debian/", "$mirror/debian-security/"
        let url = if top_level || distro == "debian" {
                      format!("{}//", mirror_url.trim_end_matches('/'))
                  } else {
                      format!("{}/{}//", mirror_url.trim_end_matches('/'), distro_dir)
                  };

        Ok(url)
    }

    /// Validate a PathBuf for security issues by checking its string representation.
    ///
    /// This is used for paths that are constructed programmatically (e.g., from URLs).
    ///
    /// Performs security checks:
    /// - Rejects paths containing '../' or '..\\' (directory traversal)
    /// - Validates that the path has a file name component
    /// - Validates that the file name is not empty
    ///
    /// Returns an error if the path fails any security check.
    pub fn validate_path_security(path: &Path, context: &str) -> Result<()> {
        let path_str = path.to_string_lossy();

        // Security check: reject paths with directory traversal
        if path_str.contains("../") || path_str.contains("..\\") {
            return Err(eyre!("Invalid path: directory traversal detected in '{}' ({})", path_str, context));
        }

        // Validate file name
        if let Some(file_name) = path.file_name() {
            if file_name.to_string_lossy().is_empty() {
                return Err(eyre!("Invalid path: empty file name in '{}' ({})", path_str, context));
            }
        } else {
            return Err(eyre!("Invalid path: no file name component in '{}' ({})", path_str, context));
        }

        Ok(())
    }

    /// Resolve remote URL (HTTP/HTTPS) or special mirror patterns to a cache path.
    ///
    /// Handles:
    /// - Special patterns: `$mirror/` and `///`
    /// - HTTP/HTTPS URLs
    ///
    /// Note: Security validation is performed by the caller (detect_url_proto_path).
    ///
    /// Returns an error if the URL is not a remote URL or special pattern.
    pub fn remote_url_to_path(url: &str, output_dir: &Path, repodata_name: &str) -> Result<PathBuf> {
        // Check for special mirror patterns first
        if let Some((_, str_b)) = url.split_once("$mirror/") {
            let distro_dirs = get_distro_dirs_for_repodata_name(repodata_name);
            let local_subdir = distro_dirs.last().unwrap().clone();
            let path = if local_subdir != "debian" {
                output_dir.join(&local_subdir).join(str_b)
            } else {
                output_dir.join(str_b)
            };
            return Ok(path);
        }

        if let Some((_, str_b)) = url.split_once("///") {
            let path = output_dir.join(str_b);
            return Ok(path);
        }

        // Check for HTTP/HTTPS URLs
        if url.starts_with("http://") || url.starts_with("https://") {
            return Ok(Self::resolve_http_url_path(url, output_dir));
        }

        // Not a remote URL or special pattern
        Err(eyre!("Not a supported remote URL: '{}'", url))
    }

    /// Resolve local path to a PathBuf.
    ///
    /// Handles:
    /// - `file://` URLs
    /// - Absolute paths (leading `/`)
    /// - Relative paths (leading `./`)
    /// - Existing files (unknown pattern but file exists)
    ///
    /// Note: Security validation is performed by the caller (detect_url_proto_path).
    ///
    /// Returns `None` if the path is not a valid local path.
    pub fn local_url_to_path(spec: &str) -> Option<PathBuf> {
        // Check for file:// URLs or relative paths (./)
        if let Some(local_path) = spec
            .strip_prefix("file://")
            .or_else(|| spec.strip_prefix("./"))
        {
            return Some(PathBuf::from(local_path));
        }

        // Check for absolute paths (leading /)
        if spec.starts_with('/') {
            return Some(PathBuf::from(spec));
        }

        // Check if it's an existing file (unknown pattern but file exists)
        let path = Path::new(spec);
        if path.exists() && path.is_file() {
            return Some(path.to_path_buf());
        }

        // Not a valid local path
        None
    }

    /// Detect URL protocol and resolve to path by trying remote_url_to_path() first, then local_url_to_path().
    ///
    /// Performs security validation on all resolved paths before returning them.
    ///
    /// Returns:
    /// - `(UrlProtocol::Http, PathBuf)` for remote URLs or special patterns
    /// - `(UrlProtocol::Local, PathBuf)` for local paths
    pub fn detect_url_proto_path(url: &str, repodata_name: &str) -> Result<(UrlProtocol, PathBuf)> {
        let output_dir = crate::dirs().epkg_downloads_cache.clone();
        // Try remote URL first
        match Self::remote_url_to_path(url, &output_dir, repodata_name) {
            Ok(path) => {
                // Validate path security before returning
                Self::validate_path_security(&path, "detect_url_proto_path: remote URL")?;
                Ok((UrlProtocol::Http, path))
            }
            Err(_) => {
                // Try local path
                if let Some(path) = Self::local_url_to_path(url) {
                    // Validate path security before returning
                    Self::validate_path_security(&path, "detect_url_proto_path: local path")?;
                    Ok((UrlProtocol::Local, path))
                } else {
                    // Handle case like DNS/path/to/file where DNS contains at least one '.'
                    // e.g. dl-cdn.alpinelinux.org/MIRRORS.txt
                    // in which case add 'https://' prefix and try resolve_http_url_path()
                    if let Some(first_slash) = url.find('/') {
                        let dns_part = &url[..first_slash];
                        if dns_part.contains('.') {
                            // Looks like a DNS name, try adding https:// prefix
                            let https_url = format!("https://{}", url);
                            let path = Self::resolve_http_url_path(&https_url, &output_dir);
                            // Validate the generated path
                            Self::validate_path_security(&path, "detect_url_proto_path: DNS pattern")?;
                            return Ok((UrlProtocol::Http, path));
                        }
                    }

                    Err(eyre!("Unsupport URL: '{}'", url))
                }
            }
        }
    }

    pub fn url_to_cache_path(url: &str, repodata_name: &str) -> Result<PathBuf> {
        log::debug!("url_to_cache_path {} {}", url, repodata_name);
        let (_, path) = Self::detect_url_proto_path(url, repodata_name)?;
        Ok(path)
    }

    /// Resolve HTTP(S) URL to a cache path by substituting protocol prefix with output_dir.
    ///
    /// Simply replaces "http://" or "https://" with "$output_dir/", preserving the rest of the path.
    /// Saves to index.html when the URL is a site or directory:
    /// - No path (e.g. `http://www.bing.com`) → site, use index.html
    /// - Path ends with '/' (e.g. `http://example.com/foo/`) → dir, use index.html
    ///
    /// Note: Security validation is performed by the caller (detect_url_proto_path).
    ///
    /// Examples:
    /// - `https://example.com` -> `output_dir/example.com/index.html`
    /// - `https://example.com/path/to/file.txt` -> `output_dir/example.com/path/to/file.txt`
    /// - `https://example.com/path/` -> `output_dir/example.com/path/index.html`
    pub fn resolve_http_url_path(url: &str, output_dir: &Path) -> PathBuf {
        let url_stripped = match url.strip_prefix("http://").or_else(|| url.strip_prefix("https://")) {
            Some(stripped) => stripped,
            None => url
        };

        // Site: no '/' after host (e.g. http://www.bing.com). Dir: path ends with /
        let is_site = !url_stripped.contains('/');
        let is_dir = url.ends_with('/');

        // Build path by joining output_dir with the stripped URL path segments
        let parts: Vec<&str> = url_stripped.split('/').filter(|s| !s.is_empty()).collect();
        let mut path = output_dir.to_path_buf();
        for part in parts {
            path = path.join(part);
        }

        if is_site || is_dir {
            path.join("index.html")
        } else {
            path
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_resolve_http_url_path_basic() {
        let output_dir = PathBuf::from("/cache");
        let url = "https://example.com/path/to/file.txt";
        let result = Mirrors::resolve_http_url_path(url, &output_dir);
        assert_eq!(result, PathBuf::from("/cache/example.com/path/to/file.txt"));
    }

    #[test]
    fn test_resolve_http_url_path_http() {
        let output_dir = PathBuf::from("/cache");
        let url = "http://example.com/path/to/file.txt";
        let result = Mirrors::resolve_http_url_path(url, &output_dir);
        assert_eq!(result, PathBuf::from("/cache/example.com/path/to/file.txt"));
    }

    #[test]
    fn test_resolve_http_url_path_prevents_collision() {
        let output_dir = PathBuf::from("/cache");

        // Two URLs with same filename but different paths should map to different cache paths
        let url1 = "https://mirrors.tuna.tsinghua.edu.cn/anaconda/pkgs/main/linux-64/current_repodata.json.gz";
        let url2 = "https://mirrors.tuna.tsinghua.edu.cn/anaconda/pkgs/main/noarch/current_repodata.json.gz";

        let result1 = Mirrors::resolve_http_url_path(url1, &output_dir);
        let result2 = Mirrors::resolve_http_url_path(url2, &output_dir);

        assert_ne!(result1, result2, "Different paths should produce different cache paths");
        assert_eq!(result1, PathBuf::from("/cache/mirrors.tuna.tsinghua.edu.cn/anaconda/pkgs/main/linux-64/current_repodata.json.gz"));
        assert_eq!(result2, PathBuf::from("/cache/mirrors.tuna.tsinghua.edu.cn/anaconda/pkgs/main/noarch/current_repodata.json.gz"));
    }

    #[test]
    fn test_resolve_http_url_path_with_empty_segments() {
        let output_dir = PathBuf::from("/cache");
        let url = "https://example.com//path//to//file.txt";
        let result = Mirrors::resolve_http_url_path(url, &output_dir);
        // Empty segments should be skipped
        assert_eq!(result, PathBuf::from("/cache/example.com/path/to/file.txt"));
    }

    #[test]
    fn test_resolve_http_url_path_root_path() {
        let output_dir = PathBuf::from("/cache");
        let url = "https://example.com/file.txt";
        let result = Mirrors::resolve_http_url_path(url, &output_dir);
        assert_eq!(result, PathBuf::from("/cache/example.com/file.txt"));
    }

    #[test]
    fn test_resolve_http_url_path_non_http() {
        let url = "file:///path/to/file.txt";
        let result = Mirrors::local_url_to_path(url);
        // Should fallback to filename only
        assert_eq!(result, Some(PathBuf::from("/path/to/file.txt")));
    }

    #[test]
    fn test_resolve_http_url_path_complex_path() {
        let output_dir = PathBuf::from("/cache");
        let url = "https://repo.example.com/conda/main/linux-64/repodata.json";
        let result = Mirrors::resolve_http_url_path(url, &output_dir);
        assert_eq!(result, PathBuf::from("/cache/repo.example.com/conda/main/linux-64/repodata.json"));
    }

    #[test]
    fn test_resolve_http_url_path_with_port() {
        let output_dir = PathBuf::from("/cache");
        let url = "https://example.com:8080/path/file.txt";
        let result = Mirrors::resolve_http_url_path(url, &output_dir);
        // Port is part of host
        assert_eq!(result, PathBuf::from("/cache/example.com:8080/path/file.txt"));
    }

    #[test]
    fn test_remote_url_to_path_integration() {
        let output_dir = PathBuf::from("/cache");

        // Test that remote_url_to_path uses resolve_http_url_path for regular URLs
        let url1 = "https://mirror.com/repo/linux-64/file.gz";
        let url2 = "https://mirror.com/repo/noarch/file.gz";

        let result1 = Mirrors::remote_url_to_path(url1, &output_dir, "test").unwrap();
        let result2 = Mirrors::remote_url_to_path(url2, &output_dir, "test").unwrap();

        assert_ne!(result1, result2, "Different paths should produce different cache paths");
    }
}
