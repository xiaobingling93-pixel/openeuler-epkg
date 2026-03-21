
use std::collections::HashMap;
use std::sync::Arc;
use std::fs;
use crate::lfs;
use std::io::Read;
#[cfg(unix)]
use std::io::{self};
use std::path::Path;
#[cfg(unix)] use std::os::unix::fs::{PermissionsExt, FileTypeExt, MetadataExt};
#[cfg(unix)] use tar::Archive;
#[cfg(unix)] use zstd::stream::Decoder;
#[cfg(unix)] use nix::unistd;
use color_eyre::Result;
use color_eyre::eyre::{self, eyre, WrapErr};
use walkdir::WalkDir;
use uuid::Uuid;
use crate::models::{dirs, Package, PackageFormat, InstalledPackageInfo};
use crate::package;
#[cfg(unix)] use crate::userdb;
use crate::mtree::escape_mtree_path;
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
    format_hint: Option<PackageFormat>,
) -> Result<(String, String)> {
    // Unpack the package
    let final_dir = unpack_mv_package_with_format(file_path, Some(pkgkey), Some(store_pkglines_by_pkgname), format_hint)
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
#[cfg(unix)]
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
        if file_info.file_type != crate::mtree::MtreeFileType::File {
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
            if !lfs::exists_on_host(&fs_dir) {
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
    if !lfs::exists_on_host(&fs_dir) {
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
            if lfs::exists_on_host(&current_file_path) && lfs::exists_on_host(&existing_file_path) {
                // Double check their file sizes match before creating hardlink
                let current_metadata = match lfs::metadata_on_host(&current_file_path) {
                    Ok(m) => m,
                    Err(e) => {
                        log::debug!("Failed to get metadata for {}: {}", current_file_path.display(), e);
                        continue;
                    }
                };

                let existing_metadata = match lfs::metadata_on_host(&existing_file_path) {
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

                if let Err(e) = lfs::rename(&current_file_path, &temp_file_path) {
                    log::debug!("Failed to rename file {} for hardlink: {}", current_file_path.display(), e);
                    continue;
                }

                // Try to create hardlink
                if let Err(e) = lfs::hard_link(&existing_file_path, &current_file_path) {
                    log::debug!("Failed to create hardlink from {} to {}: {}",
                               existing_file_path.display(), current_file_path.display(), e);
                    // If hardlink fails, rename back
                    if let Err(rename_err) = lfs::rename(&temp_file_path, &current_file_path) {
                        log::warn!("Failed to restore file {} after hardlink failure: {}",
                                  current_file_path.display(), rename_err);
                    }
                } else {
                    // Hardlink succeeded, remove the temporary file
                    if let Err(e) = lfs::remove_file(&temp_file_path) {
                        log::debug!("Failed to remove temporary file {}: {}", temp_file_path.display(), e);
                    }
                    dedup_count += 1;
                    log::trace!("De-duplicated file {} by hardlink to {}",
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
/// If format_hint is provided, uses it directly instead of detecting from file extension
pub fn unpack_mv_package_with_format(
    package_file: &str,
    pkgkey: Option<&str>,
    store_pkglines_by_pkgname: Option<&HashMap<String, Vec<String>>>,
    format_hint: Option<PackageFormat>,
) -> Result<std::path::PathBuf> {
    // Create temporary directory for unpacking
    let temp_name = Uuid::new_v4().to_string();
    let store_tmp_dir = crate::dirs::unpack_basedir().join(&temp_name);
    lfs::create_dir_all(&store_tmp_dir)?;

    // Unpack the package (with optional format hint)
    general_unpack_package(Path::new(package_file), &store_tmp_dir, pkgkey, format_hint)
        .wrap_err_with(|| format!("Failed to unpack package {} to {}", package_file, store_tmp_dir.display()))?;

    // Calculate content-addressable hash
    #[allow(unused)]
    let store_tmp_dir_str = store_tmp_dir.to_str().ok_or_else(|| eyre::eyre!("Invalid UTF-8 in temporary directory path: {}", store_tmp_dir.display()))?;
    let ca_hash_real = crate::hash::epkg_store_hash(store_tmp_dir_str)
        .wrap_err_with(|| format!("Failed to calculate content-addressable hash for directory: {}", store_tmp_dir.display()))?;

    // Read package.txt to get package name and version
    let package_txt_path = crate::dirs::path_join(&store_tmp_dir, &["info", "package.txt"]);
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
        lfs::write(&package_txt_path, updated_content)?;
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
    if lfs::exists_on_host(&final_dir) {
        log::info!("Target store directory already exists: {}", final_dir.display());
        lfs::remove_dir_all(&final_dir)?;
    } else {
        let parent_dir = final_dir.parent()
            .ok_or_else(|| eyre::eyre!("Failed to get parent directory for: {}", final_dir.display()))?;
        lfs::create_dir_all(parent_dir)?;
    }

    log::info!("Unpacking pkgkey {:?} file to store: {} -> {}", pkgkey, package_file, final_dir.display());
    crate::utils::rename_or_copy_dir(&store_tmp_dir, &final_dir)
        .wrap_err_with(|| format!("Failed to move package from {} to {}", store_tmp_dir.display(), final_dir.display()))?;

    Ok(final_dir)
}

/// Backward-compatible wrapper for unpack_mv_package_with_format
/// Detects format from file extension
pub fn unpack_mv_package(
    package_file: &str,
    pkgkey: Option<&str>,
    store_pkglines_by_pkgname: Option<&HashMap<String, Vec<String>>>,
) -> Result<std::path::PathBuf> {
    unpack_mv_package_with_format(package_file, pkgkey, store_pkglines_by_pkgname, None)
}

/// Generic package unpacking function that detects format and delegates to appropriate handler
/// If format_hint is provided, uses it directly; otherwise detects from file extension
fn general_unpack_package<P: AsRef<Path>>(
    package_file: P,
    store_tmp_dir: P,
    pkgkey: Option<&str>,
    format_hint: Option<PackageFormat>,
) -> Result<()> {
    let package_file = package_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Use provided format hint or detect from file extension
    let format = match format_hint {
        Some(fmt) => fmt,
        None => detect_package_format(package_file)
            .wrap_err_with(|| format!("Failed to detect package format for: {}", package_file.display()))?,
    };

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
        #[cfg(unix)]
        PackageFormat::Brew => {
            crate::brew_pkg::unpack_package(package_file, store_tmp_dir, pkgkey)?
        }
        #[cfg(unix)]
        PackageFormat::Epkg => {
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

/// One mtree line (including trailing newline) for a path under the package `fs/` tree.
fn filelist_mtree_line_for_entry(path: &Path, relative_path_str: &str) -> Result<String> {
    let metadata = lfs::symlink_metadata(path)?;
    let file_type = metadata.file_type();

    let mut attrs = Vec::new();

    // File type
    if file_type.is_file() {
        attrs.push("type=file".to_string());

        // Add mode if not default (644)
        #[cfg(unix)]
        {
            let mode = metadata.permissions().mode() & 0o777;
            if mode != 0o644 {
                attrs.push(format!("mode={:o}", mode));
            }
        }

        // Add SHA256 hash for regular files
        if metadata.len() > 0 {
            let hash = calculate_file_sha256(path)
                .wrap_err_with(|| format!("Failed to calculate SHA256 hash for: {}", path.display()))?;
            attrs.push(format!("sha256={}", hash));
        }
    } else if file_type.is_dir() {
        attrs.push("type=dir".to_string());

        // Add mode if not default (755)
        #[cfg(unix)]
        {
            let mode = metadata.permissions().mode() & 0o777;
            if mode != 0o755 {
                attrs.push(format!("mode={:o}", mode));
            }
        }
    } else if lfs::is_symlink(path) {
        attrs.push("type=link".to_string());

        // Add link target
        if let Ok(target) = fs::read_link(path) {
            let target_str = target.to_string_lossy();
            // Note: mtree specification is ambiguous about whether link targets should be escaped.
            // While pathnames require escaping of backslashes and non-printable ASCII,
            // some mtree implementations allow unescaped spaces in link targets.
            // We follow the pathname escaping rules for consistency.
            // IMPORTANT: Since we don't escape spaces (0x20), link targets containing spaces
            // will produce unparseable mtree output (spaces separate tokens in mtree format).
            // Link targets with spaces are effectively not supported by this implementation.
            // Alternative: escape spaces as \040 would support link targets with spaces,
            // but violates the mtree specification.
            attrs.push(format!("link={}", escape_mtree_path(&target_str)));
        }
    } else {
        // Handle special files
        #[cfg(unix)]
        {
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
    }

    // Add owner/group if not root
    #[cfg(unix)]
    {
        let uid = metadata.uid();
        let gid = metadata.gid();
        let euid = unistd::geteuid();
        let egid = unistd::getegid();

        if uid != 0 && uid != euid.as_raw() {
            if let Ok(username) = userdb::get_username_by_uid(uid, None) {
                attrs.push(format!("uname={}", username));
            }
        }

        if gid != 0 && gid != egid.as_raw() {
            if let Ok(groupname) = userdb::get_groupname_by_gid(gid, None) {
                attrs.push(format!("gname={}", groupname));
            }
        }
    }

    let attrs_str = attrs.join(" ");
    let mut escaped_path = escape_mtree_path(relative_path_str);
    if file_type.is_dir() {
        escaped_path.push('/');
    }
    Ok(format!("{} {}\n", escaped_path, attrs_str))
}

/// Creates filelist.txt in mtree format from the filesystem layout
pub fn create_filelist_txt<P: AsRef<Path>>(store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let fs_dir = store_tmp_dir.join("fs");
    let filelist_path = crate::dirs::path_join(&store_tmp_dir, &["info", "filelist.txt"]);

    if !lfs::exists_on_host(&fs_dir) {
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

        // Skip files under info/ directory (metadata files that are commonly duplicated)
        let relative_path_str = relative_path.to_string_lossy().to_string();
        if relative_path_str.starts_with("info/") {
            continue;
        }

        output.push_str(&filelist_mtree_line_for_entry(path, &relative_path_str)?);
    }

    lfs::write(&filelist_path, output)?;
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
        "summary", "description", "homepage", "license", "licenseFamily", "arch", "maintainer",
        "buildRequires", "checkRequires", "requiresPre", "requires", "provides", "conflicts", "obsoletes",
        "suggests", "recommends", "supplements", "enhances", "breaks", "replaces", "originUrl",
        "recipeMaintainers", "subdir", "constrains", "requirements", "commit", "caHash", "caHashVersion",
        "size", "installedSize", "section", "priority", "provider_priority", "replaces_priority", "buildTime", "buildHost", "group", "cookie", "platform",
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
        "buildString", "buildNumber", "trackFeatures", "noarch",  // conda
        "pkgkey", "status", "storePath", "dependDepth", "installTime",
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
            log::info!("Field name '{}' with value '{}' not found in predefined field order list", original_field, value);
            output.push_str(&format!("{}: {}\n", original_field, value));
        }
    }

    output
}

/// Saves package fields to package.txt file with consistent field ordering
/// If pkgkey is provided, merges fields from repo package into the store package.txt
pub fn save_package_txt<P: AsRef<Path>>(mut package_fields: HashMap<String, String>, store_tmp_dir: P, pkgkey: Option<&str>) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let package_txt_path = crate::dirs::path_join(&store_tmp_dir, &["info", "package.txt"]);

    // If pkgkey is provided, merge fields from repo package before writing
    if let Some(pkgkey) = pkgkey {
        if let Ok(repo_package) = crate::mmio::map_pkgkey2package(pkgkey) {
            // First verify fields (read-only, logs warnings)
            verify_repo_fields(&package_fields, &repo_package, &package_txt_path);

            // Check and update core package fields (pkgname, version, arch)
            check_and_update_core_fields(&mut package_fields, &repo_package, &package_txt_path);

            // Then add repo-only fields
            add_repo_fields(&mut package_fields, &repo_package);
        }
    }

    // Format the package fields
    let output = format_package_fields(&package_fields);

    lfs::write(&package_txt_path, output)?;

    Ok(())
}

/// Legacy functions for existing .epkg format support

#[cfg(unix)]
pub fn untar_zst(file_path: &str, output_dir: &str, package_flag: bool) -> Result<()> {
    if package_flag && lfs::exists_on_host(Path::new(output_dir)) {
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

    if !lfs::exists_on_host(&store_dir) {
        log::debug!("collect_store_pkglines: store directory does not exist: {:?}", store_dir);
        return Ok((store_pkglines_by_pkgkey, store_pkglines_by_pkgname));
    }

    log::debug!("collect_store_pkglines: scanning store directory: {:?}", store_dir);
    let mut total_pkglines = 0;
    let mut skipped_missing_fs = 0;

    // Collect all pkglines from the store and organize by both pkgkey and pkgname in a single pass
    if let Ok(entries) = fs::read_dir(&store_dir) {
        for entry in entries.flatten() {
            let package_path = entry.path();
            if package_path.is_dir() {
                let fs_dir = package_path.join("fs");
                // Check if fs directory exists and has actual files
                // (for Move link type, files are moved to env leaving empty fs)
                if !lfs::exists_on_host(&fs_dir) {
                    log::debug!("Skipping package {} - 'fs' directory does not exist", package_path.display());
                    skipped_missing_fs += 1;
                    continue;
                }

                // Check if fs directory has any regular files (not just empty dirs)
                let has_files = walkdir::WalkDir::new(&fs_dir)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .any(|e| e.file_type().is_file());
                if !has_files {
                    log::debug!("Skipping package {} - 'fs' directory is empty (files moved to env)", package_path.display());
                    skipped_missing_fs += 1;
                    continue;
                }

                if let Some(pkgline) = package_path.file_name().and_then(|name| name.to_str()) {
                    total_pkglines += 1;
                    // Parse the pkgline to extract both pkgkey and pkgname
                    if let Ok(pkgkey) = pkgline2pkgkey(pkgline) {
                        store_pkglines_by_pkgkey
                            .entry(pkgkey)
                            .or_insert_with(Vec::new)
                            .push(pkgline.to_string());
                    } else {
                        log::debug!("collect_store_pkglines: failed to parse pkgkey from pkgline: {}", pkgline);
                    }

                    if let Ok(parsed) = parse_pkgline(pkgline) {
                        store_pkglines_by_pkgname
                            .entry(parsed.pkgname)
                            .or_insert_with(Vec::new)
                            .push(pkgline.to_string());
                    } else {
                        log::debug!("collect_store_pkglines: failed to parse pkgline: {}", pkgline);
                    }
                }
            }
        }
    }

    log::debug!("collect_store_pkglines: found {} pkglines ({} skipped due to missing fs), organized into {} pkgkey entries, {} pkgname entries",
        total_pkglines, skipped_missing_fs,
        store_pkglines_by_pkgkey.len(), store_pkglines_by_pkgname.len());
    Ok((store_pkglines_by_pkgkey, store_pkglines_by_pkgname))
}

/// Match a package from repodata with packages in the store by comparing Package fields
/// Returns the matching pkgline if found, None otherwise
fn match_package_with_store(
    repodata_package: &Package,
    store_pkglines: &[String],
) -> Result<Option<String>> {
    log::trace!("match_package_with_store: checking {} store pkglines against repodata package", store_pkglines.len());
    // Try each candidate pkgline from the store
    for (i, store_pkgline) in store_pkglines.iter().enumerate() {
        log::trace!("match_package_with_store: checking candidate {}/{}: {}", i+1, store_pkglines.len(), store_pkgline);
        // Load Package from store
        match crate::package_cache::map_pkgline2package(store_pkgline) {
            Ok(store_package) => {
                // Compare Package fields
                if packages_match(repodata_package, &store_package) {
                    // Validate store integrity before accepting match
                    if !validate_store_integrity(store_pkgline) {
                        log::warn!("Store package {} failed integrity check, skipping", store_pkgline);
                        continue;
                    }
                    log::trace!("match_package_with_store: found match at candidate {}: {}", i+1, store_pkgline);
                    return Ok(Some(store_pkgline.clone()));
                }
            }
            Err(e) => {
                log::debug!("Failed to load package from store pkgline {}: {}", store_pkgline, e);
                continue;
            }
        }
    }

    log::trace!("match_package_with_store: no match found after checking {} candidates", store_pkglines.len());
    Ok(None)
}

/// Marker file created when a store package is consumed by LinkType::Move
/// The file is placed in info/consumed.json before files are moved to env.
/// If this file exists, the store should be skipped as invalid.
const CONSUMED_MARKER_FILE: &str = "info/consumed.json";

/// Create a consumed marker file for a store package.
/// This is called before LinkType::Move to mark the store as consumed.
/// If the move fails partway, the marker ensures the store is not reused.
pub fn create_consumed_marker(store_path: &Path, _env_name: &str, env_root: &Path) -> Result<()> {
    let info_dir = store_path.join("info");
    lfs::create_dir_all(&info_dir)?;

    let marker_path = info_dir.join("consumed.json");

    // Create marker content with metadata
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let content = serde_json::json!({
        "env_root": env_root.display().to_string(),
        "consumed_at": timestamp,
        "link_type": "Move"
    });

    let content_str = serde_json::to_string_pretty(&content)
        .wrap_err("Failed to serialize consumed marker")?;

    lfs::write(&marker_path, &content_str)?;
    log::debug!("Created consumed marker: {}", marker_path.display());

    Ok(())
}

/// Check if a store package has been consumed by LinkType::Move.
/// Returns true if the consumed marker exists.
pub fn is_store_consumed(store_path: &Path) -> bool {
    let marker_path = store_path.join(CONSUMED_MARKER_FILE);
    if lfs::exists_on_host(&marker_path) {
        log::debug!("Store at {} has been consumed (marker exists)", store_path.display());
        return true;
    }
    false
}

/// Validate store package integrity by checking if consumed marker exists or files are missing.
/// Returns true if the store appears valid, false if consumed or corrupted.
fn validate_store_integrity(pkgline: &str) -> bool {
    let store_path = dirs().epkg_store.join(pkgline);

    // First check if consumed marker exists (LinkType::Move was used)
    if is_store_consumed(&store_path) {
        log::debug!("validate_store_integrity: store {} is consumed by Move", pkgline);
        return false;
    }

    let fs_dir = store_path.join("fs");

    // Check that fs directory exists
    if !lfs::exists_on_host(&fs_dir) {
        log::debug!("validate_store_integrity: fs directory missing for {}", pkgline);
        return false;
    }

    // Check filelist.txt exists and has content
    let filelist_path = crate::dirs::path_join(&store_path, &["info", "filelist.txt"]);
    if !lfs::exists_on_host(&filelist_path) {
        log::debug!("validate_store_integrity: filelist.txt missing for {}", pkgline);
        return false;
    }

    // Read filelist.txt and verify at least some files exist
    let filelist_content = match std::fs::read_to_string(&filelist_path) {
        Ok(content) => content,
        Err(e) => {
            log::debug!("validate_store_integrity: failed to read filelist.txt for {}: {}", pkgline, e);
            return false;
        }
    };

    // Check if any regular files are listed (not just directories)
    let has_listed_files = filelist_content.lines().any(|line| {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            return false;
        }
        let path = parts[0];
        // Check if it's a file entry (has type=file or no type means file)
        let is_file = parts.iter().any(|p| p.starts_with("type=file"))
            || (!parts.iter().any(|p| p.starts_with("type=")));
        is_file && !path.ends_with('/')
    });

    if !has_listed_files {
        log::debug!("validate_store_integrity: no files listed in filelist.txt for {}", pkgline);
        return false;
    }

    // Sample check: verify a few files from filelist actually exist
    let mut checked = 0;
    let mut missing = 0;
    for line in filelist_content.lines().take(10) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        let path = parts[0];
        if path.ends_with('/') {
            continue; // Skip directories
        }
        let file_path = fs_dir.join(path);
        if !lfs::exists_on_host(&file_path) {
            missing += 1;
            log::trace!("validate_store_integrity: file missing in {}: {}", pkgline, path);
        }
        checked += 1;
    }

    // If more than half of sampled files are missing, consider store corrupted
    if checked > 0 && missing * 2 > checked {
        log::debug!("validate_store_integrity: {} of {} sampled files missing for {}", missing, checked, pkgline);
        return false;
    }

    true
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

/// Check and update pkgkey fields (pkgname, version, arch) with repo package values
/// If fields differ, warn and update with repo package values to ensure pkgkey consistency
fn check_and_update_core_fields(
    package_fields: &mut HashMap<String, String>,
    repo_package: &Package,
    package_txt_path: &Path,
) {
    // Check pkgname, version, arch for consistency with repo package
    // If different, warn and update with repo package values
    if let Some(store_pkgname) = package_fields.get("pkgname") {
        if store_pkgname != &repo_package.pkgname {
            log::warn!("pkgname mismatch: repo={}, store={} ({})",
                repo_package.pkgname, store_pkgname, package_txt_path.display());
            package_fields.insert("pkgname".to_string(), repo_package.pkgname.clone());
        }
    } else {
        // Field missing, add it
        package_fields.insert("pkgname".to_string(), repo_package.pkgname.clone());
    }

    if let Some(store_version) = package_fields.get("version") {
        if store_version != &repo_package.version {
            log::warn!("version mismatch: repo={}, store={} ({})",
                repo_package.version, store_version, package_txt_path.display());
            package_fields.insert("version".to_string(), repo_package.version.clone());
        }
    } else {
        package_fields.insert("version".to_string(), repo_package.version.clone());
    }

    if let Some(store_arch) = package_fields.get("arch") {
        if store_arch != &repo_package.arch {
            log::warn!("arch mismatch: repo={}, store={} ({})",
                repo_package.arch, store_arch, package_txt_path.display());
            package_fields.insert("arch".to_string(), repo_package.arch.clone());
        }
    } else {
        package_fields.insert("arch".to_string(), repo_package.arch.clone());
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


/// Match AUR packages using relaxed criteria: homepage, summary, buildRequires
/// Note: pkgname and version are checked by the caller before this function is called
fn packages_match_aur(repodata_pkg: &Package, store_pkg: &Package) -> bool {
    // For AUR packages, use relaxed matching: homepage, summary, buildRequires
    // Note: pkgname and version are already checked by the caller

    // Compare homepage if available
    if !repodata_pkg.homepage.is_empty() && !store_pkg.homepage.is_empty() {
        if repodata_pkg.homepage != store_pkg.homepage {
            log::trace!("packages_match_aur: homepage mismatch: repo={} store={}", repodata_pkg.homepage, store_pkg.homepage);
            return false;
        }
    }

    // Compare summary if available
    if !repodata_pkg.summary.is_empty() && !store_pkg.summary.is_empty() {
        if repodata_pkg.summary != store_pkg.summary {
            log::trace!("packages_match_aur: summary mismatch: repo={} store={}", repodata_pkg.summary, store_pkg.summary);
            return false;
        }
    }

    // Compare buildRequires if available
    if !repodata_pkg.build_requires.is_empty() && !store_pkg.build_requires.is_empty() {
        if repodata_pkg.build_requires != store_pkg.build_requires {
            log::trace!("packages_match_aur: build_requires mismatch");
            return false;
        }
    }

    // If we got here, all required fields match
    log::trace!("packages_match_aur: packages match");
    true
}

/// Compare two Package structs to determine if they represent the same package
/// Compares multiple fields: pkgname, version, arch, source, sha256sum, sha1sum, buildTime
/// For AUR packages, uses relaxed matching: pkgname, version, homepage, summary, buildRequires
fn packages_match(repodata_pkg: &Package, store_pkg: &Package) -> bool {
    // First check basic fields that all packages must match
    if repodata_pkg.pkgname != store_pkg.pkgname {
        log::trace!("packages_match: pkgname mismatch: repo={} store={}", repodata_pkg.pkgname, store_pkg.pkgname);
        return false;
    }
    if repodata_pkg.version != store_pkg.version {
        log::trace!("packages_match: version mismatch: repo={} store={}", repodata_pkg.version, store_pkg.version);
        return false;
    }

    let is_aur = repodata_pkg.repodata_name == "aur" || store_pkg.repodata_name == "aur";

    // For AUR packages, use relaxed matching
    if is_aur {
        return packages_match_aur(repodata_pkg, store_pkg);
    }

    // For non-AUR packages, use the original strict matching logic
    // Only compare size if both are non-zero (some packages don't store size in package.txt)
    if repodata_pkg.size != 0 && store_pkg.size != 0 {
        if repodata_pkg.size != store_pkg.size {
            log::trace!("packages_match: size mismatch: repo={} store={}", repodata_pkg.size, store_pkg.size);
            return false;
        }
    }

    // Only compare installed_size if both are non-zero
    if repodata_pkg.installed_size != 0 && store_pkg.installed_size != 0 {
        if repodata_pkg.installed_size != store_pkg.installed_size {
            log::trace!("packages_match: installed_size mismatch: repo={} store={}", repodata_pkg.installed_size, store_pkg.installed_size);
            return false;
        }
    }

    if repodata_pkg.arch != store_pkg.arch {
        log::trace!("packages_match: arch mismatch: repo={} store={}", repodata_pkg.arch, store_pkg.arch);
        return false;
    }

    // Compare source if available (helps identify packages from different sources)
    if let (Some(repodata_source), Some(store_source)) = (&repodata_pkg.source, &store_pkg.source) {
        if repodata_source != store_source {
            log::trace!("packages_match: source mismatch: repo={} store={}", repodata_source, store_source);
            return false;
        }
    }

    // Compare checksums if both are available (strongest match indicator)
    // If only one side has a checksum, we still allow the match if basic fields match
    // (e.g., Arch packages don't store sha256sum in package.txt, only in repodata)
    if let (Some(repodata_sha256), Some(store_sha256)) = (&repodata_pkg.sha256sum, &store_pkg.sha256sum) {
        if repodata_sha256 == store_sha256 {
            log::trace!("packages_match: sha256sum match");
            return true;
        } else {
            log::trace!("packages_match: sha256sum mismatch");
            return false;
        }
    }

    if let (Some(repodata_sha1), Some(store_sha1)) = (&repodata_pkg.sha1sum, &store_pkg.sha1sum) {
        if repodata_sha1 == store_sha1 {
            log::trace!("packages_match: sha1sum match");
            return true;
        } else {
            log::trace!("packages_match: sha1sum mismatch");
            return false;
        }
    }

    // Compare buildTime if both are available (strong match indicator similar to checksums)
    if let (Some(repodata_build_time), Some(store_build_time)) =
        (&repodata_pkg.build_time, &store_pkg.build_time)
    {
        if repodata_build_time == store_build_time {
            log::trace!("packages_match: build_time match");
            return true;
        } else {
            log::trace!("packages_match: build_time mismatch");
            return false;
        }
    }

    log::trace!("packages_match: no strong match indicators available, returning false");
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
        Some(pkglines) => {
            log::trace!("collect_candidate_pkglines: found {} pkglines for pkgkey {}", pkglines.len(), pkgkey);
            pkglines.clone()
        }
        None => {
            log::trace!("collect_candidate_pkglines: no pkglines found for pkgkey {}", pkgkey);
            Vec::new()
        }
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

    log::trace!("collect_candidate_pkglines: total {} candidates for pkgkey {}", candidate_pkglines.len(), pkgkey);
    candidate_pkglines
}

/// Try to match and fill pkgline for a single package entry
/// Returns true if a match was found and pkgline was filled, false otherwise
fn try_match_and_fill_pkgline(
    pkgkey: &str,
    package_info: &mut InstalledPackageInfo,
    store_pkglines_by_pkgkey: &std::collections::HashMap<String, Vec<String>>,
) -> Result<bool> {
    // Skip if pkgline is already filled and the fs directory exists
    if !package_info.pkgline.is_empty() {
        let store_dir = crate::models::dirs().epkg_store.clone();
        let fs_dir = store_dir.join(&package_info.pkgline).join("fs");
        if lfs::exists_on_host(&fs_dir) {
            log::trace!("try_match_and_fill_pkgline: pkgkey {} already has pkgline {} with existing fs dir, skipping", pkgkey, package_info.pkgline);
            return Ok(false);
        } else {
            // Clear pkgline if fs directory doesn't exist, necessary to trigger package
            // download/unpack when called from from import_packages_and_create_metadata()
            log::trace!("try_match_and_fill_pkgline: pkgkey {} has pkgline {} but fs dir missing, clearing pkgline", pkgkey, package_info.pkgline);
            package_info.pkgline.clear();
        }
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
    log::trace!("try_match_and_fill_pkgline: pkgkey {} has {} candidate pkglines", pkgkey, candidate_pkglines.len());

    // If no candidates found, return false
    if candidate_pkglines.is_empty() {
        log::trace!("try_match_and_fill_pkgline: no candidate pkglines for pkgkey {}", pkgkey);
        return Ok(false);
    }

    // Match with store packages
    match match_package_with_store(&repodata_package, &candidate_pkglines) {
        Ok(Some(matching_pkgline)) => {
            log::trace!("try_match_and_fill_pkgline: matched pkgkey {} to store pkgline {}", pkgkey, matching_pkgline);
            package_info.pkgline = matching_pkgline;
            Ok(true)
        }
        Ok(None) => {
            log::trace!("try_match_and_fill_pkgline: no store package matches pkgkey {} (checked {} candidates)", pkgkey, candidate_pkglines.len());
            Ok(false)
        }
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
    let total_by_pkgkey = store_pkglines_by_pkgkey.values().map(|v| v.len()).sum::<usize>();
    let total_by_pkgname = store_pkglines_by_pkgname.values().map(|v| v.len()).sum::<usize>();
    plan.store_pkglines_by_pkgname = store_pkglines_by_pkgname;

    log::debug!("fill_pkglines_in_plan: collected {} store pkglines by pkgkey, {} by pkgname",
        total_by_pkgkey, total_by_pkgname);

    let mut matched_count = 0;
    let mut processed_count = 0;

    // Process new packages (fresh installs and upgrades)
    for op in &mut plan.ordered_operations {
        if let Some(pkgkey) = &op.new_pkgkey {
            processed_count += 1;
            // Update the package info in plan.new_pkgs
            if let Some(package_info) = plan.new_pkgs.get_mut(pkgkey) {
                log::trace!("fill_pkglines_in_plan: processing pkgkey {}", pkgkey);
                if try_match_and_fill_pkgline(pkgkey, Arc::make_mut(package_info), &store_pkglines_by_pkgkey)? {
                    matched_count += 1;
                    log::trace!("fill_pkglines_in_plan: matched pkgkey {} -> pkgline {}", pkgkey, package_info.pkgline);
                } else {
                    log::trace!("fill_pkglines_in_plan: no match found for pkgkey {}", pkgkey);
                }
            } else {
                log::trace!("fill_pkglines_in_plan: pkgkey {} not found in plan.new_pkgs", pkgkey);
            }
        }
    }

    // Also process skipped reinstalls - they need pkgline for exposure
    for (pkgkey, info_arc) in plan.skipped_reinstalls.iter_mut() {
        log::trace!("fill_pkglines_in_plan: processing skipped reinstall {}", pkgkey);
        if try_match_and_fill_pkgline(pkgkey, Arc::make_mut(info_arc), &store_pkglines_by_pkgkey)? {
            matched_count += 1;
            log::trace!("fill_pkglines_in_plan: matched skipped reinstall {} -> pkgline {}", pkgkey, info_arc.pkgline);
        } else {
            log::warn!("fill_pkglines_in_plan: no match found for skipped reinstall {}", pkgkey);
        }
    }

    log::trace!("fill_pkglines_in_plan: processed {} packages, matched {}", processed_count, matched_count);
    Ok(matched_count)
}
