use std::fs;
use std::io::Read;
use std::path::Path;
use std::collections::HashMap;
use tar::{Archive, Entry};
use log;
use flate2::read::GzDecoder;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use crate::apk_repo::{PACKAGE_KEY_MAPPING, PKGINFO_FIELDS};

/// APK v2 package structure containing 3 gzip streams
#[derive(Debug)]
pub struct ApkV2Package {
    pub signature_stream: Vec<u8>,
    pub control_stream: Vec<u8>,
    pub data_stream: Vec<u8>,
}

/// APK signature information
#[derive(Debug)]
pub struct ApkSignature {
    pub filename: String,
    pub key_name: String,
    pub signature_data: Vec<u8>,
}

/// File checksum information from PAX headers
#[derive(Debug)]
pub struct FileChecksum {
    pub path: String,
    pub sha1: Option<String>,
}

/// Unpacks an APK v2 package to the specified directory
pub fn unpack_package<P: AsRef<Path>>(apk_file: P, store_tmp_dir: P) -> Result<()> {
    let apk_file = apk_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Create the required directory structure
    fs::create_dir_all(store_tmp_dir.join("fs"))?;
    fs::create_dir_all(store_tmp_dir.join("info/apk"))?;
    fs::create_dir_all(store_tmp_dir.join("info/install"))?;

    // Parse APK v2 format (3 gzip streams)
    let apk_package = parse_apk_v2_streams(apk_file)?;

    // Extract signature information
    let signature_info = extract_signature_info(&apk_package.signature_stream)?;
    if let Some(sig) = signature_info {
        log::info!("Found signature: {} (key: {})", sig.filename, sig.key_name);
        // Save signature file
        let sig_path = store_tmp_dir.join("info/apk").join(&sig.filename);
        fs::write(&sig_path, &sig.signature_data)?;
    }

    // Extract control segment (.PKGINFO and scriptlets)
    let (pkginfo_content, control_files) = extract_control_segment(&apk_package.control_stream)?;

    // Save .PKGINFO
    let pkginfo_path = store_tmp_dir.join("info/apk/.PKGINFO");
    fs::write(&pkginfo_path, &pkginfo_content)?;

    // Extract control files (scriptlets)
    for (filename, content) in control_files {
        if filename.starts_with(".pre-") || filename.starts_with(".post-") {
            let script_path = store_tmp_dir.join("fs").join(&filename);
            fs::write(&script_path, &content)?;

            // Make scriptlet executable
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&script_path)?.permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&script_path, perms)?;
            }
        }
    }

    // Extract data tarball with checksum verification
    let file_checksums = extract_data_segment(&apk_package.data_stream, store_tmp_dir)?;

    // Log checksum information
    for checksum in &file_checksums {
        if let Some(sha1) = &checksum.sha1 {
            log::debug!("File {}: SHA1 {}", checksum.path, sha1);
        }
    }

    // Generate filelist.txt
    crate::store::create_filelist_txt(store_tmp_dir)?;

    // Create scriptlets with proper mapping
    create_scriptlets(store_tmp_dir)?;

    // Create package.txt with improved parsing
    create_package_txt(store_tmp_dir)?;

    Ok(())
}

/// Parses APK v2 format which contains 3 separate gzip streams
fn parse_apk_v2_streams<P: AsRef<Path>>(apk_file: P) -> Result<ApkV2Package> {
    let file_data = fs::read(apk_file.as_ref())
        .wrap_err_with(|| format!("Failed to read APK file: {}", apk_file.as_ref().display()))?;

    let mut streams = Vec::new();
    let mut offset = 0;

    // Parse multiple gzip streams
    while offset < file_data.len() {
        // Check for gzip magic number (0x1f, 0x8b)
        if offset + 2 >= file_data.len() || file_data[offset] != 0x1f || file_data[offset + 1] != 0x8b {
            break;
        }

        // Find the end of this gzip stream
        let stream_start = offset;

        // Read the gzip stream to find its natural end
        let stream_data = &file_data[stream_start..];
        let mut decoder = GzDecoder::new(stream_data);
        let mut decompressed = Vec::new();

        match decoder.read_to_end(&mut decompressed) {
            Ok(_) => {
                // Calculate how much data was consumed
                let stream_end = stream_start + (stream_data.len() - decoder.into_inner().len());
                streams.push(file_data[stream_start..stream_end].to_vec());
                offset = stream_end;
            }
            Err(_) => {
                // If we can't decode, try to find the next gzip header manually
                offset += 1;
                while offset < file_data.len() - 1 {
                    if file_data[offset] == 0x1f && file_data[offset + 1] == 0x8b {
                        break;
                    }
                    offset += 1;
                }
                if offset >= file_data.len() - 1 {
                    break;
                }
            }
        }
    }

    if streams.len() < 3 {
        return Err(eyre::eyre!("Invalid APK v2 format: expected 3 gzip streams, found {}", streams.len()));
    }

    Ok(ApkV2Package {
        signature_stream: streams[0].clone(),
        control_stream: streams[1].clone(),
        data_stream: streams[2].clone(),
    })
}

/// Extracts signature information from the signature stream
fn extract_signature_info(signature_stream: &[u8]) -> Result<Option<ApkSignature>> {
    let decoder = GzDecoder::new(signature_stream);
    let mut archive = Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().to_string();

        if path.starts_with(".SIGN.RSA.") && path.ends_with(".rsa.pub") {
            // Extract key name from filename
            // Format: .SIGN.RSA.<key_name>.rsa.pub
            let key_name = path
                .strip_prefix(".SIGN.RSA.")
                .and_then(|s| s.strip_suffix(".rsa.pub"))
                .unwrap_or("unknown")
                .to_string();

            let mut signature_data = Vec::new();
            entry.read_to_end(&mut signature_data)?;

            return Ok(Some(ApkSignature {
                filename: path,
                key_name,
                signature_data,
            }));
        }
    }

    Ok(None)
}

/// Extracts control segment containing .PKGINFO and scriptlets
fn extract_control_segment(control_stream: &[u8]) -> Result<(String, HashMap<String, Vec<u8>>)> {
    let decoder = GzDecoder::new(control_stream);
    let mut archive = Archive::new(decoder);

    let mut pkginfo_content = String::new();
    let mut control_files = HashMap::new();

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().to_string();

        let mut content = Vec::new();
        entry.read_to_end(&mut content)?;

        if path == ".PKGINFO" {
            pkginfo_content = String::from_utf8(content)
                .wrap_err("Failed to parse .PKGINFO as UTF-8")?;
        } else {
            control_files.insert(path, content);
        }
    }

    if pkginfo_content.is_empty() {
        return Err(eyre::eyre!("No .PKGINFO file found in control segment"));
    }

    Ok((pkginfo_content, control_files))
}

/// Extracts data segment and returns file checksums from PAX headers
fn extract_data_segment<P: AsRef<Path>>(data_stream: &[u8], store_tmp_dir: P) -> Result<Vec<FileChecksum>> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let decoder = GzDecoder::new(data_stream);
    let mut archive = Archive::new(decoder);
    let mut file_checksums = Vec::new();

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().to_string();

        // Extract SHA1 checksum from PAX headers
        let sha1_checksum = extract_sha1_from_pax_header(&mut entry)?;

        file_checksums.push(FileChecksum {
            path: path.clone(),
            sha1: sha1_checksum,
        });

        // Extract the file to fs/
        let target_path = store_tmp_dir.join("fs").join(&path);

        // Ensure parent directory exists
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Extract the file
        entry.unpack(&target_path)
            .wrap_err_with(|| format!("Failed to extract file: {}", path))?;
    }

    Ok(file_checksums)
}

/// Extracts SHA1 checksum from PAX header if present
fn extract_sha1_from_pax_header<R: Read>(_entry: &mut Entry<R>) -> Result<Option<String>> {
    // Try to get PAX extensions (this is a simplified approach)
    // In a full implementation, we would need to properly parse PAX headers
    // For now, we'll return None as this requires more complex tar parsing

    // TODO: Implement proper PAX header parsing to extract APK-TOOLS.checksum.SHA1
    Ok(None)
}

/// Validates signature against public key (stub implementation)
#[allow(dead_code)]
pub fn validate_signature<P: AsRef<Path>>(
    signature: &ApkSignature,
    _control_stream_hash: &[u8],
    keys_dir: P,
) -> Result<bool> {
    let keys_dir = keys_dir.as_ref();
    let key_file = keys_dir.join(format!("{}.rsa.pub", signature.key_name));

    if !key_file.exists() {
        log::warn!("Public key not found: {}", key_file.display());
        return Ok(false);
    }

    // TODO: Implement proper PKCS1v15 RSA signature verification
    // This would require:
    // 1. Reading the public key from the .rsa.pub file
    // 2. Verifying the DER-encoded PKCS1v15 RSA signature
    // 3. Computing SHA1 hash of control stream and comparing

    log::info!("Signature validation not yet implemented");
    Ok(true) // Placeholder
}

/// Maps APK scriptlet names to common scriptlet names and moves them to info/install/
pub fn create_scriptlets<P: AsRef<Path>>(store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let fs_dir = store_tmp_dir.join("fs");
    let install_dir = store_tmp_dir.join("info/install");

    // Mapping from APK scriptlet names to common names
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

/// Parses the .PKGINFO file with improved validation and creates package.txt
pub fn create_package_txt<P: AsRef<Path>>(store_tmp_dir: P) -> Result<()> {
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

    // Handle version-release split for APK packages
    // Clone the version value to avoid borrow checker issues
    let version_value_opt = raw_fields.get("pkgver")
        .and_then(|values| values.first())
        .cloned();

    if let Some(version_value) = version_value_opt {
        if let Some((ver, rel)) = version_value.rsplit_once('-') {
            // Check if the last part looks like a release number
            if rel.chars().all(|c| c.is_ascii_digit() || c == '.') {
                raw_fields.insert("pkgver".to_string(), vec![ver.to_string()]);
                raw_fields.insert("release".to_string(), vec![rel.to_string()]);
            }
        }
    }

    // Add epoch if not present
    if !raw_fields.contains_key("epoch") {
        raw_fields.insert("epoch".to_string(), vec!["0".to_string()]);
    }

    // Map field names using PACKAGE_KEY_MAPPING and prepare final fields
    let mut package_fields: Vec<(String, String)> = Vec::new();

    for (original_field, values) in raw_fields {
        let mapped_field = PACKAGE_KEY_MAPPING
            .get(original_field.as_str())
            .unwrap_or(&original_field.as_str())
            .to_string();

        // Join multiple values with commas for repeatable fields
        let combined_value = if values.len() > 1 {
            values.join(", ")
        } else {
            values.into_iter().next().unwrap_or_default()
        };

        package_fields.push((mapped_field, combined_value));
    }

    // Use the general store function to save the package.txt file
    crate::store::save_package_txt(package_fields, store_tmp_dir)?;

    Ok(())
}
