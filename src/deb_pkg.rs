use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::collections::HashMap;
use tar::Archive;
use log;
use flate2::read::GzDecoder;
use liblzma::read::XzDecoder;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use crate::deb_repo::PACKAGE_KEY_MAPPING;

/// Unpacks a Debian package to the specified directory
pub fn unpack_package<P: AsRef<Path>>(deb_file: P, store_tmp_dir: P, pkgkey: Option<&str>) -> Result<()> {
    let deb_file = deb_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Create the required directory structure
    fs::create_dir_all(store_tmp_dir.join("fs"))?;
    fs::create_dir_all(store_tmp_dir.join("info/deb"))?;
    fs::create_dir_all(store_tmp_dir.join("info/install"))?;

    // Extract the AR archive and process tar files
    extract_ar_archive(deb_file, store_tmp_dir)?;

    // Generate filelist.txt
    crate::store::create_filelist_txt(store_tmp_dir)?;

    // Create scriptlets
    create_scriptlets(store_tmp_dir)?;

    // Parse and store DEB triggers
    parse_deb_triggers(store_tmp_dir)?;

    // Create package.txt
    create_package_txt(deb_file, store_tmp_dir, pkgkey)?;

    Ok(())
}

/// Extracts an AR archive from a Debian package file and processes the tar files
fn extract_ar_archive<P: AsRef<Path>>(deb_file: P, store_tmp_dir: P) -> Result<()> {
    let deb_file = deb_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Open the AR archive
    let file = fs::File::open(deb_file)
        .wrap_err_with(|| format!("Failed to open deb file: {}", deb_file.display()))?;

    let mut archive = ar::Archive::new(file);
    let mut data_tar_path = None;
    let mut control_tar_path = None;

    // Extract AR archive entries
    while let Some(entry_result) = archive.next_entry() {
        let mut entry = entry_result
            .wrap_err("Failed to read AR archive entry")?;

        let header = entry.header().clone();
        let identifier = std::str::from_utf8(header.identifier())
            .wrap_err("Invalid UTF-8 in AR entry identifier")?;

        match identifier {
            "data.tar.gz" | "data.tar.xz" | "data.tar.zst" | "data.tar" => {
                let temp_path = store_tmp_dir.join(identifier);
                let mut temp_file = fs::File::create(&temp_path)?;
                io::copy(&mut entry, &mut temp_file)?;
                data_tar_path = Some(temp_path);
            }
            "control.tar.gz" | "control.tar.xz" | "control.tar.zst" | "control.tar" => {
                let temp_path = store_tmp_dir.join(identifier);
                let mut temp_file = fs::File::create(&temp_path)?;
                io::copy(&mut entry, &mut temp_file)?;
                control_tar_path = Some(temp_path);
            }
            _ => {
                // Skip other entries like debian-binary
                continue;
            }
        }
    }

    // Extract data.tar to fs/
    if let Some(data_tar) = data_tar_path {
        extract_tar(&data_tar, &store_tmp_dir.join("fs"))?;
        fs::remove_file(&data_tar)?;
    } else {
        return Err(eyre::eyre!("No data.tar found in deb archive"));
    }

    // Extract control.tar to info/deb/
    if let Some(control_tar) = control_tar_path {
        extract_tar(&control_tar, &store_tmp_dir.join("info/deb"))?;
        fs::remove_file(&control_tar)?;
    } else {
        return Err(eyre::eyre!("No control.tar found in deb archive"));
    }

    Ok(())
}

/// Extracts a tar archive (with automatic compression detection) to the target directory
fn extract_tar<P: AsRef<Path>>(tar_path: P, target_dir: P) -> Result<()> {
    let tar_path = tar_path.as_ref();
    let target_dir = target_dir.as_ref();

    fs::create_dir_all(target_dir)?;

    let file = fs::File::open(tar_path)?;
    let filename = tar_path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    let reader: Box<dyn Read> = if filename.ends_with(".gz") {
        Box::new(GzDecoder::new(file))
    } else if filename.ends_with(".xz") {
        Box::new(XzDecoder::new(file))
    } else if filename.ends_with(".zst") {
        Box::new(zstd::stream::Decoder::new(file)?)
    } else {
        Box::new(file)
    };

    let mut archive = Archive::new(reader);
    archive.unpack(target_dir)
        .wrap_err_with(|| format!("Failed to extract tar archive: {}", tar_path.display()))?;

    Ok(())
}

/// Maps Debian scriptlet names to common scriptlet names and moves them to info/install/
fn create_scriptlets<P: AsRef<Path>>(store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let deb_dir = store_tmp_dir.join("info/deb");
    let install_dir = store_tmp_dir.join("info/install");

    // Mapping from Debian scriptlet names to common names
    // Debian upgrade uses the same scripts as install
    let scriptlet_mapping: HashMap<&str, &str> = [
        ("preinst", "pre_install.sh"),
        ("postinst", "post_install.sh"),
        ("prerm", "pre_uninstall.sh"),
        ("postrm", "post_uninstall.sh"),
    ].into_iter().collect();

    crate::utils::copy_scriptlets_by_mapping(&scriptlet_mapping, &deb_dir, &install_dir, false)?;

    Ok(())
}

#[derive(Debug, Clone)]
struct TriggerEntry {
    name: String,
    await_mode: bool, // true = await, false = noawait
}

/// Parse DEB triggers file and store trigger information
/// Reference: man deb-triggers, /usr/share/doc/dpkg/spec/triggers.txt
/// Supports all trigger directive variants: interest, interest-await, interest-noawait,
/// activate, activate-await, activate-noawait
fn parse_deb_triggers<P: AsRef<Path>>(store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let deb_dir = store_tmp_dir.join("info/deb");
    let triggers_path = deb_dir.join("triggers");

    if !triggers_path.exists() {
        return Ok(());
    }

    let triggers_content = fs::read_to_string(&triggers_path)?;
    let (interest_triggers, activate_triggers) = parse_triggers_content(&triggers_content, &triggers_path)?;

    parse_deb_interest_triggers(&interest_triggers, store_tmp_dir)?;
    parse_deb_activate_triggers(&activate_triggers, store_tmp_dir)?;

    // Additionally, generate Arch-style .hook files under info/install/ so that
    // Debian triggers can be handled by the generic hooks engine. For now we
    // only emit hooks for file-style interest triggers (those whose trigger
    // name starts with '/'), mapping them to Path hooks that fire on any
    // install/upgrade/remove touching the path.
    write_deb_trigger_hooks(&interest_triggers, &activate_triggers, store_tmp_dir)?;

    Ok(())
}

/// Parse triggers file content into interest and activate trigger entries
/// Returns (interest_triggers, activate_triggers)
fn parse_triggers_content<P: AsRef<Path>>(
    triggers_content: &str,
    triggers_path: P,
) -> Result<(Vec<TriggerEntry>, Vec<TriggerEntry>)> {
    let triggers_path = triggers_path.as_ref();
    let mut interest_triggers: Vec<TriggerEntry> = Vec::new();
    let mut activate_triggers: Vec<TriggerEntry> = Vec::new();

    for (line_num, line) in triggers_content.lines().enumerate() {
        let line = line.trim();
        let line_num = line_num + 1; // 1-based line numbers

        // Skip comments and empty lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Parse trigger directives
        // Format: "<directive> <trigger-name>"
        // Directives: interest, interest-await, interest-noawait, activate, activate-await, activate-noawait
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        let directive = parts[0];
        let trigger_name = if parts.len() > 1 {
            parts[1..].join(" ")
        } else {
            // Legacy: simple trigger name without directive (treated as interest)
            if !line.contains(' ') {
                interest_triggers.push(TriggerEntry {
                    name: line.to_string(),
                    await_mode: true, // Default to await
                });
            } else {
                // Format: "<package> <path-pattern>" - file trigger interest
                interest_triggers.push(TriggerEntry {
                    name: line.to_string(),
                    await_mode: true,
                });
            }
            continue;
        };

        match directive {
            "interest" | "interest-await" => {
                interest_triggers.push(TriggerEntry {
                    name: trigger_name,
                    await_mode: true,
                });
            }
            "interest-noawait" => {
                interest_triggers.push(TriggerEntry {
                    name: trigger_name,
                    await_mode: false,
                });
            }
            "activate" | "activate-await" => {
                activate_triggers.push(TriggerEntry {
                    name: trigger_name,
                    await_mode: true,
                });
            }
            "activate-noawait" => {
                activate_triggers.push(TriggerEntry {
                    name: trigger_name,
                    await_mode: false,
                });
            }
            _ => {
                return Err(eyre::eyre!(
                    "Unknown trigger directive '{}' in triggers file '{}' at line {}",
                    directive,
                    triggers_path.display(),
                    line_num
                ));
            }
        }
    }

    Ok((interest_triggers, activate_triggers))
}

/// Parse and write DEB interest triggers
///
/// Output Layout:
/// ==============
/// Creates a single file in info/install/:
///
/// File: deb_interest.triggers
/// Format: One trigger name per line
/// Lines: "<trigger-name>" or "<trigger-name>/noawait"
/// - Without /noawait suffix: await mode (default)
/// - With /noawait suffix: noawait mode
///
/// Example:
/// mime-support
/// menu/noawait
/// package-name /etc/foo.conf
///
/// File is only created if interest_triggers is non-empty.
fn parse_deb_interest_triggers<P: AsRef<Path>>(interest_triggers: &[TriggerEntry], store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let install_dir = store_tmp_dir.join("info/install");

    // Write trigger metadata files with await mode information
    // Format: "<trigger-name>[/noawait]" (similar to dpkg's format)
    if !interest_triggers.is_empty() {
        let metadata_path = install_dir.join("deb_interest.triggers");
        let content: Vec<String> = interest_triggers.iter()
            .map(|t| {
                if t.await_mode {
                    t.name.clone()
                } else {
                    format!("{}/noawait", t.name)
                }
            })
            .collect();
        fs::write(&metadata_path, content.join("\n"))?;
    }

    Ok(())
}

/// Parse and write DEB activate triggers
///
/// Output Layout:
/// ==============
/// Creates a single file in info/install/:
///
/// File: deb_activate.triggers
/// Format: One trigger name per line
/// Lines: "<trigger-name>" or "<trigger-name>/noawait"
/// - Without /noawait suffix: await mode (default)
/// - With /noawait suffix: noawait mode
///
/// Example:
/// mime-support
/// menu/noawait
///
/// File is only created if activate_triggers is non-empty.
fn parse_deb_activate_triggers<P: AsRef<Path>>(activate_triggers: &[TriggerEntry], store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let install_dir = store_tmp_dir.join("info/install");

    // Write trigger metadata files with await mode information
    // Format: "<trigger-name>[/noawait]" (similar to dpkg's format)
    if !activate_triggers.is_empty() {
        let metadata_path = install_dir.join("deb_activate.triggers");
        let content: Vec<String> = activate_triggers.iter()
            .map(|t| {
                if t.await_mode {
                    t.name.clone()
                } else {
                    format!("{}/noawait", t.name)
                }
            })
            .collect();
        fs::write(&metadata_path, content.join("\n"))?;
    }

    Ok(())
}

/// Generate Arch-style .hook files under info/install/ for Debian triggers.
///
/// Current mapping (conservative, file-trigger only):
/// - For each interest trigger whose name starts with '/', we create a Path hook:
///   - [Trigger]:
///     - Operation = Install|Upgrade|Remove
///     - Type = Path
///     - Target = <trigger path as-is>
///   - [Action]:
///     - When = PostTransaction
///     - Exec = /bin/true          (no-op placeholder for now)
///
/// This allows the generic hooks engine to see where Debian file triggers
/// would conceptually fire, without changing the existing dpkg-style trigger
/// runtime in `deb_triggers.rs`.
fn write_deb_trigger_hooks<P: AsRef<Path>>(
    interest_triggers: &[TriggerEntry],
    activate_triggers: &[TriggerEntry],
    store_tmp_dir: P,
) -> Result<()> {
    use std::fmt::Write as FmtWrite;

    let store_tmp_dir = store_tmp_dir.as_ref();
    let install_dir = store_tmp_dir.join("info/install");

    if interest_triggers.is_empty() && activate_triggers.is_empty() {
        return Ok(());
    }

    fs::create_dir_all(&install_dir)?;

    // Generate hooks for interest triggers
    // These hooks will run when matching packages activate the trigger
    let mut hook_index: usize = 0;

    for entry in interest_triggers {
        let name = entry.name.trim();
        hook_index += 1;

        let mut buf = String::new();

        // Map await mode to When phase:
        // - noawait -> PostInstall (immediate, per-package processing)
        // - await -> PostTransaction (batched, after all packages are processed)
        let when_phase = if entry.await_mode {
            "PostTransaction"
        } else {
            "PostInstall"
        };

        // Determine trigger type: file trigger (starts with '/') or explicit trigger
        let (hook_type, target) = if name.starts_with('/') {
            // File trigger: Path type
            // Strip leading '/' from target path
            ("Path", name.strip_prefix('/').unwrap_or(name))
        } else {
            // Explicit trigger: Package type
            ("Package", name)
        };

        // [Trigger]
        buf.push_str("[Trigger]\n");
        buf.push_str("Operation = Install\n");
        buf.push_str("Operation = Upgrade\n");
        buf.push_str("Operation = Remove\n");
        writeln!(buf, "Type = {}", hook_type)?;
        writeln!(buf, "Target = {}", target)?;

        // [Action]
        buf.push_str("\n[Action]\n");
        writeln!(buf, "When = {}", when_phase)?;
        writeln!(
            buf,
            "Description = DEB {} trigger for {} (defer_mode={})",
            if hook_type == "Path" { "file" } else { "explicit" },
            target,
            if entry.await_mode { "await" } else { "noawait" }
        )?;
        // Exec will call the package's postinst with "triggered" argument
        // The hook engine will need to resolve the package context and call:
        // postinst triggered <trigger-name>
        // For now, use a placeholder that indicates this is a DEB trigger hook
        buf.push_str("Exec = /bin/true\n");

        let hook_name = if hook_type == "Path" {
            format!("deb-file-trigger-{}", hook_index)
        } else {
            format!("deb-explicit-trigger-{}", hook_index)
        };
        let hook_path = install_dir.join(format!("{}.hook", hook_name));
        fs::write(&hook_path, buf)
            .with_context(|| format!("Failed to write DEB hook file {}", hook_path.display()))?;
    }

    // Note: activate_triggers don't generate hooks directly - they are used
    // to match against interest triggers. The hook engine will need to check
    // which packages activate which triggers and match them against interest hooks.

    Ok(())
}

/// Parses the control file and creates package.txt with mapped field names
fn create_package_txt<P: AsRef<Path>>(deb_file: P, store_tmp_dir: P, pkgkey: Option<&str>) -> Result<()> {
    let deb_file = deb_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();
    let control_path = store_tmp_dir.join("info/deb/control");

    if !control_path.exists() {
        return Err(eyre::eyre!("Control file not found: {}", control_path.display()));
    }

    let control_content = fs::read_to_string(&control_path)?;
    let mut raw_fields: Vec<(String, String)> = Vec::new();
    let mut current_field = None;
    let mut current_value = String::new();

    // Parse the control file
    for line in control_content.lines() {
        if line.is_empty() {
            continue;
        }

        if line.starts_with(' ') || line.starts_with('\t') {
            // Continuation line
            if !current_value.is_empty() {
                current_value.push('\n');
            }
            current_value.push_str(line.trim());
        } else if let Some((key, value)) = line.split_once(": ") {
            // Save previous field if exists
            if let Some(field_name) = current_field.take() {
                raw_fields.push((field_name, current_value.clone()));
            }

            current_field = Some(key.to_string());
            current_value = value.to_string();
        }
    }

    // Save the last field
    if let Some(field_name) = current_field {
        raw_fields.push((field_name, current_value));
    }

    // Map field names using PACKAGE_KEY_MAPPING
    let mut package_fields: HashMap<String, String> = HashMap::new();

    for (original_field, value) in raw_fields {
        if original_field == "Description" {
            // Special handling for Description field - split into summary and description
            let lines: Vec<&str> = value.lines().collect();
            if !lines.is_empty() {
                // First line becomes summary
                package_fields.insert("summary".to_string(), lines[0].to_string());

                // Remaining lines become description (if any)
                if lines.len() > 1 {
                    let description_lines = &lines[1..];
                    let description_content = description_lines.join("\n");
                    // Apply proper indentation for multi-line descriptions
                    let indented_description = description_content.replace("\n", "\n ");
                    package_fields.insert("description".to_string(), indented_description);
                }
            }
        } else if let Some(mapped_field) = PACKAGE_KEY_MAPPING.get(original_field.as_str()) {
            let mut current_value = value; // `value` is the parsed field value String
            if *mapped_field == "installedSize" {
                // Debian original Installed-Size is in KB. Append "000" to represent bytes.
                // Assuming current_value is a string representation of a number.
                current_value.push_str("000");
            }
            package_fields.insert(mapped_field.to_string(), current_value);
        } else {
            log::warn!("Field name '{}' not found in predefined mapping list", original_field);
            // Include unmapped fields with their original names
            package_fields.insert(original_field, value);
        }
    }

    // Calculate SHA256 hash of the deb file and add it to raw_fields
    let sha256 = crate::store::calculate_file_sha256(deb_file)
        .wrap_err_with(|| format!("Failed to calculate SHA256 hash for deb file: {}", deb_file.display()))?;
    package_fields.insert("sha256".to_string(), sha256);

    package_fields.insert("format".to_string(), "deb".to_string());

    // Use the general store function to save the package.txt file
    crate::store::save_package_txt(package_fields, store_tmp_dir, pkgkey)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_description_field_splitting() {
        // Create a temporary directory for testing
        let temp_dir = TempDir::new().unwrap();
        let store_tmp_dir = temp_dir.path();

        // Create required directory structure
        let deb_dir = store_tmp_dir.join("info/deb");
        fs::create_dir_all(&deb_dir).unwrap();

        // Create a mock control file with multi-line Description
        let control_content = r#"Package: base-passwd
Version: 3.6.3
Priority: required
Section: admin
Maintainer: Colin Watson <cjwatson@debian.org>
Description: Debian base system master password and group files
 These are the canonical master copies of the user database files
 (/etc/passwd and /etc/group), containing the Debian-allocated user and
 group IDs. The update-passwd tool is provided to keep the system databases
 synchronized with these master files.
Architecture: all
"#;

        let control_path = deb_dir.join("control");
        fs::write(&control_path, control_content).unwrap();

        // Create a mock deb file for SHA256 calculation
        let mock_deb_file = store_tmp_dir.join("mock.deb");
        fs::write(&mock_deb_file, b"mock deb file content").unwrap();

        // Run the function - both arguments must be the same type
        let store_tmp_dir_buf = store_tmp_dir.to_path_buf();
        create_package_txt(&mock_deb_file, &store_tmp_dir_buf, None).unwrap();

        // Read the generated package.txt file
        let package_txt_path = store_tmp_dir.join("info/package.txt");
        assert!(package_txt_path.exists());

        let package_txt_content = fs::read_to_string(&package_txt_path).unwrap();
        println!("Generated package.txt content:\n{}", package_txt_content);

        // Verify the content contains both summary and description fields
        assert!(package_txt_content.contains("summary: Debian base system master password and group files"));
        assert!(package_txt_content.contains("description: These are the canonical master copies of the user database files"));
        assert!(package_txt_content.contains(" (/etc/passwd and /etc/group), containing the Debian-allocated user and"));
        assert!(package_txt_content.contains(" group IDs. The update-passwd tool is provided to keep the system databases"));
        assert!(package_txt_content.contains(" synchronized with these master files."));
    }
}
