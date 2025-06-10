use crate::rpm_repo::PACKAGE_KEY_MAPPING;
#[cfg(debug_assertions)]
use crate::rpm_verify;
use color_eyre::eyre::WrapErr;
use color_eyre::Result;
use rpm::{DependencyFlags, FileMode, Package};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use regex::Regex;

/// Unpacks an RPM package to the specified directory
pub fn unpack_package<P: AsRef<Path>>(rpm_file: P, store_tmp_dir: P) -> Result<()> {
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

    // Create package.txt
    create_package_txt(&package, rpm_file, store_tmp_dir)?;

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
                            // Use original permissions from RPM, only ensure minimum read for owner if needed
                            let mode = if permissions & 0o600 != 0o600 {
                                permissions | 0o600  // Add owner rw if not present
                            } else {
                                permissions  // Use original permissions as-is
                            };
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
                            // Use original permissions from RPM, only ensure minimum access for owner if needed
                            let mode = if permissions & 0o700 != 0o700 {
                                permissions | 0o700  // Add owner rwx if not fully present
                            } else {
                                permissions  // Use original permissions as-is
                            };
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
    let scriptlet_mapping: HashMap<&str, Vec<&str>> = [
        ("prein", vec!["pre_install.sh", "pre_upgrade.sh"]),
        ("postin", vec!["post_install.sh", "post_upgrade.sh"]),
        ("preun", vec!["pre_uninstall.sh"]),
        ("postun", vec!["post_uninstall.sh"]),
        ("pretrans", vec!["pre_upgrade.sh"]),
        ("posttrans", vec!["post_upgrade.sh"]),
    ].into_iter().collect();

    let metadata = &package.metadata;

    // Extract scriptlets using the correct methods with interpreter detection
    for (rpm_script, common_scripts) in &scriptlet_mapping {
        if let Some((script_content, file_extension)) = get_scriptlet_with_extension(metadata, rpm_script) {
            for common_script in common_scripts {
                // Use the detected file extension instead of always .sh
                let script_name = if file_extension != "sh" {
                    format!("{}.{}", common_script.trim_end_matches(".sh"), file_extension)
                } else {
                    common_script.to_string()
                };

                let target_path = install_dir.join(&script_name);

                // Write the script content
                fs::write(&target_path, &script_content)
                    .wrap_err_with(|| format!("Failed to write script content to {}", target_path.display()))?;

                // Make it executable
                #[cfg(unix)]
                {
                    let mut perms = fs::metadata(&target_path)
                        .wrap_err_with(|| format!("Failed to get metadata for script at {}", target_path.display()))?.permissions();
                    perms.set_mode(0o755);
                    fs::set_permissions(&target_path, perms)
                        .wrap_err_with(|| format!("Failed to set executable permissions for script at {}", target_path.display()))?;
                }
            }
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
        _ => None,
    }?;

    let script_content = scriptlet.script.clone();
    let (file_extension, modified_content) = determine_script_extension(&scriptlet, &script_content);

    Some((modified_content, file_extension))
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
    } else {
        // CASE 4: Program is None but we can try to detect script type from content
        // To workaround rpm-rs bug: program can be None even the libgcc rpm has interpreter defined:
        // % rpm -q --xml /home/wfg/.cache/epkg/downloads/openeuler/openEuler-24.03-LTS-SP1/everything/x86_64/Packages/libgcc-12.3.1-62.oe2403sp1.x86_64.rpm|l
        // <rpmTag name="Postinprog">
        // <string>&lt;lua&gt;</string>
        // </rpmTag>
        if looks_like_lua_script(script_content) {
            log::debug!("Detected Lua script from content pattern");
            extension = "lua".to_string();
        }
    }

    // Default to shell script if still no extension determined
    if extension.is_empty() {
        extension = "sh".to_string();
    }

    (extension, content)
}

/// Detects if a script content looks like Lua code based on common patterns
fn looks_like_lua_script(content: &str) -> bool {
    use regex::Regex;
    use std::sync::OnceLock;

    // Define regex patterns for Lua code detection
    static LUA_PATTERNS: OnceLock<Vec<(Regex, i32)>> = OnceLock::new();

    let patterns = LUA_PATTERNS.get_or_init(|| {
        vec![
            // Function calls with posix, os, io modules (strong indicators)
            (Regex::new(r"^\s*posix\.[a-zA-Z_][a-zA-Z0-9_]*\s*\(").unwrap(), 3),
            (Regex::new(r"^\s*os\.[a-zA-Z_][a-zA-Z0-9_]*\s*\(").unwrap(), 2),
            (Regex::new(r"^\s*io\.stdout:[a-zA-Z_][a-zA-Z0-9_]*\s*\(").unwrap(), 2),
            (Regex::new(r"^\s*io\.stderr:[a-zA-Z_][a-zA-Z0-9_]*\s*\(").unwrap(), 2),
            (Regex::new(r"^\s*io\.(write|read|open|close)\s*\(").unwrap(), 2),

            // Control structures (medium indicators)
            (Regex::new(r"^\s*if\s+[^;]*\s+then\s*$").unwrap(), 2),
            (Regex::new(r"^\s*for\s+[^;]*\s+do\s*$").unwrap(), 2),
            (Regex::new(r"^\s*while\s+[^;]*\s+do\s*$").unwrap(), 2),
            (Regex::new(r"^\s*function\s+[a-zA-Z_][a-zA-Z0-9_]*\s*\(").unwrap(), 2),

            // Variable declarations (medium indicators)
            (Regex::new(r"^\s*local\s+[a-zA-Z_][a-zA-Z0-9_]*\s*=").unwrap(), 1),

            // Common Lua keywords and patterns (weak indicators)
            (Regex::new(r"^\s*end\s*$").unwrap(), 1),
            (Regex::new(r"^\s*elseif\s+.*\s+then").unwrap(), 1),
            (Regex::new(r"^\s*require\s*\(").unwrap(), 1),
            (Regex::new(r"^\s*--[^-]").unwrap(), 1), // Lua comments
        ]
    });

    // Split content into lines and check each line against patterns
    let mut lua_score = 0;
    // log::debug!("lua detect '{:?}'", content);

    for line in content.lines() {
        for (pattern, score) in patterns {
            if pattern.is_match(line) {
                lua_score += score;

                // Strong indicators that immediately suggest Lua
                if *score >= 3 {
                    log::debug!("Strong Lua indicator found: '{}'", line);
                    return true;
                }
            }
        }
    }

    // Require a minimum score to avoid false positives
    let is_lua = lua_score >= 3;
    if is_lua {
        log::debug!("Lua script detected with score: {}", lua_score);
    }

    is_lua
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
pub fn create_package_txt<P: AsRef<Path>>(package: &Package, rpm_file: P, store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let metadata = &package.metadata;

    let mut raw_fields: Vec<(String, String)> = Vec::new();

    // Extract basic metadata fields
    raw_fields.push(("name".to_string(), metadata.get_name().unwrap_or("unknown").to_string()));
    raw_fields.push(("version".to_string(), format!("{}-{}",
        metadata.get_version().unwrap_or("unknown"),
        metadata.get_release().unwrap_or("unknown"))));
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
    let mut package_fields: Vec<(String, String)> = Vec::new();

    for (original_field, value) in raw_fields {
        if let Some(mapped_field) = PACKAGE_KEY_MAPPING.get(original_field.as_str()) {
            package_fields.push((mapped_field.to_string(), value));
        } else {
            log::warn!("Field name '{}' not found in predefined mapping list", original_field);
            // Include unmapped fields with their original names
            package_fields.push((original_field, value));
        }
    }

    // Calculate SHA256 hash of the rpm file and add it to package_fields
    let sha256 = crate::store::calculate_file_sha256(rpm_file.as_ref())
        .wrap_err_with(|| format!("Failed to calculate SHA256 hash for rpm file: {}", rpm_file.as_ref().display()))?;
    package_fields.push(("sha256".to_string(), sha256));

    // Use the general store function to save the package.txt file
    crate::store::save_package_txt(package_fields, store_tmp_dir)?;

    Ok(())
}
