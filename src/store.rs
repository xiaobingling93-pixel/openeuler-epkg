use std::collections::HashMap;
use std::sync::Arc;
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::os::unix::fs::{PermissionsExt, FileTypeExt, MetadataExt};
use tar::Archive;
use zstd::stream::Decoder;
use nix::unistd::{User, Group};
use nix::unistd;
use color_eyre::Result;
use color_eyre::eyre::{self, eyre, WrapErr};
use walkdir::WalkDir;
use uuid::Uuid;
use crate::models::{dirs, Package, PackageFormat, InstalledPackageInfo};
use crate::package;
use log;

/// Unpack a package file
///
/// Unpacks a package from a file path and returns the actual package key and pkgline.
/// Does not link the package - linking must be done separately via link_package().
///
pub fn unpack_package(
    file_path: &str,
    pkgkey: &str,
    store_pkglines_by_pkgname: &HashMap<String, Vec<String>>,
) -> Result<(String, String)> {
    // Unpack the package
    let final_dir = unpack_mv_package(file_path, Some(pkgkey), Some(store_pkglines_by_pkgname))
        .with_context(|| format!("Failed to unpack package: {}", file_path))?;

    // Get the pkgline from the directory name
    let pkgline = final_dir.file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| eyre!("Invalid UTF-8 in package directory name: {}", final_dir.display()))?;

    // Parse the pkgline which now includes architecture
    let parsed = package::parse_pkgline(pkgline)
        .map_err(|e| eyre!("Failed to parse package line: {}", e))?;

    // Format the package key using the exact architecture from the package
    let actual_pkgkey = package::format_pkgkey(&parsed.pkgname, &parsed.version, &parsed.arch);

    Ok((actual_pkgkey, pkgline.to_string()))
}

/// Unpacks multiple packages and moves them to the store
/// Returns a vector of paths to the final directories where packages were unpacked
pub fn unpack_packages(package_files: Vec<String>) -> Result<Vec<std::path::PathBuf>> {
    let mut final_dirs = Vec::new();
    for package_file in package_files {
        let final_dir = unpack_mv_package(&package_file, None, None)
            .wrap_err_with(|| format!("Failed to unpack package: {}", package_file))?;
        final_dirs.push(final_dir);
    }
    Ok(final_dirs)
}

/// Build sha256 mapping for a single package
/// Returns a HashMap where key is sha256 and value is (relative_filename, pkgline)
fn build_sha256_mapping_for_package(
    fs_dir: &Path,
    pkgline: &str,
) -> Result<HashMap<String, (String, String)>> {
    use crate::utils::list_package_files_with_info;

    let mut sha256_to_file: HashMap<String, (String, String)> = HashMap::new();

    // Use list_package_files_with_info to parse filelist.txt
    let fs_dir_str = fs_dir.to_str()
        .ok_or_else(|| eyre::eyre!("Invalid UTF-8 in fs_dir path: {}", fs_dir.display()))?;

    let file_infos = match list_package_files_with_info(fs_dir_str) {
        Ok(infos) => infos,
        Err(_) => {
            // If filelist.txt doesn't exist or parsing fails, return empty mapping
            return Ok(sha256_to_file);
        }
    };

    for file_info in file_infos {
        // Only process regular files with sha256
        if file_info.file_type != crate::utils::MtreeFileType::File {
            continue;
        }

        if let Some(sha256_val) = file_info.sha256 {
            // file_info.path is already relative
            let relative_filename = file_info.path.clone();
            // Only store the first occurrence (one is enough for de-duplication)
            sha256_to_file.entry(sha256_val).or_insert_with(|| (relative_filename, pkgline.to_string()));
        }
    }

    Ok(sha256_to_file)
}

/// Build sha256 mapping from other packages with the same pkgname
fn build_sha256_mapping_from_other_packages(
    pkgname: &str,
    current_pkgline: &str,
    store_pkglines_by_pkgname: &HashMap<String, Vec<String>>,
) -> Result<HashMap<String, (String, String)>> {
    let mut sha256_mapping: HashMap<String, (String, String)> = HashMap::new();

    // Get all pkglines for this pkgname
    if let Some(pkglines) = store_pkglines_by_pkgname.get(pkgname) {
        for pkgline in pkglines {
            // Skip the current package
            if pkgline == current_pkgline {
                continue;
            }

            // Read filelist.txt from this package
            let package_dir = dirs().epkg_store.join(pkgline);
            let fs_dir = package_dir.join("fs");

            // Only process if fs directory exists
            if !fs_dir.exists() {
                continue;
            }

            // Build sha256 mapping for this package and add to combined mapping
            let file_mapping = build_sha256_mapping_for_package(&fs_dir, pkgline)?;
            for (sha256, (filename, _)) in file_mapping {
                // Only store the first occurrence (one is enough for de-duplication)
                sha256_mapping.entry(sha256).or_insert_with(|| (filename, pkgline.clone()));
            }
        }
    }

    Ok(sha256_mapping)
}

/// De-duplicate files by creating hardlinks to existing files with the same sha256
fn deduplicate_files_by_hardlink(
    store_tmp_dir: &Path,
    other_packages_mapping: &HashMap<String, (String, String)>,
) -> Result<()> {
    let fs_dir = store_tmp_dir.join("fs");
    if !fs_dir.exists() {
        return Ok(());
    }

    // Build sha256 mapping for the current package using the helper
    // Use empty string for pkgline since we don't need it for current package comparison
    let current_package_mapping = build_sha256_mapping_for_package(&fs_dir, "")?;

    let mut dedup_count = 0;

    // Operate on both mappings: for each file in current package, check if it exists in other packages
    for (sha256_val, (current_filename, _)) in &current_package_mapping {
        // Check if there's a matching file in other packages
        if let Some((existing_filename, existing_pkgline)) = other_packages_mapping.get(sha256_val) {
            // Build paths
            let current_file_path = fs_dir.join(current_filename);
            let existing_package_dir = dirs().epkg_store.join(existing_pkgline);
            let existing_file_path = existing_package_dir.join("fs").join(existing_filename);

            // Check if both files exist
            if current_file_path.exists() && existing_file_path.exists() {
                // Double check their file sizes match before creating hardlink
                let current_metadata = match fs::metadata(&current_file_path) {
                    Ok(m) => m,
                    Err(e) => {
                        log::debug!("Failed to get metadata for {}: {}", current_file_path.display(), e);
                        continue;
                    }
                };

                let existing_metadata = match fs::metadata(&existing_file_path) {
                    Ok(m) => m,
                    Err(e) => {
                        log::debug!("Failed to get metadata for {}: {}", existing_file_path.display(), e);
                        continue;
                    }
                };

                if current_metadata.len() != existing_metadata.len() {
                    log::warn!("Skipping hardlink for {} - file sizes don't match (current: {}, existing: {})",
                               current_file_path.display(), current_metadata.len(), existing_metadata.len());
                    continue;
                }

                // Rename current file to temporary name first
                let temp_file_path = {
                    let mut temp_path = current_file_path.clone();
                    let file_name = temp_path.file_name()
                        .and_then(|n| n.to_str())
                        .map(|s| format!("{}.tmp", s))
                        .unwrap_or_else(|| format!("{}.tmp", Uuid::new_v4()));
                    temp_path.set_file_name(&file_name);
                    temp_path
                };

                if let Err(e) = fs::rename(&current_file_path, &temp_file_path) {
                    log::debug!("Failed to rename file {} for hardlink: {}", current_file_path.display(), e);
                    continue;
                }

                // Try to create hardlink
                if let Err(e) = fs::hard_link(&existing_file_path, &current_file_path) {
                    log::debug!("Failed to create hardlink from {} to {}: {}",
                               existing_file_path.display(), current_file_path.display(), e);
                    // If hardlink fails, rename back
                    if let Err(rename_err) = fs::rename(&temp_file_path, &current_file_path) {
                        log::warn!("Failed to restore file {} after hardlink failure: {}",
                                  current_file_path.display(), rename_err);
                    }
                } else {
                    // Hardlink succeeded, remove the temporary file
                    if let Err(e) = fs::remove_file(&temp_file_path) {
                        log::debug!("Failed to remove temporary file {}: {}", temp_file_path.display(), e);
                    }
                    dedup_count += 1;
                    log::debug!("De-duplicated file {} by hardlink to {}",
                               current_file_path.display(), existing_file_path.display());
                }
            }
        }
    }

    if dedup_count > 0 {
        log::info!("De-duplicated {} files by hardlink", dedup_count);
    }

    Ok(())
}

/// Unpacks a single package and moves it to the final store location
/// Returns the path to the final directory where the package was unpacked
pub fn unpack_mv_package(
    package_file: &str,
    pkgkey: Option<&str>,
    store_pkglines_by_pkgname: Option<&HashMap<String, Vec<String>>>,
) -> Result<std::path::PathBuf> {
    // Create temporary directory for unpacking
    let temp_name = Uuid::new_v4().to_string();
    let store_tmp_dir = dirs().epkg_cache.join("unpack").join(&temp_name);
    fs::create_dir_all(&store_tmp_dir)
        .wrap_err_with(|| format!("Failed to create temporary directory: {}", store_tmp_dir.display()))?;

    // Unpack the package
    general_unpack_package(Path::new(package_file), &store_tmp_dir, pkgkey)
        .wrap_err_with(|| format!("Failed to unpack package {} to {}", package_file, store_tmp_dir.display()))?;

    // Calculate content-addressable hash
    let store_tmp_dir_str = store_tmp_dir.to_str().ok_or_else(|| eyre::eyre!("Invalid UTF-8 in temporary directory path: {}", store_tmp_dir.display()))?;
    let ca_hash_real = crate::hash::epkg_store_hash(store_tmp_dir_str)
        .wrap_err_with(|| format!("Failed to calculate content-addressable hash for directory: {}", store_tmp_dir.display()))?;

    // Read package.txt to get package name and version
    let package_txt_path = store_tmp_dir.join("info/package.txt");
    let package_content = fs::read_to_string(&package_txt_path)
        .wrap_err_with(|| format!("Failed to read package.txt file: {}", package_txt_path.display()))?;

    let mut pkgname = String::new();
    let mut version = String::new();
    let mut ca_hash = String::new();
    let mut arch = String::new();

    for line in package_content.lines() {
        if let Some((key, value)) = line.split_once(": ") {
            match key {
                "pkgname" => pkgname = value.to_string(),
                "version" => version = value.to_string(),
                "caHash"  => ca_hash = value.to_string(),
                "arch"    => arch    = value.to_string(),
                _ => {}
            }
        }
    }

    if pkgname.is_empty() || version.is_empty() {
        return Err(eyre::eyre!("Package name or version not found in package.txt"));
    }

    // Add caHash to package.txt if not present
    if ca_hash.is_empty() {
        let mut updated_content = package_content;
        updated_content.push_str(&format!("caHash: {}\n", ca_hash_real));
        fs::write(&package_txt_path, updated_content)
            .wrap_err_with(|| format!("Failed to update package.txt file: {}", package_txt_path.display()))?;
    } else if ca_hash != ca_hash_real {
        return Err(eyre::eyre!("caHash in package.txt does not match calculated hash"));
    }

    // Use default arch if not found in package.txt
    if arch.is_empty() {
        arch = crate::models::config().common.arch.clone();
    }

    // Create final package directory name with architecture
    let pkgline = crate::package::format_pkgline(&ca_hash_real, &pkgname, &version, &arch);
    let final_dir = dirs().epkg_store.join(&pkgline);

    // De-duplicate files by hardlink if store_pkglines_by_pkgname is provided
    if let Some(store_pkglines_by_pkgname) = store_pkglines_by_pkgname {
        // Build sha256 mapping from other packages with the same pkgname
        if let Ok(sha256_mapping) = build_sha256_mapping_from_other_packages(&pkgname, &pkgline, store_pkglines_by_pkgname) {
            // De-duplicate files by creating hardlinks
            if let Err(e) = deduplicate_files_by_hardlink(&store_tmp_dir, &sha256_mapping) {
                log::warn!("Failed to de-duplicate files for package {}: {}", pkgname, e);
            }
        }
    }

    // Move to final location
    if final_dir.exists() {
        log::info!("Target store directory already exists: {}", final_dir.display());
        fs::remove_dir_all(&final_dir)
            .wrap_err_with(|| format!("Failed to remove old store directory: {}", final_dir.display()))?;
    } else {
        let parent_dir = final_dir.parent()
            .ok_or_else(|| eyre::eyre!("Failed to get parent directory for: {}", final_dir.display()))?;
        fs::create_dir_all(parent_dir)
            .wrap_err_with(|| format!("Failed to create directory: {}", parent_dir.display()))?;
    }

    fs::rename(&store_tmp_dir, &final_dir)
        .wrap_err_with(|| format!("Failed to move package from {} to {}", store_tmp_dir.display(), final_dir.display()))?;

    Ok(final_dir)
}

/// Generic package unpacking function that detects format and delegates to appropriate handler
fn general_unpack_package<P: AsRef<Path>>(package_file: P, store_tmp_dir: P, pkgkey: Option<&str>) -> Result<()> {
    let package_file = package_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Detect package format from file extension
    let format = detect_package_format(package_file)
        .wrap_err_with(|| format!("Failed to detect package format for: {}", package_file.display()))?;

    match format {
        PackageFormat::Deb => {
            crate::deb_pkg::unpack_package(package_file, store_tmp_dir, pkgkey)?
        }
        PackageFormat::Rpm => {
            crate::rpm_pkg::unpack_package(package_file, store_tmp_dir, pkgkey)?
        }
        PackageFormat::Apk => {
            crate::apk_pkg::unpack_package(package_file, store_tmp_dir, pkgkey)?
        }
        PackageFormat::Pacman => {
            crate::arch_pkg::unpack_package(package_file, store_tmp_dir, pkgkey)?
        }
        PackageFormat::Conda => {
            crate::conda_pkg::unpack_package(package_file, store_tmp_dir, pkgkey)?
        }
        PackageFormat::Epkg => {
            // Handle existing .epkg format
            crate::epkg::unpack_package(package_file, store_tmp_dir)?
        }
        _ => {
            return Err(eyre::eyre!("Unsupported package format: {:?}", format));
        }
    }

    Ok(())
}

/// Detects package format from file extension
pub fn detect_package_format(package_file: &Path) -> Result<PackageFormat> {
    let file_name = package_file.file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| eyre::eyre!("Invalid package file name"))?;

    PackageFormat::from_suffix(file_name)
        .wrap_err_with(|| format!("Unknown package format for file: {}", file_name))
}

/// Creates filelist.txt in mtree format from the filesystem layout
pub fn create_filelist_txt<P: AsRef<Path>>(store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let fs_dir = store_tmp_dir.join("fs");
    let filelist_path = store_tmp_dir.join("info/filelist.txt");

    if !fs_dir.exists() {
        return Ok(()); // No filesystem files to list
    }

    let mut output = String::new();

    // Walk through all files in the fs directory
    for entry_result in WalkDir::new(&fs_dir).sort_by_file_name() {
        let entry = entry_result?;
        let path = entry.path();

        // Get relative path from fs directory
        let relative_path = path.strip_prefix(&fs_dir)
            .wrap_err_with(|| format!("Failed to get relative path from {} to {}", fs_dir.display(), path.display()))?;
        if relative_path.as_os_str().is_empty() {
            continue; // Skip the fs directory itself
        }

        let metadata = fs::symlink_metadata(path)
            .wrap_err_with(|| format!("Failed to get metadata for: {}", path.display()))?;
        let file_type = metadata.file_type();

        // Build attributes string
        let mut attrs = Vec::new();
        let mut relative_path_str = relative_path.to_string_lossy();

        // File type
        if file_type.is_file() {
            attrs.push("type=file".to_string());

            // Add mode if not default (644)
            let mode = metadata.permissions().mode() & 0o777;
            if mode != 0o644 {
                attrs.push(format!("mode={:o}", mode));
            }

            // Add SHA256 hash for regular files
            if metadata.len() > 0 {
                let hash = calculate_file_sha256(path)
                    .wrap_err_with(|| format!("Failed to calculate SHA256 hash for: {}", path.display()))?;
                attrs.push(format!("sha256={}", hash));
            }
        } else if file_type.is_dir() {
            attrs.push("type=dir".to_string());

            relative_path_str += "/";

            // Add mode if not default (755)
            let mode = metadata.permissions().mode() & 0o777;
            if mode != 0o755 {
                attrs.push(format!("mode={:o}", mode));
            }
        } else if file_type.is_symlink() {
            attrs.push("type=link".to_string());

            // Add link target
            if let Ok(target) = fs::read_link(path) {
                attrs.push(format!("link={}", target.display()));
            }
        } else {
            // Handle special files
            if metadata.file_type().is_char_device() {
                attrs.push("type=char".to_string());
            } else if metadata.file_type().is_block_device() {
                attrs.push("type=block".to_string());
            } else if metadata.file_type().is_fifo() {
                attrs.push("type=fifo".to_string());
            } else if metadata.file_type().is_socket() {
                attrs.push("type=socket".to_string());
            }
        }

        // Add owner/group if not root
        let uid = metadata.uid();
        let gid = metadata.gid();
        let euid = unistd::geteuid();
        let egid = unistd::getegid();

        if uid != 0 && uid != euid.as_raw() {
            if let Ok(Some(user)) = User::from_uid(uid.into()) {
                attrs.push(format!("uname={}", user.name));
            }
        }

        if gid != 0 && gid != egid.as_raw() {
            if let Ok(Some(group)) = Group::from_gid(gid.into()) {
                attrs.push(format!("gname={}", group.name));
            }
        }

        // Write entry to filelist
        let attrs_str = attrs.join(" ");
        output.push_str(&format!("{} {}\n", relative_path_str, attrs_str));
    }

    fs::write(&filelist_path, output)
        .wrap_err_with(|| format!("Failed to write filelist.txt: {}", filelist_path.display()))?;
    Ok(())
}

/// Calculates SHA256 hash of a file
pub fn calculate_file_sha256(path: &Path) -> Result<String> {
    use sha2::{Sha256, Digest};

    let mut file = fs::File::open(path)
        .wrap_err_with(|| format!("Failed to open file for hash calculation: {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];

    loop {
        let bytes_read = file.read(&mut buffer)
            .wrap_err_with(|| format!("Failed to read from file: {}", path.display()))?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

/// Define the preferred order for common package fields
fn get_field_order() -> &'static [&'static str] {
    &[
        "pkgname", "source", "version", "release", "format", "repo",
        "summary", "description", "homepage", "license", "arch", "maintainer",
        "buildRequires", "checkRequires", "requiresPre", "requires", "provides", "conflicts",
        "suggests", "recommends", "supplements", "enhances", "breaks", "replaces", "originUrl",
        "recipeMaintainers", "subdir", "constrains", "requirements", "commit", "caHash", "caHashVersion",
        "size", "installedSize", "section", "priority", "buildTime", "buildHost", "group", "cookie", "platform",
        "sourcePkgId", "rsaHeader", "sha256Header", "OriginalVcsBrowser", "OriginalVcsGit", "builtUsing",
        "originalMaintainer", "conffiles", "changelogTime", "changelogName", "changelogText",
        "location", "sha256", "sha512", "sha1", "md5sum", "descriptionMd5", "multiArch", "tag",
        "protected", "essential", "important", "buildEssential", "buildIds", "comment",
        "rubyVersions", "luaVersions", "pythonVersion", "pythonEggName", "staticBuiltUsing",
        "javascriptBuiltUsing", "xCargoBuiltUsing", "builtUsingNewlibSource", "goImportPath",
        "ghcPackage", "efiVendor", "cnfIgnoreCommands", "cnfVisiblePkgname", "cnfExtraCommands",
        "gstreamerVersion", "gstreamerElements", "gstreamerUriSources", "gstreamerUriSinks",
        "gstreamerEncoders", "gstreamerDecoders", "postgresqlCatversion", "vendor", "files",
        "xdata", "pkgbase", "backup",
        "pkgkey", "status",
    ]
}

/// Formats package fields with consistent field ordering
pub fn format_package_fields(package_fields: &HashMap<String, String>) -> String {
    let mut output = String::new();
    let field_order = get_field_order();

    // First, write fields in the preferred order
    for preferred_field in field_order {
        if let Some(value) = package_fields.get(*preferred_field) {
            output.push_str(&format!("{}: {}\n", preferred_field, value));
        }
    }

    // Then write any remaining fields that weren't in the preferred order
    for (original_field, value) in package_fields {
        if !field_order.contains(&original_field.as_str()) {
            log::info!("Field name '{}' not found in predefined field order list", original_field);
            output.push_str(&format!("{}: {}\n", original_field, value));
        }
    }

    output
}

/// Saves package fields to package.txt file with consistent field ordering
/// If pkgkey is provided, merges fields from repo package into the store package.txt
pub fn save_package_txt<P: AsRef<Path>>(mut package_fields: HashMap<String, String>, store_tmp_dir: P, pkgkey: Option<&str>) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let package_txt_path = store_tmp_dir.join("info/package.txt");

    // If pkgkey is provided, merge fields from repo package before writing
    if let Some(pkgkey) = pkgkey {
        if let Ok(repo_package) = crate::mmio::map_pkgkey2package(pkgkey) {
            // First verify fields (read-only, logs warnings)
            verify_repo_fields(&package_fields, &repo_package, &package_txt_path);

            // Then add repo-only fields
            add_repo_fields(&mut package_fields, &repo_package);
        }
    }

    // Format the package fields
    let output = format_package_fields(&package_fields);

    fs::write(&package_txt_path, output)
        .wrap_err_with(|| format!("Failed to write package.txt file: {}", package_txt_path.display()))?;

    Ok(())
}

/// Legacy functions for existing .epkg format support

pub fn untar_zst(file_path: &str, output_dir: &str, package_flag: bool) -> Result<()> {
    if package_flag && Path::new(output_dir).exists() {
        return Ok(());
    }

    // Open the compressed file
    let file = fs::File::open(file_path)
        .wrap_err_with(|| format!("Failed to open compressed file: {}", file_path))?;
    let buffered_reader = io::BufReader::new(file);

    // Create a Zstandard decoder
    let zstd_decoder = Decoder::new(buffered_reader)
        .wrap_err_with(|| format!("Failed to create Zstandard decoder for file: {}", file_path))?;

    // Create a tar archive from the Zstandard decoder
    let mut archive = Archive::new(zstd_decoder);

    // Unpack the archive into the output directory
    archive.unpack(output_dir)
        .wrap_err_with(|| format!("Failed to unpack archive to directory: {}", output_dir))?;

    Ok(())
}

/// Collect all pkglines from the epkg store and organize them by pkgkey and pkgname
/// Returns a tuple of (store_pkglines_by_pkgkey, store_pkglines_by_pkgname)
/// where:
/// - store_pkglines_by_pkgkey: key is pkgkey (pkgname__version__arch), value is list of matching pkglines
/// - store_pkglines_by_pkgname: key is pkgname, value is list of matching pkglines
fn collect_store_pkglines() -> Result<(std::collections::HashMap<String, Vec<String>>, std::collections::HashMap<String, Vec<String>>)> {
    use crate::models::dirs;
    use crate::package::{pkgline2pkgkey, parse_pkgline};
    use std::fs;
    use std::collections::HashMap;

    let store_dir = dirs().epkg_store.clone();
    let mut store_pkglines_by_pkgkey: HashMap<String, Vec<String>> = HashMap::new();
    let mut store_pkglines_by_pkgname: HashMap<String, Vec<String>> = HashMap::new();

    if !store_dir.exists() {
        return Ok((store_pkglines_by_pkgkey, store_pkglines_by_pkgname));
    }

    // Collect all pkglines from the store and organize by both pkgkey and pkgname in a single pass
    if let Ok(entries) = fs::read_dir(&store_dir) {
        for entry in entries.flatten() {
            let package_path = entry.path();
            if package_path.is_dir() {
                let fs_dir = package_path.join("fs");
                if !fs_dir.exists() {
                    // on LinkType::Move, files were moved into env
                    log::debug!("Skipping package {} - 'fs' directory does not exist", package_path.display());
                    continue;
                }

                if let Some(pkgline) = package_path.file_name().and_then(|name| name.to_str()) {
                    // Parse the pkgline to extract both pkgkey and pkgname
                    if let Ok(pkgkey) = pkgline2pkgkey(pkgline) {
                        store_pkglines_by_pkgkey
                            .entry(pkgkey)
                            .or_insert_with(Vec::new)
                            .push(pkgline.to_string());
                    }

                    if let Ok(parsed) = parse_pkgline(pkgline) {
                        store_pkglines_by_pkgname
                            .entry(parsed.pkgname)
                            .or_insert_with(Vec::new)
                            .push(pkgline.to_string());
                    }
                }
            }
        }
    }

    Ok((store_pkglines_by_pkgkey, store_pkglines_by_pkgname))
}

/// Match a package from repodata with packages in the store by comparing Package fields
/// Returns the matching pkgline if found, None otherwise
fn match_package_with_store(
    repodata_package: &Package,
    store_pkglines: &[String],
) -> Result<Option<String>> {
    // Try each candidate pkgline from the store
    for store_pkgline in store_pkglines {
        // Load Package from store
        match crate::package_cache::map_pkgline2package(store_pkgline) {
            Ok(store_package) => {
                // Compare Package fields
                if packages_match(repodata_package, &store_package) {
                    return Ok(Some(store_pkgline.clone()));
                }
            }
            Err(e) => {
                log::debug!("Failed to load package from store pkgline {}: {}", store_pkgline, e);
                continue;
            }
        }
    }

    Ok(None)
}

/// Helper function to parse u32 from HashMap value
fn parse_u32_from_fields(fields: &HashMap<String, String>, key: &str) -> u32 {
    fields.get(key)
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0)
}

/// Helper function to parse Option<u32> from HashMap value
fn parse_option_u32_from_fields(fields: &HashMap<String, String>, key: &str) -> Option<u32> {
    fields.get(key)
        .and_then(|v| v.parse::<u32>().ok())
}

/// Verify fields from repo package against store package fields
/// Compares fields checked in packages_match() and logs warnings for mismatches
/// This function is read-only and does not modify the package_fields
fn verify_repo_fields(
    package_fields: &HashMap<String, String>,
    repo_package: &Package,
    package_txt_path: &Path,
) {
    // Verify size if both are non-zero
    let store_size = parse_u32_from_fields(package_fields, "size");
    if repo_package.size != 0 && store_size != 0 {
        if repo_package.size != store_size {
            log::warn!("Size mismatch: repo={}, store={} ({})",
                repo_package.size, store_size, package_txt_path.display());
        }
    }

    // Verify installed_size if both are non-zero
    let store_installed_size = parse_u32_from_fields(package_fields, "installedSize");
    if repo_package.installed_size != 0 && store_installed_size != 0 {
        if repo_package.installed_size != store_installed_size {
            log::warn!("Installed size mismatch: repo={}, store={} ({})",
                repo_package.installed_size, store_installed_size, package_txt_path.display());
        }
    }

    // Verify source if both are available
    if let Some(ref repo_source) = &repo_package.source {
        if let Some(store_source) = package_fields.get("source") {
            if repo_source != store_source {
                log::warn!("Source mismatch: repo={}, store={} ({})",
                    repo_source, store_source, package_txt_path.display());
            }
        }
    }

    // Verify checksums if both are available
    if let Some(ref repo_sha256) = &repo_package.sha256sum {
        if let Some(store_sha256) = package_fields.get("sha256") {
            if repo_sha256 != store_sha256 {
                log::warn!("SHA256 mismatch: repo={}, store={} ({})",
                    repo_sha256, store_sha256, package_txt_path.display());
            }
        }
    }

    if let Some(ref repo_sha1) = &repo_package.sha1sum {
        if let Some(store_sha1) = package_fields.get("sha1") {
            if repo_sha1 != store_sha1 {
                log::warn!("SHA1 mismatch: repo={}, store={} ({})",
                    repo_sha1, store_sha1, package_txt_path.display());
            }
        }
    }

    // Verify buildTime if both are available
    if let Some(repo_build_time) = repo_package.build_time {
        if let Some(store_build_time) = parse_option_u32_from_fields(package_fields, "buildTime") {
            if repo_build_time != store_build_time {
                log::warn!("Build time mismatch: repo={}, store={} ({})",
                    repo_build_time, store_build_time, package_txt_path.display());
            }
        }
    }
}

/// Add repo-only fields to store package fields HashMap
/// Adds fields that are present in repo but missing in store
fn add_repo_fields(
    package_fields: &mut HashMap<String, String>,
    repo_package: &Package,
) {
    // Get store values for size and installed_size to check if they're zero
    let store_size = parse_u32_from_fields(package_fields, "size");
    let store_installed_size = parse_u32_from_fields(package_fields, "installedSize");

    // Add repo field if not present in store
    if !repo_package.repodata_name.is_empty() {
        if !package_fields.contains_key("repo") {
            package_fields.insert("repo".to_string(), repo_package.repodata_name.clone());
        }
    }

    // Add format if not present in store
    if !package_fields.contains_key("format") {
        let format_str = repo_package.format.to_str();
        package_fields.insert("format".to_string(), format_str.to_string());
    }

    // Add source if only in repo
    if repo_package.source.is_some() && !package_fields.contains_key("source") {
        if let Some(ref source) = repo_package.source {
            package_fields.insert("source".to_string(), source.clone());
        }
    }

    // Add sha256sum if only in repo
    if repo_package.sha256sum.is_some() && !package_fields.contains_key("sha256") {
        if let Some(ref sha256) = repo_package.sha256sum {
            package_fields.insert("sha256".to_string(), sha256.clone());
        }
    }

    // Add sha1sum if only in repo
    if repo_package.sha1sum.is_some() && !package_fields.contains_key("sha1") {
        if let Some(ref sha1) = repo_package.sha1sum {
            package_fields.insert("sha1".to_string(), sha1.clone());
        }
    }

    // Add buildTime if only in repo
    if repo_package.build_time.is_some() && !package_fields.contains_key("buildTime") {
        if let Some(build_time) = repo_package.build_time {
            package_fields.insert("buildTime".to_string(), build_time.to_string());
        }
    }

    // Add size if only in repo (and non-zero)
    if repo_package.size != 0 && store_size == 0 {
        package_fields.insert("size".to_string(), repo_package.size.to_string());
    }

    // Add installed_size if only in repo (and non-zero)
    if repo_package.installed_size != 0 && store_installed_size == 0 {
        package_fields.insert("installedSize".to_string(), repo_package.installed_size.to_string());
    }
}


/// Match AUR packages using relaxed criteria: pkgname, version, homepage, summary, buildRequires
fn packages_match_aur(repodata_pkg: &Package, store_pkg: &Package) -> bool {
    // For AUR packages, use relaxed matching: pkgname, version, homepage, summary, buildRequires
    if repodata_pkg.pkgname != store_pkg.pkgname
        || repodata_pkg.version != store_pkg.version {
        return false;
    }

    // Compare homepage if available
    if !repodata_pkg.homepage.is_empty() && !store_pkg.homepage.is_empty() {
        if repodata_pkg.homepage != store_pkg.homepage {
            return false;
        }
    }

    // Compare summary if available
    if !repodata_pkg.summary.is_empty() && !store_pkg.summary.is_empty() {
        if repodata_pkg.summary != store_pkg.summary {
            return false;
        }
    }

    // Compare buildRequires if available
    if !repodata_pkg.build_requires.is_empty() && !store_pkg.build_requires.is_empty() {
        if repodata_pkg.build_requires != store_pkg.build_requires {
            return false;
        }
    }

    // If we got here, all required fields match
    true
}

/// Compare two Package structs to determine if they represent the same package
/// Compares multiple fields: pkgname, version, arch, source, sha256sum, sha1sum, buildTime
/// For AUR packages, uses relaxed matching: pkgname, version, homepage, summary, buildRequires
fn packages_match(repodata_pkg: &Package, store_pkg: &Package) -> bool {
    let common_matches = packages_match_aur(repodata_pkg, store_pkg);
    if common_matches == false {
        return false;
    }

    let is_aur = repodata_pkg.repodata_name == "aur" || store_pkg.repodata_name == "aur";
    if is_aur && common_matches == true {
        return true;
    }

    // For non-AUR packages, use the original strict matching logic
    // Only compare size if both are non-zero (some packages don't store size in package.txt)
    if repodata_pkg.size != 0 && store_pkg.size != 0 {
        if repodata_pkg.size != store_pkg.size {
            return false;
        }
    }

    // Only compare installed_size if both are non-zero
    if repodata_pkg.installed_size != 0 && store_pkg.installed_size != 0 {
        if repodata_pkg.installed_size != store_pkg.installed_size {
            return false;
        }
    }

    if repodata_pkg.arch != store_pkg.arch {
        return false;
    }

    // Compare source if available (helps identify packages from different sources)
    if let (Some(repodata_source), Some(store_source)) = (&repodata_pkg.source, &store_pkg.source) {
        if repodata_source != store_source {
            return false;
        }
    }

    // Compare checksums if both are available (strongest match indicator)
    // If only one side has a checksum, we still allow the match if basic fields match
    // (e.g., Arch packages don't store sha256sum in package.txt, only in repodata)
    if let (Some(repodata_sha256), Some(store_sha256)) = (&repodata_pkg.sha256sum, &store_pkg.sha256sum) {
        return repodata_sha256 == store_sha256;
    }

    if let (Some(repodata_sha1), Some(store_sha1)) = (&repodata_pkg.sha1sum, &store_pkg.sha1sum) {
        return repodata_sha1 == store_sha1;
    }

    // Compare buildTime if both are available (strong match indicator similar to checksums)
    if let (Some(repodata_build_time), Some(store_build_time)) =
        (&repodata_pkg.build_time, &store_pkg.build_time)
    {
        return repodata_build_time == store_build_time;
    }

    false
}

/// Collect candidate pkglines from store for a given pkgkey
/// For AUR packages, also tries with arch replaced by std::env::consts::ARCH
fn collect_candidate_pkglines(
    pkgkey: &str,
    repodata_package: &Package,
    store_pkglines_by_pkgkey: &std::collections::HashMap<String, Vec<String>>,
) -> Vec<String> {
    // Get candidate pkglines from store for this pkgkey
    let mut candidate_pkglines = match store_pkglines_by_pkgkey.get(pkgkey) {
        Some(pkglines) => pkglines.clone(),
        None => Vec::new(),
    };

    // For AUR packages, also try with arch replaced by std::env::consts::ARCH
    if repodata_package.repodata_name == "aur" {
        // Parse pkgkey to extract pkgname, version, and arch
        if let Ok((pkgname, version, _arch)) = crate::package::parse_pkgkey(pkgkey) {
            // Create a new pkgkey with arch replaced by std::env::consts::ARCH
            let arch_substituted_pkgkey = crate::package::format_pkgkey(&pkgname, &version, std::env::consts::ARCH);

            // Get candidate pkglines for the arch-substituted pkgkey
            if let Some(arch_pkglines) = store_pkglines_by_pkgkey.get(&arch_substituted_pkgkey) {
                log::debug!(
                    "Found {} candidate pkglines for arch-substituted pkgkey {}",
                    arch_pkglines.len(),
                    arch_substituted_pkgkey
                );
                // Combine with existing candidates (avoid duplicates)
                for pkgline in arch_pkglines {
                    if !candidate_pkglines.contains(pkgline) {
                        candidate_pkglines.push(pkgline.clone());
                    }
                }
            }
        }
    }

    candidate_pkglines
}

/// Try to match and fill pkgline for a single package entry
/// Returns true if a match was found and pkgline was filled, false otherwise
fn try_match_and_fill_pkgline(
    pkgkey: &str,
    package_info: &mut InstalledPackageInfo,
    store_pkglines_by_pkgkey: &std::collections::HashMap<String, Vec<String>>,
) -> Result<bool> {
    // Skip if pkgline is already filled
    if !package_info.pkgline.is_empty() {
        return Ok(false);
    }

    // Load Package from repodata
    let repodata_package = match crate::package_cache::load_package_info(pkgkey) {
        Ok(pkg) => pkg,
        Err(e) => {
            log::debug!("Failed to load package info for {}: {}", pkgkey, e);
            return Ok(false);
        }
    };

    // Collect candidate pkglines from store
    let candidate_pkglines = collect_candidate_pkglines(pkgkey, &repodata_package, store_pkglines_by_pkgkey);

    // If no candidates found, return false
    if candidate_pkglines.is_empty() {
        return Ok(false);
    }

    // Match with store packages
    match match_package_with_store(&repodata_package, &candidate_pkglines) {
        Ok(Some(matching_pkgline)) => {
            package_info.pkgline = matching_pkgline;
            Ok(true)
        }
        Ok(None) => Ok(false),
        Err(e) => {
            log::debug!("Error matching package {}: {}", pkgkey, e);
            Ok(false)
        }
    }
}

/// Fill pkglines in the installation plan by matching packages with existing store packages
/// Returns the number of packages that were matched and filled
pub fn fill_pkglines_in_plan(
    plan: &mut crate::plan::InstallationPlan,
) -> Result<usize> {
    // Collect store pkglines organized by both pkgkey (for matching) and pkgname (for reuse in unpack_mv_package)
    let (store_pkglines_by_pkgkey, store_pkglines_by_pkgname) = collect_store_pkglines()?;
    plan.store_pkglines_by_pkgname = store_pkglines_by_pkgname;

    if store_pkglines_by_pkgkey.is_empty() {
        return Ok(0);
    }

    let mut matched_count = 0;

    // Process new packages (fresh installs and upgrades)
    for op in &mut plan.ordered_operations {
        if let Some((pkgkey, package_info)) = &mut op.new_pkg {
            if try_match_and_fill_pkgline(pkgkey, Arc::make_mut(package_info), &store_pkglines_by_pkgkey)? {
                matched_count += 1;
            }
        }
    }

    Ok(matched_count)
}
