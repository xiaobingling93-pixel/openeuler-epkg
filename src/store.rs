use std::fs;
use std::io::{self, BufReader, BufWriter, Read};
use std::path::Path;
use std::os::unix::fs::{PermissionsExt, FileTypeExt, MetadataExt};
use tar::Archive;
use nix::unistd::{chown, User, Group};
use zstd::stream::Decoder;
use users::get_effective_uid;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use walkdir::WalkDir;
use uuid::Uuid;
use crate::models::{dirs, PackageFormat};
use log::warn;

/// Unpacks multiple packages and moves them to the store
pub fn unpack_packages(package_files: Vec<String>) -> Result<Vec<String>> {
    let mut pkgidlines = Vec::new();
    for package_file in package_files {
        let pkgidline = unpack_mv_package(&package_file)?;
        pkgidlines.push(pkgidline);
    }
    Ok(pkgidlines)
}

/// Unpacks a single package and moves it to the final store location
pub fn unpack_mv_package(package_file: &str) -> Result<String> {
    // Create temporary directory for unpacking
    let temp_name = Uuid::new_v4().to_string();
    let store_tmp_dir = dirs().epkg_cache.join("unpack").join(&temp_name);
    fs::create_dir_all(&store_tmp_dir)?;

    // Unpack the package
    general_unpack_package(Path::new(package_file), &store_tmp_dir)?;

    // Calculate content-addressable hash
    let ca_hash_real = crate::hash::epkg_store_hash(store_tmp_dir.to_str().unwrap())?;

    // Read package.txt to get package name and version
    let package_txt_path = store_tmp_dir.join("info/package.txt");
    let package_content = fs::read_to_string(&package_txt_path)?;

    let mut pkgname = String::new();
    let mut version = String::new();
    let mut ca_hash = String::new();
    let mut pkgid = String::new();

    for line in package_content.lines() {
        if let Some((key, value)) = line.split_once(": ") {
            match key {
                "pkgname" => pkgname = value.to_string(),
                "version" => version = value.to_string(),
                "caHash"  => ca_hash = value.to_string(),
                "sha256"  => pkgid = value.to_string(),
                "sha1"    => pkgid = value.to_string(),
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
        fs::write(&package_txt_path, updated_content)?;
    } else if ca_hash != ca_hash_real {
        return Err(eyre::eyre!("caHash in package.txt does not match calculated hash"));
    }

    // Create final package directory name
    let pkgline = format!("{}__{}__{}", ca_hash_real, pkgname, version);
    let final_dir = dirs().epkg_store.join(&pkgline);

    // Move to final location
    if final_dir.exists() {
        fs::remove_dir_all(&store_tmp_dir)?;
    }

    fs::rename(&store_tmp_dir, &final_dir)?;
    set_perm_and_owner(final_dir.to_str().unwrap())?;

    let pkgidline = format!("{}__{}", pkgid, pkgline);
    Ok(pkgidline)
}

/// Generic package unpacking function that detects format and delegates to appropriate handler
pub fn general_unpack_package<P: AsRef<Path>>(package_file: P, store_tmp_dir: P) -> Result<()> {
    let package_file = package_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Detect package format from file extension
    let format = detect_package_format(package_file)?;

    match format {
        PackageFormat::Deb => {
            crate::deb_pkg::unpack_package(package_file, store_tmp_dir)?;
        }
        PackageFormat::Rpm => {
            crate::rpm_pkg::unpack_package(package_file, store_tmp_dir)?;
        }
        PackageFormat::Apk => {
            crate::apk_pkg::unpack_package(package_file, store_tmp_dir)?;
        }
        PackageFormat::Epkg => {
            // Handle existing .epkg format
            crate::epkg::unpack_package(package_file, store_tmp_dir)?;
        }
        _ => {
            return Err(eyre::eyre!("Unsupported package format: {:?}", format));
        }
    }

    Ok(())
}

/// Detects package format from file extension
fn detect_package_format(package_file: &Path) -> Result<PackageFormat> {
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
    for entry in WalkDir::new(&fs_dir).sort_by_file_name() {
        let entry = entry?;
        let path = entry.path();

        // Get relative path from fs directory
        let relative_path = path.strip_prefix(&fs_dir)?;
        if relative_path.as_os_str().is_empty() {
            continue; // Skip the fs directory itself
        }

        let metadata = fs::symlink_metadata(path)?;
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
                let hash = calculate_file_sha256(path)?;
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

        if uid != 0 {
            if let Ok(Some(user)) = User::from_uid(uid.into()) {
                attrs.push(format!("uname={}", user.name));
            }
        }

        if gid != 0 {
            if let Ok(Some(group)) = Group::from_gid(gid.into()) {
                attrs.push(format!("gname={}", group.name));
            }
        }

        // Write entry to filelist
        let relative_path_str = relative_path.to_string_lossy();
        let attrs_str = attrs.join(" ");
        output.push_str(&format!("{} {}\n", relative_path_str, attrs_str));
    }

    fs::write(filelist_path, output)?;
    Ok(())
}

/// Calculates SHA256 hash of a file
fn calculate_file_sha256(path: &Path) -> Result<String> {
    use sha2::{Sha256, Digest};

    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];

    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

/// Saves package fields to package.txt file with consistent field ordering
pub fn save_package_txt<P: AsRef<Path>>(package_fields: Vec<(String, String)>, store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let package_txt_path = store_tmp_dir.join("info/package.txt");

    // Write package.txt with mapped field names in a consistent order
    let mut output = String::new();

    // Define the preferred order for common fields
    let field_order = [
        "pkgname", "version", "summary", "license", "release", "homepage", "arch", "maintainer",
        "description", "buildRequires", "requiresPre", "requires", "provides", "conflicts",
        "suggests", "recommends", "supplements", "enhances", "breaks", "replaces", "originUrl",
        "recipeMaintainers", "subdir", "constrains", "requirements", "commit", "caHash", "caHashVersion",
        "size", "section", "priority", "buildTime", "buildHost", "group", "cookie", "platform", "source",
        "sourcePkgId", "rsaHeader", "sha256Header", "OriginalVcsBrowser", "OriginalVcsGit", "builtUsing",
        "originalMaintainer", "conffiles", "changelogTime", "changelogName", "changelogText",
        "installedSize", "location", "sha256", "md5sum", "descriptionMd5", "multiArch", "tag",
        "protected", "essential", "important", "buildEssential", "buildIds", "comment",
        "rubyVersions", "luaVersions", "pythonVersion", "pythonEggName", "staticBuiltUsing",
        "javascriptBuiltUsing", "xCargoBuiltUsing", "builtUsingNewlibSource", "goImportPath",
        "ghcPackage", "efiVendor", "cnfIgnoreCommands", "cnfVisiblePkgname", "cnfExtraCommands",
        "gstreamerVersion", "gstreamerElements", "gstreamerUriSources", "gstreamerUriSinks",
        "gstreamerEncoders", "gstreamerDecoders", "postgresqlCatversion"
    ];

    // First, write fields in the preferred order
    for preferred_field in &field_order {
        for (original_field, value) in &package_fields {
            if original_field == preferred_field {
                output.push_str(&format!("{}: {}\n", original_field, value));
                break;
            }
        }
    }

    // Then write any remaining fields that weren't in the preferred order
    for (original_field, value) in &package_fields {
        if !field_order.contains(&original_field.as_str()) {
            warn!("Field name '{}' not found in predefined field order list", original_field);
            output.push_str(&format!("{}: {}\n", original_field, value));
        }
    }

    fs::write(package_txt_path, output)?;

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

pub fn set_perm_and_owner(dir_str: &str) -> Result<()> {
    // get uid | gid
    let current_uid = get_effective_uid();
    let user_account = User::from_uid(current_uid.into())
        .wrap_err("Failed to get user information from UID")?;
    let user_account = user_account
        .ok_or_else(|| eyre::eyre!("Current user not found"))?;
    let uid = Some(user_account.uid);
    let gid = Some(user_account.gid);

    // chmod 755, chown USER:USER
    for entry_result in WalkDir::new(dir_str) {
        let entry = entry_result
            .wrap_err_with(|| format!("Failed to access entry in directory: {}", dir_str))?;
        let path = entry.path();

        if !path.exists() || path.is_symlink() {
            continue;
        }

        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .wrap_err_with(|| format!("Failed to set permissions on: {}", path.display()))?;

        chown(path, uid, gid)
            .wrap_err_with(|| format!("Failed to change ownership of: {}", path.display()))?;
    }
    Ok(())
}
