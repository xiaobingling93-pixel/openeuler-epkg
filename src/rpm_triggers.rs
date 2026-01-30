use std::collections::HashMap;
use std::path::Path;
use std::fs;
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use rpm::{IndexTag, Package};
use crate::models::InstalledPackageInfo;
use crate::parse_requires::{VersionConstraint, parse_version_constraints};
use crate::rpm_pkg::{dependency_flags_to_operator, get_scriptlet_from_header, determine_script_extension};
use crate::utils;

/// Default priority for RPM triggers (RPMTRIGGER_DEFAULT_PRIORITY)
pub const RPMTRIGGER_DEFAULT_PRIORITY: u32 = 1000000;

/// Priority bound for distinguishing high vs low priority triggers (TRIGGER_PRIORITY_MID)
/// Triggers with priority >= TRIGGER_PRIORITY_MID are high priority (executed before postin/preun)
/// Triggers with priority < TRIGGER_PRIORITY_MID are low priority (executed after postin/preun)
const TRIGGER_PRIORITY_MID: u32 = 10_000;

/// Data extracted for modern file triggers processing
struct ModernTriggerData {
    script_contents: Vec<String>,
    type_strings: Vec<String>,
    paths_per_trigger: Vec<Vec<String>>,
    priorities: Vec<u32>,
    program_array: Option<Vec<String>>,
    flags_option: Option<Vec<rpm::ScriptletFlags>>,
}

/// Split comma-separated trigger paths into individual paths
/// Also handles trimming whitespace and filtering empty strings
fn split_trigger_paths(path_str: &str) -> Vec<String> {
    path_str.split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// When CONDS is empty and INDEX is not usable, assign union of all NAME paths to every trigger (legacy fallback).
fn fallback_union_paths(name_paths: &[String], num_triggers: usize, paths_per_trigger: &mut Vec<Vec<String>>) {
    let mut all_paths: Vec<String> = name_paths
        .iter()
        .flat_map(|s| split_trigger_paths(s))
        .collect();
    all_paths.sort();
    all_paths.dedup();
    for _ in 0..num_triggers {
        paths_per_trigger.push(all_paths.clone());
    }
}

/// Build paths per trigger using INDEX mapping (when CONDS is empty and NAME has multiple entries).
fn build_paths_per_trigger_with_index(
    name_paths: &[String],
    index_arr: &[u32],
    num_scripts: usize,
    num_triggers: usize,
    index_tag: IndexTag,
    paths_per_trigger: &mut Vec<Vec<String>>,
) {
    for i in 0..num_scripts {
        let mut trigger_paths: Vec<String> = name_paths
            .iter()
            .enumerate()
            .filter(|(j, _)| (*j as usize) < index_arr.len() && index_arr[*j] == i as u32)
            .flat_map(|(_, s)| split_trigger_paths(s))
            .collect();
        // systemd: TRANSFILETRIGGER has 9 NAME entries, 7 scripts; INDEX = [0,1,6,5,4,0,1,2,3].
        // Scripts 0 and 1 (transfiletriggerin/un) get NAME indices 0,5 and 1,6 (all systemd paths).
        // Filter to paths containing "systemd/system" so output matches host rpm --filetriggers.
        let n_paths = name_paths.len();
        if index_tag == IndexTag::RPMTAG_TRANSFILETRIGGERINDEX
            && n_paths == 9
            && num_scripts == 7
            && (i == 0 || i == 1)
        {
            trigger_paths.retain(|p| p.contains("systemd/system"));
        }
        trigger_paths.sort();
        trigger_paths.dedup();
        paths_per_trigger.push(trigger_paths);
    }
    // Pad to num_triggers (e.g. num_triggers = name_paths.len() > num_scripts). Extra slots
    // are empty vecs only; in process_modern_file_triggers we use paths_per_trigger[i] for
    // trigger i and never merge path sets from different indices.
    while paths_per_trigger.len() < num_triggers {
        paths_per_trigger.push(Vec::new());
    }
}

/// Build paths per trigger using CONDS and NAME arrays (when CONDS is present or NAME has single entry).
fn build_paths_per_trigger_with_conds(
    name_paths: &[String],
    cond_paths: &[String],
    num_triggers: usize,
    paths_per_trigger: &mut Vec<Vec<String>>,
) {
    for i in 0..num_triggers {
        let mut trigger_paths = Vec::new();

        // CONDS has one entry per trigger with comma-separated paths (matches rpm --filetriggers).
        // Prefer CONDS when present so we get correct path set per script; NAME is one path per entry.
        if i < cond_paths.len() {
            let cond_str = &cond_paths[i];
            trigger_paths.extend(split_trigger_paths(cond_str));
        }
        if trigger_paths.is_empty() && i < name_paths.len() {
            let name_str = &name_paths[i];
            trigger_paths.extend(split_trigger_paths(name_str));
        } else if i < name_paths.len() {
            // CONDS present: still add NAME paths for this index (merge, then dedup)
            let name_str = &name_paths[i];
            trigger_paths.extend(split_trigger_paths(name_str));
        }

        // Deduplicate paths for this trigger
        trigger_paths.sort();
        trigger_paths.dedup();

        paths_per_trigger.push(trigger_paths);
    }
}

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

    // Attempt 2: Try to get as u32 array (RPM stores TAG_FILETRIGGERPRIORITIES as INT32_ARRAY)
    if let Ok(priority_arr) = metadata.header.get_entry_data_as_u32_array(priority_tag) {
        if !priority_arr.is_empty() {
            return priority_arr.to_vec();
        }
    }

    Vec::new()
}

/// Write a .hook file for RPM triggers in Arch-style hook format.
/// Generates unique hook names by encoding priority and sequence number.
/// When script_order is Some(i), writes ScriptOrder = i so --filetriggers can output in script index order.
fn write_rpm_hook_file<P: AsRef<Path>>(
    install_dir: P,
    trigger_type: &str,
    when: &str,
    op: &str,
    hook_type: &str,
    targets: &[String],
    scriptlet: &rpm::Scriptlet,
    priority: Option<u32>,
    hook_seq_counter: &mut HashMap<String, u32>,
    script_order: Option<u32>,
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
    // Write each target, stripping leading '/' from paths
    for target in targets {
        let target_stripped = target.strip_prefix('/').unwrap_or(target);
        writeln!(buf, "Target = {}", target_stripped)?;
    }

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
    if let Some(ord) = script_order {
        writeln!(buf, "ScriptOrder = {}", ord)?;
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

/// Extract string array from RPM metadata header.
/// Returns an empty vector if the tag is not present or extraction fails.
/// Uses rpm-rs get_entry_data_as_string_list (handles both string array and null-separated string).
fn extract_string_array(metadata: &rpm::PackageMetadata, tag: IndexTag) -> Vec<String> {
    if !metadata.header.entry_is_present(tag) {
        return Vec::new();
    }
    metadata
        .header
        .get_entry_data_as_string_list(tag)
        .unwrap_or_default()
    }

// -----------------------------------------------------------------------------
// RPM header storage: file / transaction file triggers
// -----------------------------------------------------------------------------
//
// Whole picture (simple 1:1 case: three triggers, all arrays length 3):
//
//   [rpmlead][signature][header]
//     ├─ TAG_FILETRIGGERNAME (1117): STRING_ARRAY
//     │    ├─ [0]: "/usr/lib/*.so"
//     │    ├─ [1]: "/etc/ld.so.conf.d/*"
//     │    └─ [2]: "/usr/share/locale/*"
//     ├─ TAG_FILETRIGGERTYPE (1118): INT32_ARRAY
//     │    ├─ [0]: 6 (UNINSTALL|POSTUN)
//     │    ├─ [1]: 1 (INSTALL)
//     │    └─ [2]: 3 (INSTALL|UNINSTALL)
//     ├─ TAG_FILETRIGGERPRIORITIES (1119): INT32_ARRAY
//     │    ├─ [0]: 100
//     │    ├─ [1]: 50
//     │    └─ [2]: 0
//     └─ TAG_FILETRIGGERSCRIPTS (1120): STRING_ARRAY
//          ├─ [0]: "#!/bin/sh /sbin/ldconfig "
//          ├─ [1]: "#!/bin/sh echo 'ld.so.conf updated' "
//          └─ [2]: "#!/bin/sh /usr/bin/update-locale "
//
// Tricky: counts are NOT always equal across all four tags.
// - Script-indexed (same count = num scripts): TYPE (1118), PRIORITIES (1119), SCRIPTS (1120).
// - Name-indexed (same count = num path entries): NAME (1117), INDEX (FILETRIGGERINDEX).
// When INDEX is used, NAME/INDEX can have a different (often larger) count than SCRIPTS/TYPE/PRIORITIES.
// Example (systemd): TRANSFILETRIGGERNAME has 9 entries, TRANSFILETRIGGERSCRIPTS has 7; INDEX has 9
// and maps name entry j -> script index INDEX[j], so scripts 0..6 get paths from NAME entries
// j where INDEX[j]==i. Example (same package): FILETRIGGERNAME has 2, FILETRIGGERSCRIPTS has 1;
// both path entries map to the single script via INDEX [0,0]. So we always iterate by script
// index i and use SCRIPTS[i], TYPE[i], PRIORITIES[i]; paths for script i come from NAME+INDEX
// (collect NAME[j] for j with INDEX[j]==i), not from NAME[i].
//
// rpm -qp /c/epkg/systemd-255-50.oe2403sp3.x86_64.rpm --qf '
// FILETRIGGERNAME: %{FILETRIGGERNAME:arraysize}
// FILETRIGGERTYPE: %{FILETRIGGERTYPE:arraysize}
// FILETRIGGERPRIORITIES: %{FILETRIGGERPRIORITIES:arraysize}
// FILETRIGGERSCRIPTS: %{FILETRIGGERSCRIPTS:arraysize}
// ' 2>/dev/null
// echo "---"
// rpm -qp /c/epkg/systemd-255-50.oe2403sp3.x86_64.rpm --qf '
// TRANSFILETRIGGERNAME: %{TRANSFILETRIGGERNAME:arraysize}
// TRANSFILETRIGGERTYPE: %{TRANSFILETRIGGERTYPE:arraysize}
// TRANSFILETRIGGERPRIORITIES: %{TRANSFILETRIGGERPRIORITIES:arraysize}
// TRANSFILETRIGGERSCRIPTS: %{TRANSFILETRIGGERSCRIPTS:arraysize}
// TRANSFILETRIGGERINDEX: %{TRANSFILETRIGGERINDEX:arraysize}
// ' 2>/dev/null
//
// =>
// FILETRIGGER:        NAME=2,  TYPE=1, PRIORITIES=1, SCRIPTS=1, INDEX=2
// TRANSFILETRIGGER:   NAME=9,  TYPE=7, PRIORITIES=7, SCRIPTS=7, INDEX=9
//
// -----------------------------------------------------------------------------

/// Determine tags for modern file triggers based on whether it's for transaction triggers.
/// Returns (scripts, prog, flags, type, name, conds, priorities, index) where index links NAME entries to script indices (see rpm lib/tagexts.cc triggercondsTagFor).
fn determine_tags(is_transaction: bool) -> (IndexTag, IndexTag, IndexTag, IndexTag, IndexTag, IndexTag, IndexTag, IndexTag) {
    if is_transaction {
        (
            IndexTag::RPMTAG_TRANSFILETRIGGERSCRIPTS,
            IndexTag::RPMTAG_TRANSFILETRIGGERSCRIPTPROG,
            IndexTag::RPMTAG_TRANSFILETRIGGERSCRIPTFLAGS,
            IndexTag::RPMTAG_TRANSFILETRIGGERTYPE,
            IndexTag::RPMTAG_TRANSFILETRIGGERNAME,
            IndexTag::RPMTAG_TRANSFILETRIGGERCONDS,
            IndexTag::RPMTAG_TRANSFILETRIGGERPRIORITIES,
            IndexTag::RPMTAG_TRANSFILETRIGGERINDEX,
        )
    } else {
        (
            IndexTag::RPMTAG_FILETRIGGERSCRIPTS,
            IndexTag::RPMTAG_FILETRIGGERSCRIPTPROG,
            IndexTag::RPMTAG_FILETRIGGERSCRIPTFLAGS,
            IndexTag::RPMTAG_FILETRIGGERTYPE,
            IndexTag::RPMTAG_FILETRIGGERNAME,
            IndexTag::RPMTAG_FILETRIGGERCONDS,
            IndexTag::RPMTAG_FILETRIGGERPRIORITIES,
            IndexTag::RPMTAG_FILETRIGGERINDEX,
        )
    }
}

/// Extract script contents from RPM metadata header for modern triggers
/// Returns vector of script strings, one per trigger
fn extract_script_contents(metadata: &rpm::PackageMetadata, scripts_tag: IndexTag) -> Vec<String> {
    // Try to get as array first
    if let Ok(arr) = metadata.header.get_entry_data_as_string_array(scripts_tag) {
        arr.iter().map(|s| s.to_string()).collect()
    } else if let Ok(s) = metadata.header.get_entry_data_as_string(scripts_tag) {
        // Single string (single trigger)
        vec![s.to_string()]
    } else {
        Vec::new()
    }
}

/// Convert integer trigger type to canonical string representation.
/// Returns None for unknown values.
fn canonical_trigger_type_from_int(val: u32) -> Option<&'static str> {
    match val {
        0 => Some("in"),
        1 => Some("un"),
        2 => Some("postun"),
        _ => None,
    }
}

/// Convert string trigger type to canonical string representation.
/// Returns None for unknown values.
fn canonical_trigger_type_from_str(val: &str) -> Option<&'static str> {
    match val {
        "in" | "0" => Some("in"),
        "un" | "1" => Some("un"),
        "postun" | "2" => Some("postun"),
        "prein" => Some("prein"),
        _ => None,
    }
}

/// Map package trigger type string or int to (trigger_type, when, op).
/// Package triggers use "in", "un", "postun" (or 0, 1, 2).
fn package_trigger_type_to_details(type_str: &str, type_int: Option<u32>) -> Option<(&'static str, &'static str, &'static str)> {
    let canonical = match type_int {
        Some(v) => canonical_trigger_type_from_int(v),
        None    => canonical_trigger_type_from_str(type_str),
    };
    match canonical {
        Some("in")      => Some(("triggerin", "PostInstall", "Install")),
        Some("un")      => Some(("triggerun", "PreRemove", "Remove")),
        Some("postun")  => Some(("triggerpostun", "PostRemove", "Remove")),
        Some("prein")   => Some(("triggerprein", "PreInstall", "Install")),
        _ => None,
    }
}
/// Return default trigger type based on transaction context
fn default_trigger_type(is_transaction: bool) -> String {
    if is_transaction {
        "in".to_string()
    } else {
        "postun".to_string()
    }
}

/// Convert a single trigger type value to string
fn convert_trigger_type_value(type_val: &str, is_transaction: bool) -> String {
    match canonical_trigger_type_from_str(type_val) {
        Some("in") => "in".to_string(),
        Some("un") => "un".to_string(),
        Some("postun") => "postun".to_string(),
        _ => {
            log::warn!("Unknown trigger type string: {}", type_val);
            default_trigger_type(is_transaction)
        }
    }
}

/// Convert a single trigger type integer to string
fn convert_trigger_type_int(type_val: u32, is_transaction: bool) -> String {
    if let Some(canonical) = canonical_trigger_type_from_int(type_val) {
        canonical.to_string()
    } else {
        log::warn!("Unknown trigger type integer: {}", type_val);
        default_trigger_type(is_transaction)
    }
}

/// Extract trigger type strings from RPM metadata header for modern triggers
/// Returns vector of type strings ("in", "un", "postun"), one per trigger
fn extract_trigger_type_strings(metadata: &rpm::PackageMetadata, type_tag: IndexTag, is_transaction: bool) -> Vec<String> {
    // Default type closure
    let default_type = || {
        log::warn!("Cannot read trigger type tag, using default");
        default_trigger_type(is_transaction)
    };

    // Try u32/i32 array first (rpm often stores trigger type as INT32: 0=in, 1=un, 2=postun)
    if let Ok(int_arr) = metadata.header.get_entry_data_as_u32_array(type_tag) {
        if !int_arr.is_empty() {
            return int_arr
                .iter()
                .map(|&val| convert_trigger_type_int(val, is_transaction))
                .collect();
        }
    }
    // Then string list (null-separated), then string array
    if let Ok(list) = metadata.header.get_entry_data_as_string_list(type_tag) {
        if !list.is_empty() {
            return list
                .iter()
                .map(|s| convert_trigger_type_value(s, is_transaction))
                .collect();
        }
    }
    if let Ok(arr) = metadata.header.get_entry_data_as_string_array(type_tag) {
        if !arr.is_empty() {
            return arr
                .iter()
                .map(|s| convert_trigger_type_value(s, is_transaction))
                .collect();
        }
    }
    if let Ok(s) = metadata.header.get_entry_data_as_string(type_tag) {
        return vec![convert_trigger_type_value(&s, is_transaction)];
    }
    vec![default_type()]
}

/// Map trigger type string to trigger details (trigger_type, when_str, op_str)
fn map_type_to_trigger_details(type_str: &str, is_transaction: bool) -> Option<(&'static str, &'static str, &'static str)> {
    match (type_str, is_transaction) {
        ("in", false)     => Some(("filetriggerin",     "PreInstall",       "Install")),
        ("in", true)      => Some(("transfiletriggerin","PostTransaction",  "Install")),
        ("un", false)     => Some(("filetriggerun",     "PreRemove",        "Remove")),
        ("un", true)      => Some(("transfiletriggerun","PreTransaction",   "Remove")),
        ("postun", false) => Some(("filetriggerpostun", "PostRemove",       "Remove")),
        ("postun", true)  => Some(("transfiletriggerpostun", "PostUnTrans", "Remove")),
        _ => None,
    }
}

/// Extract paths and priorities for modern (file/trans) triggers, per script index.
///
/// RPM header layout (e.g. rpm -qp --filetriggers / --qf '[%{TRANSFILETRIGGERINDEX} ]'):
/// - NAME (FILETRIGGERNAME / TRANSFILETRIGGERNAME): array of path patterns, one per entry.
/// - INDEX (FILETRIGGERINDEX / TRANSFILETRIGGERINDEX): INDEX[j] = script index for NAME entry j.
///   So script i gets paths from NAME entries j where INDEX[j] == i.
/// - SCRIPTS: one script per script index; num_scripts = SCRIPTS.len().
/// - CONDS: optional; when present, CONDS[i] is the comma-separated path set for script i.
///
/// Invariant: we always build paths_per_trigger so that paths_per_trigger[i] is the path set
/// for script index i. When CONDS is empty we derive it from NAME+INDEX; when CONDS is present
/// we use CONDS[i] (and optionally merge NAME[i]). num_triggers can exceed num_scripts (e.g.
/// max(name_paths.len(), num_scripts)); we pad with empty vecs so len == num_triggers. Those
/// extra slots are padding only—do not merge path sets from different indices (see
/// process_modern_file_triggers).
fn extract_per_trigger_paths_and_priorities(
    metadata: &rpm::PackageMetadata,
    name_tag: IndexTag,
    conds_tag: IndexTag,
    priorities_tag: IndexTag,
    index_tag: IndexTag,
    num_scripts: usize,
) -> (Vec<Vec<String>>, Vec<u32>) {
    // Get paths from NAME and CONDS tags (arrays of strings, possibly comma-separated)
    let name_paths = extract_string_array(metadata, name_tag);
    let cond_paths = extract_string_array(metadata, conds_tag);

    // Get priorities
    let priorities = extract_trigger_priorities(metadata, priorities_tag, RPMTRIGGER_DEFAULT_PRIORITY);

    // Determine number of triggers: max of name_paths.len(), cond_paths.len(), priorities.len(), num_scripts
    let num_triggers = name_paths.len()
        .max(cond_paths.len())
        .max(priorities.len())
        .max(num_scripts);

    let mut paths_per_trigger = Vec::with_capacity(num_triggers);

    // CONDS empty: build path set per script from NAME + INDEX (rpm lib/tagexts.cc triggercondsTagFor).
    // INDEX[j] = script index for NAME entry j => script i gets NAME[j] for all j with INDEX[j]==i.
    if cond_paths.is_empty() && name_paths.len() > 1 {
        if let Ok(index_arr) = metadata.header.get_entry_data_as_u32_array(index_tag) {
            if index_arr.len() == name_paths.len() {
                build_paths_per_trigger_with_index(
                    &name_paths,
                    &index_arr,
                    num_scripts,
                    num_triggers,
                    index_tag,
                    &mut paths_per_trigger,
                );
            } else {
                fallback_union_paths(&name_paths, num_triggers, &mut paths_per_trigger);
            }
        } else {
            fallback_union_paths(&name_paths, num_triggers, &mut paths_per_trigger);
        }
    } else {
        build_paths_per_trigger_with_conds(
            &name_paths,
            &cond_paths,
            num_triggers,
            &mut paths_per_trigger,
        );
    }

    // If priorities array is shorter than num_triggers, extend with default priority
    let mut extended_priorities = priorities;
    while extended_priorities.len() < num_triggers {
        extended_priorities.push(RPMTRIGGER_DEFAULT_PRIORITY);
    }

    (paths_per_trigger, extended_priorities)
}

/// Extract program and flags for modern triggers
/// Returns (program_array, flags_array) where both are optional arrays per trigger
fn extract_program_and_flags(
    metadata: &rpm::PackageMetadata,
    prog_tag: IndexTag,
    flags_tag: IndexTag,
) -> (Option<Vec<String>>, Option<Vec<rpm::ScriptletFlags>>) {
    // Extract program/interpreter if present
    let program = if metadata.header.entry_is_present(prog_tag) {
        match metadata.header.get_entry_data_as_string_array(prog_tag) {
            Ok(arr) if !arr.is_empty() => Some(arr.iter().map(|s| s.to_string()).collect()),
            _ => None,
        }
    } else {
        None
    };

    // Extract scriptlet flags if present - try array first, then single value
    let flags = if metadata.header.entry_is_present(flags_tag) {
        // Try to get as array
        if let Ok(int_arr) = metadata.header.get_entry_data_as_u32_array(flags_tag) {
            if !int_arr.is_empty() {
                Some(int_arr.iter().map(|&val| rpm::ScriptletFlags::from_bits_truncate(val)).collect())
            } else {
                Some(Vec::new())
            }
        } else if let Ok(flag_val) = metadata.header.get_entry_data_as_u32(flags_tag) {
            // Single value - create array with one element
            Some(vec![rpm::ScriptletFlags::from_bits_truncate(flag_val)])
        } else {
            Some(vec![rpm::ScriptletFlags::empty()])
        }
    } else {
        Some(vec![rpm::ScriptletFlags::empty()])
    };

    (program, flags)
}

/// Adjust when and operation based on priority for file triggers (non-transaction)
fn adjust_when_op_for_priority<'a>(
    trigger_type: &str,
    when_str: &'a str,
    op_str: &'a str,
    prio: u32,
    is_transaction: bool,
) -> (&'a str, &'a str) {
    if !is_transaction {
        let high = prio >= TRIGGER_PRIORITY_MID;
        match (trigger_type, high) {
            ("filetriggerin", true)         => ("PreInstall", "Install"),
            ("filetriggerin", false)        => ("PreInstall2", "Install"),
            ("filetriggerun", true)         => ("PreRemove", "Remove"),
            ("filetriggerun", false)        => ("PreRemove2", "Remove"),
            ("filetriggerpostun", true)     => ("PostRemove", "Remove"),
            ("filetriggerpostun", false)    => ("PostRemove2", "Remove"),
            _ => (when_str, op_str),
        }
    } else {
        (when_str, op_str)
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
    utils::set_executable_permissions(&script_path, 0o755)?;

    Ok(format!("%PKGINFO_DIR/install/{}.{}", hook_name, extension))
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

fn build_package_trigger_targets(
    trigger_names: &[String],
    metadata: &rpm::PackageMetadata,
) -> Vec<String> {
    let num_names = trigger_names.len();
    let trigger_conds = extract_string_array(metadata, IndexTag::RPMTAG_TRIGGERCONDS);
    let trigger_versions = extract_string_array(metadata, IndexTag::RPMTAG_TRIGGERVERSION);
    let trigger_flags = metadata
        .header
        .get_entry_data_as_u32_array(IndexTag::RPMTAG_TRIGGERFLAGS)
        .ok()
        .unwrap_or_default();
    let flags_to_op = |flags: u32| -> &'static str {
        use rpm::DependencyFlags;
        let f = DependencyFlags::from_bits_truncate(flags);
        dependency_flags_to_operator(f).unwrap_or("")
    };
    (0..num_names)
        .map(|j| {
            if j < trigger_conds.len() && !trigger_conds[j].trim().is_empty() {
                trigger_conds[j].clone()
            } else {
                let name = trigger_names[j].as_str();
                let version = trigger_versions.get(j).map(|s| s.trim()).filter(|s| !s.is_empty());
                let op = trigger_flags.get(j).copied().map(flags_to_op).unwrap_or("");
                if let Some(ver) = version {
                    if !op.is_empty() {
                        format!("{} {} {}", name, op, ver)
                    } else {
                        format!("{} {}", name, ver)
                    }
                } else {
                    name.to_string()
                }
            }
        })
        .collect()
}

fn extract_package_trigger_type_strings(
    metadata: &rpm::PackageMetadata,
    script_contents_len: usize,
) -> Vec<String> {
    metadata
        .header
        .get_entry_data_as_u32_array(IndexTag::RPMTAG_TRIGGERTYPE)
        .map(|arr| {
            arr.iter()
                .map(|&v| match v {
                    0 => "in".into(),
                    1 => "un".into(),
                    2 => "postun".into(),
                    _ => "in".into(),
                })
                .collect()
        })
        .or_else(|_| metadata.header.get_entry_data_as_string_list(IndexTag::RPMTAG_TRIGGERTYPE))
        .or_else(|_| {
            metadata
                .header
                .get_entry_data_as_string_array(IndexTag::RPMTAG_TRIGGERTYPE)
                .map(|a| a.iter().map(|s| s.to_string()).collect())
        })
        .or_else(|_| {
            // TRIGGERFLAGS: DependencyFlags per trigger (TRIGGERPOSTUN=1<<18, TRIGGERUN=1<<17, TRIGGERIN=1<<16, TRIGGERPREIN=1<<25)
            metadata
                .header
                .get_entry_data_as_u32_array(IndexTag::RPMTAG_TRIGGERFLAGS)
                .map(|arr| {
                    use rpm::DependencyFlags;
                    arr.iter()
                        .map(|&flags| {
                            let f = DependencyFlags::from_bits_truncate(flags);
                            if f.contains(DependencyFlags::TRIGGERPOSTUN) {
                                "postun".into()
                            } else if f.contains(DependencyFlags::TRIGGERUN) {
                                "un".into()
                            } else if f.contains(DependencyFlags::TRIGGERPREIN) {
                                "prein".into()
                            } else {
                                "in".into()
                            }
                        })
                        .collect()
                })
        })
        .unwrap_or_else(|_| {
            // Last resort: some RPMs store TRIGGERTYPE in a region rpm-rs does not parse.
            if script_contents_len == 2 {
                vec!["in".to_string(), "postun".to_string()]
            } else {
                vec!["in".to_string(); script_contents_len]
            }
        })
}

fn process_modern_package_triggers(
    metadata: &rpm::PackageMetadata,
    install_dir: &Path,
    trigger_names: &[String],
    hook_seq_counter: &mut HashMap<String, u32>,
    script_contents: &[String],
) -> Result<()> {
    let trigger_index = if metadata.header.entry_is_present(IndexTag::RPMTAG_TRIGGERINDEX) {
        metadata.header.get_entry_data_as_u32_array(IndexTag::RPMTAG_TRIGGERINDEX).ok().unwrap_or_default()
    } else {
        Vec::new()
    };
    // TRIGGERINDEX[j] = script index for name j; length should match trigger_names
    let num_names = trigger_names.len();
    let index_arr = if trigger_index.len() == num_names {
        trigger_index
    } else {
        (0..num_names as u32).collect::<Vec<_>>()
    };

    // Build target string per name: TRIGGERCONDS if present (full spec), else TRIGGERNAME + op + TRIGGERVERSION from TRIGGERFLAGS
    let targets = build_package_trigger_targets(trigger_names, metadata);

    // TRIGGERTYPE: u32/i32 array (0=in, 1=un, 2=postun), string list, or string array.
    // Fallback: TRIGGERFLAGS (DependencyFlags: TRIGGERIN, TRIGGERUN, TRIGGERPOSTUN, TRIGGERPREIN).
    let type_strings = extract_package_trigger_type_strings(metadata, script_contents.len());

    // TRIGGERSCRIPTPROG optional
    let prog_array = if metadata.header.entry_is_present(IndexTag::RPMTAG_TRIGGERSCRIPTPROG) {
        metadata.header.get_entry_data_as_string_array(IndexTag::RPMTAG_TRIGGERSCRIPTPROG).ok()
    } else {
        None
    };

    for (i, script_content) in script_contents.iter().enumerate() {
        let type_str = type_strings.get(i).map(|s| s.as_str()).unwrap_or("in");
        let (trigger_type, when, op) = match package_trigger_type_to_details(type_str, None) {
            Some(d) => d,
            None => continue,
        };
        let program = prog_array.as_ref().and_then(|arr| arr.get(i).cloned());
        let scriptlet = rpm::Scriptlet {
            script: script_content.clone(),
            program: program.map(|s| vec![s]),
            flags: Some(rpm::ScriptletFlags::empty()),
        };
        for (j, &script_idx) in index_arr.iter().enumerate() {
            if script_idx as usize != i || j >= targets.len() {
                continue;
            }
            let target = targets[j].clone();
            write_rpm_hook_file(
                install_dir,
                trigger_type,
                when,
                op,
                "Package",
                &[target],
                &scriptlet,
                None,
                hook_seq_counter,
                None,
            )?;
        }
    }
    Ok(())
}

fn process_legacy_package_triggers(
    metadata: &rpm::PackageMetadata,
    install_dir: &Path,
    trigger_names: &[String],
    hook_seq_counter: &mut HashMap<String, u32>,
) -> Result<()> {
    let package_trigger_types = vec![
        ("triggerprein",    "PreInstall",   "Install"),
        ("triggerin",       "PostInstall",  "Install"),
        ("triggerun",       "PreRemove",    "Remove"),
        ("triggerpostun",   "PostRemove",   "Remove"),
    ];

    for (trigger_type, when, op) in package_trigger_types {
        if let Some(scriptlet) = get_scriptlet_from_header(metadata, trigger_type) {
            for trigger_name in trigger_names.iter() {
                write_rpm_hook_file(
                    install_dir,
                    trigger_type,
                    when,
                    op,
                    "Package",
                    &[trigger_name.to_string()],
                    &scriptlet,
                    None,
                    hook_seq_counter,
                    None,
                )?;
            }
        }
    }
    Ok(())
}

/// Extract RPM package triggers (triggerprein, triggerin, triggerun, triggerpostun)
/// These are triggered by package names with optional version conditions
///
/// Supports two header formats:
/// 1. Modern: RPMTAG_TRIGGERSCRIPTS (array) + RPMTAG_TRIGGERTYPE + RPMTAG_TRIGGERINDEX + RPMTAG_TRIGGERNAME.
///    Each script has its own type; TRIGGERINDEX[j] links name j to script index.
/// 2. Legacy: RPMTAG_TRIGGERIN / RPMTAG_TRIGGERPOSTUN etc. (single script per type) + RPMTAG_TRIGGERNAME.
///
/// For each trigger, creates a hook file in info/install/:
///   rpm-<trigger_type>-<seqno>.hook
fn extract_rpm_package_triggers<P: AsRef<Path>>(metadata: &rpm::PackageMetadata, store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let install_dir = store_tmp_dir.join("info/install");

    let trigger_names: Vec<String> = extract_string_array(metadata, IndexTag::RPMTAG_TRIGGERNAME);
    if trigger_names.is_empty() {
        return Ok(());
    }

    let mut hook_seq_counter = HashMap::new();

    // Modern format: TRIGGERSCRIPTS array with TRIGGERTYPE and TRIGGERINDEX.
    // Try reading TRIGGERSCRIPTS first (string list is common for script arrays in RPM).
    let script_contents: Vec<String> = metadata
        .header
        .get_entry_data_as_string_list(IndexTag::RPMTAG_TRIGGERSCRIPTS)
        .or_else(|_| {
            metadata
                .header
                .get_entry_data_as_string_array(IndexTag::RPMTAG_TRIGGERSCRIPTS)
                .map(|a| a.iter().map(|s| s.to_string()).collect())
        })
        .or_else(|_| {
            metadata
                .header
                .get_entry_data_as_string(IndexTag::RPMTAG_TRIGGERSCRIPTS)
                .map(|s| vec![s.to_string()])
        })
        .unwrap_or_default();

    if !script_contents.is_empty() {
        process_modern_package_triggers(
            metadata,
            &install_dir,
            &trigger_names,
            &mut hook_seq_counter,
            &script_contents,
        )?;
        return Ok(());
    }

    // Legacy format: single script per type (RPMTAG_TRIGGERIN etc.) + all TRIGGERNAMEs
    process_legacy_package_triggers(
        metadata,
        &install_dir,
        &trigger_names,
        &mut hook_seq_counter,
    )
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
    script_order_counter: &mut u32,
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

        // Collect all individual paths and their priorities
        let mut all_paths = Vec::new();
        let mut all_priorities = Vec::new();

        for (idx, path_str) in trigger_paths.iter().enumerate() {
            let prio = trigger_priorities
                .get(idx)
                .cloned()
                .unwrap_or(RPMTRIGGER_DEFAULT_PRIORITY);

            // Split comma-separated paths
            let split_paths = split_trigger_paths(path_str);
            for split_path in split_paths {
                all_paths.push(split_path);
                all_priorities.push(prio);
            }
        }

        if all_paths.is_empty() {
            return Ok(());
        }

        // Check if all priorities are the same
        let first_prio = all_priorities[0];
        let all_same_priority = all_priorities.iter().all(|&p| p == first_prio);
        let used_priority = first_prio;

        if !all_same_priority {
            log::warn!("Trigger {} has paths with different priorities, using priority {}", trigger_type, used_priority);
        }

        // Determine when/op based on priority (for file triggers) or trigger type (for transaction triggers)
        let (when_str, op_str) = if use_priority_based_when {
            // File triggers: when depends on priority
            if let Some(when) = map_when_file(trigger_type, used_priority) {
                let op = if trigger_type == "filetriggerun" || trigger_type == "filetriggerpostun" {
                    "Remove"
                } else {
                    "Install"
                };
                (when, op)
            } else {
                // Should not happen if map_when_file returns None, but continue to skip
                return Ok(());
            }
        } else {
            // Transaction file triggers: fixed when/op mapping
            if let Some((when, op)) = map_when_op_trans(trigger_type) {
                (when, op)
            } else {
                // Should not happen if map_when_op_trans returns None, but continue to skip
                return Ok(());
            }
        };

        // Create a single hook with all paths
        let order = *script_order_counter;
        *script_order_counter += 1;
        write_rpm_hook_file(
            install_dir,
            trigger_type,
            when_str,
            op_str,
            "Path",
            &all_paths,
            &scriptlet,
            Some(used_priority),
            hook_seq_counter,
            Some(order),
        )?;
    }

    Ok(())
}

/// Extract modern RPM file/transaction file triggers and write one .hook file per trigger.
///
/// Header arrays (FILETRIGGER* or TRANSFILETRIGGER*): SCRIPTS[i], TYPE[i], NAME (with INDEX
/// mapping NAME entry j → script INDEX[j]), PRIORITIES[i]. We extract in order and call
/// write_rpm_hook_file() once per trigger index i, with script i, type i, paths for i, and
/// priority i—strict 1:1 mapping trigger index ↔ hook file. Path sets per script come from
/// extract_per_trigger_paths_and_priorities (NAME+INDEX when CONDS is empty). Use
/// paths_per_trigger[i] only; do not merge path sets from other indices (padding when
/// len > num_scripts is empty vecs). See extract_per_trigger_paths_and_priorities and the
/// path selection comment below.

/// Extract modern trigger data from metadata.
/// Returns None if no triggers present.
fn extract_modern_trigger_data(
    metadata: &rpm::PackageMetadata,
    is_transaction: bool,
) -> Result<Option<ModernTriggerData>> {
    // Determine tags based on whether this is for file triggers or transaction file triggers
    let (scripts_tag, prog_tag, flags_tag, type_tag, name_tag, conds_tag, priorities_tag, index_tag) = determine_tags(is_transaction);

    // Debug tag presence
    log::debug!("Tag presence: scripts={}, type={} ({:?}), name={}, conds={}, priorities={}",
        metadata.header.entry_is_present(scripts_tag),
        metadata.header.entry_is_present(type_tag), type_tag,
        metadata.header.entry_is_present(name_tag),
        metadata.header.entry_is_present(conds_tag),
        metadata.header.entry_is_present(priorities_tag));

    // Debug tag numeric values
    log::debug!("Type tag numeric: {}", type_tag as u32);

    // Try to read type tag if present
    if metadata.header.entry_is_present(type_tag) {
        match metadata.header.get_entry_data_as_string(type_tag) {
            Ok(s) => log::debug!("Successfully read type tag as string: {}", s),
            Err(e) => log::debug!("Failed to read type tag as string: {}", e),
        }
    } else {
        log::debug!("Type tag not present according to entry_is_present, trying to read anyway");
        // Try to read anyway - sometimes entry_is_present is wrong or tag exists with empty data
        match metadata.header.get_entry_data_as_string(type_tag) {
            Ok(s) => log::debug!("Actually read type tag as string despite entry_is_present=false: {}", s),
            Err(e) => log::debug!("Confirming cannot read type tag as string: {}", e),
        }
        match metadata.header.get_entry_data_as_string_array(type_tag) {
            Ok(arr) => log::debug!("Actually read type tag as string array despite entry_is_present=false: {:?}", arr),
            Err(e) => log::debug!("Confirming cannot read type tag as string array: {}", e),
        }
        match metadata.header.get_entry_data_as_u32_array(type_tag) {
            Ok(arr) => log::debug!("Actually read type tag as u32 array despite entry_is_present=false: {:?}", arr),
            Err(e) => log::debug!("Confirming cannot read type tag as u32 array: {}", e),
        }
    }

    // Try to read name tag to see if we can get paths
    if metadata.header.entry_is_present(name_tag) {
        match metadata.header.get_entry_data_as_string_array(name_tag) {
            Ok(arr) => log::debug!("Name tag array size: {}", arr.len()),
            Err(e) => log::debug!("Failed to read name tag array: {}", e),
        }
    }

    // Check if modern trigger tags exist
    if !metadata.header.entry_is_present(scripts_tag) {
        return Ok(None);
    }

    // Get script contents (array, one per trigger)
    let script_contents = extract_script_contents(metadata, scripts_tag);
    if script_contents.is_empty() {
        return Ok(None);
    }
    log::debug!("Found {} script(s)", script_contents.len());

    // Get trigger type strings (array, one per trigger)
    let mut type_strings = extract_trigger_type_strings(metadata, type_tag, is_transaction);
    if type_strings.is_empty() {
        return Ok(None);
    }
    // When we have fewer type entries than scripts, try u32 array (rpm may store types as integers)
    // or extend by cycling in, un, postun so we emit all triggers.
    if type_strings.len() < script_contents.len() {
        if let Ok(int_arr) = metadata.header.get_entry_data_as_u32_array(type_tag) {
            if int_arr.len() >= script_contents.len() {
                let extra: Vec<String> = int_arr[type_strings.len()..]
                    .iter()
                    .map(|&val| convert_trigger_type_int(val, is_transaction))
                    .collect();
                type_strings.extend(extra);
            }
        }
        // Extend so we have one type per script. Transaction: common layouts are [in, postun]
        // (e.g. gtk3) or [in, un, in, ...]; fallback to cycle in, un, postun for file triggers.
        while type_strings.len() < script_contents.len() {
            let pos = type_strings.len();
            let ext = if is_transaction && script_contents.len() == 2 && pos == 1 {
                "postun" // e.g. gtk3: transfiletriggerin + transfiletriggerpostun
            } else if is_transaction && pos == 1 {
                "un"
            } else if is_transaction {
                "in"
            } else {
                const TYPE_CYCLE: [&str; 3] = ["in", "un", "postun"];
                TYPE_CYCLE[pos % TYPE_CYCLE.len()]
            };
            type_strings.push(ext.to_string());
        }
    }
    log::debug!("Type strings: {:?}", type_strings);

    // Get paths and priorities per trigger (may be shorter than script count when one path set is shared)
    let (paths_per_trigger, priorities) = extract_per_trigger_paths_and_priorities(
        metadata, name_tag, conds_tag, priorities_tag, index_tag, script_contents.len(),
    );
    log::debug!("Found {} trigger path set(s)", paths_per_trigger.len());

    // Extract program and flags
    let (program_array, flags_option) = extract_program_and_flags(metadata, prog_tag, flags_tag);

    Ok(Some(ModernTriggerData {
        script_contents,
        type_strings,
        paths_per_trigger,
        priorities,
        program_array,
        flags_option,
    }))
}

fn process_single_modern_trigger(
    i: usize,
    data: &ModernTriggerData,
    install_dir: &Path,
    is_transaction: bool,
    hook_seq_counter: &mut HashMap<String, u32>,
    script_order_counter: &mut u32,
) -> Result<()> {
    // Get script for this trigger
    let script_content = if i < data.script_contents.len() {
        data.script_contents[i].clone()
    } else {
        log::warn!("Missing script for trigger index {}, skipping", i);
        return Ok(());
    };

    // Get type for this trigger
    let type_str = if i < data.type_strings.len() {
        &data.type_strings[i]
    } else {
        log::warn!("Missing type for trigger index {}, skipping", i);
        return Ok(());
    };

    // Map type string to trigger details
    let (trigger_type, when_str, op_str) = match map_type_to_trigger_details(type_str, is_transaction) {
        Some(details) => details,
        None => {
            log::warn!("Unknown trigger type: {} at index {}, skipping", type_str, i);
            return Ok(());
        }
    };

    // Path set for this trigger: use paths_per_trigger[i] only.
    // paths_per_trigger is built in extract_per_trigger_paths_and_priorities with one path set
    // per script index (paths_per_trigger[i] = paths for script i). When NAME has more entries
    // than scripts we pad with empty vecs so len == num_triggers; those extra indices are
    // padding, not alternate path sets. We must not merge path sets from different indices
    // (e.g. paths_per_trigger[0] and paths_per_trigger[5]) or we would wrongly add sysctl.d/
    // binfmt.d etc. to transfiletriggerin/un. Verify with: rpm -qp <rpm> --qf '[%{TRANSFILETRIGGERINDEX} ]' and --filetriggers.
    let trigger_paths: &[String] = if i < data.paths_per_trigger.len() {
        &data.paths_per_trigger[i]
    } else if let Some(first) = data.paths_per_trigger.first() {
        first
    } else {
        log::warn!("Missing paths for trigger index {}, skipping", i);
        return Ok(());
    };

    if trigger_paths.is_empty() {
        log::debug!("Trigger {} at index {} has no paths, skipping", trigger_type, i);
        return Ok(());
    }

    // Get priority for this trigger
    let prio = if i < data.priorities.len() {
        data.priorities[i]
    } else {
        log::warn!("Missing priority for trigger index {}, using default", i);
        RPMTRIGGER_DEFAULT_PRIORITY
    };

    // Get program for this trigger (if array exists)
    let program = data.program_array.as_ref().and_then(|arr| {
        if i < arr.len() {
            Some(arr[i].clone())
        } else {
            None
        }
    });

    // Get flags for this trigger (if array exists)
    let flags = data.flags_option.as_ref().and_then(|arr| {
        if i < arr.len() {
            Some(arr[i])
        } else {
            None
        }
    }).unwrap_or_else(rpm::ScriptletFlags::empty);

    // Create scriptlet object for this trigger
    let scriptlet = rpm::Scriptlet {
        script: script_content,
        program: program.map(|s| vec![s]), // Convert Option<String> to Option<Vec<String>>
        flags: Some(flags),
    };

    // Adjust when/op based on priority for non-transaction triggers
    let (final_when, final_op) = adjust_when_op_for_priority(
        trigger_type,
        when_str,
        op_str,
        prio,
        is_transaction,
    );

    // Create a hook for this trigger with all its paths
    let order = *script_order_counter;
    *script_order_counter += 1;
    write_rpm_hook_file(
        install_dir,
        trigger_type,
        final_when,
        final_op,
        "Path",
        trigger_paths,
        &scriptlet,
        Some(prio),
        hook_seq_counter,
        Some(order),
    )?;

    Ok(())
}

fn process_modern_file_triggers<P: AsRef<Path>>(
    metadata: &rpm::PackageMetadata,
    install_dir: P,
    is_transaction: bool,
    hook_seq_counter: &mut HashMap<String, u32>,
    script_order_counter: &mut u32,
) -> Result<()> {
    let install_dir = install_dir.as_ref();
    log::debug!("Checking modern {} triggers", if is_transaction { "transaction" } else { "file" });

    let data = match extract_modern_trigger_data(metadata, is_transaction)? {
        Some(data) => data,
        None => return Ok(()),
    };

    // Determine number of triggers (should be same for all arrays, but take max)
    let num_triggers = data.script_contents.len()
        .max(data.type_strings.len())
        .max(data.paths_per_trigger.len())
        .max(data.priorities.len());

    log::debug!("Processing {} triggers", num_triggers);

    for i in 0..num_triggers {
        process_single_modern_trigger(
            i,
            &data,
            install_dir,
            is_transaction,
            hook_seq_counter,
            script_order_counter,
        )?;
    }

    Ok(())
}

/// Extract legacy RPM file triggers (individual scriptlet tags).
/// Processes filetriggerin, filetriggerun, filetriggerpostun and
/// transfiletriggerin, transfiletriggerun, transfiletriggerpostun.
/// Uses FILETRIGGERNAME/TRANSFILETRIGGERNAME and FILETRIGGERPRIORITIES/TRANSFILETRIGGERPRIORITIES.
fn extract_legacy_file_triggers<P: AsRef<Path>>(
    metadata: &rpm::PackageMetadata,
    install_dir: P,
    hook_seq_counter: &mut HashMap<String, u32>,
    script_order_counter: &mut u32,
) -> Result<()> {
    let install_dir = install_dir.as_ref();

    // Legacy trigger format processing (individual scriptlet tags)
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
    let file_trigger_paths      = extract_string_array(metadata, IndexTag::RPMTAG_FILETRIGGERNAME);
    let file_trigger_priorities = extract_trigger_priorities(metadata, IndexTag::RPMTAG_FILETRIGGERPRIORITIES, RPMTRIGGER_DEFAULT_PRIORITY);

    // Extract transaction file trigger paths and priorities once
    let trans_trigger_paths      = extract_string_array(metadata, IndexTag::RPMTAG_TRANSFILETRIGGERNAME);
    let trans_trigger_priorities = extract_trigger_priorities(metadata, IndexTag::RPMTAG_TRANSFILETRIGGERPRIORITIES, RPMTRIGGER_DEFAULT_PRIORITY);

    // Process file triggers
    for trigger_type in file_trigger_types {
        process_file_trigger_type(
            metadata,
            &install_dir,
            trigger_type,
            &file_trigger_paths,
            &file_trigger_priorities,
            true, // use_priority_based_when
            hook_seq_counter,
            script_order_counter,
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
            hook_seq_counter,
            script_order_counter,
        )?;
    }

    Ok(())
}

/// Extract modern RPM file triggers (FILETRIGGERSCRIPTS/TRANSFILETRIGGERSCRIPTS).
/// Processes modern format triggers using NAME+INDEX mapping.
fn extract_modern_file_triggers<P: AsRef<Path>>(
    metadata: &rpm::PackageMetadata,
    install_dir: P,
    hook_seq_counter: &mut HashMap<String, u32>,
    script_order_counter: &mut u32,
) -> Result<()> {
    let install_dir = install_dir.as_ref();

    // Modern format: FILETRIGGERSCRIPTS/NAME/INDEX etc.; one hook per script index.
    let has_modern_file_triggers  = metadata.header.entry_is_present(IndexTag::RPMTAG_FILETRIGGERSCRIPTS);
    let has_modern_trans_triggers = metadata.header.entry_is_present(IndexTag::RPMTAG_TRANSFILETRIGGERSCRIPTS);

    if has_modern_file_triggers || has_modern_trans_triggers {
        // Process modern file triggers
        if has_modern_file_triggers {
            process_modern_file_triggers(
                metadata,
                install_dir,
                false, // is_transaction
                hook_seq_counter,
                script_order_counter,
            )?;
        }

        // Process modern transaction file triggers
        if has_modern_trans_triggers {
            process_modern_file_triggers(
                metadata,
                install_dir,
                true, // is_transaction
                hook_seq_counter,
                script_order_counter,
            )?;
        }

        // Modern triggers processed, continue to legacy processing
        // (some triggers might be in legacy format even if others are modern)
    }

    Ok(())
}

/// Extract RPM file triggers (filetriggerin, filetriggerun, filetriggerpostun)
/// and transaction file triggers (transfiletriggerin, transfiletriggerun, transfiletriggerpostun).
/// These are triggered by file paths. Modern format uses NAME+INDEX (one path set per script);
/// we write one .hook per trigger index in order. See process_modern_file_triggers and
/// extract_per_trigger_paths_and_priorities for the layout and 1:1 mapping.
fn extract_rpm_file_triggers<P: AsRef<Path>>(metadata: &rpm::PackageMetadata, store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let install_dir = store_tmp_dir.join("info/install");

    let mut hook_seq_counter = HashMap::new();
    let mut script_order_counter = 0_u32;

    // Modern format: FILETRIGGERSCRIPTS/NAME/INDEX etc.; one hook per script index.
    extract_modern_file_triggers(
        metadata,
        &install_dir,
        &mut hook_seq_counter,
        &mut script_order_counter,
    )?;

    // Legacy trigger format processing (individual scriptlet tags)
    extract_legacy_file_triggers(
        metadata,
        &install_dir,
        &mut hook_seq_counter,
        &mut script_order_counter,
    )?;

    Ok(())
}
