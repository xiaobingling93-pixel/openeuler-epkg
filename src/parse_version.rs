//! Package version parsing and comparison module
//!
//! This module provides PackageVersion struct and functions for parsing and comparing
//! package versions in different formats (RPM, Debian, etc.).

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

use color_eyre::Result;

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
    /// - Revision: Optional part after last dash
    ///   - Debian: debian_revision (e.g., "1.0-5", "1.0-1ubuntu2", "2.14.14-z")
    ///   - RPM: release (e.g., "1.0-2.el8", "1.0-1.fc35")
    ///   - Pre-release markers (rc, beta, alpha, pre, dev, snapshot) are treated as part of upstream
    ///   - Other alphabetic suffixes after the last dash are treated as revisions
    ///
    /// Examples:
    /// - "1.0" -> epoch=0, upstream="1.0", revision="0"
    /// - "2:1.0-rc1" -> epoch=2, upstream="1.0-rc1", revision="0"
    /// - "1.0-rc1-5" -> epoch=0, upstream="1.0-rc1", revision="5"
    /// - "2.14.14-z" -> epoch=0, upstream="2.14.14", revision="z"
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
    /// Revision is identified by scanning from right to left, but with special handling
    /// for cases where multiple dashes are followed by digits (revisions can contain dashes).
    /// This distinguishes between:
    /// - Upstream suffixes: "1.0-rc1", "2.0-beta", "1.5-alpha2" (pre-release markers)
    /// - Actual revisions: "1.0-5", "1.0-1ubuntu2", "1.0-2.el8", "2.14.14-z" (dash + digit/letter)
    /// - Revisions with dashes: "1.48.0-2-3" -> revision="2-3" (revisions can contain dashes)
    ///
    /// Pre-release markers (rc, beta, alpha, etc.) are treated as part of upstream.
    /// Other suffixes after the first valid dash are treated as revisions.
    fn split_upstream_revision(version_part: &str) -> (String, String) {
        // Find all dash positions
        let dash_positions: Vec<usize> = version_part.match_indices('-').map(|(pos, _)| pos).collect();

        if dash_positions.is_empty() {
            // No dashes - entire part is upstream
            return (version_part.to_string(), "0".to_string());
        }

        // Check dashes from right to left, but track the leftmost dash followed by digits
        // This handles cases like "1.48.0-2-3" where revision="2-3" contains dashes
        let mut rightmost_candidate: Option<usize> = None;
        let mut leftmost_digit_dash: Option<usize> = None;

        for &dash_pos in dash_positions.iter().rev() {
            let after_dash = &version_part[dash_pos + 1..];
            if after_dash.is_empty() {
                continue;
            }

            let first_char = after_dash.chars().next().unwrap();

            if first_char.is_ascii_digit() {
                // Dash followed by digit - track both rightmost and leftmost
                if rightmost_candidate.is_none() {
                    rightmost_candidate = Some(dash_pos);
                }
                leftmost_digit_dash = Some(dash_pos);
                // Continue to check if there are more dashes to the left
            } else if first_char.is_ascii_alphabetic() {
                let lower_after = after_dash.to_lowercase();
                // Check if it's a pre-release marker
                let is_prerelease = lower_after.starts_with("rc") ||
                                    lower_after.starts_with("beta") ||
                                    lower_after.starts_with("alpha") ||
                                    lower_after.starts_with("pre") ||
                                    lower_after.starts_with("dev") ||
                                    lower_after.starts_with("snapshot");

                if !is_prerelease {
                    // Not a pre-release marker
                    // If we already found a dash followed by a digit, use that instead
                    // (e.g., "1.0-git20230101-1" -> use "-1", not "-git20230101")
                    if rightmost_candidate.is_some() && leftmost_digit_dash.is_some() {
                        // We already have a digit dash, ignore this letter dash
                        break;
                    }
                    // Otherwise, treat as revision (e.g., "2.14.14-z")
                    if rightmost_candidate.is_none() {
                        rightmost_candidate = Some(dash_pos);
                    }
                    // Stop here - we found a non-prerelease letter, so this is the revision separator
                    break;
                }
                // Otherwise, it's a pre-release marker, continue looking for earlier dashes
            } else {
                // Dash followed by something else (not digit or letter) - stop here
                // This might be part of upstream
                break;
            }
        }

        if let Some(separator_pos) = rightmost_candidate {
            // If we found multiple dashes followed by digits, use the leftmost one
            // This handles "1.48.0-2-3" -> revision="2-3"
            let final_separator = leftmost_digit_dash.unwrap_or(separator_pos);
            let upstream_part = &version_part[..final_separator];
            let revision_part = &version_part[final_separator + 1..];
            (upstream_part.to_string(), revision_part.to_string())
        } else {
            // No revision found - entire part is upstream
            (version_part.to_string(), "0".to_string())
        }
    }

    /// Normalize APK revision format for comparison
    /// In Alpine/APK, -r0, -r1, etc. are explicit revisions.
    /// For comparison: strip 'r' prefix and compare numerically, but mark as explicit
    /// Explicit revisions should compare less than implicit ones when numeric values are equal.
    pub fn normalize_apk_revision(revision: &str) -> String {
        // If revision starts with 'r' followed by digits, strip the 'r' and compare numerically
        // But we need to distinguish explicit vs implicit. For now, strip 'r' and compare.
        // Note: This means r0 == 0 numerically, but we want r0 < 0 (explicit < implicit)
        // We'll handle this by checking if both are "0" and one is explicit
        if revision.len() > 1 && revision.starts_with('r') {
            let after_r = &revision[1..];
            if after_r.chars().all(|c| c.is_ascii_digit()) {
                // Return with a special marker to indicate it's explicit
                // Use a character that sorts before digits: ~
                return format!("~{}", after_r);
            }
        }
        revision.to_string()
    }

    /// Check if a string after a dash is a recognized pre-release marker
    pub fn is_prerelease_marker(after_dash: &str) -> bool {
        if after_dash.is_empty() {
            return false;
        }
        let first_char = after_dash.chars().next().unwrap();
        if !first_char.is_ascii_alphabetic() {
            return false;
        }
        let lower_after = after_dash.to_lowercase();
        // Check for multi-character pre-release markers
        if lower_after.starts_with("rc") ||
           lower_after.starts_with("beta") ||
           lower_after.starts_with("alpha") ||
           lower_after.starts_with("pre") ||
           lower_after.starts_with("dev") ||
           lower_after.starts_with("snapshot") {
            return true;
        }
        // Check for single-letter pre-release markers (e.g., "a0", "b0", "a1", "b2")
        // These are common in Conda versioning (alpha, beta, etc.)
        // But be careful: "b24" could be a build identifier, not a beta pre-release
        // Only treat as pre-release if it's a single letter followed by digits and
        // the pattern matches common pre-release formats (typically short numbers like "a0", "b1")
        if after_dash.len() >= 2 {
            let second_char = after_dash.chars().nth(1).unwrap();
            if second_char.is_ascii_digit() {
                // For single-letter markers, only treat as pre-release if:
                // 1. It's "a" (alpha) or "b" (beta) followed by a short number (typically 0-9)
                // 2. The number is very short (single digit or low double digits)
                // This is a heuristic to distinguish "b1" (beta 1) from "b24" (build 24)
                let first_lower = first_char.to_ascii_lowercase();
                if first_lower == 'a' || first_lower == 'b' {
                    // Extract the numeric part
                    let numeric_part = &after_dash[1..];
                    if let Ok(num) = numeric_part.parse::<u32>() {
                        // Only treat as pre-release if the number is small (typically 0-9, sometimes up to 20)
                        // This distinguishes "a0", "a1", "b2" (pre-releases) from "b24", "a100" (build identifiers)
                        if num <= 20 {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// Extract a segment stopping at dots (for RPM version comparison)
    pub fn extract_segment_stopping_at_dot(chars: &mut std::iter::Peekable<std::str::Chars>) -> (String, bool) {
        let mut segment = String::new();
        let mut is_digit = false;
        let mut first_char = true;

        while let Some(&ch) = chars.peek() {
            // Stop at dots - they're segment separators
            if ch == '.' {
                break;
            }

            if first_char {
                is_digit = ch.is_ascii_digit();
                first_char = false;
            } else if ch.is_ascii_digit() != is_digit {
                // Changed from digit to non-digit or vice versa - stop here
                break;
            }

            segment.push(ch);
            chars.next();
        }

        // Skip the dot if we stopped at one
        if chars.peek() == Some(&'.') {
            chars.next();
        }

        (segment, is_digit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

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

        // Test alphabetic revision
        let v4 = PackageVersion::parse("2.14.14-z").unwrap();
        assert_eq!(v4.epoch, 0);
        assert_eq!(v4.upstream, "2.14.14");
        assert_eq!(v4.revision, "z");
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

        // Version with revision containing dashes (Debian format)
        // This is the case that was failing: "1.48.0-2-3" should parse as revision="2-3"
        let v6 = PackageVersion::parse("1.48.0-2-3").unwrap();
        assert_eq!(v6.upstream, "1.48.0");
        assert_eq!(v6.revision, "2-3");

        // Test that "1.48.0-2-3" >= "1.48.0-2" comparison works correctly
        let v7 = PackageVersion::parse("1.48.0-2").unwrap();
        assert_eq!(v7.upstream, "1.48.0");
        assert_eq!(v7.revision, "2");
        // v6 should be greater than v7 because revision "2-3" > "2"
        assert_eq!(v6.compare(&v7), Ordering::Greater);
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

}
