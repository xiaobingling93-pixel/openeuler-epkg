use color_eyre::Result;
use std::cmp::Ordering;
use versions::Versioning;
use crate::models::Package;
use log;

/// Package version comparison module
///
/// Supports both RPM and Debian package version formats:
/// - RPM: [epoch:]upstream_version[-release]
/// - Debian: [epoch:]upstream_version[-debian_revision]
///
/// Comparison priority (high to low):
/// 1. Epoch (number before colon, defaults to 0)
/// 2. Upstream version (main version part)
/// 3. Release/Revision (part after last dash)

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageVersion {
    pub epoch: u64,
    pub upstream: String,
    pub revision: String,
}

impl PackageVersion {
    /// Parse a package version string into components
    ///
    /// Format: [epoch:]upstream_version[-revision]
    ///
    /// Key parsing rules:
    /// - Epoch: Optional number before colon (:), defaults to 0
    /// - Upstream: Main version part, may contain dashes for pre-release markers (e.g., "1.0-rc1")
    /// - Revision: Optional part after last dash that STARTS WITH A DIGIT
    ///   - Debian: debian_revision (e.g., "1.0-5", "1.0-1ubuntu2")
    ///   - RPM: release (e.g., "1.0-2.el8", "1.0-1.fc35")
    ///
    /// Examples:
    /// - "1.0" -> epoch=0, upstream="1.0", revision="0"
    /// - "2:1.0-rc1" -> epoch=2, upstream="1.0-rc1", revision="0"
    /// - "1.0-rc1-5" -> epoch=0, upstream="1.0-rc1", revision="5"
    /// - "2:1.18.3~beta+dfsg1-5+b1" -> epoch=2, upstream="1.18.3~beta+dfsg1", revision="5+b1"
    pub fn parse(version_str: &str) -> Result<Self> {
        let version_str = version_str.trim();

        // Split by epoch (colon)
        let (epoch_str, remaining) = if let Some(colon_pos) = version_str.find(':') {
            let epoch_part = &version_str[..colon_pos];
            let remaining_part = &version_str[colon_pos + 1..];
            (epoch_part, remaining_part)
        } else {
            ("0", version_str)
        };

        let epoch = epoch_str.parse::<u64>().unwrap_or(0);

        // Find revision: rightmost dash followed by a digit
        // This correctly handles upstream versions like "1.0-rc1" vs revisions like "5"
        let (upstream, revision) = Self::split_upstream_revision(remaining);

        Ok(PackageVersion {
            epoch,
            upstream,
            revision,
        })
    }

    /// Split upstream and revision parts
    ///
    /// Revision is identified as the part after the rightmost dash that starts with a digit.
    /// This distinguishes between:
    /// - Upstream suffixes: "1.0-rc1", "2.0-beta", "1.5-alpha2" (dash + letter)
    /// - Actual revisions: "1.0-5", "1.0-1ubuntu2", "1.0-2.el8" (dash + digit)
    fn split_upstream_revision(version_part: &str) -> (String, String) {
        // Find all dash positions
        let dash_positions: Vec<usize> = version_part.match_indices('-').map(|(pos, _)| pos).collect();

        // Check dashes from right to left to find the first one followed by a digit
        for &dash_pos in dash_positions.iter().rev() {
            let after_dash = &version_part[dash_pos + 1..];
            if !after_dash.is_empty() && after_dash.chars().next().unwrap().is_ascii_digit() {
                // Found revision: dash followed by digit
                let upstream_part = &version_part[..dash_pos];
                let revision_part = &version_part[dash_pos + 1..];
                return (upstream_part.to_string(), revision_part.to_string());
            }
        }

        // No revision found - entire part is upstream
        (version_part.to_string(), "0".to_string())
    }

    /// Compare two package versions according to Debian/RPM rules
    pub fn compare(&self, other: &PackageVersion) -> Ordering {
        // 1. Compare epoch first (highest priority)
        match self.epoch.cmp(&other.epoch) {
            Ordering::Equal => {},
            other => return other,
        }

        // 2. Compare upstream version (middle priority)
        match Self::compare_upstream(&self.upstream, &other.upstream) {
            Ordering::Equal => {},
            other => return other,
        }

        // 3. Compare revision (lowest priority)
        Self::compare_upstream(&self.revision, &other.revision)
    }

    /// Compare upstream version strings using Debian version comparison rules
    ///
    /// Rules:
    /// - ~ (tilde) has lowest priority (pre-release indicator)
    /// - Letters < Numbers < Other symbols (except ~)
    /// - Numbers are compared numerically
    /// - Letters are compared by ASCII value
    fn compare_upstream(a: &str, b: &str) -> Ordering {
        // Special case: if one version is a prefix of the other and the continuation
        // starts with a pre-release marker (dash + letter), the shorter one is newer
        if let Some(stripped_a) = a.strip_prefix(b) {
            if stripped_a.starts_with('-') && stripped_a.len() > 1 {
                // a = "1.0-rc1", b = "1.0" -> b is newer (pre-release < final)
                let next_char = stripped_a.chars().nth(1);
                if next_char.map_or(false, |c| c.is_ascii_alphabetic()) {
                    return Ordering::Less; // a < b (pre-release < final)
                }
            }
        }
        if let Some(stripped_b) = b.strip_prefix(a) {
            if stripped_b.starts_with('-') && stripped_b.len() > 1 {
                // b = "1.0-rc1", a = "1.0" -> a is newer (final > pre-release)
                let next_char = stripped_b.chars().nth(1);
                if next_char.map_or(false, |c| c.is_ascii_alphabetic()) {
                    return Ordering::Greater; // a > b (final > pre-release)
                }
            }
        }

        // Use Debian-style comparison if either version contains special Debian characters
        if a.contains('~') || b.contains('~') || a.contains('+') || b.contains('+') {
            return Self::debian_version_compare(a, b);
        }

        // Try using the versions crate first for standard semantic versioning
        if let (Some(ver_a), Some(ver_b)) = (Versioning::new(a), Versioning::new(b)) {
            return ver_a.cmp(&ver_b);
        }

        // Fall back to Debian-style comparison for non-semantic versions
        Self::debian_version_compare(a, b)
    }

    /// Debian-style version comparison implementation
    fn debian_version_compare(a: &str, b: &str) -> Ordering {
        let mut chars_a = a.chars().peekable();
        let mut chars_b = b.chars().peekable();

        loop {
            match (chars_a.peek(), chars_b.peek()) {
                (None, None) => return Ordering::Equal,
                (None, Some('~')) => return Ordering::Greater, // Something > ~
                (None, Some(_)) => return Ordering::Less,
                (Some('~'), None) => return Ordering::Less,    // ~ < Something
                (Some(_), None) => return Ordering::Greater,
                (Some(_), Some(_)) => {
                    // Extract next segment (numbers or non-numbers)
                    let (seg_a, is_num_a) = Self::extract_segment(&mut chars_a);
                    let (seg_b, is_num_b) = Self::extract_segment(&mut chars_b);

                    match (is_num_a, is_num_b) {
                        (true, true) => {
                            // Both are numbers - compare numerically
                            let num_a: u64 = seg_a.parse().unwrap_or(0);
                            let num_b: u64 = seg_b.parse().unwrap_or(0);
                            match num_a.cmp(&num_b) {
                                Ordering::Equal => continue,
                                other => return other,
                            }
                        },
                        (false, false) => {
                            // Both are non-numbers - use Debian character precedence
                            match Self::debian_string_compare(&seg_a, &seg_b) {
                                Ordering::Equal => continue,
                                other => return other,
                            }
                        },
                        (true, false) => {
                            // Number vs non-number: number has higher precedence
                            // unless the non-number starts with ~
                            if seg_b.starts_with('~') {
                                return Ordering::Greater;
                            } else {
                                return Ordering::Less;
                            }
                        },
                        (false, true) => {
                            // Non-number vs number
                            if seg_a.starts_with('~') {
                                return Ordering::Less;
                            } else {
                                return Ordering::Greater;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Extract the next segment (consecutive digits or consecutive non-digits)
    fn extract_segment(chars: &mut std::iter::Peekable<std::str::Chars>) -> (String, bool) {
        let mut segment = String::new();
        let mut is_digit = false;
        let mut first_char = true;

        while let Some(&ch) = chars.peek() {
            if first_char {
                is_digit = ch.is_ascii_digit();
                first_char = false;
            } else if ch.is_ascii_digit() != is_digit {
                break;
            }

            segment.push(ch);
            chars.next();
        }

        (segment, is_digit)
    }

    /// Compare non-numeric strings using Debian precedence rules
    /// ~ < letters < other symbols
    fn debian_string_compare(a: &str, b: &str) -> Ordering {
        let mut chars_a = a.chars();
        let mut chars_b = b.chars();

        loop {
            match (chars_a.next(), chars_b.next()) {
                (None, None) => return Ordering::Equal,
                (None, Some(_)) => return Ordering::Less,
                (Some(_), None) => return Ordering::Greater,
                (Some(ch_a), Some(ch_b)) => {
                    let order_a = Self::char_precedence(ch_a);
                    let order_b = Self::char_precedence(ch_b);

                    match order_a.cmp(&order_b) {
                        Ordering::Equal => {
                            // Same precedence class, compare by ASCII value
                            match ch_a.cmp(&ch_b) {
                                Ordering::Equal => continue,
                                other => return other,
                            }
                        },
                        other => return other,
                    }
                }
            }
        }
    }

    /// Get character precedence for Debian version comparison
    /// Returns: 0 for ~, 1 for letters, 2 for other symbols
    fn char_precedence(ch: char) -> u8 {
        match ch {
            '~' => 0,  // Lowest precedence (pre-release)
            c if c.is_ascii_alphabetic() => 1,  // Letters
            _ => 2,    // Other symbols (highest precedence)
        }
    }
}

/// Check if version `new_version` is newer than `current_version`
pub fn select_highest_version(packages: Vec<Package>) -> Option<Package> {
    if packages.is_empty() {
        return None;
    }

    let mut highest_package: Option<Package> = None;
    let mut highest_version_parsed: Option<PackageVersion> = None;

    for package in packages {
        match PackageVersion::parse(&package.version) {
            Ok(current_pkg_version) => {
                if highest_package.is_none() {
                    highest_package = Some(package);
                    highest_version_parsed = Some(current_pkg_version);
                } else {
                    if let Some(ref highest_pv) = highest_version_parsed {
                        if current_pkg_version.compare(highest_pv) == Ordering::Greater {
                            highest_package = Some(package);
                            highest_version_parsed = Some(current_pkg_version);
                        }
                    }
                }
            }
            Err(e) => {
                log::warn!(
                    "Failed to parse version '{}' for package '{}': {}",
                    package.version,
                    package.pkgname,
                    e
                );
            }
        }
    }
    highest_package
}

pub fn is_version_newer(new_version: &str, current_version: &str) -> bool {
    match (PackageVersion::parse(new_version), PackageVersion::parse(current_version)) {
        (Ok(new_ver), Ok(current_ver)) => {
            new_ver.compare(&current_ver) == Ordering::Greater
        },
        _ => {
            // Fall back to simple string comparison if parsing fails
            log::warn!("Failed to parse versions, falling back to string comparison: '{}' vs '{}'",
                      new_version, current_version);
            new_version > current_version
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_epoch_comparison() {
        // Epoch has highest priority
        assert!(is_version_newer("2:1.0-1", "1:9.0-1"));
        assert!(is_version_newer("1:1.0", "1.9"));
        assert!(!is_version_newer("1.0", "1:1.0"));
    }

    #[test]
    fn test_tilde_precedence() {
        // ~ indicates pre-release (lowest precedence)
        assert!(!is_version_newer("1.0~beta", "1.0"));
        assert!(is_version_newer("1.0", "1.0~beta"));
        assert!(!is_version_newer("1.0~alpha", "1.0~beta"));
        assert!(is_version_newer("1.0~beta", "1.0~alpha"));
    }

    #[test]
    fn test_numeric_comparison() {
        // Numbers are compared numerically, not lexicographically
        assert!(is_version_newer("1.10", "1.2"));
        assert!(is_version_newer("1.0.10", "1.0.2"));
        assert!(!is_version_newer("1.2", "1.10"));
    }

    #[test]
    fn test_revision_comparison() {
        // Revision (after dash) has lowest priority
        assert!(is_version_newer("1.0-10", "1.0-9"));
        assert!(is_version_newer("1.0-2", "1.0-1"));
        assert!(!is_version_newer("1.0-1", "1.0-2"));
    }

    #[test]
    fn test_complex_debian_versions() {
        // Real Debian package version examples
        assert!(is_version_newer("2:1.18.3~beta+dfsg1-6", "2:1.18.3~beta+dfsg1-5+b1"));
        assert!(!is_version_newer("1.18~beta", "1.18"));
        assert!(is_version_newer("2024.12.31", "2024.3.15"));
    }

    #[test]
    fn test_character_precedence() {
        // Test character precedence: ~ < letters < other symbols
        assert!(!is_version_newer("1.0~", "1.0a"));
        assert!(is_version_newer("1.0a", "1.0~"));
        assert!(is_version_newer("1.0+", "1.0a"));
        assert!(!is_version_newer("1.0a", "1.0+"));
    }

    #[test]
    fn test_missing_components() {
        // Test versions with missing epoch or revision
        assert!(is_version_newer("1:1.0", "1.0"));  // 1:1.0 vs 0:1.0
        assert!(is_version_newer("1.0-1", "1.0"));  // 1.0-1 vs 1.0-0
        assert!(!is_version_newer("1.0", "1.0-1"));
    }

    #[test]
    fn test_version_parsing() {
        let v1 = PackageVersion::parse("2:1.18.3~beta+dfsg1-5+b1").unwrap();
        assert_eq!(v1.epoch, 2);
        assert_eq!(v1.upstream, "1.18.3~beta+dfsg1");
        assert_eq!(v1.revision, "5+b1");

        let v2 = PackageVersion::parse("1.0").unwrap();
        assert_eq!(v2.epoch, 0);
        assert_eq!(v2.upstream, "1.0");
        assert_eq!(v2.revision, "0");

        let v3 = PackageVersion::parse("1:2.0-3").unwrap();
        assert_eq!(v3.epoch, 1);
        assert_eq!(v3.upstream, "2.0");
        assert_eq!(v3.revision, "3");
    }

    #[test]
    fn test_upstream_with_dashes() {
        // Upstream versions with pre-release markers (no revision)
        let v1 = PackageVersion::parse("1.0-rc1").unwrap();
        assert_eq!(v1.epoch, 0);
        assert_eq!(v1.upstream, "1.0-rc1");
        assert_eq!(v1.revision, "0");

        let v2 = PackageVersion::parse("2.0-beta").unwrap();
        assert_eq!(v2.epoch, 0);
        assert_eq!(v2.upstream, "2.0-beta");
        assert_eq!(v2.revision, "0");

        let v3 = PackageVersion::parse("1.5-alpha2").unwrap();
        assert_eq!(v3.epoch, 0);
        assert_eq!(v3.upstream, "1.5-alpha2");
        assert_eq!(v3.revision, "0");

        // Upstream with pre-release markers AND revision
        let v4 = PackageVersion::parse("1.0-rc1-5").unwrap();
        assert_eq!(v4.epoch, 0);
        assert_eq!(v4.upstream, "1.0-rc1");
        assert_eq!(v4.revision, "5");

        let v5 = PackageVersion::parse("2.0-beta-1ubuntu2").unwrap();
        assert_eq!(v5.epoch, 0);
        assert_eq!(v5.upstream, "2.0-beta");
        assert_eq!(v5.revision, "1ubuntu2");

        // With epoch
        let v6 = PackageVersion::parse("2:1.0-rc1").unwrap();
        assert_eq!(v6.epoch, 2);
        assert_eq!(v6.upstream, "1.0-rc1");
        assert_eq!(v6.revision, "0");

        let v7 = PackageVersion::parse("1:1.0-rc1-3").unwrap();
        assert_eq!(v7.epoch, 1);
        assert_eq!(v7.upstream, "1.0-rc1");
        assert_eq!(v7.revision, "3");
    }

    #[test]
    fn test_complex_upstream_parsing() {
        // Multiple dashes in upstream, revision starts with digit
        let v1 = PackageVersion::parse("1.0-beta-rc2-1").unwrap();
        assert_eq!(v1.upstream, "1.0-beta-rc2");
        assert_eq!(v1.revision, "1");

        // Multiple dashes, no revision (last part starts with letter)
        let v2 = PackageVersion::parse("1.0-beta-rc2").unwrap();
        assert_eq!(v2.upstream, "1.0-beta-rc2");
        assert_eq!(v2.revision, "0");

        // Real-world examples
        let v3 = PackageVersion::parse("7.4.052-1ubuntu3").unwrap();
        assert_eq!(v3.upstream, "7.4.052");
        assert_eq!(v3.revision, "1ubuntu3");

        let v4 = PackageVersion::parse("1.2.3-rc1-2.el8").unwrap();
        assert_eq!(v4.upstream, "1.2.3-rc1");
        assert_eq!(v4.revision, "2.el8");

        // Version with git hash-like suffix
        let v5 = PackageVersion::parse("1.0-git20230101-1").unwrap();
        assert_eq!(v5.upstream, "1.0-git20230101");
        assert_eq!(v5.revision, "1");
    }

    #[test]
    fn test_edge_cases_parsing() {
        // Just revision number
        let v1 = PackageVersion::parse("5").unwrap();
        assert_eq!(v1.upstream, "5");
        assert_eq!(v1.revision, "0");

        // Single dash with letter
        let v2 = PackageVersion::parse("1-beta").unwrap();
        assert_eq!(v2.upstream, "1-beta");
        assert_eq!(v2.revision, "0");

        // Single dash with number
        let v3 = PackageVersion::parse("1-5").unwrap();
        assert_eq!(v3.upstream, "1");
        assert_eq!(v3.revision, "5");

        // Empty after dash
        let v4 = PackageVersion::parse("1.0-").unwrap();
        assert_eq!(v4.upstream, "1.0-");
        assert_eq!(v4.revision, "0");

        // Multiple consecutive dashes
        let v5 = PackageVersion::parse("1.0--5").unwrap();
        assert_eq!(v5.upstream, "1.0-");
        assert_eq!(v5.revision, "5");
    }

    #[test]
    fn test_semantic_versions() {
        // Test with semantic versions that the versions crate can handle
        assert!(is_version_newer("2.1.0", "2.0.5"));
        assert!(is_version_newer("1.2.3", "1.2.2"));
        assert!(!is_version_newer("1.0.0", "1.0.1"));
    }

    #[test]
    fn test_version_comparison_with_dashes() {
        // Test that upstream versions with dashes are compared correctly

        // RC versions should be less than final versions
        assert!(!is_version_newer("1.0-rc1", "1.0"));
        assert!(is_version_newer("1.0", "1.0-rc1"));

        // RC versions with revisions
        assert!(is_version_newer("1.0-rc1-2", "1.0-rc1-1"));
        assert!(!is_version_newer("1.0-rc1-1", "1.0-rc1-2"));

        // Same upstream with different revisions
        assert!(is_version_newer("1.0-beta-5", "1.0-beta-3"));

        // Compare different upstream versions with dashes
        assert!(is_version_newer("2.0-rc1", "1.0-rc2"));
        assert!(!is_version_newer("1.0-rc2", "2.0-rc1"));

        // Compare final vs pre-release with revision
        assert!(is_version_newer("1.0-1", "1.0-rc1-10"));
        assert!(!is_version_newer("1.0-rc1-10", "1.0-1"));
    }
}
