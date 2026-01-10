use std::fs;
use std::path::Path;
use std::collections::HashMap;
use tar::Archive;
use log;
use flate2::read::MultiGzDecoder;
use lazy_static::lazy_static;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use crate::utils;

lazy_static! {
    pub static ref PACKAGE_KEY_MAPPING: std::collections::HashMap<&'static str, &'static str> = {
        let mut m = std::collections::HashMap::new();

		// Map APK field names to common field names based on gen-package.py
		// Core package metadata
		m.insert("pkgname",     "pkgname");
		m.insert("pkgver",      "version");
		m.insert("pkgdesc",     "summary");
		m.insert("url",         "homepage");
		m.insert("builddate",   "buildTime");
		m.insert("packager",    "maintainer");
		m.insert("size",        "installedSize");
		m.insert("arch",        "arch");
		m.insert("origin",      "source");
		m.insert("commit",      "commit");
		m.insert("maintainer",  "maintainer");
		m.insert("license",     "license");

        // Dependencies and relationships
        m.insert("depend",      "requires");
        m.insert("conflict",    "conflicts");
        m.insert("provides",    "provides");
        m.insert("replaces",    "replaces");
        m.insert("install_if",  "suggests");
        m.insert("triggers",    "triggers");

        // Priority and versioning
        m.insert("replaces_priority", "replaces_priority");
        m.insert("provider_priority", "provider_priority");

        // Checksums and hashes
        m.insert("datahash",    "sha256");
        m.insert("checksum",    "md5sum");

        m
    };
}

/// PKGINFO field definitions based on APK v2 specification
pub struct PkgInfoField {
    #[allow(dead_code)]
    pub name: &'static str,
    #[allow(dead_code)]
    pub description: &'static str,
    pub repeatable: bool,
}

lazy_static! {
    pub static ref PKGINFO_FIELDS: std::collections::HashMap<&'static str, PkgInfoField> = {
        let mut m = std::collections::HashMap::new();

        m.insert("pkgname", PkgInfoField {
            name: "pkgname",
            description: "package name",
            repeatable: false,
        });
        m.insert("pkgver", PkgInfoField {
            name: "pkgver",
            description: "package version",
            repeatable: false,
        });
        m.insert("pkgdesc", PkgInfoField {
            name: "pkgdesc",
            description: "package description",
            repeatable: false,
        });
        m.insert("url", PkgInfoField {
            name: "url",
            description: "package url",
            repeatable: false,
        });
        m.insert("builddate", PkgInfoField {
            name: "builddate",
            description: "unix timestamp of the package build date/time",
            repeatable: false,
        });
        m.insert("packager", PkgInfoField {
            name: "packager",
            description: "name (and typically email) of person who built the package",
            repeatable: false,
        });
        m.insert("size", PkgInfoField {
            name: "size",
            description: "the installed-size of the package",
            repeatable: false,
        });
        m.insert("arch", PkgInfoField {
            name: "arch",
            description: "the architecture of the package (ex: x86_64)",
            repeatable: false,
        });
        m.insert("origin", PkgInfoField {
            name: "origin",
            description: "the origin name of the package",
            repeatable: false,
        });
        m.insert("commit", PkgInfoField {
            name: "commit",
            description: "the commit hash from which the package was built",
            repeatable: false,
        });
        m.insert("maintainer", PkgInfoField {
            name: "maintainer",
            description: "name (and typically email) of the package maintainer",
            repeatable: false,
        });
        m.insert("replaces_priority", PkgInfoField {
            name: "replaces_priority",
            description: "replaces priority field for package (integer)",
            repeatable: false,
        });
        m.insert("provider_priority", PkgInfoField {
            name: "provider_priority",
            description: "provider priority for the package (integer)",
            repeatable: false,
        });
        m.insert("license", PkgInfoField {
            name: "license",
            description: "license string for the package",
            repeatable: false,
        });
        m.insert("datahash", PkgInfoField {
            name: "datahash",
            description: "hex-encoded sha256 checksum of the data tarball",
            repeatable: false,
        });

        // Repeatable fields
        m.insert("depend", PkgInfoField {
            name: "depend",
            description: "dependencies for the package",
            repeatable: true,
        });
        m.insert("replaces", PkgInfoField {
            name: "replaces",
            description: "packages this package replaces",
            repeatable: true,
        });
        m.insert("provides", PkgInfoField {
            name: "provides",
            description: "what this package provides",
            repeatable: true,
        });
        m.insert("triggers", PkgInfoField {
            name: "triggers",
            description: "what packages this package triggers on",
            repeatable: true,
        });
        m.insert("install_if", PkgInfoField {
            name: "install_if",
            description: "install this package if these packages are present",
            repeatable: true,
        });

        m
    };
}

/// Unpacks an APK package to the specified directory
pub fn unpack_package<P: AsRef<Path>>(apk_file: P, store_tmp_dir: P, pkgkey: Option<&str>) -> Result<()> {
    let apk_file = apk_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Create the required directory structure
    fs::create_dir_all(store_tmp_dir.join("fs"))?;
    fs::create_dir_all(store_tmp_dir.join("info/apk"))?;
    fs::create_dir_all(store_tmp_dir.join("info/install"))?;

    // Unpack the APK package
    log::debug!("Unpacking APK package: {}", apk_file.display());
    unpack_apk(apk_file, store_tmp_dir)
        .wrap_err_with(|| format!("Failed to unpack APK package: {}", apk_file.display()))?;

    // Generate filelist.txt
    log::debug!("Creating filelist.txt");
    crate::store::create_filelist_txt(store_tmp_dir)
        .wrap_err_with(|| format!("Failed to create filelist.txt for {}", store_tmp_dir.display()))?;

    // Create scriptlets with proper mapping
    log::debug!("Creating scriptlets");
    create_scriptlets(store_tmp_dir)
        .wrap_err_with(|| format!("Failed to create scriptlets for {}", store_tmp_dir.display()))?;

    // Create package.txt with improved parsing
    log::debug!("Creating package.txt");
    create_package_txt(store_tmp_dir, pkgkey)
        .wrap_err_with(|| format!("Failed to create package.txt for {}", store_tmp_dir.display()))?;

    log::debug!("APK unpacking completed successfully");
    Ok(())
}

/// Unpacks an APK package (concatenated gzip streams containing tar archives)
fn unpack_apk<P: AsRef<Path>>(apk_file: P, store_tmp_dir: &Path) -> Result<()> {
    let apk_file = apk_file.as_ref();
    log::debug!("Unpacking APK package: {}", apk_file.display());

    // Create required directories
    fs::create_dir_all(store_tmp_dir.join("fs"))?;
    fs::create_dir_all(store_tmp_dir.join("info/apk"))?;

    // Open the APK file
    let file = fs::File::open(apk_file)
        .wrap_err_with(|| format!("Failed to open APK file: {}", apk_file.display()))?;

    // Use MultiGzDecoder to handle concatenated gzip streams
    let decoder = MultiGzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    let mut entries_processed = 0;

    // Change to the target directory before extracting to fix hard link issues
    let original_dir = std::env::current_dir()?;
    std::env::set_current_dir(store_tmp_dir.join("fs"))?;

    // Process tar entries
    for entry_result in archive.entries()? {
        let mut entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                log::error!("Error reading tar entry: {}", e);
                // If it's a corrupt compression stream, mark the file as bad and fail
                if e.to_string().contains("corrupt deflate stream") || e.to_string().contains("corrupt xz stream") {
                    utils::mark_file_bad(apk_file)?;
                    // Restore original working directory before returning error
                    std::env::set_current_dir(original_dir)?;
                    return Err(eyre::eyre!("Corrupt compression stream detected in APK package {}: {} (file marked as .bad)", apk_file.display(), e));
                }
                continue; // Skip other problematic entries
            }
        };

        let path = match entry.path() {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(e) => {
                log::error!("Error getting path from tar entry: {}", e);
                // If it's a corrupt compression stream, mark the file as bad and fail
                if e.to_string().contains("corrupt deflate stream") || e.to_string().contains("corrupt xz stream") {
                    utils::mark_file_bad(apk_file)?;
                    // Restore original working directory before returning error
                    std::env::set_current_dir(original_dir)?;
                    return Err(eyre::eyre!("Corrupt compression stream detected in APK package {}: {} (file marked as .bad)", apk_file.display(), e));
                }
                continue; // Skip other problematic entries
            }
        };

        entries_processed += 1;
        log::trace!("Processing tar entry #{}: {}", entries_processed, path);

        // Create the target path - for dot files use just the filename, for others preserve path
        let target_path = if path.starts_with(".") {
            // For dot files, just use the filename part
            let file_name = Path::new(&path)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone());
            store_tmp_dir.join("info/apk").join(file_name)
        } else {
            // For regular files, preserve the full path under fs/
            Path::new(&path).to_path_buf()
        };

        // Ensure parent directory exists
        if let Some(parent) = target_path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                log::warn!("Failed to create directory {}: {}", parent.display(), e);
                continue;
            }
        }

        // Extract the file
        if let Err(e) = entry.unpack(&target_path) {
            log::error!("Failed to extract file {}: {}", path, e);
            // If it's a corrupt compression stream, mark the file as bad and fail
            if e.to_string().contains("corrupt deflate stream") || e.to_string().contains("corrupt xz stream") {
                utils::mark_file_bad(apk_file)?;
                // Restore original working directory before returning error
                std::env::set_current_dir(original_dir)?;
                return Err(eyre::eyre!("Corrupt compression stream detected while extracting file {} in APK package {}: {} (file marked as .bad)", path, apk_file.display(), e));
            }
            continue;
        }
        utils::fixup_file_permissions(&target_path);
    }

    // Restore original working directory
    std::env::set_current_dir(original_dir)?;

    log::debug!("Successfully unpacked APK package with {} tar entries", entries_processed);
    Ok(())
}

/// Maps APK scriptlet names to common scriptlet names and moves them to info/install/
pub fn create_scriptlets<P: AsRef<Path>>(store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let apk_dir = store_tmp_dir.join("info/apk");
    let install_dir = store_tmp_dir.join("info/install");

    // Mapping from APK scriptlet names to common names
    // APK scriptlet types: pre-install, post-install, pre-upgrade, post-upgrade, pre-deinstall, post-deinstall
    // Common names match ScriptletType::get_script_names() which uses pre_remove/post_remove for removals
    let scriptlet_mapping: HashMap<&str, &str> = [
        (".pre-install", "pre_install.sh"),
        (".post-install", "post_install.sh"),
        (".pre-deinstall", "pre_remove.sh"),
        (".post-deinstall", "post_remove.sh"),
        (".pre-upgrade", "pre_upgrade.sh"),
        (".post-upgrade", "post_upgrade.sh"),
    ].into_iter().collect();

    crate::utils::copy_scriptlets_by_mapping(&scriptlet_mapping, &apk_dir, &install_dir, false)?;

    Ok(())
}

/// Handle the case where arch in .PKGINFO and packages.txt adisagree
fn fixup_inconsistent_arch(raw_fields: &mut HashMap<String, Vec<String>>, pkgkey: Option<&str>) {
    // Handle arch field: if arch is "noarch" and we have a pkgkey, get the correct arch from repository
    if pkgkey.is_none() {
        log::debug!("No pkgkey provided, skipping arch fixup");
        return;
    }

    let pkgkey = pkgkey.unwrap();
    log::debug!("Processing pkgkey: {}", pkgkey);

    let arch = match crate::package::pkgkey2arch(pkgkey) {
        Ok(arch) => {
            log::debug!("Extracted arch '{}' from pkgkey '{}'", arch, pkgkey);
            arch
        },
        Err(e) => {
            log::warn!("Failed to extract arch from pkgkey '{}': {}", pkgkey, e);
            return;
        }
    };

    if let Some(arch_values) = raw_fields.get("arch") {
        if let Some(arch_value) = arch_values.first() {
            if arch_value != &arch {
                log::debug!("Warning: using arch '{}' instead of '{}' for {}", arch, arch_value, pkgkey);
                // Replace the arch value with the correct arch from pkgkey
                raw_fields.insert("arch".to_string(), vec![arch.clone()]);
            }
            return;
        }
    }

    log::debug!("Warning: no arch field found in .PKGINFO");
}

/// Parses the .PKGINFO file with improved validation and creates package.txt
pub fn create_package_txt<P: AsRef<Path>>(store_tmp_dir: P, pkgkey: Option<&str>) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let pkginfo_path = store_tmp_dir.join("info/apk/.PKGINFO");

    if !pkginfo_path.exists() {
        return Err(eyre::eyre!(".PKGINFO file not found: {}", pkginfo_path.display()));
    }

    let pkginfo_content = fs::read_to_string(&pkginfo_path)?;
    let mut raw_fields: HashMap<String, Vec<String>> = HashMap::new();

    // Parse the .PKGINFO file with strict format validation
    for (line_num, line) in pkginfo_content.lines().enumerate() {
        let line = line.trim();

        // Skip comments and empty lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Strict parsing: must be exactly "key = value" format
        if let Some((key, value)) = line.split_once(" = ") {
            let key = key.trim().to_string();
            let value = value.trim().to_string();

            // Validate field name against known PKGINFO fields
            if let Some(field_def) = PKGINFO_FIELDS.get(key.as_str()) {
                if field_def.repeatable {
                    raw_fields.entry(key).or_insert_with(Vec::new).push(value);
                } else {
                    if raw_fields.contains_key(&key) {
                        log::warn!("Duplicate non-repeatable field '{}' at line {}", key, line_num + 1);
                    }
                    raw_fields.insert(key, vec![value]);
                }
            } else {
                log::warn!("Unknown PKGINFO field '{}' at line {}", key, line_num + 1);
                raw_fields.entry(key).or_insert_with(Vec::new).push(value);
            }
        } else {
            log::warn!("Invalid PKGINFO line format at line {}: {}", line_num + 1, line);
        }
    }

    fixup_inconsistent_arch(&mut raw_fields, pkgkey);

    // Map field names using PACKAGE_KEY_MAPPING and prepare final fields
    let mut package_fields: HashMap<String, String> = HashMap::new();
    let mut conflicts_values = Vec::new();

    for (original_field, values) in raw_fields {
        let mapped_field = PACKAGE_KEY_MAPPING
            .get(original_field.as_str())
            .unwrap_or(&original_field.as_str())
            .to_string();

        // Special handling for "depend" field: separate conflicts (starting with '!') from regular requires
        if original_field == "depend" {
            let mut requires = Vec::new();
            let mut conflicts = Vec::new();

            for value in &values {
                // Split by whitespace in case there are multiple dependencies in one value
                for dep in value.split_whitespace() {
                    if dep.starts_with('!') {
                        // Conflict: remove '!' prefix and add to conflicts
                        conflicts.push(dep[1..].to_string());
                    } else {
                        // Regular dependency: add to requires
                        requires.push(dep.to_string());
                    }
                }
            }

            // Add requires field if there are any regular dependencies
            if !requires.is_empty() {
                let requires_value = if requires.len() > 1 {
                    requires.join(", ")
                } else {
                    requires.into_iter().next().unwrap_or_default()
                };
                package_fields.insert("requires".to_string(), requires_value);
            }

            // Collect conflicts to add later
            conflicts_values.extend(conflicts);
        } else {
            // Join multiple values with commas for repeatable fields
            let combined_value = if values.len() > 1 {
                values.join(", ")
            } else {
                values.into_iter().next().unwrap_or_default()
            };

            package_fields.insert(mapped_field, combined_value);
        }
    }

    // Add conflicts field if there are any conflicts
    if !conflicts_values.is_empty() {
        let conflicts_value = if conflicts_values.len() > 1 {
            conflicts_values.join(", ")
        } else {
            conflicts_values.into_iter().next().unwrap_or_default()
        };
        package_fields.insert("conflicts".to_string(), conflicts_value);
    }

    package_fields.insert("format".to_string(), "apk".to_string());

    if let Some(triggers_str) = package_fields.get("triggers") {
        write_apk_hook_file(store_tmp_dir, triggers_str)?;
    }

    // Use the general store function to save the package.txt file
    crate::store::save_package_txt(package_fields, store_tmp_dir, pkgkey)?;

    Ok(())
}

/// APK (Alpine Linux) triggers support
///
/// APK triggers monitor directories and execute trigger scripts when files in those directories
/// are modified during package installation, upgrade, or removal.
///
/// Triggers are converted to Arch-style .hook files during package unpacking, allowing them
/// to be handled by the unified hooks infrastructure.
///
/// References:
/// - https://wiki.alpinelinux.org/wiki/APKBUILD_Reference
/// - /c/package-managers/apk-tools/doc/apk-package.5.scd
/// - /c/package-managers/apk-tools/src/commit.c run_triggers()
/// - /c/package-managers/apk-tools/src/package.c apk_ipkg_run_script()
/// - grep -h triggers ~/.epkg/store/*/info/apk/.PKGINFO
/// - head ~/.epkg/store/*/info/apk/.trigger

/// Write APK trigger hooks as Arch-style .hook files
/// Similar to extract_rpm_triggers() and write_deb_trigger_hooks()
///
/// Creates .hook files in info/install/ that will be picked up by the hooks infrastructure.
/// Each trigger pattern becomes a Path-type hook that executes the .trigger script.
fn write_apk_hook_file<P: AsRef<Path>>(
    store_tmp_dir: P,
    triggers_str: &str,
) -> Result<()> {
    use std::fmt::Write;

    let trigger_patterns: Vec<String> = triggers_str
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    if trigger_patterns.is_empty() {
        return Ok(());
    }

    // Check if .trigger script exists in info/apk/.trigger
    let store_tmp_dir = store_tmp_dir.as_ref();
    let trigger_script_path = store_tmp_dir.join("info/apk/.trigger");
    if !trigger_script_path.exists() {
        log::warn!("Package has triggers but no .trigger script, skipping trigger hook generation");
        return Ok(());
    }

    // Create a hook file with one [Trigger] section per trigger pattern
    // APK triggers run after package operations (PostTransaction)
    let mut buf = String::new();

    // Create one [Trigger] section per trigger pattern
    // so that match_path_trigger() bottom part can find the exact matched positive_targets[]
    for pattern in &trigger_patterns {
        // Handle "+" prefix (only pass when modified during transaction)
        // For now, we treat all patterns the same way
        let target = pattern.strip_prefix("+").unwrap_or(pattern);

        // [Trigger] section
        buf.push_str("[Trigger]\n");
        buf.push_str("Operation = Install\n");
        buf.push_str("Operation = Upgrade\n");
        buf.push_str("Operation = Remove\n");
        writeln!(buf, "Type = Path")?;
        writeln!(buf, "Target = {}", target)?;
        buf.push_str("\n");
    }

    // [Action] section
    buf.push_str("[Action]\n");
    writeln!(buf, "When = PostTransaction")?;
    writeln!(buf, "Description = APK trigger")?;
    // Exec points to the trigger script
    // The hook engine will pass matched directories as arguments
    writeln!(buf, "Exec = {}", trigger_script_path.to_string_lossy())?;
    writeln!(buf, "NeedsTargets")?; // Pass matched paths as arguments

    let install_dir = store_tmp_dir.join("info/install");
    let hook_path = install_dir.join("apk-trigger.hook");
    fs::create_dir_all(&install_dir)?;
    fs::write(&hook_path, buf)
        .with_context(|| format!("Failed to write APK hook file {}", hook_path.display()))?;

    Ok(())
}
