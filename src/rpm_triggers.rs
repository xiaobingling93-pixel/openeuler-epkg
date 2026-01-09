use std::collections::HashMap;
use std::path::Path;
use std::fs;
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use rpm::{IndexTag, Package};
use crate::models::InstalledPackageInfo;
use crate::parse_requires::{VersionConstraint, parse_version_constraints};
use crate::rpm_pkg::{get_scriptlet_from_header, determine_script_extension};

/// Default priority for RPM triggers (RPMTRIGGER_DEFAULT_PRIORITY)
pub const RPMTRIGGER_DEFAULT_PRIORITY: u32 = 1000000;

/// Priority bound for distinguishing high vs low priority triggers (TRIGGER_PRIORITY_MID)
/// Triggers with priority >= TRIGGER_PRIORITY_MID are high priority (executed before postin/preun)
/// Triggers with priority < TRIGGER_PRIORITY_MID are low priority (executed after postin/preun)
const TRIGGER_PRIORITY_MID: u32 = 10_000;

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
/// Generates unique hook names by encoding priority and sequence number.
fn write_rpm_hook_file<P: AsRef<Path>>(
    install_dir: P,
    trigger_type: &str,
    when: &str,
    op: &str,
    hook_type: &str,
    target: &str,
    scriptlet: &rpm::Scriptlet,
    priority: Option<u32>,
    hook_seq_counter: &mut HashMap<String, u32>,
) -> Result<()> {
    use std::fmt::Write;

    let install_dir = install_dir.as_ref();

    let base_hook_name = format!("rpm-{}", trigger_type);

    // Generate unique hook name by encoding priority and sequence number
    let priority_str = priority.map(|p| format!("-p{}", p)).unwrap_or_default();
    let seqno = hook_seq_counter.entry(base_hook_name.clone()).or_insert(0);
    *seqno += 1;
    let hook_name = format!("{}-{}{}", base_hook_name, seqno, priority_str);

    // Build exec command by writing script to disk
    let exec = build_exec_for_script(scriptlet, install_dir, &hook_name)?;

    let mut buf = String::new();
    // [Trigger] section
    buf.push_str("[Trigger]\n");
    writeln!(buf, "Operation = {}", op)?;
    writeln!(buf, "Type = {}", hook_type)?;
    // Strip leading '/' from target path
    let target_stripped = target.strip_prefix('/').unwrap_or(target);
    writeln!(buf, "Target = {}", target_stripped)?;

    // [Action] section
    buf.push_str("\n[Action]\n");
    writeln!(buf, "When = {}", when)?;
    writeln!(buf, "Exec = {}", exec)?;
    if hook_type == "Path" {
        writeln!(buf, "NeedsTargets")?;
    }
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

/// Build Exec value for a trigger script body.
///
/// Writes the script body to a disk file with appropriate shebang,
/// makes it executable, and returns Exec command path
fn build_exec_for_script<P: AsRef<Path>>(
    scriptlet: &rpm::Scriptlet,
    install_dir: P,
    hook_name: &str,
) -> Result<String> {
    let install_dir = install_dir.as_ref();
    let body = scriptlet.script.trim();

    if body.is_empty() {
        log::warn!("Empty script body for hook {}", hook_name);
        return Ok("/bin/true".to_string());
    }

    // Reuse determine_script_extension() to get extension and modified content
    let (extension, script_content) = determine_script_extension(scriptlet, body);

    // Create script file path
    let script_path = install_dir.join(format!("{}.{}", hook_name, extension));

    // Write script to disk
    std::fs::write(&script_path, script_content)
        .wrap_err_with(|| format!("Failed to write trigger script to {}", script_path.display()))?;

    // Make script executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_path)
            .wrap_err_with(|| format!("Failed to get metadata for {}", script_path.display()))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms)
            .wrap_err_with(|| format!("Failed to set permissions for {}", script_path.display()))?;
    }

    Ok(script_path.to_string_lossy().to_string())
}

/// Extract RPM trigger scriptlets (package triggers, file triggers, transaction file triggers)
/// and store them as Arch-style .hook files under info/install/.
pub fn extract_rpm_triggers<P: AsRef<Path>>(package: &Package, store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let metadata = &package.metadata;

    extract_rpm_package_triggers(metadata, store_tmp_dir)?;
    extract_rpm_file_triggers(metadata, store_tmp_dir)?;

    Ok(())
}

/// Extract RPM package triggers (triggerprein, triggerin, triggerun, triggerpostun)
/// These are triggered by package names with optional version conditions
///
/// For each trigger type that exists, creates a hook file in info/install/:
///
///   rpm-<trigger_type>-<seqno>[-p<PRIORITY>].hook
///
/// Example layout:
///   [Trigger]
///   Operation = Install|Remove
///   Type = Package
///   Target = <package names with optional version constraints>
///
///   [Action]
///   When = PreInstall|PostInstall|PreRemove|PostRemove
///   Exec = <saved-script>
fn extract_rpm_package_triggers<P: AsRef<Path>>(metadata: &rpm::PackageMetadata, store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let install_dir = store_tmp_dir.join("info/install");

    let trigger_names: Vec<String> = extract_string_array(metadata, IndexTag::RPMTAG_TRIGGERNAME);
    if trigger_names.is_empty() {
        return Ok(());
    }

    let package_trigger_types = vec![
        ("triggerprein",    "PreInstall",   "Install"),
        ("triggerin",       "PostInstall",  "Install"),
        ("triggerun",       "PreRemove",    "Remove"),
        ("triggerpostun",   "PostRemove",   "Remove"),
    ];

    let mut hook_seq_counter = HashMap::new();

    for (trigger_type, when, op) in package_trigger_types {
        if let Some(scriptlet) = get_scriptlet_from_header(metadata, trigger_type) {
            // Create a separate hook for each trigger entry (each index)
            for trigger_name in trigger_names.iter() {
                // NOTE: No wrapper is necessary since store paths are visible inside the env.
                write_rpm_hook_file(
                    &install_dir,
                    trigger_type,
                    when,
                    op,
                    "Package",
                    trigger_name,
                    &scriptlet,
                    None, // Package triggers has no priority
                    &mut hook_seq_counter,
                )?;
            }
        }
    }

    Ok(())
}

/// Process file triggers for a given trigger type
fn process_file_trigger_type<P: AsRef<Path>>(
    metadata: &rpm::PackageMetadata,
    install_dir: P,
    trigger_type: &str,
    trigger_paths: &[String],
    trigger_priorities: &[u32],
    use_priority_based_when: bool,
    hook_seq_counter: &mut HashMap<String, u32>,
) -> Result<()> {
    let install_dir = install_dir.as_ref();

    if let Some(scriptlet) = get_scriptlet_from_header(metadata, trigger_type) {
        if trigger_paths.is_empty() {
            return Ok(());
        }

        // Map trigger_type/priority to When string for file triggers
        let map_when_file = |t: &str, prio: u32| -> Option<&'static str> {
            let high = prio >= TRIGGER_PRIORITY_MID;
            match (t, high) {
                ("filetriggerin", true)         => Some("PreInstall"),
                ("filetriggerin", false)        => Some("PreInstall2"),
                ("filetriggerun", true)         => Some("PreRemove"),
                ("filetriggerun", false)        => Some("PreRemove2"),
                ("filetriggerpostun", true)     => Some("PostRemove"),
                ("filetriggerpostun", false)    => Some("PostRemove2"),
                _ => None,
            }
        };

        // Map transaction file trigger type to (When, Operation) pair
        let map_when_op_trans = |t: &str| -> Option<(&'static str, &'static str)> {
            match t {
                "transfiletriggerin" => Some(("PostTransaction", "Install")),
                "transfiletriggerun" => Some(("PreTransaction", "Remove")),
                "transfiletriggerpostun" => Some(("PostUnTrans", "Remove")),
                _ => None,
            }
        };

        // Create a separate hook for each trigger entry (each index)
        for (idx, path) in trigger_paths.iter().enumerate() {
            let prio = trigger_priorities
                .get(idx)
                .cloned()
                .unwrap_or(RPMTRIGGER_DEFAULT_PRIORITY);

            let (when_str, op_str) = if use_priority_based_when {
                // File triggers: when depends on priority
                if let Some(when) = map_when_file(trigger_type, prio) {
                    let op = if trigger_type == "filetriggerun" || trigger_type == "filetriggerpostun" {
                        "Remove"
                    } else {
                        "Install"
                    };
                    (when, op)
                } else {
                    continue;
                }
            } else {
                // Transaction file triggers: fixed when/op mapping
                if let Some((when, op)) = map_when_op_trans(trigger_type) {
                    (when, op)
                } else {
                    continue;
                }
            };

            write_rpm_hook_file(
                install_dir,
                trigger_type,
                when_str,
                op_str,
                "Path",
                path,
                &scriptlet,
                Some(prio),
                hook_seq_counter,
            )?;
        }
    }

    Ok(())
}

/// Extract RPM file triggers (filetriggerin, filetriggerun, filetriggerpostun)
/// and transaction file triggers (transfiletriggerin, transfiletriggerun, transfiletriggerpostun)
/// These are triggered by file paths
fn extract_rpm_file_triggers<P: AsRef<Path>>(metadata: &rpm::PackageMetadata, store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let install_dir = store_tmp_dir.join("info/install");

    // File triggers configuration (simplified - only trigger types)
    let file_trigger_types = vec![
        "filetriggerin",
        "filetriggerun",
        "filetriggerpostun",
    ];

    // Transaction file triggers configuration (simplified - only trigger types)
    let trans_trigger_types = vec![
        "transfiletriggerin",
        "transfiletriggerun",
        "transfiletriggerpostun",
    ];

    // Extract file trigger paths and priorities once
    let file_trigger_paths = extract_string_array(metadata, IndexTag::RPMTAG_FILETRIGGERNAME);
    let file_trigger_priorities = extract_trigger_priorities(metadata, IndexTag::RPMTAG_FILETRIGGERPRIORITIES, RPMTRIGGER_DEFAULT_PRIORITY);

    // Extract transaction file trigger paths and priorities once
    let trans_trigger_paths = extract_string_array(metadata, IndexTag::RPMTAG_TRANSFILETRIGGERNAME);
    let trans_trigger_priorities = extract_trigger_priorities(metadata, IndexTag::RPMTAG_TRANSFILETRIGGERPRIORITIES, RPMTRIGGER_DEFAULT_PRIORITY);

    let mut hook_seq_counter = HashMap::new();

    // Process file triggers
    for trigger_type in file_trigger_types {
        process_file_trigger_type(
            metadata,
            &install_dir,
            trigger_type,
            &file_trigger_paths,
            &file_trigger_priorities,
            true, // use_priority_based_when
            &mut hook_seq_counter,
        )?;
    }

    // Process transaction file triggers
    for trigger_type in trans_trigger_types {
        process_file_trigger_type(
            metadata,
            &install_dir,
            trigger_type,
            &trans_trigger_paths,
            &trans_trigger_priorities,
            false, // use_priority_based_when
            &mut hook_seq_counter,
        )?;
    }

    Ok(())
}
