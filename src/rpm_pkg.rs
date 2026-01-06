use crate::rpm_repo::PACKAGE_KEY_MAPPING;
#[cfg(debug_assertions)]
use crate::rpm_verify;
use color_eyre::eyre::WrapErr;
use color_eyre::Result;
use rpm::{DependencyFlags, FileMode, IndexTag, Package};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Unpacks an RPM package to the specified directory
pub fn unpack_package<P: AsRef<Path>>(rpm_file: P, store_tmp_dir: P, pkgkey: Option<&str>) -> Result<()> {
    let rpm_file = rpm_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Create the required directory structure
    fs::create_dir_all(store_tmp_dir.join("fs"))
        .wrap_err_with(|| format!("Failed to create info/rpm directory at {}", store_tmp_dir.join("info/rpm").display()))?;
    fs::create_dir_all(store_tmp_dir.join("info/rpm"))
        .wrap_err_with(|| format!("Failed to create info/rpm directory at {}", store_tmp_dir.join("info/rpm").display()))?;
    fs::create_dir_all(store_tmp_dir.join("info/install"))
        .wrap_err_with(|| format!("Failed to create info/install directory at {}", store_tmp_dir.join("info/install").display()))?;

    // Open and parse the RPM package
    let package = Package::open(rpm_file)
        .wrap_err_with(|| format!("Failed to open RPM file: {}", rpm_file.display()))?;

    // Extract files to fs/
    let target_fs_dir = store_tmp_dir.join("fs");
    extract_rpm_files(&package, &target_fs_dir)?;

    // ---- Verification Step ----
    // Only run verification in debug builds when rpm_verify module is compiled
    #[cfg(debug_assertions)]
    {
        if let Err(e) = rpm_verify::verify_rpm_extraction(rpm_file, &target_fs_dir) {
            log::warn!("RPM extraction verification check failed for {}: {}. Continuing with epkg's version.", rpm_file.display(), e);
            // Do not propagate this error, as it's a debug/verification feature.
        }
    }
    // ---- End Verification Step ----

    // Generate filelist.txt
    crate::store::create_filelist_txt(store_tmp_dir)?;

    // Create scriptlets
    create_scriptlets(&package, store_tmp_dir)?;

    // Extract trigger scriptlets (package triggers, file triggers, transaction file triggers)
    extract_rpm_triggers(&package, store_tmp_dir)?;

    // Extract and store install prefixes for relocatable packages
    extract_install_prefixes(&package, store_tmp_dir)?;

    // Create package.txt
    create_package_txt(&package, rpm_file, store_tmp_dir, pkgkey)?;

    Ok(())
}

/// Extracts RPM package files to the target directory
/// Based on the rpm crate's extract method but improved to handle edge cases
fn extract_rpm_files<P: AsRef<Path>>(package: &Package, target_dir: P) -> Result<()> {
    let target_dir = target_dir.as_ref();

    // Check if the package has any files before attempting extraction
    match package.metadata.get_file_entries() {
        Ok(file_entries) if file_entries.is_empty() => {
            // Package contains no files, nothing to extract
            log::debug!("RPM package contains no files, skipping extraction");
            return Ok(());
        }
        Ok(_) => {
            // Package has files, proceed with extraction using the built-in files() method
            for file_result in package.files()
                .wrap_err_with(|| "Failed to get file iterator from RPM package")? {

                let file = file_result
                    .wrap_err_with(|| "Failed to read file from RPM package")?;

                // Skip ghost files - these are not included in the CPIO payload
                // This matches the behavior of official RPM tools like rpm2cpio
                if file.metadata.flags.contains(rpm::FileFlags::GHOST) {
                    log::debug!("Skipping ghost file: {}", file.metadata.path.display());
                    continue;
                }

                let file_path = target_dir.join(file.metadata.path.to_string_lossy().trim_start_matches('/'));

                // Create parent directories if they don't exist
                if let Some(parent) = file_path.parent() {
                    fs::create_dir_all(parent)
                        .wrap_err_with(|| format!("Failed to create parent directory at {}", parent.display()))?;
                }

                match file.metadata.mode {
                    FileMode::Regular { permissions } => {
                        // Write the actual file content
                        fs::write(&file_path, &file.content)
                            .wrap_err_with(|| format!("Failed to write file content to {}", file_path.display()))?;

                        // Set file permissions - preserve original permissions from RPM
                        #[cfg(unix)]
                        {
                            let mode = permissions | 0o600;  // Always ensure owner has rw
                            fs::set_permissions(&file_path, fs::Permissions::from_mode(mode.into()))
                                .wrap_err_with(|| format!("Failed to set permissions for file at {}", file_path.display()))?;
                        }
                    }
                    FileMode::Dir { permissions } => {
                        // Create directory
                        fs::create_dir_all(&file_path)
                            .wrap_err_with(|| format!("Failed to create directory at {}", file_path.display()))?;

                        #[cfg(unix)]
                        {
                            // Ensure directories are writable by owner so they can be removed later
                            // This prevents issues with read-only directories like /usr/lib (dr-xr-xr-x)
                            let mode = permissions | 0o700;  // Always ensure owner has rwx
                            fs::set_permissions(&file_path, fs::Permissions::from_mode(mode.into()))
                                .wrap_err_with(|| format!("Failed to set permissions for directory at {}", file_path.display()))?;
                        }
                    }
                    FileMode::SymbolicLink { permissions: _ } => {
                        // Create symbolic link
                        if !file.metadata.linkto.is_empty() {
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs;
                                if let Err(e) = fs::symlink(&file.metadata.linkto, &file_path) {
                                    log::warn!("Failed to create symlink {:?} -> {:?}: {}", file_path, file.metadata.linkto, e);
                                }
                            }
                        }
                    }
                    FileMode::Invalid { raw_mode: _, reason } => {
                        log::warn!("Invalid file mode for {:?}: {}", file_path, reason);
                    }
                    _ => {
                        log::warn!("Unsupported file mode for {:?}", file_path);
                    }
                }
            }
        }
        Err(_) => {
            // If we can't get file entries, assume it's an empty package
            log::debug!("Failed to get file entries, assuming empty package");
        }
    }

    Ok(())
}

/// Creates scriptlets with appropriate file extensions based on interpreter information
pub fn create_scriptlets<P: AsRef<Path>>(package: &Package, store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let install_dir = store_tmp_dir.join("info/install");

    // Mapping from RPM scriptlet names to common names
    // Note: Transaction scriptlets (pretrans, posttrans, preuntrans, postuntrans) use distinct filenames
    // to avoid conflicts with regular upgrade scriptlets
    let scriptlet_mapping: HashMap<&str, &str> = [
        ("prein", "pre_install.sh"),
        ("postin", "post_install.sh"),
        ("preun", "pre_uninstall.sh"),
        ("postun", "post_uninstall.sh"),
        ("pretrans", "pre_trans.sh"),      // Distinct filename for transaction scriptlets
        ("posttrans", "post_trans.sh"),    // Distinct filename for transaction scriptlets
        ("preuntrans", "pre_untrans.sh"),
        ("postuntrans", "post_untrans.sh"),
    ].into_iter().collect();

    let metadata = &package.metadata;

    // Extract scriptlets using the correct methods with interpreter detection
    for (rpm_script, common_script) in &scriptlet_mapping {
        if let Some((script_content, file_extension)) = get_scriptlet_with_extension(metadata, rpm_script) {
            // Use the detected file extension instead of always .sh
            let script_name = if file_extension != "sh" {
                format!("{}.{}", common_script.trim_end_matches(".sh"), file_extension)
            } else {
                common_script.to_string()
            };

            let target_path = install_dir.join(&script_name);
            crate::utils::write_scriptlet_content(&target_path, script_content.as_bytes())?;
        }
    }

    Ok(())
}

/// Extract RPM trigger scriptlets (package triggers, file triggers, transaction file triggers)
/// and store them as Arch-style .hook files under info/install/.
fn extract_rpm_triggers<P: AsRef<Path>>(package: &Package, store_tmp_dir: P) -> Result<()> {
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

/// Helper function to get scriptlet content and determine appropriate file extension
/// based on interpreter information from the RPM metadata
fn get_scriptlet_with_extension(metadata: &rpm::PackageMetadata, scriptlet_name: &str) -> Option<(String, String)> {
    let scriptlet = match scriptlet_name {
        "prein" => metadata.get_pre_install_script().ok(),
        "postin" => metadata.get_post_install_script().ok(),
        "preun" => metadata.get_pre_uninstall_script().ok(),
        "postun" => metadata.get_post_uninstall_script().ok(),
        "pretrans" => metadata.get_pre_trans_script().ok(),
        "posttrans" => metadata.get_post_trans_script().ok(),
        "preuntrans" => get_scriptlet_from_header(metadata, "preuntrans"),
        "postuntrans" => get_scriptlet_from_header(metadata, "postuntrans"),
        _ => None,
    }?;

    let script_content = scriptlet.script.clone();
    let (file_extension, modified_content) = determine_script_extension(&scriptlet, &script_content);

    Some((modified_content, file_extension))
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

/// Extract scriptlet from RPM header using IndexTag constants
/// Used for scriptlets that don't have direct methods in PackageMetadata
fn get_scriptlet_from_header(metadata: &rpm::PackageMetadata, scriptlet_name: &str) -> Option<rpm::Scriptlet> {
    let script_tag = match scriptlet_name {
        "preuntrans" => IndexTag::RPMTAG_PREUNTRANS,
        "postuntrans" => IndexTag::RPMTAG_POSTUNTRANS,
        "triggerprein" => IndexTag::RPMTAG_TRIGGERPREIN,
        "triggerin" => IndexTag::RPMTAG_TRIGGERIN,
        "triggerun" => IndexTag::RPMTAG_TRIGGERUN,
        "triggerpostun" => IndexTag::RPMTAG_TRIGGERPOSTUN,
        "filetriggerin" => IndexTag::RPMTAG_FILETRIGGERIN,
        "filetriggerun" => IndexTag::RPMTAG_FILETRIGGERUN,
        "filetriggerpostun" => IndexTag::RPMTAG_FILETRIGGERPOSTUN,
        "transfiletriggerin" => IndexTag::RPMTAG_TRANSFILETRIGGERIN,
        "transfiletriggerun" => IndexTag::RPMTAG_TRANSFILETRIGGERUN,
        "transfiletriggerpostun" => IndexTag::RPMTAG_TRANSFILETRIGGERPOSTUN,
        _ => return None,
    };

    // Check if scriptlet exists
    if !metadata.header.entry_is_present(script_tag) {
        return None;
    }

    // Get script content
    let script = metadata.header.get_entry_data_as_string(script_tag).ok()?.to_string();

    // Try to get program/interpreter - use a generic approach since PROG tags may not exist
    // For triggers, we'll try to detect the interpreter from the script content or use default
    let program = None; // Will be determined by determine_script_extension

    Some(rpm::Scriptlet {
        script,
        program,
        flags: Some(rpm::ScriptletFlags::empty()),
    })
}

/**
 * CASE 1: <lua>
 * - scriptlet.program could be vec!["<lua>"]
 * - This is handled correctly by interpreter_to_extension() which returns ext = "lua"
 * - Content remains unchanged
 * - Example: program = ["<lua>"] -> ext = "lua", content unchanged
 *
 * CASE 2: /bin/sh -c and similar common scripting language interpreter programs
 * - scriptlet.program could be vec!["/bin/sh", "-c"] or vec!["/usr/bin/texhash"]
 * - When interpreter starts with '/', we add a shebang line to script_content
 * - Format: "#!{program.join(' ')}\n{original_content}"
 * - Extension determined by interpreter_to_extension()
 * - Example: program = ["/bin/sh", "-c"] -> adds "#!/bin/sh -c\n" to content, ext = "sh"
 *
 * CASE 3: One-liner utility programs like /sbin/ldconfig, /sbin/ldconfig libs, /usr/bin/texhash
 * - These have empty script_content but meaningful program fields
 * - When script_content is empty and no extension is determined, create a .sh wrapper
 * - Format: "#!/bin/sh\n{program.join(' ')}\n"
 * - Extension set to "sh"
 * - Example: program = ["/sbin/ldconfig", "libs"], content = "" ->
 *           content = "#!/bin/sh\n/sbin/ldconfig libs\n", ext = "sh"
 */
/// Determines the appropriate file extension based on scriptlet interpreter information
/// Returns a tuple of (extension, modified_content)
fn determine_script_extension(scriptlet: &rpm::Scriptlet, script_content: &str) -> (String, String) {
    let mut extension = String::new();
    let mut content = script_content.to_string();
    // log::debug!("interpreter '{:?}' {:?}", scriptlet.program, content);

    // Process based on scriptlet.program if available
    if let Some(ref program) = scriptlet.program {
        if !program.is_empty() {
            let interpreter = &program[0];

            // CASE 1: Get extension from scripting language interpreter
            extension = interpreter_to_extension(interpreter);

            // CASE 2: Add shebang for path-based interpreters (except Lua which has special handling)
            if interpreter.starts_with("/") {
                let shebang = format!("#!{}\n", program.join(" "));
                content = format!("{}{}", shebang, content);
            }

            // CASE 3: Create shell wrapper for empty content with no determined extension
            if content.trim().is_empty() && extension.is_empty() {
                content = format!("#!/bin/sh\n{}\n", program.join(" "));
                extension = "sh".to_string();
            }
        }
    }

    // Default to shell script if still no extension determined
    if extension.is_empty() {
        extension = "sh".to_string();
    }

    (extension, content)
}

/// Maps scripting language interpreter paths/names to appropriate file extensions
fn interpreter_to_extension(interpreter: &str) -> String {
    // Handle full paths by extracting basename
    let interpreter_name = std::path::Path::new(interpreter)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(interpreter);

    match interpreter_name {
        name if name.contains("lua") => "lua".to_string(),
        name if name.contains("python") => "py".to_string(),
        name if name.contains("perl") => "pl".to_string(),
        name if name.contains("node") => "js".to_string(),
        name if name.contains("ruby") => "rb".to_string(),
        "tcl" | "tclsh" => "tcl".to_string(),
        "awk" | "gawk" | "mawk" => "awk".to_string(),
        "bash" | "sh" | "dash" | "zsh" | "fish" => "sh".to_string(),
        _ => {
            // If we can't identify the interpreter, log it for debugging
            log::debug!("Unknown interpreter '{}'", interpreter_name);
            "".to_string()
        }
    }
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
        writeln!(buf, "Target = {}", t)?;
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

// (obsolete helpers for legacy RPM trigger metadata removed)

/// Extract install prefixes from RPM package for relocatable packages
/// Stores them in info/install/install_prefixes.txt for use in scriptlet environment variables
fn extract_install_prefixes<P: AsRef<Path>>(package: &Package, store_tmp_dir: P) -> Result<()> {
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

/// Helper function to format a single RPM dependency
fn format_rpm_dependency(dep: &rpm::Dependency) -> String {
    let name = &dep.name;
    let version = &dep.version;
    let flags = dep.flags;

    // If no version, just return the name regardless of flags
    if version.is_empty() {
        return name.to_string();
    }

    // Handle different comparison operators based on flags
    if flags.contains(DependencyFlags::LE) {
        // LESS | EQUAL
        format!("{}<={}", name, version)
    } else if flags.contains(DependencyFlags::GE) {
        format!("{}>{}", name, version)
    } else if flags.contains(DependencyFlags::LESS) {
        // LESS only
        format!("{}<{}", name, version)
    } else if flags.contains(DependencyFlags::GREATER) {
        format!("{}>{}", name, version)
    } else if flags.contains(DependencyFlags::EQUAL) || flags == DependencyFlags::ANY {
        // EQUAL or ANY - use = format
        format!("{} = {}", name, version) // Added spaces to distinguish from "font(:lang=yap)"
    } else {
        // For any other flags (like SCRIPT_PRE, RPMLIB, etc.), default to = format
        format!("{} = {}", name, version)
    }
}

/// Helper function to format a vector of RPM dependencies
fn format_rpm_dependencies(deps: &[rpm::Dependency]) -> String {
    deps.iter()
        .map(format_rpm_dependency)
        .collect::<Vec<String>>()
        .join(", ")
}

/// Extracts package metadata and creates package.txt with mapped field names
pub fn create_package_txt<P: AsRef<Path>>(package: &Package, rpm_file: P, store_tmp_dir: P, pkgkey: Option<&str>) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let metadata = &package.metadata;

    let mut raw_fields: Vec<(String, String)> = Vec::new();

    // Extract basic metadata fields
    raw_fields.push(("name".to_string(), metadata.get_name().unwrap_or("unknown").to_string()));

    // Format version with epoch if present
    let version = metadata.get_version().unwrap_or("unknown");
    let release = metadata.get_release().unwrap_or("unknown");
    let epoch = metadata.get_epoch().unwrap_or(0);

    let version_str = if epoch == 0 {
        format!("{}-{}", version, release)
    } else {
        format!("{}:{}-{}", epoch, version, release)
    };
    raw_fields.push(("version".to_string(), version_str));

    raw_fields.push(("arch".to_string(), metadata.get_arch().unwrap_or("unknown").to_string()));

    if let Ok(summary) = metadata.get_summary() {
        raw_fields.push(("summary".to_string(), summary.to_string()));
    }

    if let Ok(description) = metadata.get_description() {
        raw_fields.push(("description".to_string(), description.to_string().replace("\n", "\n ")));
    }

    if let Ok(url) = metadata.get_url() {
        raw_fields.push(("url".to_string(), url.to_string()));
    }

    if let Ok(license) = metadata.get_license() {
        raw_fields.push(("license".to_string(), license.to_string()));
    }

    if let Ok(vendor) = metadata.get_vendor() {
        raw_fields.push(("vendor".to_string(), vendor.to_string()));
    }

    if let Ok(group) = metadata.get_group() {
        if group != "Unspecified" {
            raw_fields.push(("group".to_string(), group.to_string()));
        }
    }

    if let Ok(buildhost) = metadata.get_build_host() {
        raw_fields.push(("buildhost".to_string(), buildhost.to_string()));
    }

    if let Ok(source_rpm) = metadata.get_source_rpm() {
        raw_fields.push(("sourcerpm".to_string(), source_rpm.to_string()));
    }

    if let Ok(packager) = metadata.get_packager() {
        raw_fields.push(("packager".to_string(), packager.to_string()));
    }

    // Add installed size information
    if let Ok(installed_size) = metadata.get_installed_size() {
        raw_fields.push(("installed-size".to_string(), installed_size.to_string()));
    }

    // Add build time
    if let Ok(build_time) = metadata.get_build_time() {
        raw_fields.push(("time".to_string(), build_time.to_string()));
    }

    // Add dependency information - using custom formatting
    if let Ok(provides) = metadata.get_provides() {
        let formatted_provides = format_rpm_dependencies(&provides);
        if !formatted_provides.is_empty() {
            raw_fields.push(("provides".to_string(), formatted_provides));
        }
    }

    if let Ok(requires) = metadata.get_requires() {
        let formatted_requires = format_rpm_dependencies(&requires);
        if !formatted_requires.is_empty() {
            raw_fields.push(("requires".to_string(), formatted_requires));
        }
    }

    // Add file list
    if let Ok(file_entries) = metadata.get_file_entries() {
        let files: Vec<String> = file_entries.iter()
            .map(|file| file.path.to_string_lossy().to_string())
            .collect();
        if !files.is_empty() {
            raw_fields.push(("files".to_string(), files.join(", ")));
        }
    }

    // Map field names using PACKAGE_KEY_MAPPING
    let mut package_fields: HashMap<String, String> = HashMap::new();

    for (original_field, value) in raw_fields {
        if let Some(mapped_field) = PACKAGE_KEY_MAPPING.get(original_field.as_str()) {
            package_fields.insert(mapped_field.to_string(), value);
        } else {
            log::warn!("Field name '{}' not found in predefined mapping list", original_field);
            // Include unmapped fields with their original names
            package_fields.insert(original_field, value);
        }
    }

    // Calculate SHA256 hash of the rpm file and add it to package_fields
    let sha256 = crate::store::calculate_file_sha256(rpm_file.as_ref())
        .wrap_err_with(|| format!("Failed to calculate SHA256 hash for rpm file: {}", rpm_file.as_ref().display()))?;
    package_fields.insert("sha256".to_string(), sha256);

    package_fields.insert("format".to_string(), "rpm".to_string());

    // Use the general store function to save the package.txt file
    crate::store::save_package_txt(package_fields, store_tmp_dir, pkgkey)?;

    Ok(())
}
