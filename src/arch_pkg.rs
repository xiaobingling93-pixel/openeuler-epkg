use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use tar::Archive;
use log;
use lazy_static::lazy_static;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use zstd::stream::read::Decoder as ZstdDecoder;
use crate::utils;
use crate::lfs;
use crate::tar_extract::{create_package_dirs, ExtractConfig, extract_archive_with_policy};

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
pub fn unpack_package<P: AsRef<Path>>(pkg_file: P, store_tmp_dir: P, pkgkey: Option<&str>) -> Result<()> {
    let pkg_file = pkg_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Create the required directory structure
    create_package_dirs(store_tmp_dir, "arch")?;

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
    if lfs::exists_on_host(&install_path) {
        log::debug!("Processing install script");
        let install_content = fs::read(&install_path)
            .wrap_err_with(|| format!("Failed to read .INSTALL file: {}", install_path.display()))?;
        extract_install_scriptlets(&install_content, store_tmp_dir)
            .wrap_err("Failed to extract install scriptlets")?;
    }

    // Create package.txt with metadata from .PKGINFO
    log::debug!("Creating package.txt");
    create_package_txt(store_tmp_dir, pkgkey)
        .wrap_err("Failed to create package.txt")?;

    log::debug!("Arch Linux package unpacking completed successfully");
    Ok(())
}

/// Path policy for Arch Linux packages
///
/// - Dot files (metadata like .PKGINFO, .INSTALL, .MTREE) go to info/arch/
/// - Regular files go to fs/
fn arch_path_policy(path: &Path, _is_hard_link: bool, store_tmp_dir: &Path) -> Option<PathBuf> {
    let path_str = path.to_string_lossy();

    if path_str.starts_with(".") {
        // Metadata files go to info/arch/
        Some(store_tmp_dir.join("info/arch").join(path))
    } else {
        // Regular files go to fs/
        Some(store_tmp_dir.join("fs").join(path))
    }
}

/// Extract the contents of the package archive using policy-based extraction
fn extract_package_contents<R: Read>(
    archive: Archive<R>,
    store_tmp_dir: &Path,
) -> Result<()> {
    let config = ExtractConfig::new(store_tmp_dir)
        .handle_hard_links(true);

    let policy: crate::tar_extract::PathPolicy = Box::new(arch_path_policy);
    let mut archive = archive;  // Make archive mutable
    let entries = extract_archive_with_policy(&mut archive, &config, policy)?;

    // Check if .PKGINFO was extracted
    let pkginfo_path = store_tmp_dir.join("info/arch/.PKGINFO");
    if !lfs::exists_on_host(&pkginfo_path) {
        return Err(eyre::eyre!("No .PKGINFO file found in package"));
    }

    log::debug!("Successfully unpacked Arch Linux package with {} tar entries", entries);
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
                // Pass script arguments ($1, $2, etc.) to the function using "$@"
                let wrapper_content = format!(
                    "#!/bin/sh
# Wrapper script for {scriptlet_name} function
THIS_SCRIPT_DIR=$(dirname \"$0\")
source \"$THIS_SCRIPT_DIR/../arch/.INSTALL\"
{scriptlet_name} \"$@\"
"
                );

                let script_path = store_tmp_dir.join(format!("info/install/{}", standard_name));
                fs::write(&script_path, wrapper_content)
                    .wrap_err_with(|| format!("Failed to write scriptlet wrapper to {}", script_path.display()))?;

                // Make the script executable
                utils::set_executable_permissions(&script_path, 0o755)?;

                log::debug!("Created scriptlet wrapper: {}", standard_name);
            }
        }
    }

    Ok(())
}

/// Create package.txt from .PKGINFO file in info/arch/
fn create_package_txt(store_tmp_dir: &Path, pkgkey: Option<&str>) -> Result<()> {
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
    let mut package_fields: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for (original_field, values) in raw_fields {
        let mapped_field = PACKAGE_KEY_MAPPING
            .get(original_field.as_str())
            .unwrap_or(&original_field.as_str())
            .to_string();

        // For repeatable fields, join values with comma
        let value = values.join(", ");
        package_fields.insert(mapped_field, value);
    }

    package_fields.insert("format".to_string(), "pacman".to_string());

    // Use the general store function to save the package.txt file
    crate::store::save_package_txt(package_fields, store_tmp_dir, pkgkey)?;

    log::debug!("Successfully created package.txt");
    Ok(())
}
