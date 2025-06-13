use std::fs;
use std::io::Read;
use std::path::Path;
use std::collections::HashMap;
use tar::Archive;
use log;
use lazy_static::lazy_static;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use zstd::stream::read::Decoder as ZstdDecoder;

/// PKGINFO field definitions based on Arch Linux specification
pub struct PkgInfoField {
    #[allow(dead_code)]
    pub name: &'static str,
    #[allow(dead_code)]
    pub description: &'static str,
    pub repeatable: bool,
}

lazy_static! {
    pub static ref PACKAGE_KEY_MAPPING: HashMap<&'static str, &'static str> = {
        let mut m = std::collections::HashMap::new();

        // Map Arch Linux PKGINFO field names to common field names
        m.insert("pkgname",         "pkgname");
        m.insert("pkgver",          "version");
        m.insert("pkgdesc",         "summary");
        m.insert("url",             "homepage");
        m.insert("builddate",       "buildTime");
        m.insert("packager",        "maintainer");
        m.insert("size",            "installedSize");
        m.insert("arch",            "arch");
        m.insert("license",         "license");
        m.insert("group",           "group");
        m.insert("depend",          "requires");
        m.insert("optdepend",       "suggests");
        m.insert("conflict",        "conflicts");
        m.insert("provides",        "provides");
        m.insert("backup",          "backup");
        m.insert("replaces",        "replaces");
        m.insert("makedepend",      "buildRequires");
        m.insert("checkdepend",     "checkRequires");

        m
    };

    pub static ref PKGINFO_FIELDS: HashMap<&'static str, PkgInfoField> = {
        let mut m = HashMap::new();
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
            description: "upstream URL",
            repeatable: false,
        });
        m.insert("builddate", PkgInfoField {
            name: "builddate",
            description: "build date",
            repeatable: false,
        });
        m.insert("packager", PkgInfoField {
            name: "packager",
            description: "packager",
            repeatable: false,
        });
        m.insert("size", PkgInfoField {
            name: "size",
            description: "package size",
            repeatable: false,
        });
        m.insert("arch", PkgInfoField {
            name: "arch",
            description: "architecture",
            repeatable: false,
        });
        m.insert("license", PkgInfoField {
            name: "license",
            description: "license",
            repeatable: true,
        });
        m.insert("depend", PkgInfoField {
            name: "depend",
            description: "dependency",
            repeatable: true,
        });
        m.insert("optdepend", PkgInfoField {
            name: "optdepend",
            description: "optional dependency",
            repeatable: true,
        });
        m.insert("conflict", PkgInfoField {
            name: "conflict",
            description: "conflict",
            repeatable: true,
        });
        m.insert("provides", PkgInfoField {
            name: "provides",
            description: "provided package",
            repeatable: true,
        });
        m.insert("replaces", PkgInfoField {
            name: "replaces",
            description: "replaced package",
            repeatable: true,
        });
        m.insert("backup", PkgInfoField {
            name: "backup",
            description: "backup file",
            repeatable: true,
        });
        m.insert("group", PkgInfoField {
            name: "group",
            description: "package group",
            repeatable: true,
        });
        m.insert("makedepend", PkgInfoField {
            name: "makedepend",
            description: "make dependency",
            repeatable: true,
        });
        m.insert("checkdepend", PkgInfoField {
            name: "checkdepend",
            description: "check dependency",
            repeatable: true,
        });
        m
    };

    pub static ref SCRIPT_MAPPING: HashMap<&'static str, &'static str> = {
        let mut m = HashMap::new();
        m.insert("pre_install",     "pre_install.sh");
        m.insert("post_install",    "post_install.sh");
        m.insert("pre_upgrade",     "pre_upgrade.sh");
        m.insert("post_upgrade",    "post_upgrade.sh");
        m.insert("pre_remove",      "pre_remove.sh");
        m.insert("post_remove",     "post_remove.sh");
        m
    };
}

/// Unpacks an Arch Linux package to the specified directory
pub fn unpack_package<P: AsRef<Path>>(pkg_file: P, store_tmp_dir: P) -> Result<()> {
    let pkg_file = pkg_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Create the required directory structure
    fs::create_dir_all(store_tmp_dir.join("fs"))?;
    fs::create_dir_all(store_tmp_dir.join("info/arch"))?;
    fs::create_dir_all(store_tmp_dir.join("info/install"))?;

    log::debug!("Unpacking Arch Linux package: {}", pkg_file.display());

    // Check if this is a zstd compressed package
    if !pkg_file.to_string_lossy().ends_with(".pkg.tar.zst") {
        return Err(eyre::eyre!("Unsupported Arch Linux package format: {}, only .pkg.tar.zst is supported", pkg_file.display()));
    }

    // Open the package file
    let file = fs::File::open(pkg_file)
        .wrap_err_with(|| format!("Failed to open package file: {}", pkg_file.display()))?;

    // Create zstd decoder
    log::debug!("Using zstd decompression");
    let decoder = ZstdDecoder::new(file)
        .wrap_err("Failed to create zstd decoder")?;
    let archive = Archive::new(decoder);

    // Extract package contents
    extract_package_contents(archive, store_tmp_dir)
        .wrap_err("Failed to extract package contents")?;

    // Generate filelist.txt
    log::debug!("Creating filelist.txt");
    crate::store::create_filelist_txt(store_tmp_dir)
        .wrap_err_with(|| format!("Failed to create filelist.txt for {}", store_tmp_dir.display()))?;

    // Check if .INSTALL file exists and process it
    let install_path = store_tmp_dir.join("info/arch/.INSTALL");
    if install_path.exists() {
        log::debug!("Processing install script");
        let install_content = fs::read(&install_path)
            .wrap_err_with(|| format!("Failed to read .INSTALL file: {}", install_path.display()))?;
        extract_install_scriptlets(&install_content, store_tmp_dir)
            .wrap_err("Failed to extract install scriptlets")?;
    }

    // Create package.txt with metadata from .PKGINFO
    log::debug!("Creating package.txt");
    create_package_txt(store_tmp_dir)
        .wrap_err("Failed to create package.txt")?;

    log::debug!("Arch Linux package unpacking completed successfully");
    Ok(())
}

/// Extract the contents of the package archive
fn extract_package_contents<R: Read>(
    mut archive: Archive<R>,
    store_tmp_dir: &Path,
) -> Result<()> {
    let entries = archive.entries()
        .wrap_err("Failed to read entries from package archive")?;

    let mut found_pkginfo = false;
    let mut entries_processed = 0;

    for entry_result in entries {
        let mut entry = entry_result
            .wrap_err("Failed to read entry from package archive")?;

        let path = entry.path()?.to_string_lossy().to_string();
        entries_processed += 1;
        log::debug!("Processing tar entry #{}: {}", entries_processed, path);

        // Create the target path - for dot files use info/arch/, for others use fs/
        let target_path = if path.starts_with(".") {
            // Special file, store in info/arch/
            if path == ".PKGINFO" {
                found_pkginfo = true;
            }
            log::debug!("Found special file: {}", path);
            store_tmp_dir.join("info/arch").join(&path)
        } else {
            // Regular file, preserve the full path in fs/
            store_tmp_dir.join("fs").join(&path)
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
            log::warn!("Failed to extract file {}: {}", path, e);
            continue;
        }
    }

    if !found_pkginfo {
        return Err(eyre::eyre!("No .PKGINFO file found in package"));
    }

    log::debug!("Successfully unpacked Arch Linux package with {} tar entries", entries_processed);
    Ok(())
}

/// Extract install scriptlets from .INSTALL file
///
/// This implementation creates wrapper scripts that source the original .INSTALL file
/// and then call the specific function. This approach ensures that when functions in
/// .INSTALL call each other (e.g., post_upgrade calling post_install), those calls
/// work correctly because all functions are available in the script's context.
///
/// Previously, we extracted each function's body separately, which broke dependencies
/// between functions in the .INSTALL file.
///
/// Another more simple option is to include whole content of .INSTALL into each scriptlet.
fn extract_install_scriptlets(install_content: &[u8], store_tmp_dir: &Path) -> Result<()> {
    log::debug!("Extracting install scriptlets");

    // Get all scriptlet names from SCRIPT_MAPPING
    let scriptlet_names: Vec<&str> = SCRIPT_MAPPING.keys().copied().collect();

    // Create wrapper scripts for each scriptlet function
    for &scriptlet_name in scriptlet_names.iter() {
        // Check if the function exists in the .INSTALL file
        let function_pattern = format!("{scriptlet_name}() {{");
        let install_content_str = String::from_utf8_lossy(install_content);

        if install_content_str.contains(&function_pattern) {
            // Map the scriptlet name to the standard name
            if let Some(standard_name) = SCRIPT_MAPPING.get(scriptlet_name) {
                // Create a wrapper script that sources the .INSTALL file and calls the function
                let wrapper_content = format!(
                    "#!/bin/sh
# Wrapper script for {scriptlet_name} function
THIS_SCRIPT_DIR=$(dirname \"$0\")
source \"$THIS_SCRIPT_DIR/../arch/.INSTALL\"
{scriptlet_name}
"
                );

                let script_path = store_tmp_dir.join(format!("info/install/{}", standard_name));
                fs::write(&script_path, wrapper_content)
                    .wrap_err_with(|| format!("Failed to write scriptlet wrapper to {}", script_path.display()))?;

                // Make the script executable
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = fs::metadata(&script_path)?.permissions();
                    perms.set_mode(0o755); // rwxr-xr-x
                    fs::set_permissions(&script_path, perms)?;
                }

                log::debug!("Created scriptlet wrapper: {}", standard_name);
            }
        }
    }

    Ok(())
}

/// Create package.txt from .PKGINFO file in info/arch/
fn create_package_txt(store_tmp_dir: &Path) -> Result<()> {
    log::debug!("Creating package.txt from .PKGINFO");

    // Read the .PKGINFO file
    let pkginfo_path = store_tmp_dir.join("info/arch/.PKGINFO");
    let pkginfo_content = fs::read_to_string(&pkginfo_path)
        .wrap_err_with(|| format!("Failed to read .PKGINFO file: {}", pkginfo_path.display()))?;

    let mut raw_fields: HashMap<String, Vec<String>> = HashMap::new();

    // Parse .PKGINFO content
    for line in pkginfo_content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(pos) = line.find(" = ") {
            let key = line[..pos].trim();
            let value = line[pos + 3..].trim();

            // Check if this is a repeatable field
            if let Some(field_info) = PKGINFO_FIELDS.get(key) {
                if field_info.repeatable {
                    // Append to existing values
                    raw_fields.entry(key.to_string()).or_insert_with(Vec::new).push(value.to_string());
                } else {
                    // Single value field
                    raw_fields.insert(key.to_string(), vec![value.to_string()]);
                }
            } else {
                // Unknown field, treat as single value
                raw_fields.insert(key.to_string(), vec![value.to_string()]);
            }
        }
    }

    // Map field names using PACKAGE_KEY_MAPPING and prepare final fields
    let mut package_fields: Vec<(String, String)> = Vec::new();

    for (original_field, values) in raw_fields {
        let mapped_field = PACKAGE_KEY_MAPPING
            .get(original_field.as_str())
            .unwrap_or(&original_field.as_str())
            .to_string();

        // For repeatable fields, add each value separately
        for value in values {
            package_fields.push((mapped_field.clone(), value));
        }
    }

    // Use the general store function to save the package.txt file
    crate::store::save_package_txt(package_fields, store_tmp_dir)?;

    log::debug!("Successfully created package.txt");
    Ok(())
}
