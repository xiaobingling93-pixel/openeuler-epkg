use std::path::Path;
use color_eyre::Result;
use std::fs;
use std::collections::HashMap;
use rpm::{Package, FileMode};
use crate::rpm_repo::PACKAGE_KEY_MAPPING;
use color_eyre::eyre::WrapErr;
use std::fs::File;
use std::os::unix::fs::PermissionsExt;

/// Unpacks an RPM package to the specified directory
pub fn unpack_package<P: AsRef<Path>>(rpm_file: P, store_tmp_dir: P) -> Result<()> {
    let rpm_file = rpm_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Ensure the directory is created with desired permissions
    nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o022));
    // Create the required directory structure
    fs::create_dir_all(store_tmp_dir.join("info/rpm"))
        .wrap_err_with(|| format!("Failed to create info/rpm directory at {}", store_tmp_dir.join("info/rpm").display()))?;
    fs::create_dir_all(store_tmp_dir.join("info/install"))
        .wrap_err_with(|| format!("Failed to create info/install directory at {}", store_tmp_dir.join("info/install").display()))?;

    // Open and parse the RPM package
    let package = Package::open(rpm_file)
        .wrap_err_with(|| format!("Failed to open RPM file: {}", rpm_file.display()))?;

    // Extract files to fs/
    extract_rpm_files(&package, &store_tmp_dir.join("fs"))?;

    // Generate filelist.txt
    crate::store::create_filelist_txt(store_tmp_dir)?;

    // Create scriptlets
    create_scriptlets(&package, store_tmp_dir)?;

    // Create package.txt
    create_package_txt(&package, rpm_file, store_tmp_dir)?;

    Ok(())
}

/// Extracts RPM package files to the target directory
fn extract_rpm_files<P: AsRef<Path>>(package: &Package, target_dir: P) -> Result<()> {
    let target_dir = target_dir.as_ref();

    // Use the built-in extract() method from the rpm crate which properly extracts file contents
    package.extract(target_dir)
        .wrap_err_with(|| format!("Failed to extract RPM package to {}", target_dir.display()))?;

    Ok(())
}

/// Maps RPM scriptlet names to common scriptlet names and creates them in info/install/
pub fn create_scriptlets<P: AsRef<Path>>(package: &Package, store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let install_dir = store_tmp_dir.join("info/install");

    // Mapping from RPM scriptlet names to common names
    let scriptlet_mapping: HashMap<&str, Vec<&str>> = [
        ("prein", vec!["pre_install.sh"]),
        ("postin", vec!["post_install.sh"]),
        ("preun", vec!["pre_uninstall.sh"]),
        ("postun", vec!["post_uninstall.sh"]),
        ("pretrans", vec!["pre_upgrade.sh"]),
        ("posttrans", vec!["post_upgrade.sh"]),
    ].into_iter().collect();

    let metadata = &package.metadata;

    // Extract scriptlets using the correct methods
    for (rpm_script, common_scripts) in &scriptlet_mapping {
        if let Some(script_content) = get_scriptlet_content(metadata, rpm_script) {
            for common_script in common_scripts {
                let script_name = if script_content.trim_start().starts_with("--")
                    || script_content.contains("lua")
                    || script_content.contains("Lua") {
                    // If it's a Lua script, use .lua extension
                    format!("{}.lua", common_script.trim_end_matches(".sh"))
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
                    use std::os::unix::fs::PermissionsExt;
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

/// Helper function to get scriptlet content from package metadata
fn get_scriptlet_content(metadata: &rpm::PackageMetadata, scriptlet_name: &str) -> Option<String> {
    match scriptlet_name {
        "prein" => metadata.get_pre_install_script().ok().map(|s| s.script.clone()),
        "postin" => metadata.get_post_install_script().ok().map(|s| s.script.clone()),
        "preun" => metadata.get_pre_uninstall_script().ok().map(|s| s.script.clone()),
        "postun" => metadata.get_post_uninstall_script().ok().map(|s| s.script.clone()),
        "pretrans" => metadata.get_pre_trans_script().ok().map(|s| s.script.clone()),
        "posttrans" => metadata.get_post_trans_script().ok().map(|s| s.script.clone()),
        _ => None,
    }
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
        raw_fields.push(("group".to_string(), group.to_string()));
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

    // Add dependency information - using Debug format since Display is not implemented
    if let Ok(provides) = metadata.get_provides() {
        let provides_strs: Vec<String> = provides.iter()
            .map(|dep| format!("{:?}", dep))
            .collect();
        if !provides_strs.is_empty() {
            raw_fields.push(("provides".to_string(), provides_strs.join(", ")));
        }
    }

    if let Ok(requires) = metadata.get_requires() {
        let requires_strs: Vec<String> = requires.iter()
            .map(|dep| format!("{:?}", dep))
            .collect();
        if !requires_strs.is_empty() {
            raw_fields.push(("requires".to_string(), requires_strs.join(", ")));
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
