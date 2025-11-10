use color_eyre::Result;
use std::cmp::Ordering;
use versions::Versioning;
use crate::models::PackageFormat;
use crate::parse_requires::Operator;
use crate::conda_pkg::VERSION_BUILD_SEPARATOR;
use log;

// Refer to:
// https://www.debian.org/doc/debian-policy/ch-controlfields.html#special-version-conventions
// https://docs.fedoraproject.org/en-US/packaging-guidelines/Versioning/
// https://en.opensuse.org/openSUSE:Package_versioning_guidelines
//
// % rpmdev-vercmp 12.0.0 12.0.0-bp160.1.2
// 12.0.0 < 12.0.0-bp160.1.2
// 0.18.0 < 1.0.9-160000.2.2
// 3.007004 > 3.18.0-bp160.1.10

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

    /// Compare two package versions according to Debian/RPM rules
    fn compare(&self, other: &PackageVersion) -> Ordering {
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

    /// Normalize APK revision format for comparison
    /// In Alpine/APK, -r0, -r1, etc. are explicit revisions.
    /// For comparison: strip 'r' prefix and compare numerically, but mark as explicit
    /// Explicit revisions should compare less than implicit ones when numeric values are equal.
    fn normalize_apk_revision(revision: &str) -> String {
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
    fn is_prerelease_marker(after_dash: &str) -> bool {
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

    /// Extract a segment stopping at dots (for RPM version comparison)
    fn extract_segment_stopping_at_dot(chars: &mut std::iter::Peekable<std::str::Chars>) -> (String, bool) {
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

fn compare_upstream_versions_with_format(
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
fn check_apk_fuzzy_version(
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
fn check_python_compatible_release(
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
fn compare_upstream_versions_ordering_with_format(
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
fn compute_next_version(base: &str) -> String {
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

/// Normalize a constraint operand based on format-specific rules
///
/// Handles:
/// - RPM-style `~~` suffix (e.g., "0.7.5~~")
/// - Debian-style `~` suffix (e.g., "0.12.0~")
///
/// Returns the normalized operand and a flag indicating if >= should be used instead of >
fn normalize_constraint_operand(
    operand: &str,
    operator: &Operator,
    format: PackageFormat,
    provided_version: Option<&str>,
) -> (String, bool) {
    let mut use_greater_than_equal = false;

    let normalized = if operand.ends_with("~~") {
        // Handle RPM-style ~~ suffix (also used in Debian Rust packages)
        // In RPM, "X~~" means "less than next version", so:
        // - ">= X~~" means ">= X" (we can strip ~~ for >= comparisons)
        // - "< X~~" means "< next version after X" (e.g., "< 0.7.6" for "0.7.5~~")
        let base = operand.trim_end_matches("~~").trim_end_matches('-');
        match operator {
            Operator::VersionGreaterThanEqual => {
                // For >=, strip ~~ and compare normally
                base.to_string()
            }
            Operator::VersionGreaterThan => {
                // For >, check if base version has a revision (dash followed by digit)
                // If it does, treat > X-Y~~ as >= X-Y (because X-Y~~ means "less than next version after X-Y")
                // Otherwise, treat > X~~ as > X (strict comparison)
                let has_revision = if let Some(dash_pos) = base.rfind('-') {
                    let after_dash = &base[dash_pos + 1..];
                    !after_dash.is_empty() && after_dash.chars().next().unwrap().is_ascii_digit()
                } else {
                    false
                };
                if has_revision {
                    // Use >= instead of > for versions with revisions
                    use_greater_than_equal = true;
                }
                base.to_string()
            }
            Operator::VersionLessThan => {
                // For <, we need to compute the next version after the base
                // In RPM, "X~~" means "less than the next version after X"
                // For example: "< 0.7.5~~" means "< 0.7.6"
                // The base version (0.7.5) should NOT satisfy "< 0.7.5~~" because
                // it means "less than the next version", and the base version itself
                // is at the boundary (not strictly less than the next version)
                compute_next_version(&base)
            }
            Operator::VersionLessThanEqual => {
                // For <=, use the base version directly
                // The base version should satisfy "<= X~~" (it's <= X)
                base.to_string()
            }
            _ => {
                // For other operators, strip ~~
                base.to_string()
            }
        }
    } else if format == PackageFormat::Deb && operand.ends_with('~') {
        // For Debian format, handle trailing ~ for >= and > comparisons
        // In Debian, "X~" has lowest precedence. For ">= X~":
        // - If provided version has a revision (dash followed by digit), it's >= base, so strip ~
        // - If provided version is "X~Y" (has ~ with additional content), compare with ~
        // - Otherwise, X > X~, so strip ~
        match operator {
            Operator::VersionGreaterThanEqual | Operator::VersionGreaterThan => {
                if let Some(provided) = provided_version {
                    // Check if provided version has a revision (dash followed by digit)
                    // If it does, extract the upstream part to compare
                    let (provided_upstream, _) = if let Some(dash_pos) = provided.rfind('-') {
                        let after_dash = &provided[dash_pos + 1..];
                        if !after_dash.is_empty() && after_dash.chars().next().unwrap().is_ascii_digit() {
                            // Has revision - extract upstream
                            (&provided[..dash_pos], true)
                        } else {
                            (provided, false)
                        }
                    } else {
                        (provided, false)
                    };

                    // Extract constraint base (without trailing ~)
                    let constraint_base = operand.trim_end_matches('~');

                    // If provided upstream contains ~, we need to compare with ~ included
                    // because versions like "0.12.0~rc2" need to be compared with "0.12.0~"
                    // (not "0.12.0" which would be greater).
                    // If provided upstream doesn't contain ~, check if it matches constraint base:
                    // - If it matches or is greater, strip ~ from constraint (e.g., "0.0~git20210726.e7812ac-4" >= "0.0~git20210726.e7812ac~")
                    // - Otherwise, compare with ~ included
                    if provided_upstream.contains('~') {
                        // Provided version contains ~, compare with ~ included
                        // This handles cases like "0.12.0~rc2" which should be compared with "0.12.0~"
                        operand.to_string()
                    } else if provided_upstream == constraint_base ||
                              compare_versions(provided_upstream, constraint_base, format)
                                  .map(|cmp| cmp == Ordering::Greater || cmp == Ordering::Equal)
                                  .unwrap_or(false) {
                        // Provided upstream >= constraint base, strip ~
                        constraint_base.to_string()
                    } else {
                        // Provided upstream < constraint base, compare with ~ included
                        operand.to_string()
                    }
                } else {
                    // No provided version available, strip ~ (conservative approach)
                    operand.trim_end_matches('~').to_string()
                }
            }
            _ => {
                // For other operators, keep ~ as-is (it affects comparison)
                operand.to_string()
            }
        }
    } else {
        operand.to_string()
    };

    (normalized, use_greater_than_equal)
}

/// Normalize a version string for equality comparison based on format-specific rules
///
/// For Debian format, strips local version suffixes (everything after the last +)
/// This allows packages with local builds (e.g., "1.0+2") to satisfy dependencies
/// on the base version (e.g., "= 1.0").
///
/// Note: + can appear in upstream versions (e.g., "+dfsg1"), so we only strip
/// the last + suffix which is the local version indicator.
fn normalize_version_for_equality(version: &str, format: PackageFormat) -> &str {
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

/// Check if two versions are equal using format-specific logic
///
/// For RPM/Pacman/Apk formats, "=" means upstream version must match, release can differ.
/// For Debian format, local version suffixes are ignored.
/// For other formats, full version comparison is used.
fn check_version_equal(
    version1: &str,
    version2: &str,
    format: PackageFormat,
) -> Option<bool> {
    match format {
        PackageFormat::Rpm | PackageFormat::Pacman | PackageFormat::Apk => {
            // For RPM and Pacman formats, "=" means upstream version must match, release can differ
            // So we compare only the upstream part (epoch:upstream), not the full version
            compare_upstream_versions_with_format(version1, version2, Some(format)).ok()
        }
        PackageFormat::Deb => {
            // For Debian, local version suffixes (everything after the last +) should be ignored
            // when matching exact version constraints
            let v1_base = normalize_version_for_equality(version1, format);
            let v2_base = normalize_version_for_equality(version2, format);
            compare_versions(v1_base, v2_base, format)
                .map(|cmp| cmp == Ordering::Equal)
        }
        PackageFormat::Conda => {
            // For Conda, "=" means version part must match, build number can differ
            // Conda packages use format: version-build (e.g., "2.26-5")
            // When constraint is "=2.26", it should match "2.26-5" because we only care about the version part
            let v1_version_part = version1.split(VERSION_BUILD_SEPARATOR).next().unwrap_or(version1);
            let v2_version_part = version2.split(VERSION_BUILD_SEPARATOR).next().unwrap_or(version2);
            compare_versions(v1_version_part, v2_version_part, format)
                .map(|cmp| cmp == Ordering::Equal)
        }
        _ => {
            // For other formats, use normal comparison
            compare_versions(version1, version2, format)
                .map(|cmp| cmp == Ordering::Equal)
        }
    }
}

/// Core operator matching logic shared between check_version_constraint and check_single_constraint
///
/// This function implements the version constraint checking logic for all operators except:
/// - IfInstall (handled separately)
/// - VersionEqual/VersionNotEqual (handle both literal and pattern matching with '*')
///
/// Returns Some(bool) if the comparison succeeded, None if parsing failed.
/// Callers can decide how to handle None (unwrap_or(false) vs returning an error).
/// Handle Conda-style version=build_string format for comparison operators.
/// Returns Some(bool) if handled, None if not applicable (fall through to normal processing).
fn check_conda_version_build_string_constraint(
    package_version: &str,
    constraint: &crate::parse_requires::VersionConstraint,
    format: PackageFormat,
    normalized_operand: &str,
    use_greater_than_equal: bool,
) -> Option<Option<bool>> {
    // Only handle Conda format with version=build_string pattern
    if !normalized_operand.contains('=') || format != PackageFormat::Conda {
        return None;
    }

    let parts: Vec<&str> = normalized_operand.split('=').collect();
    if parts.len() != 2 {
        return None; // Invalid format, fall through to normal processing
    }

    let constraint_version = parts[0];
    let build_string_pattern = parts[1];

    // Split package_version by VERSION_BUILD_SEPARATOR to get version and build string parts
    let mut version_parts = package_version.splitn(2, VERSION_BUILD_SEPARATOR);
    let package_version_part = version_parts.next().unwrap_or(package_version);
    let package_build_string = version_parts.next();

    // First, check if the version part satisfies the constraint
    // Create a new constraint with just the version part (no build string)
    let version_only_constraint = crate::parse_requires::VersionConstraint {
        operator: constraint.operator.clone(),
        operand: constraint_version.to_string(),
    };
    let version_satisfies = check_version_constraint_core(
        package_version_part,
        &version_only_constraint,
        format,
        constraint_version,
        use_greater_than_equal,
    )?;

    if !version_satisfies {
        return Some(Some(false));
    }

    // If version part matches, check the build string pattern
    let build_string_matches = check_build_string_pattern(package_build_string, build_string_pattern);
    Some(Some(build_string_matches))
}

/// Determine if we should use upstream-only comparison for RPM/Pacman/Apk formats.
/// Returns true if we should compare only upstream versions (ignoring release/revision).
fn should_use_upstream_comparison(
    normalized_operand: &str,
    format: PackageFormat,
) -> bool {
    // Only applicable for RPM/Pacman/Apk formats
    if !matches!(format, PackageFormat::Rpm | PackageFormat::Pacman | PackageFormat::Apk) {
        return false;
    }

    // Check if the operand has an explicit revision part (dash followed by digit)
    // We exclude pre-release markers (like "rc1", "beta2") which start with letters.
    let has_explicit_revision = if let Some(dash_pos) = normalized_operand.rfind('-') {
        let after_dash = &normalized_operand[dash_pos + 1..];
        if after_dash.is_empty() {
            false
        } else {
            let first_char = after_dash.chars().next().unwrap();
            // Check if it's a digit (indicating a revision) and not a pre-release marker
            if first_char.is_ascii_digit() {
                true
            } else if first_char.is_ascii_alphabetic() {
                // Check if it's a pre-release marker - if so, it's not an explicit revision
                let lower_after = after_dash.to_lowercase();
                let is_prerelease = lower_after.starts_with("rc") ||
                                    lower_after.starts_with("beta") ||
                                    lower_after.starts_with("alpha") ||
                                    lower_after.starts_with("pre") ||
                                    lower_after.starts_with("dev") ||
                                    lower_after.starts_with("snapshot");
                !is_prerelease // If it's not a pre-release marker, treat as revision (e.g., "2.14.14-z")
            } else {
                false
            }
        }
    } else {
        false
    };

    // Use upstream comparison if there's no explicit revision and parsed revision is "0"
    !has_explicit_revision
        && PackageVersion::parse(normalized_operand)
            .map(|ver| ver.revision == "0")
            .unwrap_or(false)
}

/// Perform version comparison based on the operator.
fn compare_version_with_operator(
    package_version: &str,
    constraint: &crate::parse_requires::VersionConstraint,
    format: PackageFormat,
    normalized_operand: &str,
    use_upstream_comparison: bool,
    use_greater_than_equal: bool,
) -> Option<bool> {
    match &constraint.operator {
        crate::parse_requires::Operator::VersionGreaterThan => {
            if use_greater_than_equal {
                // Use >= instead of > for VersionGreaterThan with ~~ when base has revision
                if use_upstream_comparison {
                    compare_upstream_versions_ordering_with_format(package_version, normalized_operand, Some(format))
                        .map(|cmp| cmp != Ordering::Less)
                } else {
                    compare_versions(package_version, normalized_operand, format)
                        .map(|cmp| cmp != Ordering::Less)
                }
            } else {
                if use_upstream_comparison {
                    compare_upstream_versions_ordering_with_format(package_version, normalized_operand, Some(format))
                        .map(|cmp| cmp == Ordering::Greater)
                } else {
                    compare_versions(package_version, normalized_operand, format)
                        .map(|cmp| cmp == Ordering::Greater)
                }
            }
        }
        crate::parse_requires::Operator::VersionGreaterThanEqual => {
            if use_upstream_comparison {
                compare_upstream_versions_ordering_with_format(package_version, normalized_operand, Some(format))
                    .map(|cmp| cmp != Ordering::Less)
            } else {
                compare_versions(package_version, normalized_operand, format)
                    .map(|cmp| cmp != Ordering::Less)
            }
        }
        crate::parse_requires::Operator::VersionLessThan => {
            if use_upstream_comparison {
                compare_upstream_versions_ordering_with_format(package_version, normalized_operand, Some(format))
                    .map(|cmp| cmp == Ordering::Less)
            } else {
                compare_versions(package_version, normalized_operand, format)
                    .map(|cmp| cmp == Ordering::Less)
            }
        }
        crate::parse_requires::Operator::VersionLessThanEqual => {
            if use_upstream_comparison {
                compare_upstream_versions_ordering_with_format(package_version, normalized_operand, Some(format))
                    .map(|cmp| cmp != Ordering::Greater)
            } else {
                compare_versions(package_version, normalized_operand, format)
                    .map(|cmp| cmp != Ordering::Greater)
            }
        }
        crate::parse_requires::Operator::VersionEqual => {
            // Use format-aware equality check
            check_version_equal(package_version, normalized_operand, format)
        }
        crate::parse_requires::Operator::VersionNotEqual => {
            if use_upstream_comparison {
                compare_upstream_versions_ordering_with_format(package_version, normalized_operand, Some(format))
                    .map(|cmp| cmp != Ordering::Equal)
            } else {
                compare_versions(package_version, normalized_operand, format)
                    .map(|cmp| cmp != Ordering::Equal)
            }
        }
        crate::parse_requires::Operator::VersionCompatible => {
            // Compatible version check (e.g., "~=" in Python, "~" in Alpine)
            // Delegates to format-specific implementations
            if format == PackageFormat::Apk {
                check_apk_fuzzy_version(package_version, normalized_operand)
            } else if format == PackageFormat::Python {
                check_python_compatible_release(package_version, normalized_operand, format)
            } else {
                // For other formats, use full version comparison
                compare_versions(package_version, normalized_operand, format)
                    .map(|cmp| cmp != Ordering::Less)
            }
        }
        _ => {
            // IfInstall should be handled by callers
            None
        }
    }
}

fn check_version_constraint_core(
    package_version: &str,
    constraint: &crate::parse_requires::VersionConstraint,
    format: PackageFormat,
    normalized_operand: &str,
    use_greater_than_equal: bool,
) -> Option<bool> {
    // Handle Conda-style version=build_string format for comparison operators (>=, >, <, <=)
    // This format is already handled for VersionEqual in check_version_equal_pattern,
    // but we need to handle it here for other operators too.
    if let Some(result) = check_conda_version_build_string_constraint(
        package_version,
        constraint,
        format,
        normalized_operand,
        use_greater_than_equal,
    ) {
        return result;
    }

    // Determine if we should use upstream-only comparison for RPM/Pacman/Apk formats
    let use_upstream_comparison = should_use_upstream_comparison(normalized_operand, format);

    // Perform the actual comparison based on the operator
    compare_version_with_operator(
        package_version,
        constraint,
        format,
        normalized_operand,
        use_upstream_comparison,
        use_greater_than_equal,
    )
}

/// Check if a build string matches a pattern.
///
/// Supports:
/// - "*" matches any build string (including missing)
/// - "*_suffix" matches build strings ending with the suffix
/// - "prefix*" matches build strings starting with the prefix
/// - Exact match for literal build strings
fn check_build_string_pattern(
    package_build_string: Option<&str>,
    build_string_pattern: &str,
) -> bool {
    if build_string_pattern == "*" {
        // Match any build string (including missing)
        true
    } else if build_string_pattern.starts_with('*') {
        // Pattern like "*_cp313" - match build strings ending with the suffix
        let suffix = &build_string_pattern[1..];
        if let Some(build_str) = package_build_string {
            build_str.ends_with(suffix)
        } else {
            false // No build string, can't match pattern that requires one
        }
    } else if build_string_pattern.ends_with('*') {
        // Pattern like "cp313*" - match build strings starting with the prefix
        let prefix = &build_string_pattern[..build_string_pattern.len() - 1];
        if let Some(build_str) = package_build_string {
            build_str.starts_with(prefix)
        } else {
            false // No build string, can't match pattern that requires one
        }
    } else {
        // Exact build string match
        if let Some(build_str) = package_build_string {
            build_str == build_string_pattern
        } else {
            false // No build string, can't match exact pattern
        }
    }
}

/// Check if a package version matches a VersionEqual constraint with pattern support.
///
/// Handles various pattern matching cases:
/// - "*" matches any version
/// - "version=build_string" format for Conda (3-part match spec)
/// - "6.9.*" pattern matching
/// - "9*" pattern matching
/// - Literal version equality
fn check_version_equal_pattern(
    package_version: &str,
    constraint: &crate::parse_requires::VersionConstraint,
    format: PackageFormat,
    normalized_operand: &str,
) -> bool {
    // Special case: "*" as operand means match any version
    if constraint.operand == "*" {
        return true;
    }

    if constraint.operand.contains('=') && format == PackageFormat::Conda {
        // Handle version=build_string format (3-part match spec)
        let parts: Vec<&str> = constraint.operand.split('=').collect();
        if parts.len() == 2 {
            let version_pattern = parts[0];
            let build_string_pattern = parts[1];

            // Split package_version by VERSION_BUILD_SEPARATOR to get version and build string parts
            let mut version_parts = package_version.splitn(2, VERSION_BUILD_SEPARATOR);
            let version_part = version_parts.next().unwrap_or(package_version);
            let build_string_part = version_parts.next();

            // Check version part matches (supports patterns with '*')
            let version_matches = if version_pattern == "*" {
                true // Match any version
            } else if version_pattern.ends_with(".*") {
                // Pattern like "6.9.*" - match versions starting with "6.9."
                let prefix = &version_pattern[..version_pattern.len() - 2];
                version_part.starts_with(prefix) || version_part == prefix
            } else if version_pattern.ends_with('*') {
                // Pattern like "9*" - match versions starting with "9"
                let prefix = &version_pattern[..version_pattern.len() - 1];
                version_part.starts_with(prefix)
            } else {
                // Exact version match
                check_version_equal(version_part, version_pattern, format).unwrap_or(false)
            };

            if !version_matches {
                return false;
            }

            // Check build string part matches (supports patterns with '*')
            let build_string_matches = check_build_string_pattern(build_string_part, build_string_pattern);

            return build_string_matches;
        }
        // Invalid format, fall through to other checks
        return false;
    }

    // Pattern like "6.9.*" - match versions starting with "6.9."
    if constraint.operand.ends_with(".*") {
        // For Conda format, convert ".*" to "*"
        if format == PackageFormat::Conda {
            // Convert "11.2.0.*" to "11.2.0*" for Conda
            // mpich-mpicxx depend on sysroot_linux-64(=2.28.*) => sysroot_linux-64(=2.28*)
            // For Conda, "1.2.*" matches "1.2.xxx" (with dot) or "1.2-build" (with dash)
            let prefix_without_dot = &constraint.operand[..constraint.operand.len() - 2]; // Remove ".*", keep "11.2.0"
            // Ensure the version either exactly matches the prefix, or starts with prefix followed by a dot or dash
            // This prevents "1.*" from matching "10.2" (only matches "1", "1.0", "1.2.3", "1-build", etc.)
            if package_version == prefix_without_dot {
                return true;
            }
            if let Some(next_char) = package_version.chars().nth(prefix_without_dot.len()) {
                return package_version.starts_with(prefix_without_dot)
                    && (next_char == '.' || next_char == '-');
            }
            return false;
        } else {
            // For other formats, match versions that start with "6.9." (e.g., "6.9.10", "6.9.0")
            let prefix_with_dot = &constraint.operand[..constraint.operand.len() - 1]; // Remove "*", keep "6.9."
            return package_version.starts_with(prefix_with_dot);
        }
    }

    if constraint.operand.ends_with('*') {
        // Handle patterns like "9*" (conda-style, matches "9b", "9e", "9f", etc.)
        let prefix = &constraint.operand[..constraint.operand.len() - 1]; // Remove "*", keep "9"
        // Prefix match works on full version string (including build part for Conda)
        return package_version.starts_with(prefix);
    }

    // Use format-aware equality check for literal versions
    check_version_equal(package_version, normalized_operand, format).unwrap_or(false)
}

/// Check if a package version matches a VersionNotEqual constraint with pattern support.
///
/// Handles various pattern matching cases:
/// - "6.9.*" pattern matching
/// - "9*" pattern matching
/// - Literal version inequality
fn check_version_not_equal_pattern(
    package_version: &str,
    constraint: &crate::parse_requires::VersionConstraint,
    format: PackageFormat,
    normalized_operand: &str,
    use_greater_than_equal: bool,
) -> bool {
    if constraint.operand.ends_with(".*") {
        // Pattern like "6.9.*" - return true if version does NOT start with "6.9."
        let prefix = &constraint.operand[..constraint.operand.len() - 1]; // Remove "*", keep "6.9."
        return !package_version.starts_with(prefix);
    }

    if constraint.operand.ends_with('*') {
        // Handle patterns like "9*" (conda-style)
        let prefix = &constraint.operand[..constraint.operand.len() - 1]; // Remove "*", keep "9"
        return !package_version.starts_with(prefix);
    }

    // Use format-aware inequality check for literal versions
    check_version_constraint_core(
        package_version,
        constraint,
        format,
        normalized_operand,
        use_greater_than_equal,
    )
    .unwrap_or(false)
}

/// Check if a package version satisfies a version constraint
/// This is a high-level function that handles unexpanded RPM macros, operand normalization,
/// and delegates to check_version_constraint_core for the actual comparison.
///
/// Returns Ok(true) if the constraint is satisfied, Ok(false) if not, or an error if parsing fails.
pub fn check_version_constraint(
    package_version: &str,
    constraint: &crate::parse_requires::VersionConstraint,
    format: PackageFormat,
) -> Result<bool> {
    // Ignore constraints with unexpanded RPM macros (e.g., %{crypto_policies_version})
    // These macros should have been expanded during RPM build, but if they weren't,
    // we can't meaningfully check the constraint, so we skip it.
    if constraint.operand.contains("%{") {
        log::trace!(
            "Ignoring constraint with unexpanded RPM macro: {} {}",
            format!("{:?}", constraint.operator),
            constraint.operand
        );
        return Ok(true);
    }

    // Normalize constraint operand based on format-specific rules
    // Handles RPM-style ~~ suffix and Debian-style ~ suffix
    let (operand, use_greater_than_equal) = normalize_constraint_operand(
        &constraint.operand,
        &constraint.operator,
        format,
        Some(package_version),
    );

    // Special handling for < operator with ~~ suffix:
    // The base version itself should NOT satisfy "< X~~" even though it's < next_version
    // For example: "0.7.5" should NOT satisfy "< 0.7.5-~~" even though "0.7.5 < 0.7.6"
    // We need to check if the package version is exactly equal to the base version.
    // Use strict equality check (not upstream-only) to avoid excluding versions like "2.2.4" when base is "2"
    if matches!(constraint.operator, Operator::VersionLessThan) && constraint.operand.ends_with("~~") {
        let base = constraint.operand.trim_end_matches("~~").trim_end_matches('-');
        // Check if the package version exactly equals the base version (strict comparison)
        // Parse both versions and compare their upstream parts directly
        // This ensures we only exclude exact matches, not prefix matches
        if let (Ok(pkg_ver), Ok(base_ver)) = (PackageVersion::parse(package_version), PackageVersion::parse(&base)) {
            // Only exclude if upstream versions are exactly equal (not just prefix match)
            // For example, "2" should exclude "2" but not "2.2.4"
            if pkg_ver.upstream == base_ver.upstream {
                return Ok(false);
            }
        }
    }

    // Handle special cases first
    // For wildcard patterns, check the original operand (before normalization)
    let satisfies = match &constraint.operator {
        crate::parse_requires::Operator::VersionEqual => {
            check_version_equal_pattern(package_version, constraint, format, &operand)
        }
        crate::parse_requires::Operator::VersionNotEqual => {
            check_version_not_equal_pattern(
                package_version,
                constraint,
                format,
                &operand,
                use_greater_than_equal,
            )
        }
        crate::parse_requires::Operator::IfInstall => {
            // Conditional dependency - should be handled separately in resolution logic
            true
        }
        _ => {
            // Use shared core logic for all other operators
            check_version_constraint_core(
                package_version,
                constraint,
                format,
                &operand,
                use_greater_than_equal,
            )
            .unwrap_or(false)
        }
    };
    Ok(satisfies)
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
        use crate::models::PackageFormat;

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
        use crate::models::PackageFormat;

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
        use crate::models::PackageFormat;

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
        use crate::models::PackageFormat;

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
        use crate::models::PackageFormat;

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
        use crate::models::PackageFormat;

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
    fn test_check_version_equal_conda_with_build() {
        use crate::models::PackageFormat;

        // Test that Conda version equality ignores build numbers
        // Constraint "=2.26" should match package version "2.26-5"
        assert_eq!(
            check_version_equal("2.26", "2.26-5", PackageFormat::Conda),
            Some(true),
            "Constraint '=2.26' should match package version '2.26-5' (build number should be ignored)"
        );

        // Test reverse direction
        assert_eq!(
            check_version_equal("2.26-5", "2.26", PackageFormat::Conda),
            Some(true),
            "Package version '2.26-5' should match constraint '=2.26'"
        );

        // Test that different versions don't match even with same build
        assert_eq!(
            check_version_equal("2.26", "2.27-5", PackageFormat::Conda),
            Some(false),
            "Different versions should not match"
        );

        // Test that same version with different builds match
        assert_eq!(
            check_version_equal("2.26-5", "2.26-10", PackageFormat::Conda),
            Some(true),
            "Same version with different build numbers should match"
        );

        // Test version without build number matches version with build
        assert_eq!(
            check_version_equal("1.0", "1.0-1", PackageFormat::Conda),
            Some(true),
            "Version '1.0' should match '1.0-1'"
        );
    }

    #[test]
    fn test_conda_version_underscore_normalization() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test that Conda versions with underscores are normalized to dots for comparison
        // This is the specific bug: 1.3_7 should satisfy >=1.3.3

        // Test version comparison: 1.3_7 should be greater than 1.3.3
        let result = compare_versions("1.3_7", "1.3.3", PackageFormat::Conda);
        assert_eq!(result, Some(Ordering::Greater),
                   "1.3_7 should be greater than 1.3.3 (underscore normalized to dot)");

        // Test version equality: 1.3_7 should equal 1.3.7
        assert_eq!(
            check_version_equal("1.3_7", "1.3.7", PackageFormat::Conda),
            Some(true),
            "1.3_7 should equal 1.3.7 (underscore normalized to dot)"
        );

        // Test >= constraint: 1.3_7 should satisfy >=1.3.3
        let constraint_ge = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "1.3.3".to_string(),
        };
        assert!(check_version_constraint("1.3_7", &constraint_ge, PackageFormat::Conda).unwrap(),
                "1.3_7 should satisfy >=1.3.3 (underscore normalized to dot)");
        assert!(check_version_constraint("1.3_7-r43h6115d3f_0", &constraint_ge, PackageFormat::Conda).unwrap(),
                "1.3_7-r43h6115d3f_0 should satisfy >=1.3.3 (version part normalized)");

        // Test > constraint: 1.3_7 should satisfy >1.3.3
        let constraint_gt = VersionConstraint {
            operator: Operator::VersionGreaterThan,
            operand: "1.3.3".to_string(),
        };
        assert!(check_version_constraint("1.3_7", &constraint_gt, PackageFormat::Conda).unwrap(),
                "1.3_7 should satisfy >1.3.3");

        // Test <= constraint: 1.3_7 should NOT satisfy <=1.3.3
        let constraint_le = VersionConstraint {
            operator: Operator::VersionLessThanEqual,
            operand: "1.3.3".to_string(),
        };
        assert!(!check_version_constraint("1.3_7", &constraint_le, PackageFormat::Conda).unwrap(),
                "1.3_7 should NOT satisfy <=1.3.3");

        // Test < constraint: 1.3_7 should NOT satisfy <1.3.3
        let constraint_lt = VersionConstraint {
            operator: Operator::VersionLessThan,
            operand: "1.3.3".to_string(),
        };
        assert!(!check_version_constraint("1.3_7", &constraint_lt, PackageFormat::Conda).unwrap(),
                "1.3_7 should NOT satisfy <1.3.3");

        // Test that 1.3_2 is less than 1.3.3
        assert!(!check_version_constraint("1.3_2", &constraint_ge, PackageFormat::Conda).unwrap(),
                "1.3_2 should NOT satisfy >=1.3.3");
        assert!(check_version_constraint("1.3_2", &constraint_le, PackageFormat::Conda).unwrap(),
                "1.3_2 should satisfy <=1.3.3");

        // Test multiple underscores: 1_2_3 should equal 1.2.3
        assert_eq!(
            check_version_equal("1_2_3", "1.2.3", PackageFormat::Conda),
            Some(true),
            "1_2_3 should equal 1.2.3"
        );

        // Test mixed underscores and dots: 1.3_7 should equal 1.3.7
        assert_eq!(
            check_version_equal("1.3_7", "1.3.7", PackageFormat::Conda),
            Some(true),
            "1.3_7 should equal 1.3.7 (mixed separators normalized)"
        );
    }

    #[test]
    fn test_check_version_equal_rpm_with_dot_extension() {
        use crate::models::PackageFormat;

        // Test that RPM version equality allows dot extensions
        // Constraint "=1.14" should match package version "1.14.6-3.fc42"
        // because the upstream version "1.14.6" starts with "1.14" followed by a dot
        assert_eq!(
            check_version_equal("1.14.6-3.fc42", "1.14", PackageFormat::Rpm),
            Some(true),
            "Constraint '=1.14' should match package version '1.14.6-3.fc42' (dot extension should be allowed)"
        );

        // Test that different versions don't match
        assert_eq!(
            check_version_equal("1.14.6-3.fc42", "1.15", PackageFormat::Rpm),
            Some(false),
            "Different versions should not match"
        );

        // Test that exact match works
        assert_eq!(
            check_version_equal("1.14.6-3.fc42", "1.14.6-5.fc42", PackageFormat::Rpm),
            Some(true),
            "Same upstream version with different releases should match"
        );

        // Test that version with dash extension also works
        assert_eq!(
            check_version_equal("9.8.3-bp160.1.34", "9.8.3", PackageFormat::Rpm),
            Some(true),
            "Constraint '=9.8.3' should match package version '9.8.3-bp160.1.34' (dash extension should be allowed)"
        );
    }

    #[test]
    fn test_check_version_constraint_rpm_dot_extension() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

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
        use crate::models::PackageFormat;

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
        use crate::models::PackageFormat;

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
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

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

    #[test]
    fn test_rpm_version_constraint_with_release() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test the specific issue: version with release should satisfy >= constraint without release
        // According to RPM rules: 12.0.0 < 12.0.0-bp160.1.2
        // So 1.0.11-bp160.1.13 should satisfy >= 1.0.11
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "1.0.11".to_string(),
        };

        let satisfies = check_version_constraint("1.0.11-bp160.1.13", &constraint, PackageFormat::Rpm);
        assert!(satisfies.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies.unwrap(),
                "1.0.11-bp160.1.13 should satisfy >= 1.0.11 in RPM format (version with release >= version without release)");

        // Test the reverse: version without release should NOT satisfy >= version with release
        let constraint2 = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "1.0.11-bp160.1.13".to_string(),
        };
        let satisfies2 = check_version_constraint("1.0.11", &constraint2, PackageFormat::Rpm);
        assert!(satisfies2.is_ok(), "check_version_constraint should succeed");
        assert!(!satisfies2.unwrap(),
                "1.0.11 should NOT satisfy >= 1.0.11-bp160.1.13 in RPM format (version without release < version with release)");

        // Test with 12.0.0 example from comment
        let constraint3 = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "12.0.0".to_string(),
        };
        let satisfies3 = check_version_constraint("12.0.0-bp160.1.2", &constraint3, PackageFormat::Rpm);
        assert!(satisfies3.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies3.unwrap(),
                "12.0.0-bp160.1.2 should satisfy >= 12.0.0 in RPM format");
    }

    #[test]
    fn test_rpm_version_constraint_upstream_only_operand() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test VersionLessThanEqual with upstream-only operand (the openeuler case)
        // Package: 3.1.0-17.oe2403sp1, Constraint: <= 3.1.0
        // Should compare upstream only: 3.1.0 <= 3.1.0 -> true
        let constraint = VersionConstraint {
            operator: Operator::VersionLessThanEqual,
            operand: "3.1.0".to_string(),
        };
        let satisfies = check_version_constraint("3.1.0-17.oe2403sp1", &constraint, PackageFormat::Rpm);
        assert!(satisfies.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies.unwrap(),
                "3.1.0-17.oe2403sp1 should satisfy <= 3.1.0 in RPM format (compare upstream only)");

        // Test VersionLessThanEqual with upstream-only operand - should fail for higher upstream
        let constraint2 = VersionConstraint {
            operator: Operator::VersionLessThanEqual,
            operand: "3.0.9".to_string(),
        };
        let satisfies2 = check_version_constraint("3.1.0-17.oe2403sp1", &constraint2, PackageFormat::Rpm);
        assert!(satisfies2.is_ok(), "check_version_constraint should succeed");
        assert!(!satisfies2.unwrap(),
                "3.1.0-17.oe2403sp1 should NOT satisfy <= 3.0.9 in RPM format");

        // Test VersionGreaterThanEqual with upstream-only operand
        let constraint3 = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "3.1.0".to_string(),
        };
        let satisfies3 = check_version_constraint("3.1.0-17.oe2403sp1", &constraint3, PackageFormat::Rpm);
        assert!(satisfies3.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies3.unwrap(),
                "3.1.0-17.oe2403sp1 should satisfy >= 3.1.0 in RPM format (compare upstream only)");

        // Test VersionLessThan with upstream-only operand
        let constraint4 = VersionConstraint {
            operator: Operator::VersionLessThan,
            operand: "3.1.1".to_string(),
        };
        let satisfies4 = check_version_constraint("3.1.0-17.oe2403sp1", &constraint4, PackageFormat::Rpm);
        assert!(satisfies4.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies4.unwrap(),
                "3.1.0-17.oe2403sp1 should satisfy < 3.1.1 in RPM format (compare upstream only)");

        // Test VersionGreaterThan with upstream-only operand
        let constraint5 = VersionConstraint {
            operator: Operator::VersionGreaterThan,
            operand: "3.0.9".to_string(),
        };
        let satisfies5 = check_version_constraint("3.1.0-17.oe2403sp1", &constraint5, PackageFormat::Rpm);
        assert!(satisfies5.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies5.unwrap(),
                "3.1.0-17.oe2403sp1 should satisfy > 3.0.9 in RPM format (compare upstream only)");

        // Test VersionNotEqual with upstream-only operand
        let constraint6 = VersionConstraint {
            operator: Operator::VersionNotEqual,
            operand: "3.0.9".to_string(),
        };
        let satisfies6 = check_version_constraint("3.1.0-17.oe2403sp1", &constraint6, PackageFormat::Rpm);
        assert!(satisfies6.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies6.unwrap(),
                "3.1.0-17.oe2403sp1 should satisfy != 3.0.9 in RPM format (compare upstream only)");

        // Test VersionNotEqual with same upstream - should be false
        let constraint7 = VersionConstraint {
            operator: Operator::VersionNotEqual,
            operand: "3.1.0".to_string(),
        };
        let satisfies7 = check_version_constraint("3.1.0-17.oe2403sp1", &constraint7, PackageFormat::Rpm);
        assert!(satisfies7.is_ok(), "check_version_constraint should succeed");
        assert!(!satisfies7.unwrap(),
                "3.1.0-17.oe2403sp1 should NOT satisfy != 3.1.0 in RPM format (same upstream)");
    }

    #[test]
    fn test_rpm_version_constraint_with_release_operand() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // When operand includes release, compare full versions
        // Package: 3.1.0-17.oe2403sp1, Constraint: <= 3.1.0-16
        // Should compare full version: 3.1.0-17.oe2403sp1 <= 3.1.0-16 -> depends on full comparison
        let constraint = VersionConstraint {
            operator: Operator::VersionLessThanEqual,
            operand: "3.1.0-16".to_string(),
        };
        let satisfies = check_version_constraint("3.1.0-17.oe2403sp1", &constraint, PackageFormat::Rpm);
        assert!(satisfies.is_ok(), "check_version_constraint should succeed");
        // Full version comparison: 3.1.0-17.oe2403sp1 > 3.1.0-16, so should fail
        assert!(!satisfies.unwrap(),
                "3.1.0-17.oe2403sp1 should NOT satisfy <= 3.1.0-16 in RPM format (compare full version)");

        // Test VersionGreaterThanEqual with release operand
        let constraint2 = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "3.1.0-18".to_string(),
        };
        let satisfies2 = check_version_constraint("3.1.0-17.oe2403sp1", &constraint2, PackageFormat::Rpm);
        assert!(satisfies2.is_ok(), "check_version_constraint should succeed");
        // Full version comparison: 3.1.0-17.oe2403sp1 < 3.1.0-18, so should fail
        assert!(!satisfies2.unwrap(),
                "3.1.0-17.oe2403sp1 should NOT satisfy >= 3.1.0-18 in RPM format (compare full version)");

        // Test VersionLessThanEqual with release operand - should pass when release is higher
        let constraint3 = VersionConstraint {
            operator: Operator::VersionLessThanEqual,
            operand: "3.1.0-20".to_string(),
        };
        let satisfies3 = check_version_constraint("3.1.0-17.oe2403sp1", &constraint3, PackageFormat::Rpm);
        assert!(satisfies3.is_ok(), "check_version_constraint should succeed");
        // Full version comparison: 3.1.0-17.oe2403sp1 < 3.1.0-20, so should pass
        assert!(satisfies3.unwrap(),
                "3.1.0-17.oe2403sp1 should satisfy <= 3.1.0-20 in RPM format (compare full version)");
    }

    #[test]
    fn test_pacman_apk_version_constraint_upstream_only_operand() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test Pacman format - same behavior as RPM (upstream-only comparison)
        let constraint = VersionConstraint {
            operator: Operator::VersionLessThanEqual,
            operand: "3.1.0".to_string(),
        };
        let satisfies = check_version_constraint("3.1.0-17", &constraint, PackageFormat::Pacman);
        assert!(satisfies.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies.unwrap(),
                "3.1.0-17 should satisfy <= 3.1.0 in Pacman format (compare upstream only)");

        // Test Apk format - same behavior as RPM (upstream-only comparison)
        let constraint2 = VersionConstraint {
            operator: Operator::VersionLessThanEqual,
            operand: "5.82".to_string(),
        };
        let satisfies2 = check_version_constraint("5.82-r0", &constraint2, PackageFormat::Apk);
        assert!(satisfies2.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies2.unwrap(),
                "5.82-r0 should satisfy <= 5.82 in Apk format (compare upstream only)");
    }

    #[test]
    fn test_pacman_version_constraint_without_explicit_epoch() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test the specific bug case: libvirt-storage-gluster requires libvirt(=11.9.0)
        // Package libvirt__1:11.9.0-1__x86_64 should satisfy constraint libvirt(=11.9.0)
        // This tests that constraints without explicit epochs match packages with epochs

        // Test VersionEqual: constraint "11.9.0" should match package "1:11.9.0-1"
        let constraint = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "11.9.0".to_string(),
        };
        let satisfies = check_version_constraint("1:11.9.0-1", &constraint, PackageFormat::Pacman);
        assert!(satisfies.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies.unwrap(),
                "Package '1:11.9.0-1' should satisfy constraint 'libvirt(=11.9.0)' in Pacman format");

        // Test check_version_equal directly
        assert_eq!(
            check_version_equal("1:11.9.0-1", "11.9.0", PackageFormat::Pacman),
            Some(true),
            "check_version_equal: '1:11.9.0-1' should match '11.9.0' in Pacman format"
        );

        // Test reverse direction
        assert_eq!(
            check_version_equal("11.9.0", "1:11.9.0-1", PackageFormat::Pacman),
            Some(true),
            "check_version_equal: '11.9.0' should match '1:11.9.0-1' in Pacman format"
        );

        // Test with different epoch
        assert_eq!(
            check_version_equal("2:11.9.0-1", "11.9.0", PackageFormat::Pacman),
            Some(true),
            "check_version_equal: '2:11.9.0-1' should match '11.9.0' in Pacman format"
        );

        // Test that explicit epoch still requires matching
        let constraint_explicit = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "1:11.9.0".to_string(),
        };
        let satisfies_explicit = check_version_constraint("1:11.9.0-1", &constraint_explicit, PackageFormat::Pacman);
        assert!(satisfies_explicit.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies_explicit.unwrap(),
                "Package '1:11.9.0-1' should satisfy constraint '1:11.9.0' (explicit epoch match)");

        // Test that explicit epoch mismatch still fails
        let constraint_mismatch = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "1:11.9.0".to_string(),
        };
        let satisfies_mismatch = check_version_constraint("2:11.9.0-1", &constraint_mismatch, PackageFormat::Pacman);
        assert!(satisfies_mismatch.is_ok(), "check_version_constraint should succeed");
        assert!(!satisfies_mismatch.unwrap(),
                "Package '2:11.9.0-1' should NOT satisfy constraint '1:11.9.0' (explicit epoch mismatch)");

        // Test ordering operators with constraint without explicit epoch
        let constraint_ge = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "11.9.0".to_string(),
        };
        let satisfies_ge = check_version_constraint("1:11.9.0-1", &constraint_ge, PackageFormat::Pacman);
        assert!(satisfies_ge.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies_ge.unwrap(),
                "Package '1:11.9.0-1' should satisfy constraint '>=11.9.0' in Pacman format");

        let constraint_le = VersionConstraint {
            operator: Operator::VersionLessThanEqual,
            operand: "11.9.0".to_string(),
        };
        let satisfies_le = check_version_constraint("1:11.9.0-1", &constraint_le, PackageFormat::Pacman);
        assert!(satisfies_le.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies_le.unwrap(),
                "Package '1:11.9.0-1' should satisfy constraint '<=11.9.0' in Pacman format");
    }

    #[test]
    fn test_rpm_version_constraint_without_explicit_epoch() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test that RPM format also supports constraints without explicit epochs
        let constraint = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "11.9.0".to_string(),
        };
        let satisfies = check_version_constraint("1:11.9.0-1", &constraint, PackageFormat::Rpm);
        assert!(satisfies.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies.unwrap(),
                "Package '1:11.9.0-1' should satisfy constraint '11.9.0' in RPM format");

        // Test check_version_equal directly
        assert_eq!(
            check_version_equal("1:11.9.0-1", "11.9.0", PackageFormat::Rpm),
            Some(true),
            "check_version_equal: '1:11.9.0-1' should match '11.9.0' in RPM format"
        );
    }

    #[test]
    fn test_deb_version_constraint_full_comparison() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Debian format should always compare full versions (not affected by this change)
        let constraint = VersionConstraint {
            operator: Operator::VersionLessThanEqual,
            operand: "3.1.0".to_string(),
        };
        let satisfies = check_version_constraint("3.1.0-1", &constraint, PackageFormat::Deb);
        assert!(satisfies.is_ok(), "check_version_constraint should succeed");
        // Debian: 3.1.0-1 > 3.1.0 (revision makes it greater), so should fail
        assert!(!satisfies.unwrap(),
                "3.1.0-1 should NOT satisfy <= 3.1.0 in Debian format (compare full version)");
    }

    #[test]
    fn test_rpm_version_constraint_openeuler_case() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test the specific openeuler case from the user's issue
        // glassfish-servlet-api <= 3.1.0 with package version 3.1.0-17.oe2403sp1
        let constraint = VersionConstraint {
            operator: Operator::VersionLessThanEqual,
            operand: "3.1.0".to_string(),
        };
        let satisfies = check_version_constraint("3.1.0-17.oe2403sp1", &constraint, PackageFormat::Rpm);
        assert!(satisfies.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies.unwrap(),
                "3.1.0-17.oe2403sp1 should satisfy <= 3.1.0 in RPM format (openeuler case)");

        // Test with exact match upstream version
        let constraint2 = VersionConstraint {
            operator: Operator::VersionLessThanEqual,
            operand: "3.1.0".to_string(),
        };
        let satisfies2 = check_version_constraint("3.1.0-1.oe2403sp1", &constraint2, PackageFormat::Rpm);
        assert!(satisfies2.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies2.unwrap(),
                "3.1.0-1.oe2403sp1 should satisfy <= 3.1.0 in RPM format");

        // Test with higher upstream version - should fail
        let constraint3 = VersionConstraint {
            operator: Operator::VersionLessThanEqual,
            operand: "3.1.0".to_string(),
        };
        let satisfies3 = check_version_constraint("3.1.1-1.oe2403sp1", &constraint3, PackageFormat::Rpm);
        assert!(satisfies3.is_ok(), "check_version_constraint should succeed");
        assert!(!satisfies3.unwrap(),
                "3.1.1-1.oe2403sp1 should NOT satisfy <= 3.1.0 in RPM format");
    }

    #[test]
    fn test_rpm_version_with_tilde_comparison() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test the specific bug: 25.0.2-3.fc42 >= 25.0.0~rc2-1 should be true
        // The tilde (~) in 25.0.0~rc2 indicates a pre-release version
        // In RPM, 25.0.2 should be greater than 25.0.0~rc2

        // Test direct comparison: "25.0.2" > "25.0.0~rc2" in RPM
        let result = compare_versions("25.0.2", "25.0.0~rc2", PackageFormat::Rpm);
        assert_eq!(
            result,
            Some(Ordering::Greater),
            "25.0.2 should be greater than 25.0.0~rc2 in RPM format, got {:?}", result
        );

        // Test with full version strings
        let result2 = compare_versions("25.0.2-3.fc42", "25.0.0~rc2-1", PackageFormat::Rpm);
        assert_eq!(
            result2,
            Some(Ordering::Greater),
            "25.0.2-3.fc42 should be greater than 25.0.0~rc2-1 in RPM format, got {:?}", result2
        );

        // Test constraint: "25.0.2-3.fc42" should satisfy >= "25.0.0~rc2-1" in RPM format
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "25.0.0~rc2-1".to_string(),
        };

        let satisfies = check_version_constraint("25.0.2-3.fc42", &constraint, PackageFormat::Rpm);
        assert!(satisfies.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies.unwrap(),
                "25.0.2-3.fc42 should satisfy >= 25.0.0~rc2-1 in RPM format");
    }

    #[test]
    fn test_rpm_version_constraint_explicit_revision_zero() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test the bug fix: when operand has explicit revision "0" (e.g., "2.1.4-0"),
        // it should use full version comparison, not upstream-only comparison.
        // This fixes the iSulad/libisula issue where libisula > 2.1.4-0 was failing.

        // Test VersionGreaterThan with explicit revision "0"
        // Constraint: > 2.1.4-0
        // Package: 2.1.5 should satisfy (greater upstream version)
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThan,
            operand: "2.1.4-0".to_string(),
        };
        let satisfies = check_version_constraint("2.1.5", &constraint, PackageFormat::Rpm);
        assert!(satisfies.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies.unwrap(),
                "2.1.5 should satisfy > 2.1.4-0 in RPM format (full version comparison)");

        // Package: 2.1.4-1 should satisfy (same upstream, greater revision)
        let satisfies2 = check_version_constraint("2.1.4-1", &constraint, PackageFormat::Rpm);
        assert!(satisfies2.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies2.unwrap(),
                "2.1.4-1 should satisfy > 2.1.4-0 in RPM format (full version comparison)");

        // Package: 2.1.4-0 should NOT satisfy (equal version)
        let satisfies3 = check_version_constraint("2.1.4-0", &constraint, PackageFormat::Rpm);
        assert!(satisfies3.is_ok(), "check_version_constraint should succeed");
        assert!(!satisfies3.unwrap(),
                "2.1.4-0 should NOT satisfy > 2.1.4-0 in RPM format (equal version)");

        // Package: 2.1.4 should NOT satisfy (equal upstream, no revision means revision "0")
        let satisfies4 = check_version_constraint("2.1.4", &constraint, PackageFormat::Rpm);
        assert!(satisfies4.is_ok(), "check_version_constraint should succeed");
        assert!(!satisfies4.unwrap(),
                "2.1.4 should NOT satisfy > 2.1.4-0 in RPM format (equal version, full comparison)");

        // Package: 2.1.3 should NOT satisfy (lesser upstream version)
        let satisfies5 = check_version_constraint("2.1.3", &constraint, PackageFormat::Rpm);
        assert!(satisfies5.is_ok(), "check_version_constraint should succeed");
        assert!(!satisfies5.unwrap(),
                "2.1.3 should NOT satisfy > 2.1.4-0 in RPM format (lesser version)");

        // Test VersionGreaterThanEqual with explicit revision "0"
        let constraint2 = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "2.1.4-0".to_string(),
        };
        // Package: 2.1.4-0 should satisfy (equal version)
        let satisfies6 = check_version_constraint("2.1.4-0", &constraint2, PackageFormat::Rpm);
        assert!(satisfies6.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies6.unwrap(),
                "2.1.4-0 should satisfy >= 2.1.4-0 in RPM format (equal version)");

        // Package: 2.1.4 should satisfy (equal upstream, revision "0" equals revision "0")
        let satisfies7 = check_version_constraint("2.1.4", &constraint2, PackageFormat::Rpm);
        assert!(satisfies7.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies7.unwrap(),
                "2.1.4 should satisfy >= 2.1.4-0 in RPM format (equal version, full comparison)");

        // Test VersionLessThan with explicit revision "0"
        let constraint3 = VersionConstraint {
            operator: Operator::VersionLessThan,
            operand: "2.1.4-0".to_string(),
        };
        // Package: 2.1.3 should satisfy (lesser upstream version)
        let satisfies8 = check_version_constraint("2.1.3", &constraint3, PackageFormat::Rpm);
        assert!(satisfies8.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies8.unwrap(),
                "2.1.3 should satisfy < 2.1.4-0 in RPM format (lesser version)");

        // Package: 2.1.4-0 should NOT satisfy (equal version)
        let satisfies9 = check_version_constraint("2.1.4-0", &constraint3, PackageFormat::Rpm);
        assert!(satisfies9.is_ok(), "check_version_constraint should succeed");
        assert!(!satisfies9.unwrap(),
                "2.1.4-0 should NOT satisfy < 2.1.4-0 in RPM format (equal version)");

        // Package: 2.1.5 should NOT satisfy (greater upstream version)
        let satisfies10 = check_version_constraint("2.1.5", &constraint3, PackageFormat::Rpm);
        assert!(satisfies10.is_ok(), "check_version_constraint should succeed");
        assert!(!satisfies10.unwrap(),
                "2.1.5 should NOT satisfy < 2.1.4-0 in RPM format (greater version)");
    }

    #[test]
    fn test_rpm_version_constraint_explicit_revision_zero_vs_no_revision() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test that explicit revision "0" (2.1.4-0) uses full comparison,
        // while no revision (2.1.4) uses upstream-only comparison

        // Constraint: > 2.1.4 (no revision) - should use upstream-only comparison
        let constraint_no_rev = VersionConstraint {
            operator: Operator::VersionGreaterThan,
            operand: "2.1.4".to_string(),
        };
        // Package: 2.1.4-1 should NOT satisfy > 2.1.4 (upstream-only: equal upstream versions)
        let satisfies1 = check_version_constraint("2.1.4-1", &constraint_no_rev, PackageFormat::Rpm);
        assert!(satisfies1.is_ok(), "check_version_constraint should succeed");
        assert!(!satisfies1.unwrap(),
                "2.1.4-1 should NOT satisfy > 2.1.4 in RPM format (upstream-only comparison, equal upstream)");

        // Package: 2.1.5 should satisfy > 2.1.4 (upstream-only: greater upstream version)
        let satisfies1b = check_version_constraint("2.1.5", &constraint_no_rev, PackageFormat::Rpm);
        assert!(satisfies1b.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies1b.unwrap(),
                "2.1.5 should satisfy > 2.1.4 in RPM format (upstream-only comparison, greater upstream)");

        // Constraint: >= 2.1.4 (no revision) - should use upstream-only comparison
        let constraint_no_rev_ge = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "2.1.4".to_string(),
        };
        // Package: 2.1.4-1 should satisfy >= 2.1.4 (upstream-only: equal upstream versions)
        let satisfies1c = check_version_constraint("2.1.4-1", &constraint_no_rev_ge, PackageFormat::Rpm);
        assert!(satisfies1c.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies1c.unwrap(),
                "2.1.4-1 should satisfy >= 2.1.4 in RPM format (upstream-only comparison, equal upstream)");

        // Constraint: > 2.1.4-0 (explicit revision "0") - should use full comparison
        let constraint_explicit_rev = VersionConstraint {
            operator: Operator::VersionGreaterThan,
            operand: "2.1.4-0".to_string(),
        };
        // Package: 2.1.4-1 should satisfy (same upstream, greater revision)
        let satisfies2 = check_version_constraint("2.1.4-1", &constraint_explicit_rev, PackageFormat::Rpm);
        assert!(satisfies2.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies2.unwrap(),
                "2.1.4-1 should satisfy > 2.1.4-0 in RPM format (full comparison)");

        // Package: 2.1.4 should NOT satisfy > 2.1.4-0 (equal version in full comparison)
        let satisfies3 = check_version_constraint("2.1.4", &constraint_explicit_rev, PackageFormat::Rpm);
        assert!(satisfies3.is_ok(), "check_version_constraint should succeed");
        assert!(!satisfies3.unwrap(),
                "2.1.4 should NOT satisfy > 2.1.4-0 in RPM format (equal version, full comparison)");
    }

    #[test]
    fn test_pacman_version_constraint_explicit_revision_zero() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test Pacman format - same behavior as RPM for explicit revision "0"
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThan,
            operand: "2.1.4-0".to_string(),
        };
        let satisfies = check_version_constraint("2.1.5", &constraint, PackageFormat::Pacman);
        assert!(satisfies.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies.unwrap(),
                "2.1.5 should satisfy > 2.1.4-0 in Pacman format (full version comparison)");

        let satisfies2 = check_version_constraint("2.1.4-0", &constraint, PackageFormat::Pacman);
        assert!(satisfies2.is_ok(), "check_version_constraint should succeed");
        assert!(!satisfies2.unwrap(),
                "2.1.4-0 should NOT satisfy > 2.1.4-0 in Pacman format (equal version)");
    }

    #[test]
    fn test_apk_version_constraint_explicit_revision_zero() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test APK format - same behavior as RPM for explicit revision "0"
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThan,
            operand: "2.1.4-0".to_string(),
        };
        let satisfies = check_version_constraint("2.1.5", &constraint, PackageFormat::Apk);
        assert!(satisfies.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies.unwrap(),
                "2.1.5 should satisfy > 2.1.4-0 in APK format (full version comparison)");

        let satisfies2 = check_version_constraint("2.1.4-0", &constraint, PackageFormat::Apk);
        assert!(satisfies2.is_ok(), "check_version_constraint should succeed");
        assert!(!satisfies2.unwrap(),
                "2.1.4-0 should NOT satisfy > 2.1.4-0 in APK format (equal version)");
    }

    #[test]
    fn test_rpm_version_constraint_explicit_revision_zero_pre_release() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test that pre-release markers (like "rc1") are not treated as explicit revisions
        // Constraint: > 2.1.4-rc1 (pre-release marker, not a revision)
        // Should use upstream-only comparison
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThan,
            operand: "2.1.4-rc1".to_string(),
        };
        // Package: 2.1.4 should satisfy (greater than pre-release)
        let satisfies = check_version_constraint("2.1.4", &constraint, PackageFormat::Rpm);
        assert!(satisfies.is_ok(), "check_version_constraint should succeed");
        assert!(satisfies.unwrap(),
                "2.1.4 should satisfy > 2.1.4-rc1 in RPM format (upstream-only comparison, pre-release)");

        // Package: 2.1.4-rc1 should NOT satisfy (equal version)
        let satisfies2 = check_version_constraint("2.1.4-rc1", &constraint, PackageFormat::Rpm);
        assert!(satisfies2.is_ok(), "check_version_constraint should succeed");
        assert!(!satisfies2.unwrap(),
                "2.1.4-rc1 should NOT satisfy > 2.1.4-rc1 in RPM format (equal version)");
    }

    #[test]
    fn test_version_equal_star_wildcard() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test VersionEqual with wildcard pattern (e.g., "6.9.*")
        let constraint = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "6.9.*".to_string(),
        };

        // Should match versions starting with "6.9."
        assert!(check_version_constraint("6.9.10", &constraint, PackageFormat::Conda).unwrap(),
                "6.9.10 should match 6.9.*");
        assert!(check_version_constraint("6.9.0", &constraint, PackageFormat::Conda).unwrap(),
                "6.9.0 should match 6.9.*");
        assert!(check_version_constraint("6.9.10.1", &constraint, PackageFormat::Conda).unwrap(),
                "6.9.10.1 should match 6.9.*");
        assert!(check_version_constraint("6.9.7.1", &constraint, PackageFormat::Conda).unwrap(),
                "6.9.7.1 should match 6.9.*");

        // For Conda, "6.9.*" is valid (only 2 dots), so "6.9" should NOT match (requires "6.9.")
        assert!(!check_version_constraint("6.10.0", &constraint, PackageFormat::Conda).unwrap(),
                "6.10.0 should NOT match 6.9.*");
        assert!(!check_version_constraint("7.0.0", &constraint, PackageFormat::Conda).unwrap(),
                "7.0.0 should NOT match 6.9.*");

        // Test Conda format with build strings (e.g., "11.2.0-h5c386dc_0" should match "11.2.0.*")
        let constraint_with_build = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "11.2.0.*".to_string(),
        };
        assert!(check_version_constraint("11.2.0-h5c386dc_0", &constraint_with_build, PackageFormat::Conda).unwrap(),
                "11.2.0-h5c386dc_0 should match 11.2.0.* (build string should be ignored)");
        assert!(check_version_constraint("11.2.0-h5c386dc_1", &constraint_with_build, PackageFormat::Conda).unwrap(),
                "11.2.0-h5c386dc_1 should match 11.2.0.*");
        assert!(check_version_constraint("11.2.0-h5c386dc_2", &constraint_with_build, PackageFormat::Conda).unwrap(),
                "11.2.0-h5c386dc_2 should match 11.2.0.*");
        // For Conda, "11.2.0.*" is treated as "11.2.0*" (remove the dot), so "11.2.0" should match
        assert!(check_version_constraint("11.2.0", &constraint_with_build, PackageFormat::Conda).unwrap(),
                "11.2.0 (without build string) should match 11.2.0.* (treated as 11.2.0* for Conda)");
        assert!(!check_version_constraint("11.2.1-h5c386dc_0", &constraint_with_build, PackageFormat::Conda).unwrap(),
                "11.2.1-h5c386dc_0 should NOT match 11.2.0.*");

        // Test with different formats
        assert!(check_version_constraint("6.9.10", &constraint, PackageFormat::Python).unwrap(),
                "6.9.10 should match 6.9.* in Python format");
        assert!(check_version_constraint("6.9.10", &constraint, PackageFormat::Deb).unwrap(),
                "6.9.10 should match 6.9.* in Debian format");
    }

    #[test]
    fn test_version_equal_dot_star_pattern_matching() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test that "1.*" correctly matches only versions starting with "1" followed by dot or dash
        // This prevents "1.*" from incorrectly matching "10.2"
        let constraint = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "1.*".to_string(),
        };

        // Should match: exact match
        assert!(check_version_constraint("1", &constraint, PackageFormat::Conda).unwrap(),
                "1 should match 1.* (exact match)");

        // Should match: prefix followed by dot
        assert!(check_version_constraint("1.0", &constraint, PackageFormat::Conda).unwrap(),
                "1.0 should match 1.*");
        assert!(check_version_constraint("1.2.3", &constraint, PackageFormat::Conda).unwrap(),
                "1.2.3 should match 1.*");
        assert!(check_version_constraint("1.10.20", &constraint, PackageFormat::Conda).unwrap(),
                "1.10.20 should match 1.*");

        // Should match: prefix followed by dash (Conda build strings)
        assert!(check_version_constraint("1-build", &constraint, PackageFormat::Conda).unwrap(),
                "1-build should match 1.* (with dash)");
        assert!(check_version_constraint("1-h5c386dc_0", &constraint, PackageFormat::Conda).unwrap(),
                "1-h5c386dc_0 should match 1.* (with dash and build string)");

        // Should NOT match: prefix followed by digit (e.g., "10.2" starts with "1" but next char is "0", not "." or "-")
        assert!(!check_version_constraint("10.2", &constraint, PackageFormat::Conda).unwrap(),
                "10.2 should NOT match 1.* (next char is '0', not '.' or '-')");
        assert!(!check_version_constraint("10", &constraint, PackageFormat::Conda).unwrap(),
                "10 should NOT match 1.*");
        assert!(!check_version_constraint("11.2.0", &constraint, PackageFormat::Conda).unwrap(),
                "11.2.0 should NOT match 1.*");

        // Test "1.2.*" pattern
        let constraint_1_2 = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "1.2.*".to_string(),
        };

        // Should match: exact match
        assert!(check_version_constraint("1.2", &constraint_1_2, PackageFormat::Conda).unwrap(),
                "1.2 should match 1.2.* (exact match)");

        // Should match: prefix followed by dot
        assert!(check_version_constraint("1.2.0", &constraint_1_2, PackageFormat::Conda).unwrap(),
                "1.2.0 should match 1.2.*");
        assert!(check_version_constraint("1.2.xxx", &constraint_1_2, PackageFormat::Conda).unwrap(),
                "1.2.xxx should match 1.2.*");

        // Should match: prefix followed by dash
        assert!(check_version_constraint("1.2-build", &constraint_1_2, PackageFormat::Conda).unwrap(),
                "1.2-build should match 1.2.* (with dash)");
        assert!(check_version_constraint("1.2-h5c386dc_0", &constraint_1_2, PackageFormat::Conda).unwrap(),
                "1.2-h5c386dc_0 should match 1.2.* (with dash and build string)");

        // Should NOT match: prefix followed by digit
        assert!(!check_version_constraint("1.20", &constraint_1_2, PackageFormat::Conda).unwrap(),
                "1.20 should NOT match 1.2.* (next char is '0', not '.' or '-')");
        assert!(!check_version_constraint("1.20.0", &constraint_1_2, PackageFormat::Conda).unwrap(),
                "1.20.0 should NOT match 1.2.*");

        // Test "11.2.0.*" pattern (from the original comment example)
        let constraint_11_2_0 = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "11.2.0.*".to_string(),
        };

        // Should match: exact match
        assert!(check_version_constraint("11.2.0", &constraint_11_2_0, PackageFormat::Conda).unwrap(),
                "11.2.0 should match 11.2.0.* (exact match)");

        // Should match: prefix followed by dot
        assert!(check_version_constraint("11.2.0.1", &constraint_11_2_0, PackageFormat::Conda).unwrap(),
                "11.2.0.1 should match 11.2.0.*");

        // Should match: prefix followed by dash
        assert!(check_version_constraint("11.2.0-h5c386dc_0", &constraint_11_2_0, PackageFormat::Conda).unwrap(),
                "11.2.0-h5c386dc_0 should match 11.2.0.*");

        // Should NOT match: prefix followed by digit
        assert!(!check_version_constraint("11.2.01", &constraint_11_2_0, PackageFormat::Conda).unwrap(),
                "11.2.01 should NOT match 11.2.0.*");
    }

    #[test]
    fn test_version_not_equal_star_wildcard() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test VersionNotEqual with wildcard pattern (e.g., "6.9.*")
        let constraint = VersionConstraint {
            operator: Operator::VersionNotEqual,
            operand: "6.9.*".to_string(),
        };

        // Should NOT match versions starting with "6.9."
        assert!(!check_version_constraint("6.9.10", &constraint, PackageFormat::Conda).unwrap(),
                "6.9.10 should NOT match != 6.9.*");
        assert!(!check_version_constraint("6.9.0", &constraint, PackageFormat::Conda).unwrap(),
                "6.9.0 should NOT match != 6.9.*");

        // Should match versions that don't start with "6.9."
        assert!(check_version_constraint("6.9", &constraint, PackageFormat::Conda).unwrap(),
                "6.9 should match != 6.9.*");
        assert!(check_version_constraint("6.10.0", &constraint, PackageFormat::Conda).unwrap(),
                "6.10.0 should match != 6.9.*");
        assert!(check_version_constraint("7.0.0", &constraint, PackageFormat::Conda).unwrap(),
                "7.0.0 should match != 6.9.*");
    }

    #[test]
    fn test_version_equal_wildcard_any() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test VersionEqual with "*" operand (matches any version)
        let constraint = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "*".to_string(),
        };

        // Should match any version
        assert!(check_version_constraint("6.9.10", &constraint, PackageFormat::Conda).unwrap(),
                "* should match 6.9.10");
        assert!(check_version_constraint("1.0.0", &constraint, PackageFormat::Conda).unwrap(),
                "* should match 1.0.0");
        assert!(check_version_constraint("2.5.3", &constraint, PackageFormat::Rpm).unwrap(),
                "* should match 2.5.3 in RPM format");
        assert!(check_version_constraint("3.14.159", &constraint, PackageFormat::Deb).unwrap(),
                "* should match 3.14.159 in Debian format");
    }

    #[test]
    fn test_version_equal_star_conda_pattern() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test VersionEqual with conda pattern like "9*" (matches "9b", "9e", "9f", etc.)
        let constraint = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "9*".to_string(),
        };

        // Should match versions starting with "9"
        assert!(check_version_constraint("9b", &constraint, PackageFormat::Conda).unwrap(),
                "9b should match 9*");
        assert!(check_version_constraint("9e", &constraint, PackageFormat::Conda).unwrap(),
                "9e should match 9*");
        assert!(check_version_constraint("9f", &constraint, PackageFormat::Conda).unwrap(),
                "9f should match 9*");
        assert!(check_version_constraint("9", &constraint, PackageFormat::Conda).unwrap(),
                "9 should match 9*");

        // Should NOT match versions that don't start with "9"
        assert!(!check_version_constraint("8d", &constraint, PackageFormat::Conda).unwrap(),
                "8d should NOT match 9*");
        assert!(!check_version_constraint("10", &constraint, PackageFormat::Conda).unwrap(),
                "10 should NOT match 9*");
    }

    #[test]
    fn test_conda_version_range_constraints() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test the specific case from the issue: >=6.9.7.1,<6.10.0a0 should match 6.9.10
        let constraint1 = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "6.9.7.1".to_string(),
        };
        let constraint2 = VersionConstraint {
            operator: Operator::VersionLessThan,
            operand: "6.10.0a0".to_string(),
        };

        // Test >= 6.9.7.1
        assert!(check_version_constraint("6.9.10", &constraint1, PackageFormat::Conda).unwrap(),
                "6.9.10 should satisfy >= 6.9.7.1");
        assert!(check_version_constraint("6.9.7.1", &constraint1, PackageFormat::Conda).unwrap(),
                "6.9.7.1 should satisfy >= 6.9.7.1");
        assert!(!check_version_constraint("6.9.7.0", &constraint1, PackageFormat::Conda).unwrap(),
                "6.9.7.0 should NOT satisfy >= 6.9.7.1");

        // Test < 6.10.0a0
        assert!(check_version_constraint("6.9.10", &constraint2, PackageFormat::Conda).unwrap(),
                "6.9.10 should satisfy < 6.10.0a0");
        assert!(check_version_constraint("6.9.99", &constraint2, PackageFormat::Conda).unwrap(),
                "6.9.99 should satisfy < 6.10.0a0");
        assert!(!check_version_constraint("6.10.0a0", &constraint2, PackageFormat::Conda).unwrap(),
                "6.10.0a0 should NOT satisfy < 6.10.0a0");
        assert!(!check_version_constraint("6.10.0", &constraint2, PackageFormat::Conda).unwrap(),
                "6.10.0 should NOT satisfy < 6.10.0a0");

        // Combined: both constraints should be satisfied by 6.9.10
        assert!(check_version_constraint("6.9.10", &constraint1, PackageFormat::Conda).unwrap() &&
                check_version_constraint("6.9.10", &constraint2, PackageFormat::Conda).unwrap(),
                "6.9.10 should satisfy both >=6.9.7.1 and <6.10.0a0");
    }

    #[test]
    fn test_conda_version_build_string_comparison_operators() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test >= operator with version=build_string format
        let constraint_ge = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "1.1.4=*_0".to_string(),
        };

        // Should match: version >= 1.1.4 AND build string ends with _0
        assert!(check_version_constraint("1.1.4-hd3eb1b0_0", &constraint_ge, PackageFormat::Conda).unwrap(),
                "1.1.4-hd3eb1b0_0 should satisfy >=1.1.4=*_0");
        assert!(check_version_constraint("1.1.5-hd3eb1b0_0", &constraint_ge, PackageFormat::Conda).unwrap(),
                "1.1.5-hd3eb1b0_0 should satisfy >=1.1.4=*_0 (higher version, matching build)");
        assert!(check_version_constraint("1.2.0-hd3eb1b0_0", &constraint_ge, PackageFormat::Conda).unwrap(),
                "1.2.0-hd3eb1b0_0 should satisfy >=1.1.4=*_0 (higher version, matching build)");

        // Should NOT match: version < 1.1.4 even with matching build
        assert!(!check_version_constraint("1.1.3-hd3eb1b0_0", &constraint_ge, PackageFormat::Conda).unwrap(),
                "1.1.3-hd3eb1b0_0 should NOT satisfy >=1.1.4=*_0 (version too low)");

        // Should NOT match: version >= 1.1.4 but build string doesn't match
        assert!(!check_version_constraint("1.1.4-hd3eb1b0_1", &constraint_ge, PackageFormat::Conda).unwrap(),
                "1.1.4-hd3eb1b0_1 should NOT satisfy >=1.1.4=*_0 (build string doesn't end with _0)");
        assert!(!check_version_constraint("1.1.5-hd3eb1b0_1", &constraint_ge, PackageFormat::Conda).unwrap(),
                "1.1.5-hd3eb1b0_1 should NOT satisfy >=1.1.4=*_0 (build string doesn't match)");

        // Test > operator with version=build_string format
        let constraint_gt = VersionConstraint {
            operator: Operator::VersionGreaterThan,
            operand: "1.1.4=*_0".to_string(),
        };

        // Should match: version > 1.1.4 AND build string ends with _0
        assert!(check_version_constraint("1.1.5-hd3eb1b0_0", &constraint_gt, PackageFormat::Conda).unwrap(),
                "1.1.5-hd3eb1b0_0 should satisfy >1.1.4=*_0");
        assert!(!check_version_constraint("1.1.4-hd3eb1b0_0", &constraint_gt, PackageFormat::Conda).unwrap(),
                "1.1.4-hd3eb1b0_0 should NOT satisfy >1.1.4=*_0 (not greater)");

        // Test <= operator with version=build_string format
        let constraint_le = VersionConstraint {
            operator: Operator::VersionLessThanEqual,
            operand: "1.1.4=*_0".to_string(),
        };

        // Should match: version <= 1.1.4 AND build string ends with _0
        assert!(check_version_constraint("1.1.4-hd3eb1b0_0", &constraint_le, PackageFormat::Conda).unwrap(),
                "1.1.4-hd3eb1b0_0 should satisfy <=1.1.4=*_0");
        assert!(check_version_constraint("1.1.3-hd3eb1b0_0", &constraint_le, PackageFormat::Conda).unwrap(),
                "1.1.3-hd3eb1b0_0 should satisfy <=1.1.4=*_0");
        assert!(!check_version_constraint("1.1.5-hd3eb1b0_0", &constraint_le, PackageFormat::Conda).unwrap(),
                "1.1.5-hd3eb1b0_0 should NOT satisfy <=1.1.4=*_0 (version too high)");

        // Test < operator with version=build_string format
        let constraint_lt = VersionConstraint {
            operator: Operator::VersionLessThan,
            operand: "1.1.4=*_0".to_string(),
        };

        // Should match: version < 1.1.4 AND build string ends with _0
        assert!(check_version_constraint("1.1.3-hd3eb1b0_0", &constraint_lt, PackageFormat::Conda).unwrap(),
                "1.1.3-hd3eb1b0_0 should satisfy <1.1.4=*_0");
        assert!(!check_version_constraint("1.1.4-hd3eb1b0_0", &constraint_lt, PackageFormat::Conda).unwrap(),
                "1.1.4-hd3eb1b0_0 should NOT satisfy <1.1.4=*_0 (not less)");
    }

    #[test]
    fn test_conda_version_build_string_patterns() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test >= with build string pattern "*" (matches any build string)
        let constraint_any_build = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "1.1.4=*".to_string(),
        };

        assert!(check_version_constraint("1.1.4-hd3eb1b0_0", &constraint_any_build, PackageFormat::Conda).unwrap(),
                "1.1.4-hd3eb1b0_0 should satisfy >=1.1.4=* (any build string)");
        assert!(check_version_constraint("1.1.4-any_build", &constraint_any_build, PackageFormat::Conda).unwrap(),
                "1.1.4-any_build should satisfy >=1.1.4=* (any build string)");
        assert!(check_version_constraint("1.1.4", &constraint_any_build, PackageFormat::Conda).unwrap(),
                "1.1.4 (no build string) should satisfy >=1.1.4=*");

        // Test >= with build string pattern "*_0" (ends with _0)
        let constraint_suffix = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "1.1.4=*_0".to_string(),
        };

        assert!(check_version_constraint("1.1.4-hd3eb1b0_0", &constraint_suffix, PackageFormat::Conda).unwrap(),
                "1.1.4-hd3eb1b0_0 should satisfy >=1.1.4=*_0 (ends with _0)");
        assert!(check_version_constraint("1.1.4-abc_0", &constraint_suffix, PackageFormat::Conda).unwrap(),
                "1.1.4-abc_0 should satisfy >=1.1.4=*_0 (ends with _0)");
        assert!(!check_version_constraint("1.1.4-hd3eb1b0_1", &constraint_suffix, PackageFormat::Conda).unwrap(),
                "1.1.4-hd3eb1b0_1 should NOT satisfy >=1.1.4=*_0 (doesn't end with _0)");
        assert!(!check_version_constraint("1.1.4", &constraint_suffix, PackageFormat::Conda).unwrap(),
                "1.1.4 (no build string) should NOT satisfy >=1.1.4=*_0 (pattern requires build string)");

        // Test >= with build string pattern "hd3*" (starts with hd3)
        let constraint_prefix = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "1.1.4=hd3*".to_string(),
        };

        assert!(check_version_constraint("1.1.4-hd3eb1b0_0", &constraint_prefix, PackageFormat::Conda).unwrap(),
                "1.1.4-hd3eb1b0_0 should satisfy >=1.1.4=hd3* (starts with hd3)");
        assert!(check_version_constraint("1.1.4-hd3xyz", &constraint_prefix, PackageFormat::Conda).unwrap(),
                "1.1.4-hd3xyz should satisfy >=1.1.4=hd3* (starts with hd3)");
        assert!(!check_version_constraint("1.1.4-abc123", &constraint_prefix, PackageFormat::Conda).unwrap(),
                "1.1.4-abc123 should NOT satisfy >=1.1.4=hd3* (doesn't start with hd3)");

        // Test >= with exact build string match
        let constraint_exact = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "1.1.4=hd3eb1b0_0".to_string(),
        };

        assert!(check_version_constraint("1.1.4-hd3eb1b0_0", &constraint_exact, PackageFormat::Conda).unwrap(),
                "1.1.4-hd3eb1b0_0 should satisfy >=1.1.4=hd3eb1b0_0 (exact match)");
        assert!(check_version_constraint("1.1.5-hd3eb1b0_0", &constraint_exact, PackageFormat::Conda).unwrap(),
                "1.1.5-hd3eb1b0_0 should satisfy >=1.1.4=hd3eb1b0_0 (higher version, exact build)");
        assert!(!check_version_constraint("1.1.4-hd3eb1b0_1", &constraint_exact, PackageFormat::Conda).unwrap(),
                "1.1.4-hd3eb1b0_1 should NOT satisfy >=1.1.4=hd3eb1b0_0 (build string mismatch)");
    }

    #[test]
    fn test_conda_version_build_string_specific_case() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test the specific case from the error: libtirpc-el8-x86_64(>=1.1.4=*_0)
        // Package: libtirpc-el8-x86_64__1.1.4-hd3eb1b0_0__all
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "1.1.4=*_0".to_string(),
        };

        // This is the exact case that was failing
        assert!(check_version_constraint("1.1.4-hd3eb1b0_0", &constraint, PackageFormat::Conda).unwrap(),
                "1.1.4-hd3eb1b0_0 should satisfy >=1.1.4=*_0 (specific case from error)");

        // Test similar cases with other el8 packages
        let constraint_audit = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "1.0.8=*_0".to_string(),
        };
        assert!(check_version_constraint("1.0.8-hd3eb1b0_0", &constraint_audit, PackageFormat::Conda).unwrap(),
                "1.0.8-hd3eb1b0_0 should satisfy >=1.0.8=*_0");

        let constraint_libselinux = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "1.33.2=*_0".to_string(),
        };
        assert!(check_version_constraint("1.33.2-hd3eb1b0_0", &constraint_libselinux, PackageFormat::Conda).unwrap(),
                "1.33.2-hd3eb1b0_0 should satisfy >=1.33.2=*_0");
    }

    #[test]
    fn test_version_equal_star_edge_cases() {
        use crate::models::PackageFormat;
        use crate::parse_requires::{VersionConstraint, Operator};

        // Test edge cases for VersionEqual with wildcard pattern
        let constraint = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "1.*".to_string(),
        };

        // Should match versions starting with "1."
        assert!(check_version_constraint("1.0", &constraint, PackageFormat::Conda).unwrap(),
                "1.0 should match 1.*");
        assert!(check_version_constraint("1.10", &constraint, PackageFormat::Conda).unwrap(),
                "1.10 should match 1.*");
        assert!(check_version_constraint("1.2.3.4", &constraint, PackageFormat::Conda).unwrap(),
                "1.2.3.4 should match 1.*");

        // Should NOT match "1" (no dot)
        assert!(!check_version_constraint("10.0", &constraint, PackageFormat::Conda).unwrap(),
                "10.0 should NOT match 1.*");

        // Test with operand that doesn't end with ".*" (should fall back to regular equality)
        let constraint_no_wildcard = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "6.9.10".to_string(), // No ".*" suffix
        };
        // Should use regular equality check
        assert!(check_version_constraint("6.9.10", &constraint_no_wildcard, PackageFormat::Conda).unwrap(),
                "6.9.10 should match = 6.9.10 (fallback to equality)");
        assert!(!check_version_constraint("6.9.11", &constraint_no_wildcard, PackageFormat::Conda).unwrap(),
                "6.9.11 should NOT match = 6.9.10 (fallback to equality)");
    }
}
