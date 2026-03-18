use std::fs;
use std::path::Path;
use tar::Archive;
use log;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use flate2::read::GzDecoder;
use crate::lfs;
use crate::utils;

/// Common metadata files that brew packages include at root level
/// These should be moved to info/brew/ to avoid conflicts between packages
const BREW_META_FILES: &[&str] = &[
    "AUTHORS",
    "CHANGELOG",
    "ChangeLog",
    "CHANGES",
    "COPYING",
    "LICENSE",
    "NEWS",
    "NEWS.md",
    "README",
    "README.md",
    "README.txt",
    "RELEASE",
    "RELEASE_NOTES",
    "sbom.spdx.json",
];

/// Check if a path component is a brew metadata file
fn is_brew_meta_file(name: &str) -> bool {
    BREW_META_FILES.iter().any(|&meta| name == meta)
}

/// Unpacks a Brew bottle to the specified directory
///
/// Brew bottles are gzipped tar archives containing precompiled binaries.
/// The structure is:
///   package_name/version/  (e.g., jq/1.7.1/)
///     bin/
///     lib/
///     share/
///     ...
///
/// We extract to fs/ and create info files separately from formula metadata
pub fn unpack_package<P: AsRef<Path>>(bottle_file: P, store_tmp_dir: P, pkgkey: Option<&str>) -> Result<()> {
    let bottle_file = bottle_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Create the required directory structure following project pattern
    fs::create_dir_all(store_tmp_dir.join("fs"))?;
    fs::create_dir_all(store_tmp_dir.join("info/brew"))?;

    log::debug!("Unpacking Brew bottle: {}", bottle_file.display());

    // Validate file exists and is readable
    let metadata = lfs::metadata_on_host(bottle_file)
        .wrap_err_with(|| format!("Failed to read file metadata: {}", bottle_file.display()))?;

    let file_size = metadata.len();
    if file_size == 0 {
        return Err(eyre::eyre!(
            "File is empty (0 bytes): {}. The download may be incomplete or the file may be corrupted.",
            bottle_file.display()
        ));
    }

    // Open and decompress the bottle
    let file = fs::File::open(bottle_file)
        .wrap_err_with(|| format!("Failed to open bottle file: {}", bottle_file.display()))?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    // Brew bottles have a top-level directory like "package_name/version/"
    // We need to strip this prefix and extract contents directly to fs/
    let mut entries_processed = 0;

    for entry_result in archive.entries()? {
        let mut entry = entry_result?;
        let path = entry.path()?.to_path_buf();
        entries_processed += 1;

        log::trace!("Processing tar entry #{}: {}", entries_processed, path.display());

        // Strip the top-level directory (package_name/version/)
        // Path looks like: "jq/1.7.1/bin/jq" -> we want "bin/jq"
        let components: Vec<_> = path.components().collect();
        if components.len() < 3 {
            // Skip top-level entries (package_name/, package_name/version/)
            log::trace!("Skipping top-level entry: {}", path.display());
            continue;
        }

        // Reconstruct path without first two components (package_name and version)
        // Check if this is a metadata file at root level (directly under package/version/)
        let stripped_components: Vec<_> = components.iter().skip(2).collect();
        let is_root_meta_file = stripped_components.len() == 1 &&
            stripped_components[0].as_os_str().to_str()
                .map(|s| is_brew_meta_file(s))
                .unwrap_or(false);

        let target_path = if is_root_meta_file {
            // Move metadata files to info/brew/ to avoid conflicts
            store_tmp_dir.join("info/brew").join(
                stripped_components[0].as_os_str()
            )
        } else {
            stripped_components.iter().fold(
                store_tmp_dir.join("fs"),
                |acc, comp| acc.join(comp.as_os_str())
            )
        };

        // Ensure parent directory exists
        if let Some(parent) = target_path.parent() {
            lfs::create_dir_all(parent)?;
        }

        // Extract the file
        entry.unpack(&target_path)?;
        utils::fixup_file_permissions(&target_path);
    }

    log::debug!("Successfully unpacked Brew bottle with {} entries", entries_processed);

    // Create package.txt from pkgkey
    // pkgkey format: {pkgname}__{version}__{arch}
    if let Some(key) = pkgkey {
        create_package_txt_from_pkgkey(store_tmp_dir, key)
            .wrap_err_with(|| format!("Failed to create package.txt for {}", store_tmp_dir.display()))?;
    } else {
        return Err(eyre::eyre!("pkgkey is required for Brew package unpacking"));
    }

    // Generate filelist.txt following project pattern
    crate::store::create_filelist_txt(store_tmp_dir)
        .wrap_err_with(|| format!("Failed to create filelist.txt for {}", store_tmp_dir.display()))?;

    Ok(())
}

/// Creates package.txt from pkgkey
fn create_package_txt_from_pkgkey<P: AsRef<Path>>(store_tmp_dir: P, pkgkey: &str) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Parse pkgkey: {pkgname}__{version}__{arch}
    let parts: Vec<&str> = pkgkey.rsplitn(3, "__").collect();
    if parts.len() != 3 {
        return Err(eyre::eyre!("Invalid pkgkey format, expected 3 parts: {}", pkgkey));
    }

    let arch = parts[0];
    let version = parts[1];
    let pkgname = parts[2];

    // Create package fields
    let mut package_fields: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    package_fields.insert("pkgname".to_string(), pkgname.to_string());
    package_fields.insert("version".to_string(), version.to_string());
    package_fields.insert("arch".to_string(), arch.to_string());
    package_fields.insert("format".to_string(), "brew".to_string());

    // Save the package.txt file using the common store function
    crate::store::save_package_txt(package_fields, store_tmp_dir, Some(pkgkey))
        .wrap_err("Failed to save package.txt")?;

    Ok(())
}
