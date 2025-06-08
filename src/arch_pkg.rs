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

/// Arch Linux package structure definition
#[derive(Debug)]
pub struct ArchPackage {
    pub pkginfo_content: String,
    pub install_script: Option<String>,
    pub mtree_content: Option<String>,
    pub buildinfo_content: Option<String>,
}

/// PKGINFO field definitions based on Arch Linux specification
pub struct PkgInfoField {
    pub name: &'static str,
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
/// Supports both .pkg.tar.zst and .pkg.tar.xz formats
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
    let mut arch_package = ArchPackage {
        pkginfo_content: String::new(),
        install_script: None,
        mtree_content: None,
        buildinfo_content: None,
    };

    extract_package_contents(archive, store_tmp_dir, &mut arch_package)
        .wrap_err("Failed to extract package contents")?;

    // Generate filelist.txt
    log::debug!("Creating filelist.txt");
    crate::store::create_filelist_txt(store_tmp_dir)
        .wrap_err_with(|| format!("Failed to create filelist.txt for {}", store_tmp_dir.display()))?;

    // Process install script if present
    if let Some(install_content) = arch_package.install_script {
        log::debug!("Processing install script");
        extract_install_scriptlets(&install_content, store_tmp_dir)
            .wrap_err("Failed to extract install scriptlets")?;
    }

    // Create package.txt with metadata from .PKGINFO
    log::debug!("Creating package.txt");
    create_package_txt(&arch_package.pkginfo_content, store_tmp_dir)
        .wrap_err("Failed to create package.txt")?;

    log::debug!("Arch Linux package unpacking completed successfully");
    Ok(())
}

/// Extract the contents of the package archive
fn extract_package_contents<R: Read>(
    mut archive: Archive<R>,
    store_tmp_dir: &Path,
    arch_package: &mut ArchPackage,
) -> Result<()> {
    let entries = archive.entries()
        .wrap_err("Failed to read entries from package archive")?;

    for entry_result in entries {
        let mut entry = entry_result
            .wrap_err("Failed to read entry from package archive")?;

        let path = entry.path()?.to_string_lossy().to_string();

        // Handle special files
        match path.as_str() {
            ".PKGINFO" => {
                log::debug!("Found .PKGINFO");
                let mut content = String::new();
                entry.read_to_string(&mut content)
                    .wrap_err("Failed to read .PKGINFO content")?;
                arch_package.pkginfo_content = content;

                // Save .PKGINFO to info/arch/
                let pkginfo_path = store_tmp_dir.join("info/arch/.PKGINFO");
                fs::write(&pkginfo_path, &arch_package.pkginfo_content)
                    .wrap_err_with(|| format!("Failed to write .PKGINFO to {}", pkginfo_path.display()))?;
            },
            ".INSTALL" => {
                log::debug!("Found .INSTALL");
                let mut content = String::new();
                entry.read_to_string(&mut content)
                    .wrap_err("Failed to read .INSTALL content")?;
                arch_package.install_script = Some(content.clone());

                // Save .INSTALL to fs/
                let install_path = store_tmp_dir.join("fs/.INSTALL");
                fs::write(&install_path, &content)
                    .wrap_err_with(|| format!("Failed to write .INSTALL to {}", install_path.display()))?;
            },
            ".MTREE" => {
                log::debug!("Found .MTREE");
                let mut content = String::new();
                entry.read_to_string(&mut content)
                    .wrap_err("Failed to read .MTREE content")?;
                arch_package.mtree_content = Some(content.clone());

                // Save .MTREE to info/arch/
                let mtree_path = store_tmp_dir.join("info/arch/.MTREE");
                fs::write(&mtree_path, &content)
                    .wrap_err_with(|| format!("Failed to write .MTREE to {}", mtree_path.display()))?;
            },
            ".BUILDINFO" => {
                log::debug!("Found .BUILDINFO");
                let mut content = String::new();
                entry.read_to_string(&mut content)
                    .wrap_err("Failed to read .BUILDINFO content")?;
                arch_package.buildinfo_content = Some(content.clone());

                // Save .BUILDINFO to info/arch/
                let buildinfo_path = store_tmp_dir.join("info/arch/.BUILDINFO");
                fs::write(&buildinfo_path, &content)
                    .wrap_err_with(|| format!("Failed to write .BUILDINFO to {}", buildinfo_path.display()))?;
            },
            ".Changelog" => {
                log::debug!("Found .Changelog");
                let mut content = String::new();
                entry.read_to_string(&mut content)
                    .wrap_err("Failed to read .Changelog content")?;

                // Save .Changelog to info/arch/
                let changelog_path = store_tmp_dir.join("info/arch/.Changelog");
                fs::write(&changelog_path, &content)
                    .wrap_err_with(|| format!("Failed to write .Changelog to {}", changelog_path.display()))?;
            },
            _ => {
                // Regular file, extract to fs directory
                if !path.starts_with('.') {
                    let target_path = store_tmp_dir.join("fs").join(&path);

                    // Ensure parent directory exists
                    if let Some(parent) = target_path.parent() {
                        fs::create_dir_all(parent)
                            .wrap_err_with(|| format!("Failed to create directory: {}", parent.display()))?;
                    }

                    // Extract the file
                    entry.unpack(&target_path)
                        .wrap_err_with(|| format!("Failed to extract file: {}", path))?;
                }
            }
        }
    }

    if arch_package.pkginfo_content.is_empty() {
        return Err(eyre::eyre!("No .PKGINFO file found in package"));
    }

    Ok(())
}

/// Extract install scriptlets from .INSTALL file
fn extract_install_scriptlets(install_content: &str, store_tmp_dir: &Path) -> Result<()> {
    log::debug!("Extracting install scriptlets");

    // Get all scriptlet names from SCRIPT_MAPPING
    let scriptlet_names: Vec<&str> = SCRIPT_MAPPING.keys().copied().collect();

    // Extract each scriptlet
    for (i, &scriptlet_name) in scriptlet_names.iter().enumerate() {
        // Determine the end pattern (next scriptlet or end of file)
        let end_pattern = if i < scriptlet_names.len() - 1 {
            scriptlet_names[i + 1]
        } else {
            ""
        };

        let start_pattern = scriptlet_name;
        let start_marker = format!("{}() {{", start_pattern);

        if let Some(start_pos) = install_content.find(&start_marker) {
            // Find the end of the scriptlet
            let end_pos = if end_pattern.is_empty() {
                // Last scriptlet, goes to the end of the file
                install_content.len()
            } else {
                // Find the next scriptlet or end of file
                let end_marker = format!("{}() {{", end_pattern);
                install_content[start_pos + start_marker.len()..]
                    .find(&end_marker)
                    .map_or(install_content.len(), |pos| start_pos + start_marker.len() + pos)
            };

            // Extract the scriptlet content
            let mut scriptlet_content = install_content[start_pos..end_pos].to_string();

            // Remove the function declaration line and closing brace
            if let Some(first_newline) = scriptlet_content.find('\n') {
                scriptlet_content = scriptlet_content[first_newline + 1..].to_string();

                // Remove the last closing brace if it exists
                if scriptlet_content.trim_end().ends_with('}') {
                    let last_brace = scriptlet_content.trim_end().rfind('}').unwrap();
                    scriptlet_content = scriptlet_content[..last_brace].trim_end().to_string();
                }

                // Map the scriptlet name to the standard name
                if let Some(standard_name) = SCRIPT_MAPPING.get(start_pattern) {
                    let script_path = store_tmp_dir.join(format!("info/install/{}", standard_name));
                    fs::write(&script_path, scriptlet_content)
                        .wrap_err_with(|| format!("Failed to write scriptlet to {}", script_path.display()))?;
                    log::debug!("Created scriptlet: {}", standard_name);
                }
            }
        }
    }

    Ok(())
}

/// Create package.txt from .PKGINFO content
fn create_package_txt(pkginfo_content: &str, store_tmp_dir: &Path) -> Result<()> {
    log::debug!("Creating package.txt from .PKGINFO");

    let mut package_data = HashMap::new();

    // Parse .PKGINFO content
    for line in pkginfo_content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(pos) = line.find(" = ") {
            let key = line[..pos].trim();
            let value = line[pos + 3..].trim();

            // Map the key to the standard key if possible
            let standard_key = PACKAGE_KEY_MAPPING.get(key).copied().unwrap_or(key);

            // Check if this is a repeatable field
            if let Some(field_info) = PKGINFO_FIELDS.get(key) {
                if field_info.repeatable {
                    // Append to existing values
                    let entry = package_data.entry(standard_key.to_string()).or_insert_with(Vec::new);
                    entry.push(value.to_string());
                } else {
                    // Single value field
                    package_data.insert(standard_key.to_string(), vec![value.to_string()]);
                }
            } else {
                // Unknown field, treat as single value
                package_data.insert(standard_key.to_string(), vec![value.to_string()]);
            }
        }
    }

    // Write package.txt
    let package_path = store_tmp_dir.join("info/package.txt");
    let mut package_content = String::new();

    for (key, values) in package_data {
        for value in values {
            package_content.push_str(&format!("{}={}\n", key, value));
        }
    }

    fs::write(&package_path, package_content)
        .wrap_err_with(|| format!("Failed to write package.txt to {}", package_path.display()))?;

    log::debug!("Successfully created package.txt");
    Ok(())
}
