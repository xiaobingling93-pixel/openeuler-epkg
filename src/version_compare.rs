//! Version comparison routines
//!
//! This module provides functions for comparing package versions according to
//! Debian/RPM versioning rules, including epoch, upstream version, and revision comparison.

use std::cmp::Ordering;
use versions::Versioning;
use color_eyre::Result;
use crate::models::PackageFormat;
use crate::parse_version::PackageVersion;

// Refer to:
// https://www.debian.org/doc/debian-policy/ch-controlfields.html#special-version-conventions
// https://docs.fedoraproject.org/en-US/packaging-guidelines/Versioning/
// https://en.opensuse.org/openSUSE:Package_versioning_guidelines
//
// % rpmdev-vercmp 12.0.0 12.0.0-bp160.1.2
// 12.0.0 < 12.0.0-bp160.1.2
// 0.18.0 < 1.0.9-160000.2.2
// 3.007004 > 3.18.0-bp160.1.10

impl PackageVersion {
    /// Compare two package versions according to Debian/RPM rules
    pub fn compare(&self, other: &PackageVersion) -> Ordering {
        self.compare_with_format(other, None)
    }

    /// Compare two package versions according to format-specific rules
    fn compare_with_format(&self, other: &PackageVersion, format: Option<PackageFormat>) -> Ordering {
        // 1. Compare epoch first (highest priority)
        match self.epoch.cmp(&other.epoch) {
            Ordering::Equal => {},
            other => return other,
        }

        // 2. Compare upstream version (middle priority)
        match Self::compare_upstream_with_format(&self.upstream, &other.upstream, format) {
            Ordering::Equal => {},
            other => return other,
        }

        // 3. Compare revision (lowest priority)
        // Special handling for Alpine/APK format: -r0, -r1, etc.
        // Strip 'r' prefix if present and compare numerically
        let rev_a = Self::normalize_apk_revision(&self.revision);
        let rev_b = Self::normalize_apk_revision(&other.revision);

        // Special handling for RPM format: "0" (no revision) should be less than any non-zero revision
        // According to RPM rules: 12.0.0 < 12.0.0-bp160.1.2
        if let Some(fmt) = format {
            if fmt == PackageFormat::Rpm || fmt == PackageFormat::Pacman {
                // In RPM/Pacman, "0" means no revision, which should be less than any actual revision
                match (rev_a.as_str(), rev_b.as_str()) {
                    ("0", rev_b) if rev_b != "0" => return Ordering::Less,
                    (rev_a, "0") if rev_a != "0" => return Ordering::Greater,
                    _ => {} // Both are "0" or both are non-zero, continue with normal comparison
                }
            }
        }

        Self::compare_upstream_with_format(&rev_a, &rev_b, format)
    }

    /// Compare upstream version strings using format-specific rules
    ///
    /// For RPM format:
    /// - Numbers have higher precedence than letters
    /// - So "2.1.76" > "2.1.fb69" (numeric segment > alphabetic segment)
    ///
    /// For Debian format:
    /// - Letters have higher precedence than numbers (unless letters start with ~)
    /// - So "2.1.fb69" > "2.1.76" (alphabetic segment > numeric segment)
    ///
    /// For Conda format:
    /// - Underscores in version strings are treated as separators (like dots)
    /// - So "1.3_7" is equivalent to "1.3.7" for comparison purposes
    ///
    /// Common rules:
    /// - ~ (tilde) has lowest priority (pre-release indicator)
    /// - Numbers are compared numerically
    /// - Letters are compared by ASCII value
    fn compare_upstream_with_format(a: &str, b: &str, format: Option<PackageFormat>) -> Ordering {
        // Normalize Conda versions: convert underscores to dots
        // This handles cases like "1.3_7" which should be treated as "1.3.7"
        let (a_normalized, b_normalized) = if format == Some(PackageFormat::Conda) {
            (a.replace('_', "."), b.replace('_', "."))
        } else {
            (a.to_string(), b.to_string())
        };
        let a = a_normalized.as_str();
        let b = b_normalized.as_str();

        // Special case: if one version is a prefix of the other and the continuation
        // starts with a recognized pre-release marker (dash + pre-release marker), the shorter one is newer
        if let Some(stripped_a) = a.strip_prefix(b) {
            if stripped_a.starts_with('-') && stripped_a.len() > 1 {
                let after_dash = &stripped_a[1..];
                // Only treat as pre-release if it's a recognized pre-release marker
                if Self::is_prerelease_marker(after_dash) {
                    return Ordering::Less; // a < b (pre-release < final)
                }
            }
        }
        if let Some(stripped_b) = b.strip_prefix(a) {
            if stripped_b.starts_with('-') && stripped_b.len() > 1 {
                let after_dash = &stripped_b[1..];
                // Only treat as pre-release if it's a recognized pre-release marker
                if Self::is_prerelease_marker(after_dash) {
                    return Ordering::Greater; // a > b (final > pre-release)
                }
            }
        }

        // Special case for Conda: handle pre-release markers directly appended (e.g., "6.10.0a0")
        // If one version is a prefix of the other and the continuation is a pre-release marker
        // (without a dash), treat the longer one as pre-release
        // Only apply this for Conda format, not for Debian where ~ has special meaning
        if format == Some(PackageFormat::Conda) {
            if let Some(stripped_a) = a.strip_prefix(b) {
                if !stripped_a.is_empty() && Self::is_prerelease_marker(stripped_a) {
                    return Ordering::Less; // a < b (pre-release < final)
                }
            }
            if let Some(stripped_b) = b.strip_prefix(a) {
                if !stripped_b.is_empty() && Self::is_prerelease_marker(stripped_b) {
                    return Ordering::Greater; // a > b (final > pre-release)
                }
            }
        }

        // Special case: handle versions like "48.alpha" vs "48.4"
        // If one version ends with a pre-release marker (like ".alpha") and the other
        // continues with a number, treat the pre-release as less
        if let Some(dot_pos) = a.rfind('.') {
            let after_dot = &a[dot_pos + 1..];
            if Self::is_prerelease_marker(after_dot) {
                if let Some(b_dot_pos) = b.rfind('.') {
                    let b_after_dot = &b[b_dot_pos + 1..];
                    if b_after_dot.chars().next().map_or(false, |c| c.is_ascii_digit()) {
                        // a ends with pre-release marker, b continues with number
                        // Check if the prefixes before the last dot match
                        if a[..dot_pos] == b[..b_dot_pos] {
                            return Ordering::Less; // pre-release < final
                        }
                    }
                }
            }
        }
        if let Some(dot_pos) = b.rfind('.') {
            let after_dot = &b[dot_pos + 1..];
            if Self::is_prerelease_marker(after_dot) {
                if let Some(a_dot_pos) = a.rfind('.') {
                    let a_after_dot = &a[a_dot_pos + 1..];
                    if a_after_dot.chars().next().map_or(false, |c| c.is_ascii_digit()) {
                        // b ends with pre-release marker, a continues with number
                        // Check if the prefixes before the last dot match
                        if b[..dot_pos] == a[..a_dot_pos] {
                            return Ordering::Greater; // final > pre-release
                        }
                    }
                }
            }
        }

        // Use format-specific comparison if format is specified
        if let Some(fmt) = format {
            match fmt {
                PackageFormat::Rpm | PackageFormat::Pacman => {
                    // For RPM/Pacman, use RPM-style comparison (numbers > letters)
                    return Self::rpm_version_compare(a, b);
                }
                _ => {
                    // For other formats, use Debian-style comparison
                    if a.contains('~') || b.contains('~') || a.contains('+') || b.contains('+') ||
                       a.chars().any(|c| c.is_ascii_alphabetic()) || b.chars().any(|c| c.is_ascii_alphabetic()) {
                        return Self::debian_version_compare(a, b);
                    }
                    // Try using the versions crate first for standard semantic versioning
                    if let (Some(ver_a), Some(ver_b)) = (Versioning::new(a), Versioning::new(b)) {
                        return ver_a.cmp(&ver_b);
                    }
                    // Fall back to Debian-style comparison for non-semantic versions
                    return Self::debian_version_compare(a, b);
                }
            }
        }

        // Default behavior (when format is None): use Debian-style comparison
        // Use Debian-style comparison if either version contains special Debian characters
        // or contains letters (for revision strings like "0ubuntu8.6")
        if a.contains('~') || b.contains('~') || a.contains('+') || b.contains('+') ||
           a.chars().any(|c| c.is_ascii_alphabetic()) || b.chars().any(|c| c.is_ascii_alphabetic()) {
            return Self::debian_version_compare(a, b);
        }

        // Try using the versions crate first for standard semantic versioning
        if let (Some(ver_a), Some(ver_b)) = (Versioning::new(a), Versioning::new(b)) {
            return ver_a.cmp(&ver_b);
        }

        // Fall back to Debian-style comparison for non-semantic versions
        Self::debian_version_compare(a, b)
    }

    /// RPM-style version comparison implementation
    /// In RPM, numbers have higher precedence than letters
    /// Dots are segment separators, so we compare segment by segment
    fn rpm_version_compare(a: &str, b: &str) -> Ordering {
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
                    // Extract next segment (numbers or non-numbers), stopping at dots
                    let (seg_a, is_num_a) = Self::extract_segment_stopping_at_dot(&mut chars_a);
                    let (seg_b, is_num_b) = Self::extract_segment_stopping_at_dot(&mut chars_b);

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
                            // Both are non-numbers - use character comparison
                            match Self::rpm_string_compare(&seg_a, &seg_b) {
                                Ordering::Equal => continue,
                                other => return other,
                            }
                        },
                        (true, false) => {
                            // Number vs non-number: in RPM, number has higher precedence
                            // unless the non-number starts with ~
                            if seg_b.starts_with('~') {
                                return Ordering::Greater;
                            } else {
                                return Ordering::Greater; // Number > letter in RPM
                            }
                        },
                        (false, true) => {
                            // Non-number vs number: in RPM, number has higher precedence
                            if seg_a.starts_with('~') {
                                return Ordering::Less;
                            } else {
                                return Ordering::Less; // Letter < number in RPM
                            }
                        }
                    }
                }
            }
        }
    }

    /// Compare non-numeric strings using RPM precedence rules
    /// ~ < letters < other symbols
    fn rpm_string_compare(a: &str, b: &str) -> Ordering {
        let mut chars_a = a.chars();
        let mut chars_b = b.chars();

        loop {
            match (chars_a.next(), chars_b.next()) {
                (None, None) => return Ordering::Equal,
                (None, Some(ch_b)) => {
                    // a ended, b continues
                    if ch_b == '~' {
                        return Ordering::Greater;
                    } else {
                        return Ordering::Less;
                    }
                },
                (Some(ch_a), None) => {
                    // b ended, a continues
                    if ch_a == '~' {
                        return Ordering::Less;
                    } else {
                        return Ordering::Greater;
                    }
                },
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
                (None, Some(ch_b)) => {
                    // a ended, b continues
                    // If b continues with ~, a > b (something > ~)
                    // Otherwise, a < b (shorter < longer)
                    if ch_b == '~' {
                        return Ordering::Greater;
                    } else {
                        return Ordering::Less;
                    }
                },
                (Some(ch_a), None) => {
                    // b ended, a continues
                    // If a continues with ~, a < b (~ < something)
                    // Otherwise, a > b (longer > shorter)
                    if ch_a == '~' {
                        return Ordering::Less;
                    } else {
                        return Ordering::Greater;
                    }
                },
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

/// Compare versions using format-specific logic
/// Returns Some(Ordering) if comparison succeeds, None if parsing fails
pub fn compare_versions(
    version1: &str,
    version2: &str,
    format: PackageFormat,
) -> Option<Ordering> {
    // Use epkg's version comparison for all formats
    compare_versions_epkg_with_format(version1, version2, Some(format))
}

/// Use epkg's version comparison with format
fn compare_versions_epkg_with_format(version1: &str, version2: &str, format: Option<PackageFormat>) -> Option<Ordering> {
    match (PackageVersion::parse(version1), PackageVersion::parse(version2)) {
        (Ok(v1), Ok(v2)) => {
            Some(v1.compare_with_format(&v2, format))
        }
        _ => {
            log::warn!("Failed to parse versions: '{}' vs '{}'", version1, version2);
            None
        }
    }
}

/// Compare only upstream versions (epoch:upstream) for RPM, Pacman, and Apk formats
/// In these formats, "=" means upstream version must match, release can differ
#[allow(dead_code)]
fn compare_upstream_versions(
    package_version: &str,
    constraint_operand: &str,
) -> Result<bool> {
    compare_upstream_versions_with_format(package_version, constraint_operand, None)
}

pub fn compare_upstream_versions_with_format(
    package_version: &str,
    constraint_operand: &str,
    format: Option<PackageFormat>,
) -> Result<bool> {
    match (PackageVersion::parse(package_version), PackageVersion::parse(constraint_operand)) {
        (Ok(pkg_ver), Ok(constraint_ver)) => {
            // Check if either argument has an explicit epoch (contains a colon)
            // If neither has an explicit epoch, compare epochs normally
            // If one has an explicit epoch and the other doesn't, skip epoch comparison
            // This handles cases like constraint "11.9.0" matching package "1:11.9.0-1"
            // and makes the function symmetric for check_version_equal
            let package_has_explicit_epoch = package_version.contains(':');
            let constraint_has_explicit_epoch = constraint_operand.contains(':');

            // Only compare epochs if both have explicit epochs, or neither has explicit epoch
            // If one has explicit epoch and the other doesn't, skip epoch comparison
            if package_has_explicit_epoch && constraint_has_explicit_epoch {
                // Both have explicit epochs - compare epochs first
                match pkg_ver.epoch.cmp(&constraint_ver.epoch) {
                    Ordering::Equal => {},
                    _ => return Ok(false),
                }
            } else if !package_has_explicit_epoch && !constraint_has_explicit_epoch {
                // Neither has explicit epoch - compare epochs normally (both default to 0)
                match pkg_ver.epoch.cmp(&constraint_ver.epoch) {
                    Ordering::Equal => {},
                    _ => return Ok(false),
                }
            }
            // If one has explicit epoch and the other doesn't, skip epoch comparison
            // and proceed to compare upstream versions

            // Compare upstream version (ignoring revision/release)
            match PackageVersion::compare_upstream_with_format(&pkg_ver.upstream, &constraint_ver.upstream, format) {
                Ordering::Equal => Ok(true),
                _ => {
                    // Special case: if package upstream starts with constraint upstream followed by a dash or dot,
                    // treat the constraint as matching (e.g., "9.8.3-bp160.1.34" matches constraint "9.8.3",
                    // and "1.14.6" matches constraint "1.14")
                    // This handles:
                    // - RPM releases that start with letters (like "bp160.1.34", "el8", "fc35") - remainder starts with '-'
                    // - Version extensions (like "1.14.6" for constraint "1.14") - remainder starts with '.'
                    if pkg_ver.upstream.starts_with(&constraint_ver.upstream) {
                        let remainder = &pkg_ver.upstream[constraint_ver.upstream.len()..];
                        // If remainder starts with a dash, it's a release/build identifier
                        // If remainder starts with a dot, it's a version extension (e.g., "1.14.6" for "1.14")
                        // This means the constraint is a complete version prefix
                        if remainder.starts_with('-') || remainder.starts_with('.') {
                            Ok(true)
                        } else {
                            Ok(false)
                        }
                    } else {
                        Ok(false)
                    }
                }
            }
        }
        _ => {
            Err(color_eyre::eyre::eyre!("Failed to parse versions for upstream comparison: '{}' vs '{}'", package_version, constraint_operand))
        }
    }
}

/// Check if a package version satisfies APK fuzzy version constraint
///
/// APK fuzzy version matching (using ~ operator) matches versions that share the same prefix.
/// For example: ~2.2 matches 2.2, 2.2.0, 2.2.1 but not 2.10 or 2.20.
///
/// Reference: apk-tools/src/version.c apk_version_compare_fuzzy()
/// https://github.com/alpinelinux/apk-tools/blob/master/src/version.c#L281
///
/// The logic matches when the constraint version reaches TOKEN_END while fuzzy is true,
/// meaning the constraint is a prefix of the package version.
pub fn check_apk_fuzzy_version(
    package_version: &str,
    normalized_operand: &str,
) -> Option<bool> {
    // Parse both versions and compare their upstream parts
    match (PackageVersion::parse(package_version), PackageVersion::parse(normalized_operand)) {
        (Ok(pkg_ver), Ok(operand_ver)) => {
            // Compare epochs first - they must match
            if pkg_ver.epoch != operand_ver.epoch {
                return Some(false);
            }

            // Check if the package version's upstream starts with the operand's upstream
            // For example: "2.2.0" starts with "2.2", "2.10" does NOT start with "2.2"
            let pkg_upstream = &pkg_ver.upstream;
            let operand_upstream = &operand_ver.upstream;

            // Check if pkg_upstream starts with operand_upstream as a prefix
            // But we need to ensure we match on version component boundaries
            // "2.2" should match "2.2", "2.2.0", "2.2.1" but not "2.10", "2.20"
            if pkg_upstream == operand_upstream {
                // Exact match
                Some(true)
            } else if pkg_upstream.starts_with(operand_upstream) {
                // Check if the next character after the prefix is a separator (., -, _) or end of string
                // This ensures "2.2" matches "2.2.0" but not "2.20"
                let next_char = pkg_upstream.chars().nth(operand_upstream.len());
                match next_char {
                    None => Some(true), // operand is a prefix and we're at the end
                    Some(ch) if ch == '.' || ch == '-' || ch == '_' => Some(true), // valid separator
                    _ => Some(false), // not a valid match (e.g., "2.20" doesn't start with "2.2" properly)
                }
            } else {
                // Prefix doesn't match - version is not compatible
                // For example: "2.10" does NOT start with "2.2", so it doesn't satisfy ~2.2
                Some(false)
            }
        }
        _ => {
            // If parsing fails, fall back to simple prefix check
            let pkg_upstream = PackageVersion::parse(package_version)
                .map(|v| v.upstream)
                .unwrap_or_else(|_| package_version.to_string());
            let operand_upstream = PackageVersion::parse(normalized_operand)
                .map(|v| v.upstream)
                .unwrap_or_else(|_| normalized_operand.to_string());

            if pkg_upstream.starts_with(&operand_upstream) {
                let next_char = pkg_upstream.chars().nth(operand_upstream.len());
                match next_char {
                    None => Some(true),
                    Some(ch) if ch == '.' || ch == '-' || ch == '_' => Some(true),
                    _ => Some(false),
                }
            } else {
                Some(false)
            }
        }
    }
}

/// Check if a package version satisfies Python PEP 440 compatible release constraint
///
/// PEP 440 compatible release (using ~= operator) matches versions that are >= operand
/// and share the same prefix based on the number of segments in the operand.
///
/// Examples:
/// - ~= 2.2 means >= 2.2, == 2.* (matches 2.x but not 3.x)
/// - ~= 2.2.0 means >= 2.2.0, == 2.2.* (matches 2.2.x but not 2.3.x)
/// - ~= 2.2.post3 means >= 2.2.post3, == 2.* (suffix ignored for prefix match)
///
/// Reference: PEP 440 - Compatible Release
/// https://peps.python.org/pep-0440/#compatible-release
pub fn check_python_compatible_release(
    package_version: &str,
    normalized_operand: &str,
    format: PackageFormat,
) -> Option<bool> {
    // Extract the release identifier (numeric part before pre/post-release markers)
    // For "2.2.post3", release is "2.2"
    // For "1.4.5a4", release is "1.4.5"
    // For "2.2.0", release is "2.2.0"
    let release_part = if let Some(pos) = normalized_operand.find(|c: char| c.is_alphabetic() && c != '.') {
        // Found a letter (pre/post-release marker), extract everything before it
        &normalized_operand[..pos].trim_end_matches('.')
    } else {
        // No pre/post-release markers, use the whole operand
        normalized_operand
    };

    // Count segments in the release identifier
    let release_segments: Vec<&str> = release_part.split('.').collect();
    let num_segments = release_segments.len();

    if num_segments < 2 {
        // PEP 440: "This operator MUST NOT be used with a single segment version number such as ~=1"
        // But we'll handle it gracefully by treating it as >= operand
        return compare_versions(package_version, normalized_operand, format)
            .map(|cmp| cmp != Ordering::Less);
    }

    // The prefix is the first (num_segments - 1) segments of the release identifier
    // For "2.2" (2 segments), prefix is "2" (1 segment)
    // For "2.2.0" (3 segments), prefix is "2.2" (2 segments)
    let prefix_segments = &release_segments[..num_segments - 1];
    let prefix = prefix_segments.join(".");

    // Check: package_version >= operand AND package_version starts with prefix
    let ge_check = compare_versions(package_version, normalized_operand, format)
        .map(|cmp| cmp != Ordering::Less);

    if ge_check == Some(false) {
        // Doesn't satisfy >= operand
        return Some(false);
    }

    // Extract release part from package version for prefix matching
    let pkg_release_part = if let Some(pos) = package_version.find(|c: char| c.is_alphabetic() && c != '.') {
        &package_version[..pos].trim_end_matches('.')
    } else {
        package_version
    };

    // Check if package version's release part starts with the prefix
    // For "2.2", prefix is "2", so match "2.0", "2.1", "2.2", "2.3", etc. but not "3.0"
    let prefix_check = if pkg_release_part.starts_with(&prefix) {
        // Check if the next character after the prefix is a dot or end of string
        let next_char = pkg_release_part.chars().nth(prefix.len());
        match next_char {
            None => true, // Exact match with prefix
            Some(ch) if ch == '.' => true, // Valid separator
            _ => false, // Not a valid match
        }
    } else {
        false
    };

    // Both conditions must be true: >= operand AND prefix matches
    ge_check.and_then(|ge| Some(ge && prefix_check))
}

/// Compare upstream versions (ignoring revision/release) and return Ordering
/// This is useful for VersionCompatible operator which should ignore revisions
pub fn compare_upstream_versions_ordering_with_format(
    package_version: &str,
    constraint_operand: &str,
    format: Option<PackageFormat>,
) -> Option<Ordering> {
    match (PackageVersion::parse(package_version), PackageVersion::parse(constraint_operand)) {
        (Ok(pkg_ver), Ok(constraint_ver)) => {
            // Check if either argument has an explicit epoch (contains a colon)
            // If neither has an explicit epoch, compare epochs normally
            // If one has an explicit epoch and the other doesn't, skip epoch comparison
            // This handles cases like constraint "11.9.0" matching package "1:11.9.0-1"
            let package_has_explicit_epoch = package_version.contains(':');
            let constraint_has_explicit_epoch = constraint_operand.contains(':');

            // Only compare epochs if both have explicit epochs, or neither has explicit epoch
            // If one has explicit epoch and the other doesn't, skip epoch comparison
            if package_has_explicit_epoch && constraint_has_explicit_epoch {
                // Both have explicit epochs - compare epochs first
                match pkg_ver.epoch.cmp(&constraint_ver.epoch) {
                    Ordering::Equal => {},
                    other => return Some(other),
                }
            } else if !package_has_explicit_epoch && !constraint_has_explicit_epoch {
                // Neither has explicit epoch - compare epochs normally (both default to 0)
                match pkg_ver.epoch.cmp(&constraint_ver.epoch) {
                    Ordering::Equal => {},
                    other => return Some(other),
                }
            }
            // If one has explicit epoch and the other doesn't, skip epoch comparison
            // and proceed to compare upstream versions

            // Compare upstream version (ignoring revision/release)
            Some(PackageVersion::compare_upstream_with_format(&pkg_ver.upstream, &constraint_ver.upstream, format))
        }
        _ => None,
    }
}

/// Compute the next version after a given base version
/// Used for RPM's ~~ operator: "X~~" means "less than the next version after X"
/// Examples:
/// - "2" -> "3" (increment the numeric segment)
/// - "0.7.5" -> "0.7.6" (increment the last numeric segment)
/// - "1.20" -> "1.21" (increment the last numeric segment)
///
/// Strategy: Increment the last numeric segment in the version string.
/// For simple integer versions like "2", increment the only segment to get "3".
pub fn compute_next_version(base: &str) -> String {
    // Handle versions with dots: find the last numeric segment and increment it
    // For "0.7.5", we want "0.7.6" (increment the "5")
    // For "2", we want "3" (increment the "2")

    // Split by dots to find segments
    let segments: Vec<&str> = base.split('.').collect();

    if segments.is_empty() {
        return base.to_string();
    }

    // Find the last segment that starts with a digit
    let mut last_num_idx = None;
    let mut last_num_value = 0u64;

    for (idx, segment) in segments.iter().enumerate().rev() {
        // Extract the numeric prefix of this segment
        let num_part: String = segment.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !num_part.is_empty() {
            if let Ok(num) = num_part.parse::<u64>() {
                last_num_idx = Some(idx);
                last_num_value = num;
                break;
            }
        }
    }

    if let Some(idx) = last_num_idx {
        // Increment the last numeric segment
        let next_num = last_num_value + 1;
        let mut result_segments: Vec<String> = segments.iter().map(|s| s.to_string()).collect();

        // Replace the segment at idx with the incremented value
        let old_segment = &segments[idx];
        let num_part_len = old_segment.chars().take_while(|c| c.is_ascii_digit()).count();
        let suffix = &old_segment[num_part_len..];
        result_segments[idx] = format!("{}{}", next_num, suffix);

        // Reconstruct the version string
        result_segments.join(".")
    } else {
        // No numeric segment found, try to parse the whole string as a number
        if let Ok(num) = base.parse::<u64>() {
            return (num + 1).to_string();
        }
        // Fallback: return base as-is
        base.to_string()
    }
}

/// Normalize a version string for equality comparison based on format-specific rules
///
/// For Debian format, strips local version suffixes (everything after the last +)
/// This allows packages with local builds (e.g., "1.0+2") to satisfy dependencies
/// on the base version (e.g., "= 1.0").
///
/// Note: + can appear in upstream versions (e.g., "+dfsg1"), so we only strip
/// the last + suffix which is the local version indicator.
pub fn normalize_version_for_equality(version: &str, format: PackageFormat) -> &str {
    if format == PackageFormat::Deb {
        // For Debian, local version suffixes (everything after the last +) should be ignored
        // when matching exact version constraints
        if let Some(pos) = version.rfind('+') {
            &version[..pos]
        } else {
            version
        }
    } else {
        version
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version_constraint::check_version_constraint;
    use crate::parse_requires::{VersionConstraint, Operator};

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

        // Test that versions with non-pre-release markers in upstream are compared correctly
        // "6.0-b24-1" should be greater than "6.0" (b24 is not a pre-release marker)
        assert!(is_version_newer("6.0-b24-1", "6.0"));
        assert!(!is_version_newer("6.0", "6.0-b24-1"));

        // Test that actual pre-release markers still work correctly
        assert!(!is_version_newer("6.0-rc1", "6.0"));
        assert!(is_version_newer("6.0", "6.0-rc1"));
    }

    #[test]
    fn test_compare_versions_deb() {

        // Test Debian format version comparison
        assert_eq!(
            compare_versions("1.0-1", "1.0-2", PackageFormat::Deb),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("1.0-2", "1.0-1", PackageFormat::Deb),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_versions("1.0-1", "1.0-1", PackageFormat::Deb),
            Some(Ordering::Equal)
        );

        // Test with epoch
        assert_eq!(
            compare_versions("2:1.0-1", "1:2.0-1", PackageFormat::Deb),
            Some(Ordering::Greater)
        );

        // Test with complex versions
        assert_eq!(
            compare_versions("2:1.18.3~beta+dfsg1-6", "2:1.18.3~beta+dfsg1-5+b1", PackageFormat::Deb),
            Some(Ordering::Greater)
        );

        // Test alphabetic revision: numeric revision < alphabetic revision
        assert_eq!(
            compare_versions("2.14.14-1", "2.14.14-z", PackageFormat::Deb),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("2.14.14-z", "2.14.14-1", PackageFormat::Deb),
            Some(Ordering::Greater)
        );
    }

    #[test]
    fn test_debian_plus_tilde_comparison() {

        // Test Debian format with + and ~: 2.5.2-1+1 >= 2.5.2-1+~
        // The ~ character has lowest precedence, so 1+~ < 1+1
        assert_eq!(
            compare_versions("2.5.2-1+1", "2.5.2-1+~", PackageFormat::Deb),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_versions("2.5.2-1+~", "2.5.2-1+1", PackageFormat::Deb),
            Some(Ordering::Less)
        );

        // Test parsing
        let v1 = PackageVersion::parse("2.5.2-1+~").unwrap();
        assert_eq!(v1.upstream, "2.5.2");
        assert_eq!(v1.revision, "1+~");

        let v2 = PackageVersion::parse("2.5.2-1+1").unwrap();
        assert_eq!(v2.upstream, "2.5.2");
        assert_eq!(v2.revision, "1+1");
    }

    #[test]
    fn test_compare_versions_rpm() {

        // Test RPM format version comparison
        assert_eq!(
            compare_versions("1.0-1.el8", "1.0-2.el8", PackageFormat::Rpm),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("1.0-2.el8", "1.0-1.el8", PackageFormat::Rpm),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_versions("1.0-1.el8", "1.0-1.el8", PackageFormat::Rpm),
            Some(Ordering::Equal)
        );

        // Test with epoch
        assert_eq!(
            compare_versions("2:1.0-1", "1:2.0-1", PackageFormat::Rpm),
            Some(Ordering::Greater)
        );

        // Test semantic versioning: 1.0.9 should be > 0.18.0
        assert_eq!(
            compare_versions("1.0.9", "0.18.0", PackageFormat::Rpm),
            Some(Ordering::Greater),
            "1.0.9 should be greater than 0.18.0"
        );
        assert_eq!(
            compare_versions("0.18.0", "1.0.9", PackageFormat::Rpm),
            Some(Ordering::Less),
            "0.18.0 should be less than 1.0.9"
        );

        // Test pkgconfig version comparison: 7.3.0 should be >= 7.0.99.1
        assert_eq!(
            compare_versions("7.3.0", "7.0.99.1", PackageFormat::Rpm),
            Some(Ordering::Greater),
            "7.3.0 should be greater than 7.0.99.1"
        );
        assert_eq!(
            compare_versions("7.0.99.1", "7.3.0", PackageFormat::Rpm),
            Some(Ordering::Less),
            "7.0.99.1 should be less than 7.3.0"
        );

        // Test RPM revision comparison: version with release > version without release
        // According to RPM rules: 12.0.0 < 12.0.0-bp160.1.2
        assert_eq!(
            compare_versions("1.0.11-bp160.1.13", "1.0.11", PackageFormat::Rpm),
            Some(Ordering::Greater),
            "1.0.11-bp160.1.13 should be greater than 1.0.11 (version with release > version without)"
        );
        assert_eq!(
            compare_versions("1.0.11", "1.0.11-bp160.1.13", PackageFormat::Rpm),
            Some(Ordering::Less),
            "1.0.11 should be less than 1.0.11-bp160.1.13 (version without release < version with release)"
        );
        assert_eq!(
            compare_versions("12.0.0-bp160.1.2", "12.0.0", PackageFormat::Rpm),
            Some(Ordering::Greater),
            "12.0.0-bp160.1.2 should be greater than 12.0.0"
        );
        assert_eq!(
            compare_versions("12.0.0", "12.0.0-bp160.1.2", PackageFormat::Rpm),
            Some(Ordering::Less),
            "12.0.0 should be less than 12.0.0-bp160.1.2"
        );
    }

    #[test]
    fn test_compare_versions_pacman() {

        // Test Pacman format version comparison
        assert_eq!(
            compare_versions("1.0-1", "1.0-2", PackageFormat::Pacman),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("1.0-2", "1.0-1", PackageFormat::Pacman),
            Some(Ordering::Greater)
        );

        // Test Pacman revision comparison: version with release > version without release
        // Same rule as RPM: version with release > version without release
        assert_eq!(
            compare_versions("1.0.11-bp160.1.13", "1.0.11", PackageFormat::Pacman),
            Some(Ordering::Greater),
            "1.0.11-bp160.1.13 should be greater than 1.0.11 in Pacman format"
        );
        assert_eq!(
            compare_versions("1.0.11", "1.0.11-bp160.1.13", PackageFormat::Pacman),
            Some(Ordering::Less),
            "1.0.11 should be less than 1.0.11-bp160.1.13 in Pacman format"
        );
    }

    #[test]
    fn test_compare_versions_apk() {

        // Test APK format version comparison
        assert_eq!(
            compare_versions("1.0-r1", "1.0-r2", PackageFormat::Apk),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("1.0-r2", "1.0-r1", PackageFormat::Apk),
            Some(Ordering::Greater)
        );
    }

    #[test]
    fn test_compare_versions_conda() {

        // Test Conda format version comparison
        let result1 = compare_versions("1.0.0", "1.0.1", PackageFormat::Conda);
        assert!(result1.is_some());
        assert_eq!(result1.unwrap(), Ordering::Less);

        let result2 = compare_versions("1.0.1", "1.0.0", PackageFormat::Conda);
        assert!(result2.is_some());
        assert_eq!(result2.unwrap(), Ordering::Greater);

        let result3 = compare_versions("1.0.0", "1.0.0", PackageFormat::Conda);
        assert!(result3.is_some());
        assert_eq!(result3.unwrap(), Ordering::Equal);
    }

    #[test]
    fn test_check_version_constraint_rpm_dot_extension() {

        // Test that RPM VersionEqual constraint allows dot extensions
        // This is the specific bug: hdf5(=1.14) should match hdf5-1.14.6-3.fc42
        let constraint = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "1.14".to_string(),
        };

        assert!(
            check_version_constraint("1.14.6-3.fc42", &constraint, PackageFormat::Rpm).unwrap(),
            "Constraint 'hdf5(=1.14)' should match package version '1.14.6-3.fc42'"
        );
    }

    #[test]
    fn test_compare_versions_python() {

        // Test Python format version comparison
        let result1 = compare_versions("1.0.0", "1.0.1", PackageFormat::Python);
        assert!(result1.is_some());
        assert_eq!(result1.unwrap(), Ordering::Less);

        let result2 = compare_versions("1.0.1", "1.0.0", PackageFormat::Python);
        assert!(result2.is_some());
        assert_eq!(result2.unwrap(), Ordering::Greater);
    }

    #[test]
    fn test_compare_versions_edge_cases() {

        // Test edge cases - the parser is lenient and handles most inputs
        // Empty strings get parsed as epoch=0, upstream="", revision="0"
        let result = compare_versions("", "1.0", PackageFormat::Deb);
        assert!(result.is_some()); // Parser is lenient, so this succeeds

        // Test that valid comparisons work correctly
        assert_eq!(
            compare_versions("1.0", "2.0", PackageFormat::Deb),
            Some(Ordering::Less)
        );
    }

    #[test]
    fn test_compare_upstream_versions_equal() {
        // Test that upstream versions match when epochs and upstream parts are equal
        // (revision should be ignored)
        assert!(compare_upstream_versions("1.0-1", "1.0-2").unwrap());
        assert!(compare_upstream_versions("1.0-10", "1.0-1").unwrap());
        assert!(compare_upstream_versions("2:1.0-1", "2:1.0-5").unwrap());

        // Different upstream versions should not match
        assert!(!compare_upstream_versions("1.0-1", "1.1-1").unwrap());
        assert!(!compare_upstream_versions("2:1.0-1", "1:1.0-1").unwrap());
    }

    #[test]
    fn test_compare_upstream_versions_with_dash_suffix() {
        // Test the special case where package upstream starts with constraint upstream followed by a dash
        // e.g., "9.8.3-bp160.1.34" should match constraint "9.8.3"
        assert!(compare_upstream_versions("9.8.3-bp160.1.34-1", "9.8.3-1").unwrap());
        assert!(compare_upstream_versions("9.8.3-el8-1", "9.8.3-1").unwrap());
        assert!(compare_upstream_versions("9.8.3-fc35-1", "9.8.3-1").unwrap());

        // And "9.8" should NOT match "9.8.3" (partial version)
        assert!(!compare_upstream_versions("9.8-1", "9.8.3-1").unwrap());
    }

    #[test]
    fn test_compare_upstream_versions_epoch_mismatch() {
        // Different epochs should not match when both have explicit epochs
        assert!(!compare_upstream_versions("2:1.0-1", "1:1.0-1").unwrap());
        assert!(!compare_upstream_versions("1:1.0-1", "2:1.0-1").unwrap());

        // Same epoch, different upstream should not match
        assert!(!compare_upstream_versions("1:1.0-1", "1:1.1-1").unwrap());
    }

    #[test]
    fn test_compare_upstream_versions_constraint_without_explicit_epoch() {
        // When constraint doesn't have explicit epoch, it should match package versions with any epoch
        // This fixes the bug where "libvirt(=11.9.0)" couldn't match "libvirt__1:11.9.0-1__x86_64"

        // Constraint "11.9.0" (no explicit epoch) should match package "1:11.9.0-1" (with epoch 1)
        assert!(compare_upstream_versions("1:11.9.0-1", "11.9.0").unwrap(),
                "Package '1:11.9.0-1' should match constraint '11.9.0' (no explicit epoch)");

        // Constraint "11.9.0" should also match package "2:11.9.0-1" (with epoch 2)
        assert!(compare_upstream_versions("2:11.9.0-1", "11.9.0").unwrap(),
                "Package '2:11.9.0-1' should match constraint '11.9.0' (no explicit epoch)");

        // Constraint "11.9.0" should match package "11.9.0-1" (no epoch, epoch defaults to 0)
        assert!(compare_upstream_versions("11.9.0-1", "11.9.0").unwrap(),
                "Package '11.9.0-1' should match constraint '11.9.0'");

        // But constraint with explicit epoch "1:11.9.0" should NOT match package "2:11.9.0-1"
        assert!(!compare_upstream_versions("2:11.9.0-1", "1:11.9.0").unwrap(),
                "Package '2:11.9.0-1' should NOT match constraint '1:11.9.0' (explicit epoch mismatch)");

        // Constraint with explicit epoch "1:11.9.0" should match package "1:11.9.0-1"
        assert!(compare_upstream_versions("1:11.9.0-1", "1:11.9.0").unwrap(),
                "Package '1:11.9.0-1' should match constraint '1:11.9.0' (explicit epoch match)");

        // Different upstream versions should still not match
        assert!(!compare_upstream_versions("1:11.9.0-1", "11.10.0").unwrap(),
                "Package '1:11.9.0-1' should NOT match constraint '11.10.0' (different upstream)");
    }

    #[test]
    fn test_compare_upstream_versions_complex() {
        // Test with complex RPM-style versions
        assert!(compare_upstream_versions("1.18.3~beta+dfsg1-6", "1.18.3~beta+dfsg1-5").unwrap());
        assert!(!compare_upstream_versions("1.18.3~beta+dfsg1-6", "1.18.4~beta+dfsg1-5").unwrap());

        // Test with versions that have pre-release markers in upstream
        assert!(compare_upstream_versions("1.0-rc1-5", "1.0-rc1-10").unwrap());
        assert!(!compare_upstream_versions("1.0-rc1-5", "1.0-rc2-1").unwrap());
    }

    #[test]
    fn test_rpm_version_comparison_fb69_vs_76() {

        // Test direct comparison: "2.1.76" > "2.1.fb69" in RPM
        let result = compare_versions("2.1.76", "2.1.fb69", PackageFormat::Rpm);
        assert_eq!(
            result,
            Some(Ordering::Greater),
            "2.1.76 should be greater than 2.1.fb69 in RPM format, got {:?}", result
        );

        // Test with full version strings
        let result2 = compare_versions("2.1.76-10.fc42", "2.1.fb69", PackageFormat::Rpm);
        assert_eq!(
            result2,
            Some(Ordering::Greater),
            "2.1.76-10.fc42 should be greater than 2.1.fb69 in RPM format, got {:?}", result2
        );

        // Now test constraint: "2.1.76-10.fc42" should satisfy >= "2.1.fb69" in RPM format
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "2.1.fb69".to_string(),
        };

        let satisfies = check_version_constraint("2.1.76-10.fc42", &constraint, PackageFormat::Rpm);
        assert!(satisfies.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies.unwrap(),
                "2.1.76-10.fc42 should satisfy >= 2.1.fb69 in RPM format");

        // Test with targetcli: "2.1.58" should satisfy >= "2.1.fb49" in RPM
        let result3 = compare_versions("2.1.58", "2.1.fb49", PackageFormat::Rpm);
        assert_eq!(
            result3,
            Some(Ordering::Greater),
            "2.1.58 should be greater than 2.1.fb49 in RPM format, got {:?}", result3
        );

        let constraint2 = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "2.1.fb49".to_string(),
        };
        let satisfies2 = check_version_constraint("2.1.58-4.fc42", &constraint2, PackageFormat::Rpm);
        assert!(satisfies2.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies2.unwrap(),
                "2.1.58-4.fc42 should satisfy >= 2.1.fb49 in RPM format");
    }

    #[test]
    fn test_compare_upstream_versions_edge_cases() {
        // Test edge cases - the parser is lenient and handles most inputs
        // Empty strings get parsed as epoch=0, upstream="", revision="0"
        let result = compare_upstream_versions("", "1.0");
        assert!(result.is_ok()); // Parser is lenient, so this succeeds

        // Test that valid comparisons work correctly
        assert!(compare_upstream_versions("1.0-1", "1.0-2").unwrap());
    }
}
