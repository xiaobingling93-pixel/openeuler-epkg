use std::collections::HashMap;
use std::path::Path;
use std::fs;
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use rpm::{IndexTag, Package};
use crate::models::InstalledPackageInfo;
use crate::parse_requires::{VersionConstraint, parse_version_constraints};
use crate::rpm_pkg::get_scriptlet_from_header;

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

/// Extract trigger priorities from RPM header
/// Attempts to extract u32 array, falls back to default priority if not available
/// RPM stores priorities as INT32 array, default is 1000000 (RPMTRIGGER_DEFAULT_PRIORITY)
fn extract_trigger_priorities(metadata: &rpm::PackageMetadata, priority_tag: IndexTag, default_priority: u32) -> Vec<u32> {
    // Try to extract priorities from header
    // The rpm crate may not expose a direct method for u32/int32 arrays
    // We'll try different approaches and fall back to default if needed

    if !metadata.header.entry_is_present(priority_tag) {
        return Vec::new();
    }

    // Attempt 1: Try to get as string array and parse (some RPM implementations store as strings)
    if let Ok(priority_strings) = metadata.header.get_entry_data_as_string_array(priority_tag) {
        let mut priorities = Vec::new();
        for s in priority_strings {
            if let Ok(priority) = s.parse::<u32>() {
                priorities.push(priority);
            } else {
                priorities.push(default_priority);
            }
        }
        if !priorities.is_empty() {
            return priorities;
        }
    }

    // Attempt 2: Try to get raw entry data and parse as u32 array
    // Note: This is a fallback - the rpm crate API may not support this directly
    // If the crate doesn't support numeric arrays, we'll use default priority

    // For now, return empty vector - callers will use default priority
    // TODO: Enhance when rpm crate adds support for numeric array extraction
    Vec::new()
}

/// Write a .hook file for RPM triggers in Arch-style hook format.
fn write_rpm_hook_file<P: AsRef<Path>>(
    install_dir: P,
    hook_name: &str,
    when: &str,
    op: &str,
    hook_type: &str,
    targets: &[String],
    exec: &str,
    priority: Option<u32>,
) -> Result<()> {
    use std::fmt::Write;

    let install_dir = install_dir.as_ref();
    if targets.is_empty() {
        return Ok(());
    }

    let mut buf = String::new();
    // [Trigger] section
    buf.push_str("[Trigger]\n");
    writeln!(buf, "Operation = {}", op)?;
    writeln!(buf, "Type = {}", hook_type)?;
    for t in targets {
        // Strip leading '/' from target path
        let target = t.strip_prefix('/').unwrap_or(t);
        writeln!(buf, "Target = {}", target)?;
    }

    // [Action] section
    buf.push_str("\n[Action]\n");
    writeln!(buf, "When = {}", when)?;
    writeln!(buf, "Exec = {}", exec)?;
    if let Some(p) = priority {
        writeln!(buf, "Priority = {}", p)?;
    }

    // NOTE: No wrapper is necessary since we can see the store path inside the env.
    let hook_path = install_dir.join(format!("{}.hook", hook_name));
    std::fs::write(&hook_path, buf)
        .wrap_err_with(|| format!("Failed to write RPM hook file {}", hook_path.display()))?;

    Ok(())
}

/// Extract install prefixes from RPM package for relocatable packages
/// Stores them in info/install/install_prefixes.txt for use in scriptlet environment variables
pub fn extract_install_prefixes<P: AsRef<Path>>(package: &Package, store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let install_dir = store_tmp_dir.join("info/install");
    let metadata = &package.metadata;

    // Check if RPMTAG_INSTPREFIXES exists in the header
    if metadata.header.entry_is_present(IndexTag::RPMTAG_INSTPREFIXES) {
        if let Ok(prefixes) = metadata.header.get_entry_data_as_string_array(IndexTag::RPMTAG_INSTPREFIXES) {
            if !prefixes.is_empty() {
                // Write prefixes to file, one per line
                let prefixes_content = prefixes.iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<String>>()
                    .join("\n");
                let prefixes_file = install_dir.join("install_prefixes.txt");
                fs::write(&prefixes_file, prefixes_content)
                    .wrap_err_with(|| format!("Failed to write install prefixes to {}", prefixes_file.display()))?;
                log::debug!("Extracted {} install prefix(es) for relocatable package", prefixes.len());
            }
        }
    }
    // If no install prefixes, package is not relocatable - that's fine
    Ok(())
}

/// Extract string array from RPM metadata header
/// Returns an empty vector if the tag is not present or extraction fails
fn extract_string_array(metadata: &rpm::PackageMetadata, tag: IndexTag) -> Vec<String> {
    if metadata.header.entry_is_present(tag) {
        metadata.header.get_entry_data_as_string_array(tag).ok()
            .unwrap_or_default()
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        Vec::new()
    }
}

fn is_one_liner(script_content: &str) -> bool {
    let trimmed = script_content.trim();
    !trimmed.is_empty() && !trimmed.contains('\n')
}

fn escape_for_sh_single_quotes(input: &str) -> String {
    // Replace each ' with '"'"' so it can be safely embedded in a single-quoted string.
    input.replace('\'', "'\"'\"'")
}

/// Build Exec value for a trigger script body.
///
/// - If the script is a one-liner, we treat it as the Exec command directly.
/// - Otherwise, we wrap the body in a generic /bin/sh -c '...' invocation.
fn build_exec_for_script(script_body: &str) -> String {
    let body = script_body.trim();
    if body.is_empty() {
        return "/bin/true".to_string();
    }
    if is_one_liner(body) {
        return body.to_string();
    }

    let escaped = escape_for_sh_single_quotes(body);
    format!("/bin/sh -c '{}'", escaped)
}

/// Extract RPM trigger scriptlets (package triggers, file triggers, transaction file triggers)
/// and store them as Arch-style .hook files under info/install/.
pub fn extract_rpm_triggers<P: AsRef<Path>>(package: &Package, store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let metadata = &package.metadata;

    extract_rpm_package_triggers(metadata, store_tmp_dir)?;
    extract_rpm_file_triggers(metadata, store_tmp_dir)?;
    extract_rpm_transaction_file_triggers(metadata, store_tmp_dir)?;

    Ok(())
}

/// Extract RPM package triggers (triggerprein, triggerin, triggerun, triggerpostun)
/// These are triggered by package names with optional version conditions
///
/// For each trigger type that exists, creates a hook file in info/install/:
///
///   rpm-<trigger_type>.hook
///
/// Example layout:
///   [Trigger]
///   Operation = Install|Remove
///   Type = Package
///   Target = <package-name>
///
///   [Action]
///   When = PreInstall|PostInstall|PreRemove|PostRemove
///   Exec = <one-liner command or interpreter invocation>
fn extract_rpm_package_triggers<P: AsRef<Path>>(metadata: &rpm::PackageMetadata, store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let install_dir = store_tmp_dir.join("info/install");

    let trigger_names: Vec<String> = extract_string_array(metadata, IndexTag::RPMTAG_TRIGGERNAME);

    let package_trigger_types = vec![
        ("triggerprein", "PreInstall", "Install"),
        ("triggerin", "PostInstall", "Install"),
        ("triggerun", "PreRemove", "Remove"),
        ("triggerpostun", "PostRemove", "Remove"),
    ];

    for (trigger_type, when, op) in package_trigger_types {
        if let Some(scriptlet) = get_scriptlet_from_header(metadata, trigger_type) {
            let exec = build_exec_for_script(&scriptlet.script);
            // NOTE: No wrapper is necessary since store paths are visible inside the env.
            write_rpm_hook_file(
                &install_dir,
                &format!("rpm-{}", trigger_type),
                when,
                op,
                "Package",
                &trigger_names,
                &exec,
                None,
            )?;
        }
    }

    Ok(())
}

/// Extract RPM file triggers (filetriggerin, filetriggerun, filetriggerpostun)
/// These are triggered by file paths
///
/// Output Layout:
/// ==============
/// For each trigger type that exists, creates files in info/install/:
///
/// 1. Scriptlet file: <trigger_type>.<ext>
///    - filetriggerin.sh, filetriggerun.sh, filetriggerpostun.sh
///    - Or with detected extension: filetriggerin.lua, etc.
///    - Contains the trigger scriptlet with appropriate shebang if needed
///
/// 2. Trigger metadata file: <trigger_type>.triggers
///    - filetriggerin.triggers, filetriggerun.triggers, filetriggerpostun.triggers
///    - Format: One file path pattern per line
///    - Example:
///      "/usr/lib/libfoo.so"
///      "/etc/foo.conf"
///
/// 3. Trigger priorities file: <trigger_type>.priorities
///    - filetriggerin.priorities, filetriggerun.priorities, filetriggerpostun.priorities
///    - Format: One priority value per line (u32 as string)
///    - Each line corresponds to the same-indexed line in .triggers file
///    - Default priority: 1000000 (RPMTRIGGER_DEFAULT_PRIORITY)
///    - Example:
///      "1000000"
///      "1000000"
fn extract_rpm_file_triggers<P: AsRef<Path>>(metadata: &rpm::PackageMetadata, store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let install_dir = store_tmp_dir.join("info/install");
    const DEFAULT_PRIORITY: u32 = 1000000; // RPMTRIGGER_DEFAULT_PRIORITY

    let file_trigger_types = vec![
        ("filetriggerin", IndexTag::RPMTAG_FILETRIGGERNAME),
        ("filetriggerun", IndexTag::RPMTAG_FILETRIGGERNAME),
        ("filetriggerpostun", IndexTag::RPMTAG_FILETRIGGERNAME),
    ];

    // Get file trigger priorities (shared across all file trigger types)
    let file_trigger_priorities: Vec<u32> =
        extract_trigger_priorities(metadata, IndexTag::RPMTAG_FILETRIGGERPRIORITIES, DEFAULT_PRIORITY);

    // For each file trigger type, create high/low priority hooks mapped to When phases.
    for (trigger_type, name_tag) in file_trigger_types {
        if let Some(scriptlet) = get_scriptlet_from_header(metadata, trigger_type) {
            let exec = build_exec_for_script(&scriptlet.script);
            let trigger_paths = extract_string_array(metadata, name_tag);
            if trigger_paths.is_empty() {
                continue;
            }

            // Partition paths into high/low priority groups using the shared priorities array.
            let mut high_paths = Vec::new();
            let mut low_paths = Vec::new();
            for (idx, path) in trigger_paths.iter().enumerate() {
                let prio = file_trigger_priorities
                    .get(idx)
                    .cloned()
                    .unwrap_or(DEFAULT_PRIORITY);
                if prio >= 10_000 {
                    high_paths.push(path.clone());
                } else {
                    low_paths.push(path.clone());
                }
            }

            // Map trigger_type/priority-class to When string.
            let map_when = |t: &str, high: bool| -> Option<&'static str> {
                match (t, high) {
                    ("filetriggerin", true) => Some("PreInstall"),
                    ("filetriggerin", false) => Some("PreInstall2"),
                    ("filetriggerun", true) => Some("PreRemove"),
                    ("filetriggerun", false) => Some("PreRemove2"),
                    ("filetriggerpostun", true) => Some("PostRemove"),
                    ("filetriggerpostun", false) => Some("PostRemove2"),
                    _ => None,
                }
            };

            if !high_paths.is_empty() {
                if let Some(when_str) = map_when(trigger_type, true) {
                    write_rpm_hook_file(
                        &install_dir,
                        &format!("rpm-{}-high", trigger_type),
                        when_str,
                        // High-priority file triggers conceptually run before or after
                        // the corresponding scriptlets, but they are still associated
                        // with install/remove operations.
                        if *trigger_type == *"filetriggerun" || *trigger_type == *"filetriggerpostun" {
                            "Remove"
                        } else {
                            "Install"
                        },
                        "Path",
                        &high_paths,
                        &exec,
                        Some(DEFAULT_PRIORITY),
                    )?;
                }
            }

            if !low_paths.is_empty() {
                if let Some(when_str) = map_when(trigger_type, false) {
                    write_rpm_hook_file(
                        &install_dir,
                        &format!("rpm-{}-low", trigger_type),
                        when_str,
                        if *trigger_type == *"filetriggerun" || *trigger_type == *"filetriggerpostun" {
                            "Remove"
                        } else {
                            "Install"
                        },
                        "Path",
                        &low_paths,
                        &exec,
                        Some(DEFAULT_PRIORITY),
                    )?;
                }
            }
        }
    }

    Ok(())
}

/// Extract RPM transaction file triggers (transfiletriggerin, transfiletriggerun, transfiletriggerpostun)
/// These are triggered by file paths at transaction level
fn extract_rpm_transaction_file_triggers<P: AsRef<Path>>(metadata: &rpm::PackageMetadata, store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let install_dir = store_tmp_dir.join("info/install");
    const DEFAULT_PRIORITY: u32 = 1000000; // RPMTRIGGER_DEFAULT_PRIORITY

    let trans_file_trigger_types = vec![
        ("transfiletriggerin", IndexTag::RPMTAG_TRANSFILETRIGGERNAME),
        ("transfiletriggerun", IndexTag::RPMTAG_TRANSFILETRIGGERNAME),
        ("transfiletriggerpostun", IndexTag::RPMTAG_TRANSFILETRIGGERNAME),
    ];

    // Get transaction file trigger priorities (shared across all transaction file trigger types)
    let trans_trigger_priorities: Vec<u32> =
        extract_trigger_priorities(metadata, IndexTag::RPMTAG_TRANSFILETRIGGERPRIORITIES, DEFAULT_PRIORITY);

    // For now, just generate hooks with PreTransaction, PostTransaction, PostUnTrans
    for (trigger_type, name_tag) in trans_file_trigger_types {
        if let Some(scriptlet) = get_scriptlet_from_header(metadata, trigger_type) {
            let exec = build_exec_for_script(&scriptlet.script);
            let trigger_paths = extract_string_array(metadata, name_tag);
            if trigger_paths.is_empty() {
                continue;
            }

            // Map transaction file trigger type to (When, Operation) pair.
            // Semantics:
            // - %transfiletriggerin  of any, set off by new  (Install side)
            // - %transfiletriggerun  of any, set off by removal of old (Remove side)
            // - %transfiletriggerpostun of any, set off by old (Remove side)
            let (when_str, op_str) = match trigger_type {
                "transfiletriggerin" => ("PostTransaction", "Install"),
                "transfiletriggerun" => ("PreTransaction", "Remove"),
                "transfiletriggerpostun" => ("PostUnTrans", "Remove"),
                _ => continue,
            };

            write_rpm_hook_file(
                &install_dir,
                &format!("rpm-{}", trigger_type),
                when_str,
                op_str,
                "Path",
                &trigger_paths,
                &exec,
                trans_trigger_priorities.first().cloned(),
            )?;
        }
    }

    Ok(())
}
