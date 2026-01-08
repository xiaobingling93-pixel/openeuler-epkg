use std::collections::HashMap;
use std::path::Path;
use crate::models::InstalledPackageInfo;
use crate::parse_requires::{VersionConstraint, parse_version_constraints};

/// Set up environment variables for RPM package scriptlets
/// Sets RPM_INSTALL_PREFIX and RPM_INSTALL_PREFIX_N for relocatable packages
pub fn setup_rpm_env_vars(
    env_vars: &mut std::collections::HashMap<String, String>,
    _pkgkey: &str,
    package_info: &InstalledPackageInfo,
    store_root: &Path,
) {
    // Try to read install prefixes from package metadata
    // RPM stores install prefixes in RPMTAG_INSTPREFIXES
    // For now, we'll try to read from a metadata file if it exists
    let install_dir = store_root.join(&package_info.pkgline).join("info/install");
    let prefixes_file = install_dir.join("install_prefixes.txt");

    if let Ok(prefixes_content) = std::fs::read_to_string(&prefixes_file) {
        let prefixes: Vec<&str> = prefixes_content.lines().filter(|s| !s.trim().is_empty()).collect();
        for (index, prefix) in prefixes.iter().enumerate() {
            let var_name = if index == 0 {
                "RPM_INSTALL_PREFIX".to_string()
            } else {
                format!("RPM_INSTALL_PREFIX_{}", index)
            };
            env_vars.insert(var_name, prefix.to_string());
        }
        // Also set RPM_INSTALL_PREFIX0 for compatibility
        if !prefixes.is_empty() {
            env_vars.insert("RPM_INSTALL_PREFIX0".to_string(), prefixes[0].to_string());
        }
    }
    // If no prefixes file exists, that's okay - package is not relocatable
}

/// Parse RPM package trigger condition with version constraints
/// Also works for single name/pkgname case for DEB/Archlinux/...
/// Only handles two cases:
///   1. Single package name with constraint: "rpcbind > 0.2.2-2.0"
///   2. List of package names without constraints: "pam, glibc, libselinux" or "kernel kernel-xen"
/// Examples:
///   "pam, glibc, libselinux" -> {("pam", []), ("glibc", []), ("libselinux", [])}
///   "kernel kernel-xen" -> {("kernel", []), ("kernel-xen", [])}
///   "rpcbind > 0.2.2-2.0" -> {("rpcbind", [VersionConstraint { operator: VersionGreaterThan, operand: "0.2.2-2.0" }])}
///   "rpm < 4.15.90-0.git14971.10" -> {("rpm", [VersionConstraint { operator: VersionLessThan, operand: "4.15.90-0.git14971.10" }])}
pub fn parse_rpm_trigger_condition(target: &str) -> HashMap<String, Vec<VersionConstraint>> {
    let mut result = HashMap::new();

    // First detect if there's any version operator: '>', '<', or '='
    let has_version_operator = target.contains('>') || target.contains('<') || target.contains('=');
    if has_version_operator {
        // Case 1: Single package name with version constraint
        // Format: "pkgname operator version"
        let parts: Vec<&str> = target.split_whitespace().collect();
        if parts.len() >= 2 {
            let pkg_name = parts[0].to_string();
            let constraint_str = parts[1..].join(" ");
            match parse_version_constraints(&constraint_str) {
                Ok(constraints) => {
                    result.entry(pkg_name).or_insert_with(Vec::new).extend(constraints);
                }
                Err(e) => {
                    log::warn!("Failed to parse version constraints for {} '{}': {}", pkg_name, constraint_str, e);
                    result.entry(pkg_name).or_insert_with(Vec::new);
                }
            }
        }
    } else {
        // Case 2: List of package names without constraints
        // Split by comma first, then by whitespace
        let comma_parts: Vec<&str> = target.split(',').collect();
        for comma_part in comma_parts {
            for pkg_name in comma_part.split_whitespace() {
                let pkg_name = pkg_name.trim();
                if !pkg_name.is_empty() {
                    result.entry(pkg_name.to_string()).or_insert_with(Vec::new);
                }
            }
        }
    }

    result
}
