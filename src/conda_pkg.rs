use std::fs;
use std::path::Path;
use std::collections::HashMap;
use std::io::{Read, Seek};
use tar::Archive;
use log;
use lazy_static::lazy_static;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use bzip2::read::BzDecoder;
use zstd::stream::read::Decoder as ZstdDecoder;
use zip::ZipArchive;

lazy_static! {
    /// Mapping from Conda package metadata fields to common field names
    /// Based on conda-package-streaming implementation and conda index.json format
    pub static ref PACKAGE_KEY_MAPPING: HashMap<&'static str, &'static str> = {
        let mut m = HashMap::new();

        // Core package metadata from index.json (conda-package-streaming standard)
        m.insert("name",            "pkgname");
        m.insert("version",         "version");
        m.insert("summary",         "summary");
        m.insert("description",     "description");
        m.insert("url",             "homepage");
        m.insert("license",         "license");
        m.insert("license_family",  "licenseFamily");
        m.insert("build",           "buildString");
        m.insert("build_number",    "buildNumber");
        m.insert("timestamp",       "buildTime");
        m.insert("size",            "size");
        m.insert("arch",            "arch");
        m.insert("platform",        "platform");
        m.insert("subdir",          "subdir");

        // Dependencies and relationships (conda-package-streaming spec)
        m.insert("depends",         "requires");
        m.insert("constrains",      "constrains");
        m.insert("track_features",  "trackFeatures");
        m.insert("features",        "features");

        // File checksums
        m.insert("md5",             "md5sum");
        m.insert("sha256",          "sha256");

        // Conda-specific fields
        m.insert("noarch",          "noarch");
        m.insert("preferred_env",   "preferredEnv");

        m
    };

    /// Scriptlet mapping for Conda packages
    /// Based on conda-package-streaming link/unlink script handling
    pub static ref SCRIPT_MAPPING: HashMap<&'static str, Vec<&'static str>> = {
        let mut m = HashMap::new();

        // Conda link scripts (executed when package is installed/linked)
        m.insert("pre-link.sh",     vec!["pre_install.sh", "pre_upgrade.sh"]);
        m.insert("post-link.sh",    vec!["post_install.sh", "post_upgrade.sh"]);

        // Conda unlink scripts (executed when package is removed/unlinked)
        m.insert("pre-unlink.sh",   vec!["pre_uninstall.sh"]);
        m.insert("post-unlink.sh",  vec!["post_uninstall.sh"]);

        // Conda environment activation/deactivation scripts
        m.insert("activate.sh",     vec!["activate.sh"]);
        m.insert("deactivate.sh",   vec!["deactivate.sh"]);

        // Windows equivalents
        m.insert("pre-link.bat",    vec!["pre_install.bat", "pre_upgrade.bat"]);
        m.insert("post-link.bat",   vec!["post_install.bat", "post_upgrade.bat"]);
        m.insert("pre-unlink.bat",  vec!["pre_uninstall.bat"]);
        m.insert("post-unlink.bat", vec!["post_uninstall.bat"]);
        m.insert("activate.bat",    vec!["activate.bat"]);
        m.insert("deactivate.bat",  vec!["deactivate.bat"]);

        m
    };
}

/// Unpacks a Conda package to the specified directory
///
/// Conda packages can be in two formats:
/// 1. Legacy .tar.bz2 format (traditional conda packages)
/// 2. Modern .conda format (ZIP archive with separate info-*.tar.zst and pkg-*.tar.zst)
///
/// Based on conda-package-streaming implementation
pub fn unpack_package<P: AsRef<Path>>(conda_file: P, store_tmp_dir: P) -> Result<()> {
    let conda_file = conda_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Create the required directory structure following project pattern
    fs::create_dir_all(store_tmp_dir.join("fs"))?;
    fs::create_dir_all(store_tmp_dir.join("info/conda"))?;
    fs::create_dir_all(store_tmp_dir.join("info/install"))?;

    log::debug!("Unpacking Conda package: {}", conda_file.display());

    // Determine package format and extract accordingly
    // Following conda-package-streaming package detection logic
    let file_name = conda_file.file_name().and_then(|n| n.to_str()).unwrap_or("");

    if file_name.ends_with(".conda") {
        // Modern .conda format (ZIP archive with zstd-compressed tar components)
        unpack_conda_format(conda_file, store_tmp_dir)
            .wrap_err_with(|| format!("Failed to unpack .conda format: {}", conda_file.display()))?;
    } else if file_name.ends_with(".tar.bz2") {
        // Legacy .tar.bz2 format (single bzip2-compressed tar archive)
        unpack_tar_bz2_format(conda_file, store_tmp_dir)
            .wrap_err_with(|| format!("Failed to unpack .tar.bz2 format: {}", conda_file.display()))?;
    } else {
        return Err(eyre::eyre!("Unsupported Conda package format: {}", file_name));
    }

    // Generate filelist.txt following project pattern
    log::debug!("Creating filelist.txt");
    crate::store::create_filelist_txt(store_tmp_dir)
        .wrap_err_with(|| format!("Failed to create filelist.txt for {}", store_tmp_dir.display()))?;

    // Create package.txt from metadata
    log::debug!("Creating package.txt");
    create_package_txt(store_tmp_dir)
        .wrap_err_with(|| format!("Failed to create package.txt for {}", store_tmp_dir.display()))?;

    // Create scriptlets (if any)
    log::debug!("Creating scriptlets");
    create_scriptlets(store_tmp_dir)
        .wrap_err_with(|| format!("Failed to create scriptlets for {}", store_tmp_dir.display()))?;

    log::debug!("Conda package unpacking completed successfully");
    Ok(())
}

/// Unpacks modern .conda format packages (ZIP archives with separate info/pkg components)
/// Based on conda-package-streaming.package_streaming.stream_conda_component implementation
fn unpack_conda_format<P: AsRef<Path>>(conda_file: P, store_tmp_dir: &Path) -> Result<()> {
    let conda_file = conda_file.as_ref();

    // Validate file exists and is readable
    let metadata = fs::metadata(conda_file)
        .wrap_err_with(|| format!("Failed to read file metadata: {}", conda_file.display()))?;

    let file_size = metadata.len();
    if file_size == 0 {
        return Err(eyre::eyre!(
            "File is empty (0 bytes): {}. The download may be incomplete or the file may be corrupted.",
            conda_file.display()
        ));
    }

    // Check ZIP magic bytes (PK header) to verify it's a ZIP file
    let mut file = fs::File::open(conda_file)
        .wrap_err_with(|| format!("Failed to open file: {}", conda_file.display()))?;

    let mut magic_bytes = [0u8; 2];
    file.read_exact(&mut magic_bytes)
        .wrap_err_with(|| format!("Failed to read file header: {}", conda_file.display()))?;
    file.seek(std::io::SeekFrom::Start(0))
        .wrap_err_with(|| format!("Failed to seek to start: {}", conda_file.display()))?;

    // ZIP files start with "PK" (0x50 0x4B)
    if magic_bytes != [0x50, 0x4B] {
        return Err(eyre::eyre!(
            "File does not appear to be a valid ZIP archive (missing PK header): {}. File size: {} bytes. The file may be corrupted or incomplete.",
            conda_file.display(),
            file_size
        ));
    }

    // Try to open the ZIP archive
    // The central directory end is at the end of the file, so if it's missing, the download is incomplete
    let mut archive = match ZipArchive::new(file) {
        Ok(archive) => archive,
        Err(e) => {
            // Check if the error is about missing central directory end
            let error_msg = e.to_string();
            if error_msg.contains("central directory end") || error_msg.contains("Could not find") {
                return Err(eyre::eyre!(
                    "Incomplete .conda package file: {}\n\
                    File size: {} bytes\n\
                    The ZIP archive is missing the central directory end, which indicates the download did not complete.\n\
                    \n\
                    Solution: Delete the incomplete file and re-download the package:\n\
                    1. Delete: {}\n\
                    2. Re-run the install command to re-download the package",
                    conda_file.display(),
                    file_size,
                    conda_file.display()
                ));
            }
            return Err(eyre::eyre!(
                "Failed to open .conda archive: {}\n\
                File size: {} bytes\n\
                Error: {}\n\
                The ZIP archive may be corrupted or incomplete.",
                conda_file.display(),
                file_size,
                e
            ));
        }
    };

    // Extract stem name for component identification
    // Following conda-package-streaming logic: info-{stem}.tar.zst and pkg-{stem}.tar.zst
    let package_stem = conda_file.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("package");

    let mut info_component = None;
    let mut pkg_component = None;

    // Find info and pkg components within the ZIP
    // Based on conda-package-streaming component detection
    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        let name = entry.name();

        if name.starts_with(&format!("info-{}", package_stem)) && name.ends_with(".tar.zst") {
            info_component = Some(name.to_string());
        } else if name.starts_with(&format!("pkg-{}", package_stem)) && name.ends_with(".tar.zst") {
            pkg_component = Some(name.to_string());
        }
    }

    // Extract info component to info/conda/
    // Following conda-package-streaming.extract.extract_stream pattern
    // Strip "info/" prefix from paths since the info component tar contains paths like "info/index.json"
    if let Some(info_name) = info_component {
        log::debug!("Extracting info component: {}", info_name);
        let info_reader = archive.by_name(&info_name)?;
        extract_zstd_tar_stream(info_reader, &store_tmp_dir.join("info/conda"), Some("info/"))
            .wrap_err("Failed to extract info component")?;
    } else {
        return Err(eyre::eyre!("No info component found in .conda package"));
    }

    // Extract pkg component to fs/
    // Following conda-package-streaming component extraction logic
    if let Some(pkg_name) = pkg_component {
        log::debug!("Extracting pkg component: {}", pkg_name);
        let pkg_reader = archive.by_name(&pkg_name)?;
        extract_zstd_tar_stream(pkg_reader, &store_tmp_dir.join("fs"), None)
            .wrap_err("Failed to extract pkg component")?;
    } else {
        return Err(eyre::eyre!("No pkg component found in .conda package"));
    }

    Ok(())
}

/// Unpacks legacy .tar.bz2 format Conda packages
/// Based on conda-package-streaming.package_streaming implementation
fn unpack_tar_bz2_format<P: AsRef<Path>>(conda_file: P, store_tmp_dir: &Path) -> Result<()> {
    let conda_file = conda_file.as_ref();

    let file = fs::File::open(conda_file)?;
    let decoder = BzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    let mut entries_processed = 0;
    let mut found_index_json = false;

    // Extract all contents, following conda-package-streaming logic
    // .tar.bz2 format contains everything in a single tar archive
    for entry_result in archive.entries()? {
        let mut entry = entry_result?;
        let path = entry.path()?.to_path_buf();
        entries_processed += 1;

        log::trace!("Processing tar entry #{}: {}", entries_processed, path.display());

        // Determine target location based on path
        // Following conda-package-streaming path classification
        let target_path = if path.starts_with("info/") {
            // Metadata files go to info/conda/ (following project pattern)
            if path.ends_with("index.json") {
                found_index_json = true;
            }
            store_tmp_dir.join("info/conda").join(path.strip_prefix("info/").unwrap())
        } else {
            // Regular files go to fs/ (following project pattern)
            store_tmp_dir.join("fs").join(&path)
        };

        // Ensure parent directory exists
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Extract the file
        entry.unpack(&target_path)?;
        crate::utils::fixup_file_permissions(&target_path);
    }

    // Verify required metadata exists
    if !found_index_json {
        return Err(eyre::eyre!("No index.json found in Conda package"));
    }

    log::debug!("Successfully unpacked .tar.bz2 Conda package with {} entries", entries_processed);
    Ok(())
}

/// Extracts a zstd-compressed tar stream
/// Based on conda-package-streaming's zstandard decompression approach
///
/// If `strip_prefix` is provided, paths starting with that prefix will have it stripped.
fn extract_zstd_tar_stream<R: Read>(reader: R, target_dir: &Path, strip_prefix: Option<&str>) -> Result<()> {
    fs::create_dir_all(target_dir)?;

    let decoder = ZstdDecoder::new(reader)
        .wrap_err("Failed to create zstd decoder")?;
    let mut archive = Archive::new(decoder);

    // Extract with proper permission handling
    for entry_result in archive.entries()? {
        let mut entry = entry_result?;
        let mut path = entry.path()?.to_path_buf();

        // Strip prefix if specified
        if let Some(prefix) = strip_prefix {
            let prefix_path = Path::new(prefix);
            if let Ok(stripped) = path.strip_prefix(prefix_path) {
                path = stripped.to_path_buf();
            }
        }

        let target_path = target_dir.join(&path);

        // Ensure parent directory exists
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Extract the file
        entry.unpack(&target_path)?;
        crate::utils::fixup_file_permissions(&target_path);
    }

    Ok(())
}

/// Creates package.txt from Conda metadata files (index.json, etc.)
/// Based on conda-package-streaming metadata extraction approach
fn create_package_txt<P: AsRef<Path>>(store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let conda_info_dir = store_tmp_dir.join("info/conda");

    // Try to read index.json (primary metadata file)
    let index_json_path = conda_info_dir.join("index.json");
    if !index_json_path.exists() {
        return Err(eyre::eyre!("index.json not found in Conda package: {}", index_json_path.display()));
    }

    let index_content = fs::read_to_string(&index_json_path)
        .wrap_err_with(|| format!("Failed to read index.json: {}", index_json_path.display()))?;

    let index_data: serde_json::Value = serde_json::from_str(&index_content)
        .wrap_err("Failed to parse index.json")?;

    let mut package_fields: Vec<(String, String)> = Vec::new();

    // Extract fields from index.json and map them
    // Following conda-package-streaming metadata extraction pattern
    if let Some(object) = index_data.as_object() {
        for (key, value) in object {
            let mapped_key = PACKAGE_KEY_MAPPING
                .get(key.as_str())
                .unwrap_or(&key.as_str())
                .to_string();

            let string_value = match value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Array(arr) => {
                    // Join array elements with commas (for dependencies, etc.)
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
                _ => continue,
            };

            if !string_value.is_empty() {
                package_fields.push((mapped_key, string_value));
            }
        }
    }

    // Try to read additional metadata files if they exist
    // Following conda-package-streaming metadata completeness approach
    let files_path = conda_info_dir.join("files");
    if files_path.exists() {
        log::debug!("Found files metadata");
        // Could add file count or other file-related metadata here
    }

    let recipe_path = conda_info_dir.join("recipe");
    if recipe_path.exists() {
        log::debug!("Found recipe metadata");
        // Could extract additional recipe information here
    }

    // Save the package.txt file using the common store function
    // Following project pattern for package.txt generation
    crate::store::save_package_txt(package_fields, store_tmp_dir)
        .wrap_err("Failed to save package.txt")?;

    Ok(())
}

/// Creates standardized scriptlets from Conda package scripts
/// Based on conda-package-streaming script handling and project script mapping pattern
fn create_scriptlets<P: AsRef<Path>>(store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let conda_info_dir = store_tmp_dir.join("info/conda");
    let install_dir = store_tmp_dir.join("info/install");

    // Process each mapped script following project pattern
    for (conda_script, common_scripts) in SCRIPT_MAPPING.iter() {
        let conda_script_path = conda_info_dir.join(conda_script);
        if conda_script_path.exists() {
            log::debug!("Found Conda script: {}", conda_script);
            for common_script in common_scripts {
                let target_path = install_dir.join(common_script);

                // Copy the script content
                let content = fs::read(&conda_script_path)
                    .wrap_err_with(|| format!("Failed to read Conda script: {}", conda_script_path.display()))?;
                fs::write(&target_path, &content)
                    .wrap_err_with(|| format!("Failed to write script: {}", target_path.display()))?;

                // Make it executable on Unix systems (following project pattern)
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = fs::metadata(&target_path)?.permissions();
                    perms.set_mode(0o755);
                    fs::set_permissions(&target_path, perms)?;
                }

                log::debug!("Created script: {} -> {}", conda_script, common_script);
            }
        }
    }

    Ok(())
}
