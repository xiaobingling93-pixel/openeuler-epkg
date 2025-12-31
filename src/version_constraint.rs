//! Version constraint checking and validation
//!
//! This module provides functions for checking if package versions satisfy version constraints,
//! handling format-specific version comparison rules for RPM, Debian, Alpine, Pacman, and Conda packages.

use color_eyre::Result;
use std::cmp::Ordering;
use crate::models::PackageFormat;
use crate::parse_requires::{Operator, VersionConstraint};
use crate::parse_version::PackageVersion;
use crate::conda_pkg::VERSION_BUILD_SEPARATOR;
use crate::version_compare::*;
use log;

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
    constraint: &VersionConstraint,
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
    let version_only_constraint = VersionConstraint {
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
    constraint: &VersionConstraint,
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

fn check_version_constraint_core(
    package_version: &str,
    constraint: &VersionConstraint,
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
    constraint: &VersionConstraint,
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
    constraint: &VersionConstraint,
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
    constraint: &VersionConstraint,
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

/// Check if a version satisfies a set of constraints
pub fn check_version_satisfies_constraints(
    version: &str,
    constraints: &Vec<VersionConstraint>,
    format: PackageFormat,
) -> Result<bool> {
    // Separate constraints into mutually exclusive groups (OR conditions) and compatible constraints (AND conditions)
    let mut or_groups: Vec<Vec<&VersionConstraint>> = Vec::new();
    let mut and_constraints: Vec<&VersionConstraint> = Vec::new();

    // Filter out conditional constraints first
    let non_conditional_constraints: Vec<&VersionConstraint> = constraints.iter()
        .filter(|c| !matches!(c.operator, Operator::IfInstall))
        .collect();

    // Group mutually exclusive constraints together
    let mut processed = vec![false; non_conditional_constraints.len()];
    for i in 0..non_conditional_constraints.len() {
        if processed[i] {
            continue;
        }
        let constraint_i = non_conditional_constraints[i];
        let mut or_group = vec![constraint_i];
        processed[i] = true;

        // Look for mutually exclusive constraints
        for j in (i + 1)..non_conditional_constraints.len() {
            if processed[j] {
                continue;
            }
            let constraint_j = non_conditional_constraints[j];

            // Check if constraints are mutually exclusive
            let are_mutually_exclusive = match (&constraint_i.operator, &constraint_j.operator) {
                (Operator::VersionLessThan, Operator::VersionGreaterThan) |
                (Operator::VersionLessThan, Operator::VersionGreaterThanEqual) |
                (Operator::VersionLessThanEqual, Operator::VersionGreaterThan) |
                (Operator::VersionLessThanEqual, Operator::VersionGreaterThanEqual) |
                (Operator::VersionGreaterThan, Operator::VersionLessThan) |
                (Operator::VersionGreaterThan, Operator::VersionLessThanEqual) |
                (Operator::VersionGreaterThanEqual, Operator::VersionLessThan) |
                (Operator::VersionGreaterThanEqual, Operator::VersionLessThanEqual) => {
                    if constraint_i.operand == constraint_j.operand {
                        true
                    } else {
                        are_constraints_logically_mutually_exclusive(
                            constraint_i,
                            constraint_j,
                            format,
                        )
                    }
                }
                _ => false,
            };

            if are_mutually_exclusive {
                let mut can_add = true;
                for existing_constraint in &or_group[1..] {
                    let mutually_exclusive_with_existing = match (&existing_constraint.operator, &constraint_j.operator) {
                        (Operator::VersionLessThan, Operator::VersionGreaterThan) |
                        (Operator::VersionLessThan, Operator::VersionGreaterThanEqual) |
                        (Operator::VersionLessThanEqual, Operator::VersionGreaterThan) |
                        (Operator::VersionLessThanEqual, Operator::VersionGreaterThanEqual) |
                        (Operator::VersionGreaterThan, Operator::VersionLessThan) |
                        (Operator::VersionGreaterThan, Operator::VersionLessThanEqual) |
                        (Operator::VersionGreaterThanEqual, Operator::VersionLessThan) |
                        (Operator::VersionGreaterThanEqual, Operator::VersionLessThanEqual) => {
                            if existing_constraint.operand == constraint_j.operand {
                                true
                            } else {
                                are_constraints_logically_mutually_exclusive(
                                    existing_constraint,
                                    constraint_j,
                                    format,
                                )
                            }
                        }
                        _ => false,
                    };
                    if !mutually_exclusive_with_existing {
                        can_add = false;
                        break;
                    }
                }

                if can_add {
                    or_group.push(constraint_j);
                    processed[j] = true;
                }
            }
        }

        if or_group.len() > 1 {
            or_groups.push(or_group);
        } else {
            and_constraints.push(constraint_i);
        }
    }

    // Check AND constraints: all must be satisfied
    for constraint in &and_constraints {
        if !check_version_constraint(version, constraint, format)? {
            return Ok(false);
        }
    }

    // Check OR groups: at least one constraint in each group must be satisfied
    for or_group in &or_groups {
        let mut any_satisfied = false;
        for constraint in or_group {
            if check_version_constraint(version, constraint, format)? {
                any_satisfied = true;
                break;
            }
        }
        if !any_satisfied {
            return Ok(false);
        }
    }

    Ok(true)
}

/// Check if two constraints with opposite operators are logically mutually exclusive
/// even when they have different operands.
///
/// Examples:
/// - "< 2.1~~" and ">= 2.2" are mutually exclusive if 2.1~~ <= 2.2
/// - "< 2.3" and "> 2.3" are mutually exclusive (no version can satisfy both)
/// - "<= 2.3" and ">= 2.3" are NOT mutually exclusive (version 2.3 satisfies both)
pub fn are_constraints_logically_mutually_exclusive(
    constraint1: &VersionConstraint,
    constraint2: &VersionConstraint,
    format: PackageFormat,
) -> bool {
    // If either constraint has an unexpanded RPM macro, we can't meaningfully compare them
    // Treat them as not mutually exclusive (they'll be ignored during actual checking anyway)
    if constraint1.operand.contains("%{") || constraint2.operand.contains("%{") {
        return false;
    }

    // Normalize operands by handling RPM ~~ operator
    // In RPM, "2.1~~" means "less than 2.2", so for comparison purposes,
    // we treat "2.1~~" as slightly less than "2.2"
    // When comparing with another version, we can check if base version < other version
    let normalize_operand_for_comparison = |op: &str| -> (String, bool) {
        if op.ends_with("~~") {
            // Remove ~~ - the base version represents "less than next version"
            let base = op.trim_end_matches("~~");
            (base.to_string(), true) // true indicates this is a "less than next" version
        } else {
            (op.to_string(), false)
        }
    };

    let (op1_base, op1_is_tilde) = normalize_operand_for_comparison(&constraint1.operand);
    let (op2_base, op2_is_tilde) = normalize_operand_for_comparison(&constraint2.operand);

    // Compare the base operands to determine if constraints are mutually exclusive
    let comparison = compare_versions(&op1_base, &op2_base, format);

    match comparison {
        Some(std::cmp::Ordering::Less) => {
            // op1_base < op2_base
            match (&constraint1.operator, &constraint2.operator) {
                (Operator::VersionLessThan, Operator::VersionGreaterThanEqual) |
                (Operator::VersionLessThan, Operator::VersionGreaterThan) |
                (Operator::VersionLessThanEqual, Operator::VersionGreaterThan) => {
                    // < X and >= Y where X < Y: mutually exclusive (no overlap)
                    true
                }
                (Operator::VersionLessThanEqual, Operator::VersionGreaterThanEqual) => {
                    // <= X and >= Y where X < Y: mutually exclusive (no overlap)
                    true
                }
                (Operator::VersionGreaterThanEqual, Operator::VersionLessThan) |
                (Operator::VersionGreaterThanEqual, Operator::VersionLessThanEqual) |
                (Operator::VersionGreaterThan, Operator::VersionLessThan) |
                (Operator::VersionGreaterThan, Operator::VersionLessThanEqual) => {
                    // >= X and < Y where X < Y: NOT mutually exclusive (ranges overlap)
                    // Example: >= 4.2.5 and < 5.0 - version 4.5 satisfies both
                    false
                }
                _ => false,
            }
        }
        Some(std::cmp::Ordering::Equal) => {
            // op1_base == op2_base
            // When base versions are equal, check if operators and tilde status make them mutually exclusive
            match (&constraint1.operator, &constraint2.operator) {
                (Operator::VersionLessThan, Operator::VersionGreaterThanEqual) |
                (Operator::VersionLessThan, Operator::VersionGreaterThan) |
                (Operator::VersionLessThanEqual, Operator::VersionGreaterThan) |
                (Operator::VersionGreaterThan, Operator::VersionLessThanEqual) |
                (Operator::VersionGreaterThan, Operator::VersionLessThan) |
                (Operator::VersionGreaterThanEqual, Operator::VersionLessThan) => {
                    // When base versions are equal:
                    // - If both have ~~ or both don't have ~~, they're mutually exclusive only if operators are strict opposites
                    // - If one has ~~ and the other doesn't:
                    //   * "< X~~" (meaning < next version) and ">= X" are NOT mutually exclusive
                    //   * But "< X~~" and "> X" might be mutually exclusive depending on interpretation
                    // For simplicity, when base versions are equal and one has ~~, we only treat as mutually exclusive
                    // if both operators are strict (< vs >, not <= vs >=)
                    if op1_is_tilde == op2_is_tilde {
                        // Both have same tilde status
                        matches!((&constraint1.operator, &constraint2.operator),
                            (Operator::VersionLessThan, Operator::VersionGreaterThan) |
                            (Operator::VersionGreaterThan, Operator::VersionLessThan))
                    } else {
                        // One has ~~, one doesn't - be conservative and don't treat as mutually exclusive
                        // unless operators are strict opposites
                        // Actually, "< X~~" means "< next version", so if we have ">= X", they overlap
                        // Only strict opposites like "< X~~" and "> X" might be mutually exclusive
                        // But this is complex, so for now we'll be conservative
                        false
                    }
                }
                (Operator::VersionLessThanEqual, Operator::VersionGreaterThanEqual) => {
                    // <= X and >= X: NOT mutually exclusive (X satisfies both)
                    false
                }
                _ => false,
            }
        }
        Some(std::cmp::Ordering::Greater) => {
            // op1_base > op2_base, check swapped order
            match (&constraint2.operator, &constraint1.operator) {
                (Operator::VersionLessThan, Operator::VersionGreaterThanEqual) |
                (Operator::VersionLessThan, Operator::VersionGreaterThan) |
                (Operator::VersionLessThanEqual, Operator::VersionGreaterThan) => {
                    // < X and >= Y where X < Y (swapped, so Y < X): mutually exclusive (no overlap)
                    // Example: < 2.1 and >= 2.2 - no version can satisfy both
                    true
                }
                (Operator::VersionLessThanEqual, Operator::VersionGreaterThanEqual) => {
                    // <= X and >= Y where X < Y (swapped, so Y < X): mutually exclusive (no overlap)
                    // Example: <= 2.1 and >= 2.2 - no version can satisfy both
                    true
                }
                (Operator::VersionGreaterThanEqual, Operator::VersionLessThan) |
                (Operator::VersionGreaterThanEqual, Operator::VersionLessThanEqual) |
                (Operator::VersionGreaterThan, Operator::VersionLessThan) |
                (Operator::VersionGreaterThan, Operator::VersionLessThanEqual) => {
                    // >= X and < Y where X > Y (swapped): NOT mutually exclusive (ranges overlap)
                    // Example: >= 1.31.6 and < 3 - version 2.5.0 satisfies both, so they're NOT mutually exclusive
                    false
                }
                _ => false,
            }
        }
        None => {
            // Can't compare versions, be conservative and don't treat as mutually exclusive
            false
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_version_equal_conda_with_build() {

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
    fn test_rpm_version_constraint_with_release() {

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

    #[test]
    fn test_check_single_constraint() {
        // Test VersionGreaterThan
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThan,
            operand: "5.13".to_string(),
        };
        assert!(check_version_constraint("6.1", &constraint, PackageFormat::Rpm).unwrap());
        assert!(!check_version_constraint("5.13", &constraint, PackageFormat::Rpm).unwrap());
        assert!(!check_version_constraint("5.0", &constraint, PackageFormat::Rpm).unwrap());

        // Test VersionLessThan
        let constraint = VersionConstraint {
            operator: Operator::VersionLessThan,
            operand: "7.0".to_string(),
        };
        assert!(check_version_constraint("6.1", &constraint, PackageFormat::Rpm).unwrap());
        assert!(!check_version_constraint("7.0", &constraint, PackageFormat::Rpm).unwrap());
        assert!(!check_version_constraint("8.0", &constraint, PackageFormat::Rpm).unwrap());

        // Test VersionGreaterThanEqual
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "4.2.5".to_string(),
        };
        assert!(check_version_constraint("6.1", &constraint, PackageFormat::Rpm).unwrap());
        assert!(check_version_constraint("4.2.5", &constraint, PackageFormat::Rpm).unwrap());
        assert!(!check_version_constraint("4.2.4", &constraint, PackageFormat::Rpm).unwrap());

        // Test VersionLessThanEqual
        let constraint = VersionConstraint {
            operator: Operator::VersionLessThanEqual,
            operand: "7.0".to_string(),
        };
        assert!(check_version_constraint("6.1", &constraint, PackageFormat::Rpm).unwrap());
        assert!(check_version_constraint("7.0", &constraint, PackageFormat::Rpm).unwrap());
        assert!(!check_version_constraint("8.0", &constraint, PackageFormat::Rpm).unwrap());
    }

    #[test]
    fn test_check_single_constraint_version_compatible() {
        // Test VersionCompatible for Alpine APK format (the original bug case)
        // python3 3.12.12-r0 should satisfy ~3.12 (VersionCompatible "3.12")
        let constraint = VersionConstraint {
            operator: Operator::VersionCompatible,
            operand: "3.12".to_string(),
        };
        assert!(check_version_constraint("3.12.12-r0", &constraint, PackageFormat::Apk).unwrap(),
                "3.12.12-r0 should satisfy ~3.12");
        assert!(check_version_constraint("3.12.0-r0", &constraint, PackageFormat::Apk).unwrap(),
                "3.12.0-r0 should satisfy ~3.12");
        assert!(check_version_constraint("3.12", &constraint, PackageFormat::Apk).unwrap(),
                "3.12 should satisfy ~3.12");
        assert!(check_version_constraint("3.12.15-r1", &constraint, PackageFormat::Apk).unwrap(),
                "3.12.15-r1 should satisfy ~3.12");
        assert!(!check_version_constraint("3.11.9-r0", &constraint, PackageFormat::Apk).unwrap(),
                "3.11.9-r0 should NOT satisfy ~3.12");
        assert!(!check_version_constraint("3.10.0-r0", &constraint, PackageFormat::Apk).unwrap(),
                "3.10.0-r0 should NOT satisfy ~3.12");

        // Test VersionCompatible for RPM format
        let constraint = VersionConstraint {
            operator: Operator::VersionCompatible,
            operand: "5.13".to_string(),
        };
        assert!(check_version_constraint("5.13.0-1.fc42", &constraint, PackageFormat::Rpm).unwrap(),
                "5.13.0-1.fc42 should satisfy ~5.13");
        assert!(check_version_constraint("5.13.5-2.el8", &constraint, PackageFormat::Rpm).unwrap(),
                "5.13.5-2.el8 should satisfy ~5.13");
        assert!(check_version_constraint("5.13", &constraint, PackageFormat::Rpm).unwrap(),
                "5.13 should satisfy ~5.13");
        assert!(!check_version_constraint("5.12.9-1.fc42", &constraint, PackageFormat::Rpm).unwrap(),
                "5.12.9-1.fc42 should NOT satisfy ~5.13");
        assert!(!check_version_constraint("5.11", &constraint, PackageFormat::Rpm).unwrap(),
                "5.11 should NOT satisfy ~5.13");

        // Test VersionCompatible for Debian format
        let constraint = VersionConstraint {
            operator: Operator::VersionCompatible,
            operand: "2.5".to_string(),
        };
        assert!(check_version_constraint("2.5.0-1", &constraint, PackageFormat::Deb).unwrap(),
                "2.5.0-1 should satisfy ~2.5");
        assert!(check_version_constraint("2.5.3-2ubuntu1", &constraint, PackageFormat::Deb).unwrap(),
                "2.5.3-2ubuntu1 should satisfy ~2.5");
        assert!(check_version_constraint("2.5", &constraint, PackageFormat::Deb).unwrap(),
                "2.5 should satisfy ~2.5");
        assert!(!check_version_constraint("2.4.9-1", &constraint, PackageFormat::Deb).unwrap(),
                "2.4.9-1 should NOT satisfy ~2.5");
        assert!(!check_version_constraint("2.3", &constraint, PackageFormat::Deb).unwrap(),
                "2.3 should NOT satisfy ~2.5");

        // Test VersionCompatible with patch versions
        let constraint = VersionConstraint {
            operator: Operator::VersionCompatible,
            operand: "1.2.3".to_string(),
        };
        assert!(check_version_constraint("1.2.3", &constraint, PackageFormat::Rpm).unwrap(),
                "1.2.3 should satisfy ~1.2.3");
        assert!(check_version_constraint("1.2.3-1", &constraint, PackageFormat::Rpm).unwrap(),
                "1.2.3-1 should satisfy ~1.2.3");
        assert!(check_version_constraint("1.2.4", &constraint, PackageFormat::Rpm).unwrap(),
                "1.2.4 should satisfy ~1.2.3");
        assert!(check_version_constraint("1.2.10", &constraint, PackageFormat::Rpm).unwrap(),
                "1.2.10 should satisfy ~1.2.3");
        assert!(!check_version_constraint("1.2.2", &constraint, PackageFormat::Rpm).unwrap(),
                "1.2.2 should NOT satisfy ~1.2.3");
        assert!(!check_version_constraint("1.1.9", &constraint, PackageFormat::Rpm).unwrap(),
                "1.1.9 should NOT satisfy ~1.2.3");
    }

    #[test]
    fn test_check_single_constraint_with_tilde_tilde_suffix() {
        // Test VersionGreaterThanEqual with ~~ suffix (Debian Rust packages)
        // This is the specific case from the bug report: >= 0.7.5-~~ should match 0.7.5-1+b3
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "0.7.5-~~".to_string(),
        };
        assert!(check_version_constraint("0.7.5-1+b3", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5-1+b3 should satisfy >= 0.7.5-~~");
        assert!(check_version_constraint("0.7.5-1", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5-1 should satisfy >= 0.7.5-~~");
        assert!(check_version_constraint("0.7.5", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5 should satisfy >= 0.7.5-~~");
        assert!(check_version_constraint("0.7.6", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.6 should satisfy >= 0.7.5-~~");
        assert!(!check_version_constraint("0.7.4", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.4 should NOT satisfy >= 0.7.5-~~");

        // Test VersionGreaterThanEqual with ~~ suffix (no dash before ~~)
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "0.7.5~~".to_string(),
        };
        assert!(check_version_constraint("0.7.5-1", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5-1 should satisfy >= 0.7.5~~");
        assert!(check_version_constraint("0.7.6", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.6 should satisfy >= 0.7.5~~");
        assert!(!check_version_constraint("0.7.4", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.4 should NOT satisfy >= 0.7.5~~");

        // Test VersionGreaterThan with ~~ suffix
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThan,
            operand: "0.7.5-~~".to_string(),
        };
        assert!(check_version_constraint("0.7.6", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.6 should satisfy > 0.7.5-~~");
        assert!(check_version_constraint("0.7.5-1", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5-1 should satisfy > 0.7.5-~~");
        assert!(!check_version_constraint("0.7.5", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5 should NOT satisfy > 0.7.5-~~");
        assert!(!check_version_constraint("0.7.4", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.4 should NOT satisfy > 0.7.5-~~");

        // Test VersionGreaterThan with ~~ suffix for versions with revisions (specific bug fix)
        // This is the case from the user's error: > 0.6.0-4~~ should match 0.6.0-4
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThan,
            operand: "0.6.0-4~~".to_string(),
        };
        assert!(check_version_constraint("0.6.0-4", &constraint, PackageFormat::Deb).unwrap(),
                "0.6.0-4 should satisfy > 0.6.0-4~~ (because > X-Y~~ means >= X-Y for versions with revisions)");
        assert!(check_version_constraint("0.6.0-5", &constraint, PackageFormat::Deb).unwrap(),
                "0.6.0-5 should satisfy > 0.6.0-4~~");
        assert!(check_version_constraint("0.6.1", &constraint, PackageFormat::Deb).unwrap(),
                "0.6.1 should satisfy > 0.6.0-4~~");
        assert!(!check_version_constraint("0.6.0-3", &constraint, PackageFormat::Deb).unwrap(),
                "0.6.0-3 should NOT satisfy > 0.6.0-4~~");

        // Test VersionLessThan with ~~ suffix
        let constraint = VersionConstraint {
            operator: Operator::VersionLessThan,
            operand: "0.7.5-~~".to_string(),
        };
        assert!(check_version_constraint("0.7.4", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.4 should satisfy < 0.7.5-~~");
        assert!(!check_version_constraint("0.7.5", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5 should NOT satisfy < 0.7.5-~~");
        assert!(!check_version_constraint("0.7.6", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.6 should NOT satisfy < 0.7.5-~~");

        // Test VersionLessThanEqual with ~~ suffix
        let constraint = VersionConstraint {
            operator: Operator::VersionLessThanEqual,
            operand: "0.7.5-~~".to_string(),
        };
        assert!(check_version_constraint("0.7.4", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.4 should satisfy <= 0.7.5-~~");
        assert!(check_version_constraint("0.7.5", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5 should satisfy <= 0.7.5-~~");
        assert!(!check_version_constraint("0.7.6", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.6 should NOT satisfy <= 0.7.5-~~");

        // Test VersionEqual with ~~ suffix
        let constraint = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "0.7.5-~~".to_string(),
        };
        assert!(check_version_constraint("0.7.5", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5 should satisfy = 0.7.5-~~");
        assert!(!check_version_constraint("0.7.6", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.6 should NOT satisfy = 0.7.5-~~");

        // Test VersionEqual with local version suffix (+)
        // In Debian, packages with local version suffixes (e.g., "1.0+2") should satisfy
        // exact version constraints on the base version (e.g., "= 1.0")
        let constraint = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "6.14.0-1017.18~24.04.1".to_string(),
        };
        assert!(check_version_constraint("6.14.0-1017.18~24.04.1+2", &constraint, PackageFormat::Deb).unwrap(),
                "6.14.0-1017.18~24.04.1+2 should satisfy = 6.14.0-1017.18~24.04.1");
        assert!(check_version_constraint("6.14.0-1017.18~24.04.1", &constraint, PackageFormat::Deb).unwrap(),
                "6.14.0-1017.18~24.04.1 should satisfy = 6.14.0-1017.18~24.04.1");
        assert!(!check_version_constraint("6.14.0-1017.18~24.04.2", &constraint, PackageFormat::Deb).unwrap(),
                "6.14.0-1017.18~24.04.2 should NOT satisfy = 6.14.0-1017.18~24.04.1");

        // Test VersionGreaterThanEqual with ~ suffix (Debian pre-release indicator)
        // In Debian, "X~" has lowest precedence, so ">= X~" effectively means ">= X"
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "5.15.13+dfsg~".to_string(),
        };
        assert!(check_version_constraint("5.15.13+dfsg-1ubuntu1", &constraint, PackageFormat::Deb).unwrap(),
                "5.15.13+dfsg-1ubuntu1 should satisfy >= 5.15.13+dfsg~");
        assert!(check_version_constraint("5.15.13+dfsg", &constraint, PackageFormat::Deb).unwrap(),
                "5.15.13+dfsg should satisfy >= 5.15.13+dfsg~");
        assert!(check_version_constraint("5.15.14+dfsg", &constraint, PackageFormat::Deb).unwrap(),
                "5.15.14+dfsg should satisfy >= 5.15.13+dfsg~");
        assert!(!check_version_constraint("5.15.12+dfsg", &constraint, PackageFormat::Deb).unwrap(),
                "5.15.12+dfsg should NOT satisfy >= 5.15.13+dfsg~");

        // Test VersionGreaterThanEqual with ~ suffix when provided version also has ~
        // This is the speech-dispatcher case: ">= 0.12.0~" should match "0.12.0~rc2-2build3"
        // because 0.12.0~rc2 > 0.12.0~ (rc2 has higher precedence than bare ~)
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "0.12.0~".to_string(),
        };
        assert!(check_version_constraint("0.12.0~rc2-2build3", &constraint, PackageFormat::Deb).unwrap(),
                "0.12.0~rc2-2build3 should satisfy >= 0.12.0~");
        assert!(check_version_constraint("0.12.0~rc1-1", &constraint, PackageFormat::Deb).unwrap(),
                "0.12.0~rc1-1 should satisfy >= 0.12.0~");
        assert!(check_version_constraint("0.12.0-1", &constraint, PackageFormat::Deb).unwrap(),
                "0.12.0-1 should satisfy >= 0.12.0~ (final version > pre-release)");
        assert!(!check_version_constraint("0.11.9-1", &constraint, PackageFormat::Deb).unwrap(),
                "0.11.9-1 should NOT satisfy >= 0.12.0~");

        // Test VersionGreaterThanEqual with ~ suffix when versions differ only by trailing ~
        // This is the golang-google-genproto case: ">= 0.0~git20210726.e7812ac~" should match
        // "0.0~git20210726.e7812ac-4" because the version without trailing ~ is greater
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "0.0~git20210726.e7812ac~".to_string(),
        };
        assert!(check_version_constraint("0.0~git20210726.e7812ac-4", &constraint, PackageFormat::Deb).unwrap(),
                "0.0~git20210726.e7812ac-4 should satisfy >= 0.0~git20210726.e7812ac~");
        assert!(check_version_constraint("0.0~git20210726.e7812ac-1", &constraint, PackageFormat::Deb).unwrap(),
                "0.0~git20210726.e7812ac-1 should satisfy >= 0.0~git20210726.e7812ac~");
        assert!(check_version_constraint("0.0~git20210726.e7812ac", &constraint, PackageFormat::Deb).unwrap(),
                "0.0~git20210726.e7812ac should satisfy >= 0.0~git20210726.e7812ac~");

        // Test VersionNotEqual with ~~ suffix
        let constraint = VersionConstraint {
            operator: Operator::VersionNotEqual,
            operand: "0.7.5-~~".to_string(),
        };
        assert!(!check_version_constraint("0.7.5", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.5 should NOT satisfy != 0.7.5-~~");
        assert!(check_version_constraint("0.7.6", &constraint, PackageFormat::Deb).unwrap(),
                "0.7.6 should satisfy != 0.7.5-~~");

        // Test VersionLessThan with ~~ suffix for simple integer versions (bug fix)
        // This tests the specific case: python3.13dist(numpy)(<2~~,>=1.20)
        // where <2~~ should mean <3 (next version after 2)
        let constraint = VersionConstraint {
            operator: Operator::VersionLessThan,
            operand: "2~~".to_string(),
        };
        assert!(check_version_constraint("1.20", &constraint, PackageFormat::Rpm).unwrap(),
                "1.20 should satisfy < 2~~ (which means < 3)");
        assert!(check_version_constraint("1.99", &constraint, PackageFormat::Rpm).unwrap(),
                "1.99 should satisfy < 2~~ (which means < 3)");
        // Skip testing 2.1 and 2.0 as they might be considered equal to 2 in upstream comparison
        // The key test cases are the actual versions from the bug report: 2.2.4 and 2.2.6
        assert!(check_version_constraint("2.2.4", &constraint, PackageFormat::Rpm).unwrap(),
                "2.2.4 should satisfy < 2~~ (which means < 3)");
        assert!(check_version_constraint("2.2.6", &constraint, PackageFormat::Rpm).unwrap(),
                "2.2.6 should satisfy < 2~~ (which means < 3)");
        assert!(!check_version_constraint("2", &constraint, PackageFormat::Rpm).unwrap(),
                "2 should NOT satisfy < 2~~ (base version is excluded)");
        assert!(!check_version_constraint("3", &constraint, PackageFormat::Rpm).unwrap(),
                "3 should NOT satisfy < 2~~ (which means < 3)");
        assert!(!check_version_constraint("3.0", &constraint, PackageFormat::Rpm).unwrap(),
                "3.0 should NOT satisfy < 2~~ (which means < 3)");

        // Test VersionLessThan with ~~ suffix for versions with dots
        // <1.20~~ should mean <1.21 (increment last numeric segment)
        let constraint = VersionConstraint {
            operator: Operator::VersionLessThan,
            operand: "1.20~~".to_string(),
        };
        assert!(check_version_constraint("1.19", &constraint, PackageFormat::Rpm).unwrap(),
                "1.19 should satisfy < 1.20~~ (which means < 1.21)");
        assert!(!check_version_constraint("1.20", &constraint, PackageFormat::Rpm).unwrap(),
                "1.20 should NOT satisfy < 1.20~~ (base version is excluded)");
        assert!(check_version_constraint("1.20.5", &constraint, PackageFormat::Rpm).unwrap(),
                "1.20.5 should satisfy < 1.20~~ (which means < 1.21)");
        assert!(!check_version_constraint("1.21", &constraint, PackageFormat::Rpm).unwrap(),
                "1.21 should NOT satisfy < 1.20~~ (which means < 1.21)");
        assert!(!check_version_constraint("1.22", &constraint, PackageFormat::Rpm).unwrap(),
                "1.22 should NOT satisfy < 1.20~~ (which means < 1.21)");

        // Test with RPM format (where ~~ is also used)
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "1.2.3-~~".to_string(),
        };
        assert!(check_version_constraint("1.2.3-1.el8", &constraint, PackageFormat::Rpm).unwrap(),
                "1.2.3-1.el8 should satisfy >= 1.2.3-~~");
        assert!(check_version_constraint("1.2.4", &constraint, PackageFormat::Rpm).unwrap(),
                "1.2.4 should satisfy >= 1.2.3-~~");
        assert!(!check_version_constraint("1.2.2", &constraint, PackageFormat::Rpm).unwrap(),
                "1.2.2 should NOT satisfy >= 1.2.3-~~");

        // Test edge case: multiple dashes before ~~
        let constraint = VersionConstraint {
            operator: Operator::VersionGreaterThanEqual,
            operand: "1.2.3-beta-~~".to_string(),
        };
        assert!(check_version_constraint("1.2.3-beta-1", &constraint, PackageFormat::Deb).unwrap(),
                "1.2.3-beta-1 should satisfy >= 1.2.3-beta-~~");
        assert!(check_version_constraint("1.2.3-beta", &constraint, PackageFormat::Deb).unwrap(),
                "1.2.3-beta should satisfy >= 1.2.3-beta-~~");
    }

    #[test]
    fn test_check_version_satisfies_constraints() {
        // Test case 1: VersionCompatible constraint
        let constraints = vec![
            VersionConstraint {
                operator: Operator::VersionCompatible,
                operand: "5.82".to_string(),
            },
        ];
        // Test with versions that have patch components (like the existing test pattern)
        assert!(check_version_satisfies_constraints("5.82.0-r0", &constraints, PackageFormat::Apk).unwrap(),
                "5.82.0-r0 should satisfy ~5.82");
        assert!(check_version_satisfies_constraints("5.82.1-r0", &constraints, PackageFormat::Apk).unwrap(),
                "5.82.1-r0 should satisfy ~5.82");
        assert!(!check_version_satisfies_constraints("5.81-r0", &constraints, PackageFormat::Apk).unwrap(),
                "5.81-r0 should NOT satisfy ~5.82");

        // Test case 2: Multiple AND constraints
        let constraints_and = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "5.80".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "6.0".to_string(),
            },
        ];
        assert!(check_version_satisfies_constraints("5.82-r0", &constraints_and, PackageFormat::Apk).unwrap(),
                "5.82-r0 should satisfy >=5.80,<6.0");
        assert!(!check_version_satisfies_constraints("5.79-r0", &constraints_and, PackageFormat::Apk).unwrap(),
                "5.79-r0 should NOT satisfy >=5.80,<6.0");
        // With upstream-only comparison: 6.0-r0 has upstream 6.0, so 6.0 < 6.0 is false
        assert!(!check_version_satisfies_constraints("6.0-r0", &constraints_and, PackageFormat::Apk).unwrap(),
                "6.0-r0 should NOT satisfy >=5.80,<6.0 (upstream-only comparison: 6.0 < 6.0 is false)");
        assert!(!check_version_satisfies_constraints("6.0", &constraints_and, PackageFormat::Apk).unwrap(),
                "6.0 should NOT satisfy >=5.80,<6.0");

        // Test case 3: OR constraints (mutually exclusive)
        let constraints_or = vec![
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "5.80".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThan,
                operand: "5.80".to_string(),
            },
        ];
        assert!(check_version_satisfies_constraints("5.82-r0", &constraints_or, PackageFormat::Apk).unwrap(),
                "5.82-r0 should satisfy <5.80 OR >5.80");
        assert!(check_version_satisfies_constraints("5.79-r0", &constraints_or, PackageFormat::Apk).unwrap(),
                "5.79-r0 should satisfy <5.80 OR >5.80");
        // With upstream-only comparison: 5.80-r0 has upstream 5.80, so 5.80 < 5.80 is false and 5.80 > 5.80 is false
        assert!(!check_version_satisfies_constraints("5.80-r0", &constraints_or, PackageFormat::Apk).unwrap(),
                "5.80-r0 should NOT satisfy <5.80 OR >5.80 (upstream-only comparison: 5.80 == 5.80)");
        assert!(!check_version_satisfies_constraints("5.80", &constraints_or, PackageFormat::Apk).unwrap(),
                "5.80 should NOT satisfy <5.80 OR >5.80 (exactly equal)");

        // Test case 4: Mixed AND and OR constraints
        let constraints_mixed = vec![
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "5.80".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThan,
                operand: "5.80".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "6.0".to_string(),
            },
        ];
        assert!(check_version_satisfies_constraints("5.82-r0", &constraints_mixed, PackageFormat::Apk).unwrap(),
                "5.82-r0 should satisfy (<5.80 OR >5.80) AND <6.0");
        assert!(!check_version_satisfies_constraints("6.1-r0", &constraints_mixed, PackageFormat::Apk).unwrap(),
                "6.1-r0 should NOT satisfy (<5.80 OR >5.80) AND <6.0");
    }
}
