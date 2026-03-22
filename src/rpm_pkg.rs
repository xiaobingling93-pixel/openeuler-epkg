use crate::rpm_repo::PACKAGE_KEY_MAPPING;
#[cfg(all(target_os = "linux", debug_assertions))]
use crate::rpm_verify;
use crate::rpm_triggers::{extract_rpm_triggers, extract_install_prefixes};
#[cfg(unix)]
use crate::utils;
use color_eyre::eyre::WrapErr;
use color_eyre::Result;
use rpm::{DependencyFlags, FileMode, IndexTag, Package};
use std::collections::HashMap;
use crate::lfs;
use crate::tar_extract::create_package_dirs;
use std::path::Path;

/// Unpacks an RPM package to the specified directory
pub fn unpack_package<P: AsRef<Path>>(rpm_file: P, store_tmp_dir: P, pkgkey: Option<&str>) -> Result<()> {
    let rpm_file = rpm_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Create the required directory structure
    create_package_dirs(store_tmp_dir, "rpm")?;

    // Open and parse the RPM package
    let package = Package::open(rpm_file)
        .wrap_err_with(|| format!("Failed to open RPM file: {}", rpm_file.display()))?;

    // Extract files to fs/
    let target_fs_dir = store_tmp_dir.join("fs");
    extract_rpm_files(&package, &target_fs_dir)?;

    // ---- Verification Step ----
    // Only run verification in debug builds on Linux when rpm_verify module is compiled
    #[cfg(all(target_os = "linux", debug_assertions))]
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

                let path_in_pkg = file.metadata.path.to_string_lossy();
                let rel = std::path::Path::new(path_in_pkg.trim_start_matches('/'));
                let file_path = target_dir.join(crate::lfs::sanitize_path_for_windows(rel));

                // Create parent directories if they don't exist
                if let Some(parent) = file_path.parent() {
                    lfs::create_dir_all_with_case_sensitivity(parent)?;
                }

                match file.metadata.mode {
                    FileMode::Regular { permissions } => {
                        // Write the actual file content
                        lfs::write(&file_path, &file.content)?;

                        // Set file permissions - preserve original permissions from RPM
                        #[cfg(unix)]
                        {
                            let mode = permissions | 0o600;  // Always ensure owner has rw
                            utils::set_permissions_from_mode(&file_path, mode.into())
                                .wrap_err_with(|| format!("Failed to set permissions for file at {}", file_path.display()))?;
                        }
                        #[cfg(not(unix))]
                        let _ = permissions;
                    }
                    FileMode::Dir { permissions } => {
                        // Create directory
                        lfs::create_dir_all_with_case_sensitivity(&file_path)?;

                        #[cfg(unix)]
                        {
                            // Ensure directories are writable by owner so they can be removed later
                            // This prevents issues with read-only directories like /usr/lib (dr-xr-xr-x)
                            let mode = permissions | 0o700;  // Always ensure owner has rwx
                            utils::set_permissions_from_mode(&file_path, mode.into())
                                .wrap_err_with(|| format!("Failed to set permissions for directory at {}", file_path.display()))?;
                        }
                        #[cfg(not(unix))]
                        let _ = permissions;
                    }
                    FileMode::SymbolicLink { permissions: _ } => {
                        // Create symbolic link
                        if !file.metadata.linkto.is_empty() {
                            #[cfg(unix)]
                            {
                                if let Err(e) = lfs::symlink(&file.metadata.linkto, &file_path) {
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
    let install_dir = crate::dirs::path_join(store_tmp_dir, &["info", "install"]);

    // Mapping from RPM scriptlet names to common names
    // Note: Transaction scriptlets (pretrans, posttrans, preuntrans, postuntrans) use distinct filenames
    // to avoid conflicts with regular upgrade scriptlets
    let scriptlet_mapping: HashMap<&str, &str> = [
        ("prein",       "pre_install.sh"),
        ("postin",      "post_install.sh"),
        ("preun",       "pre_uninstall.sh"),
        ("postun",      "post_uninstall.sh"),
        ("pretrans",    "pre_trans.sh"),     // Distinct filename for transaction scriptlets
        ("posttrans",   "post_trans.sh"),    // Distinct filename for transaction scriptlets
        ("preuntrans",  "pre_untrans.sh"),
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

/// Helper function to get scriptlet content and determine appropriate file extension
/// based on interpreter information from the RPM metadata
fn get_scriptlet_with_extension(metadata: &rpm::PackageMetadata, scriptlet_name: &str) -> Option<(String, String)> {
    let scriptlet = match scriptlet_name {
        "prein"       => metadata.get_pre_install_script().ok(),
        "postin"      => metadata.get_post_install_script().ok(),
        "preun"       => metadata.get_pre_uninstall_script().ok(),
        "postun"      => metadata.get_post_uninstall_script().ok(),
        "pretrans"    => metadata.get_pre_trans_script().ok(),
        "posttrans"   => metadata.get_post_trans_script().ok(),
        "preuntrans"  => get_scriptlet_from_header(metadata, "preuntrans"),
        "postuntrans" => get_scriptlet_from_header(metadata, "postuntrans"),
        _             => None,
    }?;

    let script_content = scriptlet.script.clone();
    let (file_extension, modified_content) = determine_script_extension(&scriptlet, &script_content);

    Some((modified_content, file_extension))
}

/// Extract scriptlet from RPM header using IndexTag constants
/// Used for scriptlets that don't have direct methods in PackageMetadata
pub fn get_scriptlet_from_header(metadata: &rpm::PackageMetadata, scriptlet_name: &str) -> Option<rpm::Scriptlet> {
    let script_tag = match scriptlet_name {
        "preuntrans"             => IndexTag::RPMTAG_PREUNTRANS,
        "postuntrans"            => IndexTag::RPMTAG_POSTUNTRANS,
        "triggerprein"           => IndexTag::RPMTAG_TRIGGERPREIN,
        "triggerin"              => IndexTag::RPMTAG_TRIGGERIN,
        "triggerun"              => IndexTag::RPMTAG_TRIGGERUN,
        "triggerpostun"          => IndexTag::RPMTAG_TRIGGERPOSTUN,
        "filetriggerin"          => IndexTag::RPMTAG_FILETRIGGERIN,
        "filetriggerun"          => IndexTag::RPMTAG_FILETRIGGERUN,
        "filetriggerpostun"      => IndexTag::RPMTAG_FILETRIGGERPOSTUN,
        "transfiletriggerin"     => IndexTag::RPMTAG_TRANSFILETRIGGERIN,
        "transfiletriggerun"     => IndexTag::RPMTAG_TRANSFILETRIGGERUN,
        "transfiletriggerpostun" => IndexTag::RPMTAG_TRANSFILETRIGGERPOSTUN,
        _                        => return None,
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
 * Determines file extension and adds shebang for RPM scriptlets.
 *
 * Step 1: Process scriptlet.program (if present and non-empty)
 *   - Deduplicate consecutive identical elements in program array
 *   - Determine extension via interpreter_to_extension()
 *   - Add shebang if content lacks one and interpreter is either:
 *        * A path starting with '/' (use full program vector)
 *        * Maps to a known extension (use "/usr/bin/env" with stripped interpreter name)
 *   - Lua interpreter mapped to "rpmlua" for RPM compatibility
 *   - Examples:
 *        program = ["<lua>"] -> ext = "lua", adds "#!/usr/bin/env -S rpmlua\n"
 *        program = ["/bin/sh", "-c"] -> ext = "sh", adds "#!/bin/sh -c\n"
 *
 * Step 2: Handle empty content with no extension (one-liner utility programs)
 *   - When script_content is empty and no extension determined, create a .sh wrapper
 *   - Format: "#!/bin/sh\n{program.join(' ')}\n"
 *   - Example: program = ["/sbin/ldconfig", "libs"], content = "" ->
 *              content = "#!/bin/sh\n/sbin/ldconfig libs\n", ext = "sh"
 *
 * Step 3: Default fallback
 *   - If no extension determined yet, default to "sh"
 */
/// Determines the appropriate file extension based on scriptlet interpreter information
/// Returns a tuple of (extension, modified_content)
pub fn determine_script_extension(scriptlet: &rpm::Scriptlet, script_content: &str) -> (String, String) {
    let mut extension = String::new();
    let mut content = script_content.to_string();
    // log::debug!("interpreter '{:?}' {:?}", scriptlet.program, content);

    // Step 1: Process scriptlet.program if available
    if let Some(ref program) = scriptlet.program {
        if program.is_empty() {
            // Empty program array - nothing to do
            // Fall through to default shell script handling
        } else {
            // Step 1a: Deduplicate consecutive identical elements in program array
            let mut program_dedup = Vec::new();
            for item in program {
                let item_str = item.as_str();
                if program_dedup.last() != Some(&item_str) {
                    program_dedup.push(item_str);
                }
            }

            let interpreter = &program_dedup[0];

            // Step 1b: Determine extension from interpreter
            extension = interpreter_to_extension(interpreter);

            // Step 1c: Add shebang if needed (for both path-based and scripting language interpreters)
            if !content.trim_start().starts_with("#!") {
                // Shebang needed for:
                // - Path-based interpreters (starts with '/')
                // - Scripting language interpreters (extension determined)
                // Skip shebang for Lua scripts (extension "lua") because rpmlua will be called directly
                if (interpreter.starts_with("/") || !extension.is_empty()) && extension != "lua" {
                    let shebang = if interpreter.starts_with("/") {
                        // Path-based interpreter: use full program vector
                        format!("#!{}\n", program_dedup.join(" "))
                    } else {
                        // Scripting language interpreter: use /usr/bin/env to locate it
                        // Strip angle brackets from interpreter name (e.g., "<lua>" -> "lua")
                        let interpreter_clean = interpreter.trim_matches(|c| c == '<' || c == '>');
                        // Map Lua interpreter to rpmlua for RPM compatibility
                        let interpreter_name = if interpreter_clean == "lua" { "rpmlua" } else { interpreter_clean };
                        let mut parts = vec!["/usr/bin/env", "-S", interpreter_name];
                        parts.extend_from_slice(&program_dedup[1..]);
                        format!("#!{}\n", parts.join(" "))
                    };
                    content = format!("{}{}", shebang, content);
                }
            }

            // Step 2: Handle empty content with no extension (one-liner utility programs)
            if content.trim().is_empty() && extension.is_empty() {
                content = format!("#!/bin/sh\n{}\n", program_dedup.join(" "));
                extension = "sh".to_string();
            }
        }
    }

    // Step 3: Default to shell script if still no extension determined
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
        name if name.contains("lua")            => "lua".to_string(),
        name if name.contains("python")         => "py".to_string(),
        name if name.contains("perl")           => "pl".to_string(),
        name if name.contains("node")           => "js".to_string(),
        name if name.contains("ruby")           => "rb".to_string(),
        "tcl" | "tclsh"                         => "tcl".to_string(),
        "awk" | "gawk" | "mawk"                 => "awk".to_string(),
        "bash" | "sh" | "dash" | "zsh" | "fish" => "sh".to_string(),
        _ => {
            // If we can't identify the interpreter, log it for debugging
            log::debug!("Unknown interpreter '{}'", interpreter_name);
            "".to_string()
        }
    }
}

/// Convert DependencyFlags to an optional operator string.
/// Returns None for ANY and other non-version-comparison flags.
pub(crate) fn dependency_flags_to_operator(flags: DependencyFlags) -> Option<&'static str> {
    if flags.contains(DependencyFlags::LE) {
        Some("<=")
    } else if flags.contains(DependencyFlags::GE) {
        Some(">=")
    } else if flags.contains(DependencyFlags::LESS) {
        Some("<")
    } else if flags.contains(DependencyFlags::GREATER) {
        Some(">")
    } else if flags.contains(DependencyFlags::EQUAL) {
        Some("=")
    } else {
        None
    }
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
    let op = dependency_flags_to_operator(flags).unwrap_or("=");
    format!("{} {} {}", name, op, version)
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
    macro_rules! add_dep_field {
        ($metadata:expr, $method:ident, $field:expr) => {
            if let Ok(deps) = $metadata.$method() {
                let formatted = format_rpm_dependencies(&deps);
                if !formatted.is_empty() {
                    raw_fields.push(($field.to_string(), formatted));
                }
            }
        };
    }
    add_dep_field!(metadata, get_provides, "provides");
    add_dep_field!(metadata, get_requires, "requires");
    add_dep_field!(metadata, get_conflicts, "conflicts");
    add_dep_field!(metadata, get_obsoletes, "obsoletes");
    add_dep_field!(metadata, get_enhances, "enhances");
    add_dep_field!(metadata, get_recommends, "recommends");
    add_dep_field!(metadata, get_suggests, "suggests");
    add_dep_field!(metadata, get_supplements, "supplements");

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
