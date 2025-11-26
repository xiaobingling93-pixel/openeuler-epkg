use std::fs;
use std::io::{self, BufReader, BufWriter, Read};
use std::path::Path;
use std::os::unix::fs::{PermissionsExt, FileTypeExt, MetadataExt};
use tar::Archive;
use zstd::stream::Decoder;
use nix::unistd::{User, Group};
use nix::unistd;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use walkdir::WalkDir;
use uuid::Uuid;
use crate::models::{dirs, PackageFormat};
use log;

/// Unpacks multiple packages and moves them to the store
/// Returns a vector of paths to the final directories where packages were unpacked
pub fn unpack_packages(package_files: Vec<String>) -> Result<Vec<std::path::PathBuf>> {
    let mut final_dirs = Vec::new();
    for package_file in package_files {
        let final_dir = unpack_mv_package(&package_file, None)
            .wrap_err_with(|| format!("Failed to unpack package: {}", package_file))?;
        final_dirs.push(final_dir);
    }
    Ok(final_dirs)
}

/// Unpacks a single package and moves it to the final store location
/// Returns the path to the final directory where the package was unpacked
pub fn unpack_mv_package(package_file: &str, pkgkey: Option<&str>) -> Result<std::path::PathBuf> {
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
pub fn general_unpack_package<P: AsRef<Path>>(package_file: P, store_tmp_dir: P, pkgkey: Option<&str>) -> Result<()> {
    let package_file = package_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Detect package format from file extension
    let format = detect_package_format(package_file)
        .wrap_err_with(|| format!("Failed to detect package format for: {}", package_file.display()))?;

    match format {
        PackageFormat::Deb => {
            crate::deb_pkg::unpack_package(package_file, store_tmp_dir)?
        }
        PackageFormat::Rpm => {
            crate::rpm_pkg::unpack_package(package_file, store_tmp_dir)?
        }
        PackageFormat::Apk => {
            crate::apk_pkg::unpack_package(package_file, store_tmp_dir, pkgkey)?
        }
        PackageFormat::Pacman => {
            crate::arch_pkg::unpack_package(package_file, store_tmp_dir)?
        }
        PackageFormat::Conda => {
            crate::conda_pkg::unpack_package(package_file, store_tmp_dir)?
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

    if file_name.ends_with(".deb") {
        Ok(PackageFormat::Deb)
    } else if file_name.ends_with(".rpm") {
        Ok(PackageFormat::Rpm)
    } else if file_name.ends_with(".epkg") {
        Ok(PackageFormat::Epkg)
    } else if file_name.ends_with(".apk") {
        Ok(PackageFormat::Apk)
    } else if file_name.ends_with(".conda") {
        Ok(PackageFormat::Conda)
    } else if file_name.ends_with(".tar.bz2") {
        Ok(PackageFormat::Conda)
    } else if file_name.ends_with(".pkg.tar.xz") || file_name.ends_with(".pkg.tar.zst") {
        Ok(PackageFormat::Pacman)
    } else {
        Err(eyre::eyre!("Unknown package format for file: {}", file_name))
    }
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
        let relative_path_str = relative_path.to_string_lossy();
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
pub fn get_field_order() -> &'static [&'static str] {
    &[
        "pkgname", "source", "version", "release",
        "summary", "description", "homepage", "license", "arch", "maintainer",
        "buildRequires", "requiresPre", "requires", "provides", "conflicts",
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
        "pkgkey", "repodataName", "status",
    ]
}

/// Formats package fields with consistent field ordering
pub fn format_package_fields(package_fields: &[(String, String)]) -> String {
    let mut output = String::new();
    let field_order = get_field_order();

    // First, write fields in the preferred order
    for preferred_field in field_order {
        for (original_field, value) in package_fields {
            if original_field == preferred_field {
                output.push_str(&format!("{}: {}\n", original_field, value));
                break;
            }
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
pub fn save_package_txt<P: AsRef<Path>>(package_fields: Vec<(String, String)>, store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let package_txt_path = store_tmp_dir.join("info/package.txt");

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

#[allow(dead_code)]
pub fn unzst(input_path: &str, output_path: &str) -> Result<()> {
    let input_file = fs::File::open(input_path)
        .wrap_err_with(|| format!("Failed to open input file: {}", input_path))?;
    let reader = BufReader::new(input_file);

    let parent_dir = Path::new(output_path).parent()
        .ok_or_else(|| eyre::eyre!("Cannot determine parent directory for: {}", output_path))?;
    fs::create_dir_all(parent_dir)
        .wrap_err_with(|| format!("Failed to create directory: {}", parent_dir.display()))?;

    let output_file = fs::File::create(output_path)
        .wrap_err_with(|| format!("Failed to create output file: {}", output_path))?;
    let mut writer = BufWriter::new(output_file);

    let mut decoder = Decoder::new(reader)
        .wrap_err_with(|| format!("Failed to create Zstandard decoder for file: {}", input_path))?;
    io::copy(&mut decoder, &mut writer)
        .wrap_err_with(|| format!("Failed to decompress {} to {}", input_path, output_path))?;

    Ok(())
}

/// Collect all pkglines from the epkg store and organize them by pkgkey
/// Returns a HashMap where key is pkgkey (pkgname__version__arch) and value is a list of matching pkglines
pub fn collect_store_pkglines_by_pkgkey() -> Result<std::collections::HashMap<String, Vec<String>>> {
    use crate::models::dirs;
    use crate::package::pkgline2pkgkey;
    use std::fs;
    use std::collections::HashMap;

    let store_dir = dirs().epkg_store.clone();
    let mut store_pkglines_by_pkgkey: HashMap<String, Vec<String>> = HashMap::new();

    if !store_dir.exists() {
        return Ok(store_pkglines_by_pkgkey);
    }

    // Collect all pkglines from the store and organize by pkgkey
    if let Ok(entries) = fs::read_dir(&store_dir) {
        for entry in entries.flatten() {
            let package_path = entry.path();
            if package_path.is_dir() {
                if let Some(pkgline) = package_path.file_name().and_then(|name| name.to_str()) {
                    // Parse the pkgline to extract pkgkey
                    if let Ok(pkgkey) = pkgline2pkgkey(pkgline) {
                        store_pkglines_by_pkgkey
                            .entry(pkgkey)
                            .or_insert_with(Vec::new)
                            .push(pkgline.to_string());
                    }
                }
            }
        }
    }

    Ok(store_pkglines_by_pkgkey)
}

/// Match a package from repodata with packages in the store by comparing Package fields
/// Returns the matching pkgline if found, None otherwise
fn match_package_with_store(
    repodata_package: &crate::models::Package,
    store_pkglines: &[String],
    package_manager: &mut crate::models::PackageManager,
) -> Result<Option<String>> {
    // Try each candidate pkgline from the store
    for store_pkgline in store_pkglines {
        // Load Package from store
        match package_manager.map_pkgline2package(store_pkgline) {
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

/// Compare two Package structs to determine if they represent the same package
/// Compares multiple fields: pkgname, version, arch, source, sha256sum, sha1sum, buildTime
fn packages_match(repodata_pkg: &crate::models::Package, store_pkg: &crate::models::Package) -> bool {
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

    if repodata_pkg.pkgname != store_pkg.pkgname
        || repodata_pkg.version != store_pkg.version
        || repodata_pkg.arch != store_pkg.arch {
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

/// Try to match and fill pkgline for a single package entry
/// Returns true if a match was found and pkgline was filled, false otherwise
fn try_match_and_fill_pkgline(
    pkgkey: &str,
    package_info: &mut crate::models::InstalledPackageInfo,
    store_pkglines_by_pkgkey: &std::collections::HashMap<String, Vec<String>>,
    package_manager: &mut crate::models::PackageManager,
) -> Result<bool> {
    // Skip if pkgline is already filled
    if !package_info.pkgline.is_empty() {
        return Ok(false);
    }

    // Get candidate pkglines from store for this pkgkey
    let candidate_pkglines = match store_pkglines_by_pkgkey.get(pkgkey) {
        Some(pkglines) => pkglines,
        None => return Ok(false),
    };

    // Load Package from repodata
    let repodata_package = match package_manager.load_package_info(pkgkey) {
        Ok(pkg) => pkg,
        Err(e) => {
            log::debug!("Failed to load package info for {}: {}", pkgkey, e);
            return Ok(false);
        }
    };

    // Match with store packages
    match match_package_with_store(&repodata_package, candidate_pkglines, package_manager) {
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
    plan: &mut crate::install::InstallationPlan,
    package_manager: &mut crate::models::PackageManager,
) -> Result<usize> {
    // Collect store pkglines organized by pkgkey
    let store_pkglines_by_pkgkey = collect_store_pkglines_by_pkgkey()?;

    if store_pkglines_by_pkgkey.is_empty() {
        return Ok(0);
    }

    let mut matched_count = 0;

    // Process fresh_installs
    for (pkgkey, package_info) in plan.fresh_installs.iter_mut() {
        if try_match_and_fill_pkgline(pkgkey, package_info, &store_pkglines_by_pkgkey, package_manager)? {
            matched_count += 1;
        }
    }

    // Process upgrades_new
    for (pkgkey, package_info) in plan.upgrades_new.iter_mut() {
        if try_match_and_fill_pkgline(pkgkey, package_info, &store_pkglines_by_pkgkey, package_manager)? {
            matched_count += 1;
        }
    }

    Ok(matched_count)
}
