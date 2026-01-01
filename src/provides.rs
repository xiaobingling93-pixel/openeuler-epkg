//! Package provides checking module
//!
//! This module handles checking if a package provides a capability that satisfies
//! version constraints, supporting various package formats and provide entry formats.

use color_eyre::Result;
use crate::models::Package;
use crate::models::PackageFormat;
use crate::parse_requires::{VersionConstraint, Operator};
use crate::version_constraint::check_version_satisfies_constraints;
use crate::parse_version::PackageVersion;

/// Extract version from a provide entry remainder (the part after the capability name)
/// Returns Some((operator, version)) or None if no version found
///
/// IMPORTANT: Package 'provides' fields only support these forms:
/// - cap_with_arch (no version, just the capability name)
/// - cap_with_arch EQUALS cap_version (exact version match only)
///
/// The remainder_trimmed, provide_entry vars and everything in this function will never
/// contain >=, >, <=, < operators, so we only need to handle the "=" operator.
fn extract_version_from_remainder<'a>(remainder: &'a str, provide_entry: &'a str) -> Option<(&'a str, &'a str)> {
    if remainder.starts_with('=') {
        // Alpine format: "=version" (no spaces)
        Some(("=", &remainder[1..]))
    } else if remainder.starts_with(" = ") {
        // RPM format with spaces: " = version"
        Some(("=", &remainder[3..]))
    } else if remainder.starts_with("(= ") {
        // Debian format: "(= version)"
        let version_start = 3;
        if let Some(close_pos) = remainder[version_start..].find(')') {
            Some(("=", &remainder[version_start..version_start + close_pos]))
        } else {
            Some(("=", &remainder[version_start..]))
        }
    } else if let Some(pos) = provide_entry.find('=') {
        // Fallback: search in entire provide_entry (for backward compatibility)
        // Check if this is Alpine format (no spaces) or RPM format (with spaces)
        if pos > 0 && pos < provide_entry.len() - 1 {
            let before = &provide_entry[pos - 1..pos];
            let after = &provide_entry[pos + 1..pos + 2];
            // If there's no space before or after, it's Alpine format
            if before != " " && after != " " {
                Some(("=", &provide_entry[pos + 1..]))
            } else if before == " " {
                // RPM format with space before: "pkgname = version"
                Some(("=", &provide_entry[pos + 3..]))
            } else {
                Some(("=", &provide_entry[pos + 1..]))
            }
        } else {
            Some(("=", &provide_entry[pos + 1..]))
        }
    } else if let Some(pos) = provide_entry.find(" = ") {
        // RPM format with spaces: "pkgname = version"
        Some(("=", &provide_entry[pos + 3..]))
    } else if let Some(pos) = provide_entry.find("(= ") {
        // Debian format: "pkgname (= version)"
        let version_start = pos + 3;
        if let Some(close_pos) = provide_entry[version_start..].find(')') {
            Some(("=", &provide_entry[version_start..version_start + close_pos]))
        } else {
            Some(("=", &provide_entry[version_start..]))
        }
    } else {
        // No version specified in provide entry
        None
    }
}

/// Check if a package implicitly provides a capability (i.e., the capability name matches the package name)
/// In Alpine and most package managers, a package implicitly provides its own name.
fn check_implicit_provide(
    provider_pkgkey: &str,
    base_capability: &str,
    provider_pkg: &Package,
    constraints: &Vec<VersionConstraint>,
    format: PackageFormat,
) -> Result<bool> {
    // Check if the capability name matches the package name itself
    if let Ok(pkgname) = crate::package::pkgkey2pkgname(provider_pkgkey) {
        if base_capability == pkgname {
            // Package implicitly provides its own name - use package's own version
            let provided_version = provider_pkg.version.trim();

            log::trace!(
                "Provider {} implicitly provides '{}' (its own name) with version '{}'",
                provider_pkgkey, base_capability, provided_version
            );

            // Check constraints against the package's own version
            if check_version_satisfies_constraints(provided_version, constraints, format)? {
                log::debug!(
                    "Provider {} implicitly provides '{}' version '{}' satisfies all constraints",
                    provider_pkgkey, base_capability, provided_version
                );
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Check if a provider package's provides satisfy version constraints for a capability
pub fn check_provider_satisfies_constraints(
    provider_pkg: &Package,
    capability: &str,
    constraints: &Vec<VersionConstraint>,
    format: PackageFormat,
) -> Result<bool> {
    let provider_pkgkey = &provider_pkg.pkgkey;

    // Strip any version constraints from the capability name for matching
    // The capability might include version constraints like "pc:libpcre2-8>=10.32"
    // but we need to match against provide entries like "pc:libpcre2-8=10.46"
    let base_capability = if let Some(pos) = capability.find(|c: char| c == '>' || c == '<' || c == '=' || c == '!') {
        capability[..pos].trim_end()
    } else {
        capability
    };

    let provider_pkg_version = provider_pkg.version.trim().to_string();

    log::trace!(
        "Checking provider {} for capability '{}' (base: '{}') with constraints {:?}",
        provider_pkgkey, capability, base_capability, constraints
    );

    // For Alpine and Pacman formats, provide entries may contain multiple space-separated provide items
    // Alpine: "pc:libpcre2-16=10.46 pc:libpcre2-32=10.46 pc:libpcre2-8=10.46"
    // Pacman: "libutil-linux libblkid.so=1-64 libfdisk.so=1-64 libmount.so=1-64"
    // For Debian/RPM format, each entry is already a complete provide entry
    // e.g., "libgcc1 (= 1:14.2.0-19)" or "test-pkg = 1.0.0"
    // Also check for bundled() variants: if looking for "cap", also check "bundled(cap)"
    let bundled_variant = format!("bundled({})", base_capability);
    for provide_entry_string in &provider_pkg.provides {
        let provide_items: Vec<&str> = if format == PackageFormat::Apk || format == PackageFormat::Pacman {
            // Alpine/Pacman: split by whitespace to get individual provide items
            provide_entry_string.split_whitespace().collect()
        } else {
            // Debian/RPM: each entry is already complete, no splitting needed
            vec![provide_entry_string.as_str()]
        };

        for provide_entry in provide_items {
            let provide_entry_trimmed = provide_entry.trim();

            // Check if this provide entry matches the capability (direct or bundled)
            // First check direct match
            let matches_direct = provide_entry_trimmed.starts_with(base_capability);
            // Then check bundled variant match
            let matches_bundled = provide_entry_trimmed.starts_with(&bundled_variant);

            if !matches_direct && !matches_bundled {
                continue; // Doesn't match at all
            }

            // Use the appropriate capability name for remainder checking
            let matched_capability = if matches_bundled {
                &bundled_variant
            } else {
                base_capability
            };

            // Check if the remainder (after capability name) is valid
            // IMPORTANT: Package 'provides' fields only support cap_with_arch or cap_with_arch EQUALS cap_version.
            //
            // Operators like >=, >, <=, < are artifacts from metadata parsing and should be ignored.
            // wfg /c/epkg% gr -c '^provides: .*>' ~/.cache/epkg/channels/|g -v ':0$'
            // /home/wfg/.cache/epkg/channels/opensuse:16.0/oss/x86_64/packages.txt:10
            // /home/wfg/.cache/epkg/channels/fedora:42/Everything-updates/x86_64/packages.txt:11
            // /home/wfg/.cache/epkg/channels/fedora:42/Everything/x86_64/packages.txt:12
            // wfg /c/epkg% gr -c '^provides: .*<' ~/.cache/epkg/channels/|g -v ':0$'
            // /home/wfg/.cache/epkg/channels/opensuse:16.0/oss/x86_64/packages.txt:10
            // /home/wfg/.cache/epkg/channels/fedora:42/Everything-updates/x86_64/packages.txt:5
            // /home/wfg/.cache/epkg/channels/fedora:42/Everything/x86_64/packages.txt:22
            //
            // Also handle library aliases like "lib.so=lib.so-64" for Arch Linux
            let remainder = &provide_entry_trimmed[matched_capability.len()..];
            let remainder_trimmed = remainder.trim_start();

            // Explicitly skip provides with invalid operators (>=, <=, >, <) - these are artifacts
            if !remainder_trimmed.is_empty() && (
                remainder_trimmed.starts_with(">=") ||
                remainder_trimmed.starts_with("<=") ||
                remainder_trimmed.starts_with(" > ") ||
                remainder_trimmed.starts_with(" < ") ||
                (remainder_trimmed.starts_with('>') && !remainder_trimmed.starts_with(">=")) ||
                (remainder_trimmed.starts_with('<') && !remainder_trimmed.starts_with("<="))
            ) {
                // Ignore provides with operators other than "=" (artifacts from metadata parsing)
                continue;
            }

            if !remainder_trimmed.is_empty() && !remainder_trimmed.starts_with('=') &&
               !remainder_trimmed.starts_with("(= ") {
                // Doesn't match - capability name is a prefix of something else
                continue;
            }

            // Check if this is a library alias (Arch Linux format: "lib.so=lib.so-64")
            // Library aliases don't have version constraints, so they satisfy any requirement for the base capability
            if format == PackageFormat::Pacman && remainder_trimmed.starts_with('=') {
                let after_equals = &remainder_trimmed[1..];
                // Check if it looks like a library alias (contains .so and doesn't start with digit)
                if after_equals.contains(".so") && !after_equals.chars().next().map_or(false, |c| c.is_ascii_digit()) {
                    // This is a library alias - it satisfies the requirement without version checking
                    // (since requires for library aliases are parsed as the base capability without constraints)
                    return Ok(true);
                }
            }

            // Extract version from provide entry if present
            // Use remainder_trimmed to handle leading spaces in Debian format
            let provide_version = extract_version_from_remainder(remainder_trimmed, provide_entry);

            // If no version in provide entry, use the package's own version
            // This is standard behavior across all package formats: when a package provides
            // a capability without a version, the package's version is used for constraint checking
            // NOTE: In all package formats (RPM, Debian, Alpine, Pacman, etc.), provides are
            // always stored as exact versions with "=", never with operators like ">=".
            // The version in the provide entry is the actual version at which the package
            // provides the capability. Version operators (>=, >, <=, <) are only used in
            // requirements/constraints, not in provides.
            let has_non_conditional_constraints = constraints.iter().any(|c| !matches!(c.operator, Operator::IfInstall));
            let mut used_package_version_directly = false;
            let provided_version = if let Some((_, version_str)) = provide_version {
                // Provide has a version - use that version for constraint checking
                version_str.trim()
            } else {
                // No version in provide entry - use package's own version
                if has_non_conditional_constraints {
                    used_package_version_directly = true;
                    provider_pkg_version.as_str()
                } else {
                    // No version constraints (or only conditional), so this satisfies
                    return Ok(true);
                }
            };

            // Check if the provided version satisfies all constraints
            if check_version_satisfies_constraints(provided_version, constraints, format)? {
                log::debug!(
                    "Provider {} provides '{}' version '{}' satisfies all constraints",
                    provider_pkgkey, capability, provided_version
                );
                return Ok(true);
            }

            // Fallback for RPM: some capabilities (e.g., php-composer()) only record upstream
            // versions in their provides entries, even though dependencies may specify a release.
            // If the provide failed because it lacked the release, retry with the package EVR.
            if !used_package_version_directly
                && format == PackageFormat::Rpm
                && rpm_constraints_require_release(constraints)
                && rpm_provide_missing_release(provided_version, provider_pkg_version.as_str())
            {
                if check_version_satisfies_constraints(provider_pkg_version.as_str(), constraints, format)? {
                    log::debug!(
                        "Provider {} fallback: using package version '{}' for capability '{}' satisfies constraints {:?}",
                        provider_pkgkey,
                        provider_pkg_version,
                        capability,
                        constraints
                    );
                    return Ok(true);
                }
            }
        }
    }

    // If no explicit provide entry matched, check if the capability name matches
    // the package name itself (implicit provide). In Alpine and most package managers,
    // a package implicitly provides its own name.
    if check_implicit_provide(provider_pkgkey, &base_capability, &provider_pkg, constraints, format)? {
        return Ok(true);
    }

    log::debug!(
        "Provider {} does not provide '{}' with version satisfying constraints",
        provider_pkgkey, capability
    );
    Ok(false)
}

fn rpm_provide_missing_release(provided_version: &str, package_version: &str) -> bool {
    let provided = match PackageVersion::parse(provided_version) {
        Ok(parsed) => parsed,
        Err(_) => return false,
    };
    let package = match PackageVersion::parse(package_version) {
        Ok(parsed) => parsed,
        Err(_) => return false,
    };

    provided.epoch == package.epoch
        && provided.upstream == package.upstream
        && provided.revision == "0"
        && package.revision != "0"
}

fn rpm_constraints_require_release(constraints: &Vec<VersionConstraint>) -> bool {
    constraints
        .iter()
        .filter(|c| !matches!(c.operator, Operator::IfInstall))
        .filter_map(|c| PackageVersion::parse(c.operand.trim()).ok())
        .any(|parsed| parsed.revision != "0")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use crate::models::{PackageFormat, PACKAGE_CACHE};
    use crate::package_cache::add_package_to_cache;


    #[test]
    fn test_check_provider_satisfies_constraints_with_or_conditions() {
        PACKAGE_CACHE.clear();

        // Create a mock package that provides python3.13dist(isort) version 6.1
        let mut provider_pkg = Package {
            pkgname: "python3-isort".to_string(),
            version: "6.1.0-1.fc42".to_string(),
            arch: "noarch".to_string(),
            pkgkey: "python3-isort__6.1.0-1.fc42__noarch".to_string(),
            provides: vec!["python3.13dist(isort) = 6.1".to_string()],
            ..Default::default()
        };

        // Cache the package
        add_package_to_cache(Arc::new(provider_pkg.clone()), PackageFormat::Rpm);

        // Test case 1: OR condition with mutually exclusive constraints
        // Requirement: ((python3.13dist(isort) < 5.13 or python3.13dist(isort) > 5.13) with python3.13dist(isort) < 7 with python3.13dist(isort) >= 4.2.5)
        // Version 6.1 should satisfy: > 5.13 (OR), < 7 (AND), >= 4.2.5 (AND)
        let constraints = vec![
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "5.13".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThan,
                operand: "5.13".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "7".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "4.2.5".to_string(),
            },
        ];

        let result = check_provider_satisfies_constraints(
            &provider_pkg,
            "python3.13dist(isort)",
            &constraints,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 6.1 should satisfy OR condition (> 5.13) and AND conditions (< 7, >= 4.2.5)");

        // Test case 2: Version that doesn't satisfy OR condition
        // Version 5.13 should NOT satisfy: < 5.13 is false, > 5.13 is false (exactly equal)
        provider_pkg.provides = vec!["python3.13dist(isort) = 5.13".to_string()];
        add_package_to_cache(Arc::new(provider_pkg.clone()), PackageFormat::Rpm);

        let result = check_provider_satisfies_constraints(
            &provider_pkg,
            "python3.13dist(isort)",
            &constraints,
            PackageFormat::Rpm,
        );
        assert!(!result.unwrap(), "Version 5.13 should NOT satisfy OR condition (neither < 5.13 nor > 5.13, exactly equal)");

        // Test case 3: Version that satisfies < 5.13 branch of OR condition
        provider_pkg.provides = vec!["python3.13dist(isort) = 5.0".to_string()];
        let constraints_v2 = vec![
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "5.13".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThan,
                operand: "5.13".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "4.2.5".to_string(),
            },
        ];
        add_package_to_cache(Arc::new(provider_pkg.clone()), PackageFormat::Rpm);

        let result = check_provider_satisfies_constraints(
            &provider_pkg,
            "python3.13dist(isort)",
            &constraints_v2,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 5.0 should satisfy OR condition (< 5.13) and AND condition (>= 4.2.5)");

        // Test case 4: Only AND constraints (no OR groups)
        let constraints_and_only = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "4.2.5".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "7".to_string(),
            },
        ];
        provider_pkg.provides = vec!["python3.13dist(isort) = 6.1".to_string()];
        add_package_to_cache(Arc::new(provider_pkg.clone()), PackageFormat::Rpm);

        let result = check_provider_satisfies_constraints(
            &provider_pkg,
            "python3.13dist(isort)",
            &constraints_and_only,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 6.1 should satisfy all AND constraints");

        // Test case 5: AND constraint failure
        let constraints_fail = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "4.2.5".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "5.0".to_string(), // 6.1 is not < 5.0
            },
        ];
        provider_pkg.provides = vec!["python3.13dist(isort) = 6.1".to_string()];
        add_package_to_cache(Arc::new(provider_pkg.clone()), PackageFormat::Rpm);

        let result = check_provider_satisfies_constraints(
            &provider_pkg,
            "python3.13dist(isort)",
            &constraints_fail,
            PackageFormat::Rpm,
        );
        assert!(!result.unwrap(), "Version 6.1 should NOT satisfy constraint < 5.0");

        // Test case 6: Multiple OR groups with same operand pattern
        // First OR group: < 3 or > 3, Second OR group: < 7 or > 7
        // Version 5.0 should satisfy: > 3 (first OR) and < 7 (second OR, but also compatible with AND)
        let constraints_multiple_or = vec![
            // First OR group: < 3 or > 3
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "3".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThan,
                operand: "3".to_string(),
            },
            // Second OR group: < 7 or > 7
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "7".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThan,
                operand: "7".to_string(),
            },
        ];
        provider_pkg.provides = vec!["python3.13dist(isort) = 5.0".to_string()];
        add_package_to_cache(Arc::new(provider_pkg.clone()), PackageFormat::Rpm);

        let result = check_provider_satisfies_constraints(
            &provider_pkg,
            "python3.13dist(isort)",
            &constraints_multiple_or,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 5.0 should satisfy both OR groups (> 3 and < 7)");
    }

    #[test]
    fn test_or_group_detection_with_different_operands() {

        PACKAGE_CACHE.clear();

        // Create a mock package that provides python3.13dist(google-api-core) version 2.11.1
        let mut provider_pkg = Package {
            pkgname: "python3-google-api-core".to_string(),
            version: "2.11.1-11.fc42".to_string(),
            arch: "noarch".to_string(),
            pkgkey: "python3-google-api-core__1:2.11.1-11.fc42__noarch".to_string(),
            provides: vec!["python3.13dist(google-api-core) = 2.11.1".to_string()],
            ..Default::default()
        };

        add_package_to_cache(Arc::new(provider_pkg.clone()), PackageFormat::Rpm);

        // Test case: OR group with different operands (without ~~ to avoid constraint checking issues)
        // Requirement: ((python3.13dist(google-api-core) < 2.1 or >= 2.2) with >= 1.31.6)
        // Version 2.11.1 should satisfy: >= 2.2 (OR) and >= 1.31.6 (AND)
        let constraints = vec![
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "2.1".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "2.2".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "1.31.6".to_string(),
            },
        ];

        let result = check_provider_satisfies_constraints(
            &provider_pkg,
            "python3.13dist(google-api-core)",
            &constraints,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 2.11.1 should satisfy OR condition (>= 2.2) and AND condition (>= 1.31.6)");

        // Test case: Version that satisfies < 2.1 branch
        provider_pkg.provides = vec!["python3.13dist(google-api-core) = 2.0.5".to_string()];
        add_package_to_cache(Arc::new(provider_pkg.clone()), PackageFormat::Rpm);

        let result = check_provider_satisfies_constraints(
            &provider_pkg,
            "python3.13dist(google-api-core)",
            &constraints,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 2.0.5 should satisfy OR condition (< 2.1) and AND condition (>= 1.31.6)");
    }

    #[test]
    fn test_or_group_detection_multiple_with_clauses() {

        PACKAGE_CACHE.clear();

        // Create a mock package that provides version 2.5.0
        let provider_pkg = Package {
            pkgname: "python3-google-api-core".to_string(),
            version: "2.5.0-1.fc42".to_string(),
            arch: "noarch".to_string(),
            pkgkey: "python3-google-api-core__1:2.5.0-1.fc42__noarch".to_string(),
            provides: vec!["python3.13dist(google-api-core) = 2.5.0".to_string()],
            ..Default::default()
        };

        add_package_to_cache(Arc::new(provider_pkg.clone()), PackageFormat::Rpm);

        // Test case: Multiple OR groups (simplified)
        // Version 2.5.0 should satisfy:
        // - First OR: >= 2.2 (since 2.5.0 >= 2.2)
        // - Second OR: >= 2.3 (since 2.5.0 >= 2.3)
        // - Third OR: > 2.3 (since 2.5.0 > 2.3)
        // - AND: < 3, >= 1.31.6
        let constraints = vec![
            // First OR group: < 2.1 or >= 2.2
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "2.1".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "2.2".to_string(),
            },
            // Second OR group: < 2.2 or >= 2.3
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "2.2".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "2.3".to_string(),
            },
            // Third OR group: < 2.3 or > 2.3 (same operand, should be detected)
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "2.3".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThan,
                operand: "2.3".to_string(),
            },
            // AND constraints
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "3".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "1.31.6".to_string(),
            },
        ];

        let result = check_provider_satisfies_constraints(
            &provider_pkg,
            "python3.13dist(google-api-core)",
            &constraints,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 2.5.0 should satisfy all OR groups and AND constraints");
    }

    #[test]
    fn test_or_group_detection_same_operand_strict() {

        PACKAGE_CACHE.clear();

        let mut provider_pkg = Package {
            pkgname: "test-pkg".to_string(),
            version: "2.3.0-1".to_string(),
            arch: "noarch".to_string(),
            pkgkey: "test-pkg__2.3.0-1__noarch".to_string(),
            provides: vec!["test-capability = 2.3".to_string()],
            ..Default::default()
        };

        add_package_to_cache(Arc::new(provider_pkg.clone()), PackageFormat::Rpm);

        // Test case: <= X and >= X should NOT be mutually exclusive (X satisfies both)
        let constraints_not_exclusive = vec![
            VersionConstraint {
                operator: Operator::VersionLessThanEqual,
                operand: "2.3".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "2.3".to_string(),
            },
        ];

        let result = check_provider_satisfies_constraints(
            &provider_pkg,
            "test-capability",
            &constraints_not_exclusive,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 2.3 should satisfy both <= 2.3 and >= 2.3 (not mutually exclusive)");

        // Test case: < X and > X should be mutually exclusive
        let constraints_exclusive = vec![
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "2.3".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionGreaterThan,
                operand: "2.3".to_string(),
            },
        ];

        let result = check_provider_satisfies_constraints(
            &provider_pkg,
            "test-capability",
            &constraints_exclusive,
            PackageFormat::Rpm,
        );
        assert!(!result.unwrap(), "Version 2.3 should NOT satisfy both < 2.3 and > 2.3 (mutually exclusive)");

        // But version 2.2 should satisfy < 2.3 (first OR branch)
        provider_pkg.provides = vec!["test-capability = 2.2".to_string()];
        add_package_to_cache(Arc::new(provider_pkg.clone()), PackageFormat::Rpm);

        let result = check_provider_satisfies_constraints(
            &provider_pkg,
            "test-capability",
            &constraints_exclusive,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Version 2.2 should satisfy OR condition (< 2.3)");
    }

    #[test]
    fn test_debian_provide_format_with_parentheses() {

        PACKAGE_CACHE.clear();

        // Test Debian format: libgcc1 (= 1:14.2.0-19)
        let provider_pkg = Package {
            pkgname: "libgcc-s1".to_string(),
            version: "14.2.0-19".to_string(),
            arch: "amd64".to_string(),
            pkgkey: "libgcc-s1__14.2.0-19__amd64".to_string(),
            provides: vec!["libgcc1 (= 1:14.2.0-19)".to_string()],
            ..Default::default()
        };

        add_package_to_cache(Arc::new(provider_pkg.clone()), PackageFormat::Rpm);

        // Test constraint: libgcc1 (>= 1:3.0)
        // Version 1:14.2.0-19 should satisfy >= 1:3.0
        let constraints = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "1:3.0".to_string(),
            },
        ];

        let result = check_provider_satisfies_constraints(
            &provider_pkg,
            "libgcc1",
            &constraints,
            PackageFormat::Deb,
        );
        assert!(result.unwrap(), "Version 1:14.2.0-19 should satisfy >= 1:3.0");
    }

    #[test]
    fn test_debian_provide_format_various_operators() {

        PACKAGE_CACHE.clear();

        // Test various Debian provide formats with parentheses
        // Note: Debian provide entries always use "= version" format
        // The constraint is what the requester needs
        let test_cases = vec![
            // Provided version 1.0.0, constraint: = 1.0.0 -> true
            ("test-pkg (= 1.0.0)", "test-pkg", Operator::VersionEqual, "1.0.0", true),
            // Provided version 1.0.0, constraint: = 1.0.1 -> false
            ("test-pkg (= 1.0.0)", "test-pkg", Operator::VersionEqual, "1.0.1", false),
            // Provided version 2.0.0, constraint: >= 1.5.0 -> true (2.0.0 >= 1.5.0)
            ("test-pkg (= 2.0.0)", "test-pkg", Operator::VersionGreaterThanEqual, "1.5.0", true),
            // Provided version 2.0.0, constraint: >= 2.5.0 -> false (2.0.0 < 2.5.0)
            ("test-pkg (= 2.0.0)", "test-pkg", Operator::VersionGreaterThanEqual, "2.5.0", false),
            // Provided version 3.0.0, constraint: <= 3.5.0 -> true (3.0.0 <= 3.5.0)
            ("test-pkg (= 3.0.0)", "test-pkg", Operator::VersionLessThanEqual, "3.5.0", true),
            // Provided version 3.0.0, constraint: <= 2.5.0 -> false (3.0.0 > 2.5.0)
            ("test-pkg (= 3.0.0)", "test-pkg", Operator::VersionLessThanEqual, "2.5.0", false),
            // Provided version 4.5.0, constraint: > 4.0.0 -> true (4.5.0 > 4.0.0)
            ("test-pkg (= 4.5.0)", "test-pkg", Operator::VersionGreaterThan, "4.0.0", true),
            // Provided version 4.0.0, constraint: > 4.0.0 -> false (4.0.0 is not > 4.0.0)
            ("test-pkg (= 4.0.0)", "test-pkg", Operator::VersionGreaterThan, "4.0.0", false),
            // Provided version 4.5.0, constraint: < 5.0.0 -> true (4.5.0 < 5.0.0)
            ("test-pkg (= 4.5.0)", "test-pkg", Operator::VersionLessThan, "5.0.0", true),
            // Provided version 5.0.0, constraint: < 5.0.0 -> false (5.0.0 is not < 5.0.0)
            ("test-pkg (= 5.0.0)", "test-pkg", Operator::VersionLessThan, "5.0.0", false),
        ];

        for (provide_entry, capability, constraint_op, constraint_operand, expected) in test_cases {
            let provider_pkg = Package {
                pkgname: "test-provider".to_string(),
                version: "1.0.0".to_string(),
                arch: "amd64".to_string(),
                pkgkey: format!("test-provider__1.0.0__amd64"),
                provides: vec![provide_entry.to_string()],
                ..Default::default()
            };

            add_package_to_cache(Arc::new(provider_pkg.clone()), PackageFormat::Rpm);

            let constraints = vec![
                VersionConstraint {
                    operator: constraint_op.clone(),
                    operand: constraint_operand.to_string(),
                },
            ];

            let result = check_provider_satisfies_constraints(
                &provider_pkg,
                capability,
                &constraints,
                PackageFormat::Deb,
            );

            assert_eq!(
                result.unwrap(),
                expected,
                "Failed for provide_entry: '{}', constraint: {:?} '{}'",
                provide_entry,
                constraint_op,
                constraint_operand
            );
        }
    }

    #[test]
    fn test_epoch_version_comparison() {

        PACKAGE_CACHE.clear();

        // Test epoch version comparison: 1:14.2.0-19 >= 1:3.0
        let provider_pkg = Package {
            pkgname: "libgcc-s1".to_string(),
            version: "14.2.0-19".to_string(),
            arch: "amd64".to_string(),
            pkgkey: "libgcc-s1__14.2.0-19__amd64".to_string(),
            provides: vec!["libgcc1 (= 1:14.2.0-19)".to_string()],
            ..Default::default()
        };

        add_package_to_cache(Arc::new(provider_pkg.clone()), PackageFormat::Rpm);

        // Test various epoch version constraints
        let test_cases = vec![
            ("1:14.2.0-19", Operator::VersionGreaterThanEqual, "1:3.0", true),
            ("1:14.2.0-19", Operator::VersionGreaterThanEqual, "1:14.2.0-19", true),
            ("1:14.2.0-19", Operator::VersionGreaterThanEqual, "1:15.0.0", false),
            ("1:14.2.0-19", Operator::VersionGreaterThanEqual, "2:1.0.0", false), // epoch 2 > epoch 1
            ("2:1.0.0", Operator::VersionGreaterThanEqual, "1:14.2.0-19", true), // epoch 2 > epoch 1
            ("1:14.2.0-19", Operator::VersionLessThanEqual, "1:15.0.0", true),
            ("1:14.2.0-19", Operator::VersionLessThanEqual, "1:14.2.0-19", true),
            ("1:14.2.0-19", Operator::VersionLessThanEqual, "1:3.0", false),
            ("1:14.2.0-19", Operator::VersionEqual, "1:14.2.0-19", true),
            ("1:14.2.0-19", Operator::VersionEqual, "1:14.2.0-20", false),
        ];

        for (provided_version, constraint_op, constraint_operand, expected) in test_cases {
            let mut test_pkg = provider_pkg.clone();
            test_pkg.provides = vec![format!("libgcc1 (= {})", provided_version)];
            test_pkg.pkgkey = format!("libgcc-s1__{}__amd64", provided_version.replace(':', "_"));

            add_package_to_cache(Arc::new(test_pkg.clone()), PackageFormat::Deb);

            let constraints = vec![
                VersionConstraint {
                    operator: constraint_op.clone(),
                    operand: constraint_operand.to_string(),
                },
            ];

            let result = check_provider_satisfies_constraints(
                &test_pkg,
                "libgcc1",
                &constraints,
                PackageFormat::Deb,
            );

            assert_eq!(
                result.unwrap(),
                expected,
                "Failed for provided_version: '{}', constraint: {:?} '{}'",
                provided_version,
                constraint_op,
                constraint_operand
            );
        }
    }

    #[test]
    fn test_rpm_vs_debian_provide_format() {

        PACKAGE_CACHE.clear();

        // Test RPM format: "capability = version" (no parentheses)
        let rpm_provider = Package {
            pkgname: "rpm-provider".to_string(),
            version: "1.0.0".to_string(),
            arch: "x86_64".to_string(),
            pkgkey: "rpm-provider__1.0.0__x86_64".to_string(),
            provides: vec!["test-capability = 1.0.0".to_string()],
            ..Default::default()
        };

        add_package_to_cache(Arc::new(rpm_provider.clone()), PackageFormat::Rpm);

        let rpm_constraints = vec![
            VersionConstraint {
                operator: Operator::VersionEqual,
                operand: "1.0.0".to_string(),
            },
        ];

        let rpm_result = check_provider_satisfies_constraints(
            &rpm_provider,
            "test-capability",
            &rpm_constraints,
            PackageFormat::Rpm,
        );
        assert!(rpm_result.unwrap(), "RPM format should work: 'test-capability = 1.0.0'");

        // Test Debian format: "capability (= version)" (with parentheses)
        let deb_provider = Package {
            pkgname: "deb-provider".to_string(),
            version: "1.0.0".to_string(),
            arch: "amd64".to_string(),
            pkgkey: "deb-provider__1.0.0__amd64".to_string(),
            provides: vec!["test-capability (= 1.0.0)".to_string()],
            ..Default::default()
        };

        add_package_to_cache(Arc::new(deb_provider.clone()), PackageFormat::Deb);

        let deb_constraints = vec![
            VersionConstraint {
                operator: Operator::VersionEqual,
                operand: "1.0.0".to_string(),
            },
        ];

        let deb_result = check_provider_satisfies_constraints(
            &deb_provider,
            "test-capability",
            &deb_constraints,
            PackageFormat::Deb,
        );
        assert!(deb_result.unwrap(), "Debian format should work: 'test-capability (= 1.0.0)'");
    }

    #[test]
    fn test_alpine_pkgconfig_multiple_provides() {

        PACKAGE_CACHE.clear();

        // Create a provider package that provides multiple pkgconfig entries in a single string
        // This is the format used by Alpine packages like pcre2-dev
        let provider_pkg = Package {
            pkgname: "pcre2-dev".to_string(),
            version: "10.46-r0".to_string(),
            arch: "x86_64".to_string(),
            pkgkey: "pcre2-dev__10.46-r0__x86_64".to_string(),
            provides: vec![
                // Multiple provide entries in a single string (space-separated)
                "pc:libpcre2-16=10.46 pc:libpcre2-32=10.46 pc:libpcre2-8=10.46 pc:libpcre2-posix=10.46 cmd:pcre2-config=10.46-r0".to_string(),
            ],
            ..Default::default()
        };

        // Cache the package
        add_package_to_cache(Arc::new(provider_pkg.clone()), PackageFormat::Rpm);

        // Test 1: Check that pc:libpcre2-8>=10.32 is satisfied by pc:libpcre2-8=10.46
        let constraints = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "10.32".to_string(),
            },
        ];

        let result = check_provider_satisfies_constraints(
            &provider_pkg,
            "pc:libpcre2-8",
            &constraints,
            PackageFormat::Apk,
        ).unwrap();

        assert!(result, "pc:libpcre2-8=10.46 should satisfy pc:libpcre2-8>=10.32");

        // Test 2: Check that pc:libpcre2-8>=10.50 is NOT satisfied by pc:libpcre2-8=10.46
        let constraints_fail = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "10.50".to_string(),
            },
        ];

        let result_fail = check_provider_satisfies_constraints(
            &provider_pkg,
            "pc:libpcre2-8",
            &constraints_fail,
            PackageFormat::Apk,
        ).unwrap();

        assert!(!result_fail, "pc:libpcre2-8=10.46 should NOT satisfy pc:libpcre2-8>=10.50");

        // Test 3: Check that capability name with version constraints is handled correctly
        let result_with_constraint_in_name = check_provider_satisfies_constraints(
            &provider_pkg,
            "pc:libpcre2-8>=10.32",  // Capability name includes constraint
            &constraints,
            PackageFormat::Apk,
        ).unwrap();

        assert!(result_with_constraint_in_name, "Should handle capability name with version constraints");

        // Test 4: Check that other provide entries in the same string are also found
        let constraints_posix = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "10.0".to_string(),
            },
        ];

        let result_posix = check_provider_satisfies_constraints(
            &provider_pkg,
            "pc:libpcre2-posix",
            &constraints_posix,
            PackageFormat::Apk,
        ).unwrap();

        assert!(result_posix, "pc:libpcre2-posix=10.46 should satisfy pc:libpcre2-posix>=10.0");
    }

    #[test]
    fn test_check_provider_satisfies_constraints_implicit_provide() {

        PACKAGE_CACHE.clear();

        // Test case 1: Implicit provide with VersionCompatible (~) operator (the bug case)
        // Package bluez__5.82-r0__x86_64 should satisfy requirement bluez~5.82
        // Use a version format that works with VersionCompatible (with patch component)
        let bluez_pkg = Package {
            pkgname: "bluez".to_string(),
            version: "5.82.0-r0".to_string(),
            arch: "x86_64".to_string(),
            pkgkey: "bluez__5.82.0-r0__x86_64".to_string(),
            provides: vec![], // No explicit provides - relies on implicit provide
            ..Default::default()
        };

        add_package_to_cache(Arc::new(bluez_pkg.clone()), PackageFormat::Apk);

        let constraints_compatible = vec![
            VersionConstraint {
                operator: Operator::VersionCompatible,
                operand: "5.82".to_string(),
            },
        ];

        let result = check_provider_satisfies_constraints(
            &bluez_pkg,
            "bluez",
            &constraints_compatible,
            PackageFormat::Apk,
        );
        assert!(result.unwrap(), "bluez__5.82.0-r0__x86_64 should satisfy bluez~5.82 via implicit provide");

        // Test case 2: Implicit provide with VersionGreaterThanEqual constraint
        let constraints_gte = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "5.80".to_string(),
            },
        ];

        let result = check_provider_satisfies_constraints(
            &bluez_pkg,
            "bluez",
            &constraints_gte,
            PackageFormat::Apk,
        );
        assert!(result.unwrap(), "bluez__5.82.0-r0__x86_64 should satisfy bluez>=5.80 via implicit provide");

        // Test case 3: Implicit provide with constraint that doesn't match
        let constraints_fail = vec![
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "5.80".to_string(),
            },
        ];

        let result = check_provider_satisfies_constraints(
            &bluez_pkg,
            "bluez",
            &constraints_fail,
            PackageFormat::Apk,
        );
        assert!(!result.unwrap(), "bluez__5.82.0-r0__x86_64 should NOT satisfy bluez<5.80");

        // Test case 4: Capability name doesn't match package name - should not use implicit provide
        let result = check_provider_satisfies_constraints(
            &bluez_pkg,
            "different-package",
            &constraints_compatible,
            PackageFormat::Apk,
        );
        assert!(!result.unwrap(), "Should not use implicit provide when capability name doesn't match package name");

        // Test case 5: Package with explicit provide should still work (explicit takes precedence)
        let bluez_with_explicit = Package {
            pkgname: "bluez".to_string(),
            version: "5.82.0-r0".to_string(),
            arch: "x86_64".to_string(),
            pkgkey: "bluez__5.82.0-r0__x86_64_explicit".to_string(),
            provides: vec!["bluez = 5.82.0-r0".to_string()], // Explicit provide
            ..Default::default()
        };

        add_package_to_cache(Arc::new(bluez_with_explicit.clone()), PackageFormat::Apk);

        let result = check_provider_satisfies_constraints(
            &bluez_with_explicit,
            "bluez",
            &constraints_compatible,
            PackageFormat::Apk,
        );
        assert!(result.unwrap(), "Explicit provide should work and take precedence over implicit");

        // Test case 6: Implicit provide with multiple constraints (AND)
        let constraints_multiple = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "5.80".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "6.0".to_string(),
            },
        ];

        let result = check_provider_satisfies_constraints(
            &bluez_pkg,
            "bluez",
            &constraints_multiple,
            PackageFormat::Apk,
        );
        assert!(result.unwrap(), "bluez__5.82.0-r0__x86_64 should satisfy bluez>=5.80,<6.0 via implicit provide");

        // Test case 7: Implicit provide with OR constraints
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

        let result = check_provider_satisfies_constraints(
            &bluez_pkg,
            "bluez",
            &constraints_or,
            PackageFormat::Apk,
        );
        assert!(result.unwrap(), "bluez__5.82.0-r0__x86_64 should satisfy bluez<5.80 OR bluez>5.80 via implicit provide");
    }

    #[test]
    fn test_rpm_provide_with_version_operators() {

        PACKAGE_CACHE.clear();

        // Test case 1: Package provides capability with exact version
        // In RPM repositories, provides are always stored as exact versions with "=".
        // When a package provides "test-cap = 3.0.0", it means the package provides
        // test-cap at version 3.0.0. We use that version to check against requirement constraints.
        let provider_pkg = Package {
            pkgname: "test-provider".to_string(),
            version: "3.0.0".to_string(),
            arch: "x86_64".to_string(),
            pkgkey: "test-provider__3.0.0__x86_64".to_string(),
            provides: vec!["test-cap = 3.0.0".to_string()], // RPM format: exact version with =
            ..Default::default()
        };

        add_package_to_cache(Arc::new(provider_pkg.clone()), PackageFormat::Rpm);

        // Requirement: test-cap(>=2.0.0)
        // Provided version 3.0.0 should satisfy >=2.0.0
        let constraints = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "2.0.0".to_string(),
            },
        ];

        let result = check_provider_satisfies_constraints(
            &provider_pkg,
            "test-cap",
            &constraints,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "Package providing test-cap=3.0.0 should satisfy requirement >=2.0.0");

        // Test case 2: Package provides version that doesn't satisfy requirement
        let provider_pkg_low = Package {
            pkgname: "test-pkg-low".to_string(),
            version: "1.0.0".to_string(),
            arch: "x86_64".to_string(),
            pkgkey: "test-pkg-low__1.0.0__x86_64".to_string(),
            provides: vec!["test-cap = 1.0.0".to_string()], // Provides at version 1.0.0
            ..Default::default()
        };

        add_package_to_cache(Arc::new(provider_pkg_low.clone()), PackageFormat::Rpm);

        let constraints_high = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "2.0.0".to_string(),
            },
        ];

        // This should fail because provided version 1.0.0 doesn't satisfy >=2.0.0
        let result_low = check_provider_satisfies_constraints(
            &provider_pkg_low,
            "test-cap",
            &constraints_high,
            PackageFormat::Rpm,
        );
        assert!(!result_low.unwrap(), "Package providing test-cap=1.0.0 should NOT satisfy requirement >=2.0.0");

        // Test case 3: Package provides version that satisfies multiple constraints
        let provider_pkg_multi = Package {
            pkgname: "test-pkg-multi".to_string(),
            version: "3.0.0".to_string(),
            arch: "x86_64".to_string(),
            pkgkey: "test-pkg-multi__3.0.0__x86_64".to_string(),
            provides: vec!["test-cap = 3.0.0".to_string()],
            ..Default::default()
        };

        add_package_to_cache(Arc::new(provider_pkg_multi.clone()), PackageFormat::Rpm);

        let constraints_multi = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "2.5.0".to_string(),
            },
            VersionConstraint {
                operator: Operator::VersionLessThan,
                operand: "4.0.0".to_string(),
            },
        ];

        let result_multi = check_provider_satisfies_constraints(
            &provider_pkg_multi,
            "test-cap",
            &constraints_multi,
            PackageFormat::Rpm,
        );
        assert!(result_multi.unwrap(), "Package providing test-cap=3.0.0 should satisfy requirement >=2.5.0,<4.0.0");

        // Test case 6: Package provides with exact version (=) - should use provided version, not package version
        let provider_pkg_eq = Package {
            pkgname: "test-pkg-eq".to_string(),
            version: "5.0.0".to_string(), // Package version is 5.0.0
            arch: "x86_64".to_string(),
            pkgkey: "test-pkg-eq__5.0.0__x86_64".to_string(),
            provides: vec!["test-cap = 3.0.0".to_string()], // But provides test-cap at version 3.0.0
            ..Default::default()
        };

        add_package_to_cache(Arc::new(provider_pkg_eq.clone()), PackageFormat::Rpm);

        let constraints_eq = vec![
            VersionConstraint {
                operator: Operator::VersionEqual,
                operand: "3.0.0".to_string(),
            },
        ];

        let result_eq = check_provider_satisfies_constraints(
            &provider_pkg_eq,
            "test-cap",
            &constraints_eq,
            PackageFormat::Rpm,
        );
        assert!(result_eq.unwrap(), "Package providing test-cap=3.0.0 should satisfy requirement =3.0.0 (using provided version, not package version)");

        // Test case 4: Real-world scenario - mesa-libglapi case
        // Package provides mesa-libglapi at its own version, which should satisfy the requirement
        let mesa_provider = Package {
            pkgname: "mesa-dri-drivers".to_string(),
            version: "25.1.9-1.fc42".to_string(),
            arch: "x86_64".to_string(),
            pkgkey: "mesa-dri-drivers__25.1.9-1.fc42__x86_64".to_string(),
            provides: vec!["mesa-libglapi = 25.1.9-1.fc42".to_string()], // Provides at package's own version
            ..Default::default()
        };

        add_package_to_cache(Arc::new(mesa_provider.clone()), PackageFormat::Rpm);

        let mesa_constraints = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "25.0.0~rc2-1".to_string(),
            },
        ];

        let mesa_result = check_provider_satisfies_constraints(
            &mesa_provider,
            "mesa-libglapi",
            &mesa_constraints,
            PackageFormat::Rpm,
        );
        assert!(mesa_result.unwrap(), "mesa-dri-drivers providing mesa-libglapi=25.1.9-1.fc42 should satisfy requirement >=25.0.0~rc2-1");
    }

    #[test]
    fn test_check_provider_satisfies_constraints_rpm_composer_fallback() {

        PACKAGE_CACHE.clear();

        // Test case: RPM composer capability fallback
        // Package provides capability with only upstream version (no release),
        // but dependency requires a release. The fallback should use the package's
        // full EVR (with release) to satisfy the constraint.
        //
        // Real-world example: php-geshi provides "php-composer(geshi/geshi) = 1.0.9.1"
        // but dokuwiki requires "php-composer(geshi/geshi) >= 1.0.9.1-5"
        let php_geshi = Package {
            pkgname: "php-geshi".to_string(),
            version: "1.0.9.1-18.20230219git7884d22.fc42".to_string(),
            arch: "noarch".to_string(),
            pkgkey: "php-geshi__1.0.9.1-18.20230219git7884d22.fc42__noarch".to_string(),
            provides: vec!["php-composer(geshi/geshi) = 1.0.9.1".to_string()], // Only upstream version, no release
            ..Default::default()
        };

        add_package_to_cache(Arc::new(php_geshi.clone()), PackageFormat::Rpm);

        // Constraint requires a release (>= 1.0.9.1-5)
        let constraints = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "1.0.9.1-5".to_string(),
            },
        ];

        // The initial check should fail because provide version "1.0.9.1" doesn't satisfy ">= 1.0.9.1-5"
        // But the fallback should succeed by using the package's full version "1.0.9.1-18.20230219git7884d22.fc42"
        let result = check_provider_satisfies_constraints(
            &php_geshi,
            "php-composer(geshi/geshi)",
            &constraints,
            PackageFormat::Rpm,
        );
        assert!(result.unwrap(), "php-geshi providing php-composer(geshi/geshi)=1.0.9.1 should satisfy requirement >=1.0.9.1-5 via fallback to package version 1.0.9.1-18.20230219git7884d22.fc42");

        // Test case 2: Constraint without release should work with provided version directly
        let constraints_no_release = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "1.0.9.1".to_string(),
            },
        ];

        let result_no_release = check_provider_satisfies_constraints(
            &php_geshi,
            "php-composer(geshi/geshi)",
            &constraints_no_release,
            PackageFormat::Rpm,
        );
        assert!(result_no_release.unwrap(), "php-geshi providing php-composer(geshi/geshi)=1.0.9.1 should satisfy requirement >=1.0.9.1 (no release needed)");

        // Test case 3: Constraint with release that's too high should fail even with fallback
        let constraints_too_high = vec![
            VersionConstraint {
                operator: Operator::VersionGreaterThanEqual,
                operand: "1.0.9.1-100".to_string(),
            },
        ];

        let result_too_high = check_provider_satisfies_constraints(
            &php_geshi,
            "php-composer(geshi/geshi)",
            &constraints_too_high,
            PackageFormat::Rpm,
        );
        assert!(!result_too_high.unwrap(), "php-geshi version 1.0.9.1-18.20230219git7884d22.fc42 should NOT satisfy requirement >=1.0.9.1-100");

        // Test case 4: Package with provide that already includes release should not use fallback
        let php_geshi_with_release = Package {
            pkgname: "php-geshi-release".to_string(),
            version: "1.0.9.1-18.20230219git7884d22.fc42".to_string(),
            arch: "noarch".to_string(),
            pkgkey: "php-geshi-release__1.0.9.1-18.20230219git7884d22.fc42__noarch".to_string(),
            provides: vec!["php-composer(geshi/geshi) = 1.0.9.1-18.20230219git7884d22.fc42".to_string()], // Already includes release
            ..Default::default()
        };

        add_package_to_cache(Arc::new(php_geshi_with_release.clone()), PackageFormat::Rpm);

        let result_with_release = check_provider_satisfies_constraints(
            &php_geshi_with_release,
            "php-composer(geshi/geshi)",
            &constraints,
            PackageFormat::Rpm,
        );
        assert!(result_with_release.unwrap(), "php-geshi-release providing php-composer(geshi/geshi)=1.0.9.1-18.20230219git7884d22.fc42 should satisfy requirement >=1.0.9.1-5 (no fallback needed)");
    }
}
