use std::fs;
use std::io;
use std::path::Path;
use std::collections::HashMap;
use tar::Archive;
use log;
use flate2::read::GzDecoder;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use crate::apk_repo::PACKAGE_KEY_MAPPING;

/// Refer to: https://wiki.alpinelinux.org/wiki/Apk_spec#Example_of_PKGINFO

/// Unpacks an APK package to the specified directory
pub fn unpack_package<P: AsRef<Path>>(apk_file: P, store_tmp_dir: P) -> Result<()> {
    let apk_file = apk_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Create the required directory structure
    fs::create_dir_all(store_tmp_dir.join("fs"))?;
    fs::create_dir_all(store_tmp_dir.join("info/apk"))?;
    fs::create_dir_all(store_tmp_dir.join("info/install"))?;

    // Extract the APK archive (tar.gz format)
    extract_apk_archive(apk_file, store_tmp_dir)?;

    // Generate filelist.txt
    crate::store::create_filelist_txt(store_tmp_dir)?;

    // Create scriptlets
    create_scriptlets(store_tmp_dir)?;

    // Create package.txt
    create_package_txt(store_tmp_dir)?;

    Ok(())
}

/// Extracts an APK archive (tar.gz format) and processes the contents
fn extract_apk_archive<P: AsRef<Path>>(apk_file: P, store_tmp_dir: P) -> Result<()> {
    let apk_file = apk_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Open the APK file (tar.gz format)
    let file = fs::File::open(apk_file)
        .wrap_err_with(|| format!("Failed to open APK file: {}", apk_file.display()))?;

    // Create gzip decoder
    let gz_decoder = GzDecoder::new(file);
    let mut archive = Archive::new(gz_decoder);

    // Extract all entries
    let entries = archive.entries()
        .wrap_err("Failed to read APK archive entries")?;

    for entry_result in entries {
        let mut entry = entry_result
            .wrap_err("Failed to read APK archive entry")?;

        // Get path and convert to owned string to avoid borrowing issues
        let path = entry.path()
            .wrap_err("Failed to get entry path")?;
        let path_str = path.to_string_lossy().into_owned();

        // Handle special APK files
        if path_str == ".PKGINFO" {
            // Extract .PKGINFO to info/apk/
            let pkginfo_path = store_tmp_dir.join("info/apk/.PKGINFO");
            let mut pkginfo_file = fs::File::create(&pkginfo_path)?;
            io::copy(&mut entry, &mut pkginfo_file)?;
        } else if path_str.starts_with(".SIGN.RSA.") {
            // Extract signature files to info/apk/
            let sig_path = store_tmp_dir.join("info/apk").join(&path_str);
            if let Some(parent) = sig_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut sig_file = fs::File::create(&sig_path)?;
            io::copy(&mut entry, &mut sig_file)?;
        } else if path_str.starts_with(".pre-") || path_str.starts_with(".post-") {
            // These are scriptlets, extract them to fs/ for now (will be moved in create_scriptlets)
            let scriptlet_path = store_tmp_dir.join("fs").join(&path_str);
            if let Some(parent) = scriptlet_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut scriptlet_file = fs::File::create(&scriptlet_path)?;
            io::copy(&mut entry, &mut scriptlet_file)?;

            // Make scriptlet executable
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&scriptlet_path)?.permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&scriptlet_path, perms)?;
            }
        } else {
            // Regular filesystem content - extract to fs/
            let target_path = store_tmp_dir.join("fs").join(&path_str);

            // Ensure parent directory exists
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)?;
            }

            // Extract the file
            entry.unpack(&target_path)
                .wrap_err_with(|| format!("Failed to extract file: {}", path_str))?;
        }
    }

    Ok(())
}

/// Maps APK scriptlet names to common scriptlet names and moves them to info/install/
pub fn create_scriptlets<P: AsRef<Path>>(store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let fs_dir = store_tmp_dir.join("fs");
    let install_dir = store_tmp_dir.join("info/install");

    // Mapping from APK scriptlet names to common names
    // Based on gen-install-scriptlets.sh, but using the updated names from deb_pkg.rs
    let scriptlet_mapping: HashMap<&str, Vec<&str>> = [
        (".pre-install", vec!["pre_install.sh", "pre_upgrade.sh"]),
        (".post-install", vec!["post_install.sh", "post_upgrade.sh"]),
        (".pre-deinstall", vec!["pre_uninstall.sh"]),
        (".post-deinstall", vec!["post_uninstall.sh"]),
        (".pre-upgrade", vec!["pre_upgrade.sh"]),
        (".post-upgrade", vec!["post_upgrade.sh"]),
    ].into_iter().collect();

    for (apk_script, common_scripts) in &scriptlet_mapping {
        let apk_script_path = fs_dir.join(apk_script);
        if apk_script_path.exists() {
            for common_script in common_scripts {
                let target_path = install_dir.join(common_script);

                // Copy the script content
                let content = fs::read(&apk_script_path)?;
                fs::write(&target_path, &content)?;

                // Make it executable
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = fs::metadata(&target_path)?.permissions();
                    perms.set_mode(0o755);
                    fs::set_permissions(&target_path, perms)?;
                }
            }

            // Remove the original scriptlet from fs/
            fs::remove_file(&apk_script_path)?;
        }
    }

    Ok(())
}

/// Parses the .PKGINFO file and creates package.txt with mapped field names
pub fn create_package_txt<P: AsRef<Path>>(store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let pkginfo_path = store_tmp_dir.join("info/apk/.PKGINFO");

    if !pkginfo_path.exists() {
        return Err(eyre::eyre!(".PKGINFO file not found: {}", pkginfo_path.display()));
    }

    let pkginfo_content = fs::read_to_string(&pkginfo_path)?;
    let mut raw_fields: Vec<(String, String)> = Vec::new();

    // Parse the .PKGINFO file (key = value format)
    for line in pkginfo_content.lines() {
        let line = line.trim();

        // Skip comments and empty lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some((key, value)) = line.split_once(" = ") {
            let key = key.trim().to_string();
            let value = value.trim().to_string();

            // Handle multiple values for the same key (e.g., multiple dependencies)
            if let Some(existing_index) = raw_fields.iter().position(|(k, _)| k == &key) {
                // Append to existing value with comma separator
                raw_fields[existing_index].1.push_str(", ");
                raw_fields[existing_index].1.push_str(&value);
            } else {
                raw_fields.push((key, value));
            }
        }
    }

    // Handle version-release split for APK packages
    // APK version format is often "version-release"
    if let Some(version_index) = raw_fields.iter().position(|(k, _)| k == "pkgver") {
        let version_value = raw_fields[version_index].1.clone();
        if let Some((ver, rel)) = version_value.rsplit_once('-') {
            // Check if the last part looks like a release number
            if rel.chars().all(|c| c.is_ascii_digit() || c == '.') {
                raw_fields[version_index].1 = ver.to_string();
                raw_fields.push(("release".to_string(), rel.to_string()));
            }
        }
    }

    // Add epoch if not present
    if !raw_fields.iter().any(|(k, _)| k == "epoch") {
        raw_fields.push(("epoch".to_string(), "0".to_string()));
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

    // Use the general store function to save the package.txt file
    crate::store::save_package_txt(package_fields, store_tmp_dir)?;

    Ok(())
}
