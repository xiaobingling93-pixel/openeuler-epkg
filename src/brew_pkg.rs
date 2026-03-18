use std::fs;
use std::path::Path;
use tar::Archive;
use log;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use flate2::read::GzDecoder;
use crate::lfs;
use crate::utils;

/// Homebrew placeholder prefixes that need to be rewritten in dylib paths
const HOMEBREW_PLACEHOLDER_PREFIXES: &[&str] = &[
    "@@HOMEBREW_CELLAR@@",
    "@@HOMEBREW_PREFIX@@",
];

/// Common metadata files that brew packages include at root level
/// These should be moved to info/brew/ to avoid conflicts between packages
const BREW_META_FILES: &[&str] = &[
    "AUTHORS",
    "CHANGELOG",
    "ChangeLog",
    "CHANGES",
    "COPYING",
    "HISTORY",
    "LICENSE",
    "NEWS",
    "NEWS.md",
    "README",
    "README.md",
    "README.txt",
    "RELEASE",
    "RELEASE_NOTES",
    "sbom.spdx.json",
    "TODO",
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
    let mut hard_links: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();

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
        // Check if this is a metadata file/directory at root level
        let stripped_components: Vec<_> = components.iter().skip(2).collect();

        // Check for .brew/ directory (contains formula with post_install etc)
        let is_brew_dir = stripped_components.first()
            .and_then(|c| c.as_os_str().to_str())
            .map(|s| s == ".brew")
            .unwrap_or(false);

        // Check for root-level metadata files
        let is_root_meta_file = !is_brew_dir && stripped_components.len() == 1 &&
            stripped_components[0].as_os_str().to_str()
                .map(|s| is_brew_meta_file(s))
                .unwrap_or(false);

        let target_path = if is_brew_dir {
            // Move .brew/ directory to info/brew/.brew/ (contains formula with post_install etc)
            stripped_components.iter().skip(1).fold(
                store_tmp_dir.join("info/brew/.brew"),
                |acc, comp| acc.join(comp.as_os_str())
            )
        } else if is_root_meta_file {
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

        // Check if this is a hard link entry
        let header = entry.header();
        let is_hard_link = matches!(header.entry_type(), tar::EntryType::Link);

        if is_hard_link {
            // For hard links, get the link target and convert to our stripped path
            if let Ok(Some(link_path)) = entry.link_name() {
                // link_path is the original path in tar, e.g., "perl/5.42.1_1/bin/perl"
                // We need to strip the first two components to get "bin/perl"
                let link_components: Vec<_> = link_path.components().collect();
                if link_components.len() >= 3 {
                    let stripped_link_components: Vec<_> = link_components.iter().skip(2).collect();
                    let source_path = stripped_link_components.iter().fold(
                        store_tmp_dir.join("fs"),
                        |acc, comp| acc.join(comp.as_os_str())
                    );
                    log::trace!("Queued hard link: {} -> {}",
                        target_path.display(), source_path.display());
                    hard_links.push((source_path, target_path));
                }
                continue;
            }
        }

        // Ensure parent directory exists
        if let Some(parent) = target_path.parent() {
            lfs::create_dir_all(parent)?;
        }

        // Extract the file
        entry.unpack(&target_path)?;
        utils::fixup_file_permissions(&target_path);
    }

    // Now create all hard links after all files have been extracted
    for (source_path, target_path) in hard_links {
        // Ensure parent directory exists for the hard link target
        if let Some(parent) = target_path.parent() {
            if let Err(e) = lfs::create_dir_all(parent) {
                log::warn!("Failed to create directory {} for hard link: {}", parent.display(), e);
                continue;
            }
        }

        // Create the hard link if the source file exists
        if lfs::exists_on_host(&source_path) {
            // Remove existing file if present (in case of re-extraction)
            if lfs::exists_on_host(&target_path) {
                if let Err(e) = lfs::remove_file(&target_path) {
                    log::warn!("Failed to remove existing file {}: {}", target_path.display(), e);
                }
            }
            if let Err(e) = fs::hard_link(&source_path, &target_path) {
                log::warn!("Failed to create hard link from {} to {}: {}",
                    source_path.display(), target_path.display(), e);
            } else {
                log::trace!("Created hard link: {} -> {}", target_path.display(), source_path.display());
            }
        } else {
            log::warn!("Cannot create hard link {}: source file {} does not exist",
                target_path.display(), source_path.display());
        }
    }

    log::debug!("Successfully unpacked Brew bottle with {} entries", entries_processed);

    // Note: Dylib path rewriting is done at link time (for Move link type)
    // because paths need to be absolute and point to the specific environment.
    // The store files retain the original placeholder paths until linking.

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

/// Rewrites Homebrew placeholder dylib paths in Mach-O binaries after linking.
///
/// Homebrew bottles contain placeholder paths like:
/// - @@HOMEBREW_CELLAR@@/pkgname/version/lib/libfoo.dylib
/// - @@HOMEBREW_PREFIX@@/opt/dependency/lib/libbar.dylib
///
/// These are rewritten to absolute paths under the environment root:
/// - @@HOMEBREW_PREFIX@@/opt/pkgname/lib/libfoo.dylib -> env_root/lib/libfoo.dylib
/// - @@HOMEBREW_CELLAR@@/pkgname/version/lib/libfoo.dylib -> env_root/lib/libfoo.dylib
///
/// This function is called after files are moved to the environment (LinkType::Move).
/// Each environment gets its own copy with paths specific to that environment.
#[cfg(target_os = "macos")]
pub fn rewrite_dylib_paths_for_env(env_root: &Path) -> Result<()> {
    // Collect all potential Mach-O files (binaries and dylibs)
    let mut mach_o_files: Vec<std::path::PathBuf> = Vec::new();

    // Scan bin/ directory
    let bin_dir = env_root.join("bin");
    if bin_dir.exists() {
        for entry in walkdir::WalkDir::new(&bin_dir).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file() && is_mach_o_file(path) {
                mach_o_files.push(path.to_path_buf());
            }
        }
    }

    // Scan lib/ directory
    let lib_dir = env_root.join("lib");
    if lib_dir.exists() {
        for entry in walkdir::WalkDir::new(&lib_dir).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file() && is_mach_o_file(path) {
                mach_o_files.push(path.to_path_buf());
            }
        }
    }

    if mach_o_files.is_empty() {
        log::debug!("No Mach-O files found in {}", env_root.display());
        return Ok(());
    }

    log::info!("Rewriting dylib paths in {} Mach-O files for env {}", mach_o_files.len(), env_root.display());

    for mach_o_path in &mach_o_files {
        if let Err(e) = rewrite_dylib_paths_for_file_in_env(mach_o_path, env_root) {
            log::warn!("Failed to rewrite dylib paths for {}: {}", mach_o_path.display(), e);
        }
    }

    Ok(())
}

/// Check if a file is a Mach-O binary (not a text file, etc.)
#[cfg(target_os = "macos")]
fn is_mach_o_file(path: &Path) -> bool {
    use std::io::Read;

    // Check file extension - skip common non-binary extensions
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ["txt", "md", "json", "xml", "html", "py", "sh", "pl", "rb"].contains(&ext) {
        return false;
    }

    // Check magic number for Mach-O
    if let Ok(mut file) = std::fs::File::open(path) {
        let mut magic = [0u8; 4];
        if file.read_exact(&mut magic).is_ok() {
            // Mach-O magic numbers:
            // 0xFEEDFACE (MH_MAGIC - 32-bit little endian)
            // 0xFEEDFACF (MH_MAGIC_64 - 64-bit little endian)
            // 0xCAFEFEED (MH_BUNDLE - universal binary)
            // 0xBEBAFECA (JAVA_CLASS)
            let magic_u32 = u32::from_ne_bytes(magic);
            return magic_u32 == 0xFEEDFACE ||
                   magic_u32 == 0xFEEDFACF ||
                   magic_u32 == 0xCAFEBABE ||
                   magic == [0xCA, 0xFE, 0xBA, 0xBE];
        }
    }
    false
}

/// Rewrite dylib paths for a single Mach-O file using install_name_tool
#[cfg(target_os = "macos")]
fn rewrite_dylib_paths_for_file_in_env(mach_o_path: &Path, env_root: &Path) -> Result<()> {
    use std::process::Command;

    // Get current dylib paths using otool -L
    let output = Command::new("otool")
        .arg("-L")
        .arg(mach_o_path)
        .output()
        .wrap_err_with(|| format!("Failed to run otool -L on {}", mach_o_path.display()))?;

    if !output.status.success() {
        return Err(eyre::eyre!("otool -L failed: {}", String::from_utf8_lossy(&output.stderr)));
    }

    let otool_output = String::from_utf8_lossy(&output.stdout);
    let mut changes: Vec<(String, String)> = Vec::new();

    for line in otool_output.lines() {
        let line = line.trim();

        // Parse dylib path from otool -L output
        // Format: "	/path/to/lib.dylib (compatibility version...)"
        // or: "	@@HOMEBREW_PREFIX@@/opt/dep/lib/libfoo.dylib (compatibility version...)"
        if let Some(path_end) = line.find(" (") {
            let dylib_path = &line[..path_end];

            // Check if this is a Homebrew placeholder path
            for prefix in HOMEBREW_PLACEHOLDER_PREFIXES {
                if dylib_path.starts_with(prefix) {
                    if let Some(new_path) = resolve_homebrew_dylib_path_for_env(dylib_path, prefix, env_root) {
                        log::debug!("Rewriting: {} -> {}", dylib_path, new_path);
                        changes.push((dylib_path.to_string(), new_path));
                    }
                    break;
                }
            }
        }
    }

    if changes.is_empty() {
        return Ok(());
    }

    // Apply changes using install_name_tool
    for (old_path, new_path) in &changes {
        let status = Command::new("install_name_tool")
            .arg("-change")
            .arg(old_path)
            .arg(new_path)
            .arg(mach_o_path)
            .status()
            .wrap_err_with(|| format!("Failed to run install_name_tool on {}", mach_o_path.display()))?;

        if !status.success() {
            log::warn!("install_name_tool -change {} {} failed for {}",
                old_path, new_path, mach_o_path.display());
        }
    }

    Ok(())
}

/// Resolve a Homebrew placeholder dylib path to an absolute path under env_root.
#[cfg(target_os = "macos")]
fn resolve_homebrew_dylib_path_for_env(placeholder_path: &str, prefix: &str, env_root: &Path) -> Option<String> {
    // Extract the path after the placeholder prefix
    let rest = &placeholder_path[prefix.len()..];

    match prefix {
        "@@HOMEBREW_PREFIX@@" => {
            // Format: /opt/pkgname/lib/libfoo.dylib or /lib/libfoo.dylib
            // The path after prefix may start with /opt/<pkgname>/ or directly /lib/
            // We want to extract the lib/foo.dylib part and resolve under env_root/lib/
            extract_lib_path_and_resolve(rest, env_root)
        }
        "@@HOMEBREW_CELLAR@@" => {
            // Format: /pkgname/version/lib/libfoo.dylib
            // Skip /pkgname/version/ part and find the actual path
            // The path structure is: /<pkgname>/<version>/<actual_path>
            let parts: Vec<&str> = rest.splitn(4, '/').collect();
            if parts.len() >= 4 {
                // parts[0] is empty (before first /), parts[1] is pkgname, parts[2] is version
                // parts[3] is the rest of the path like "lib/libfoo.dylib"
                extract_lib_path_and_resolve(&format!("/{}", parts[3]), env_root)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Extract library path and resolve under env_root.
/// Handles paths like /lib/libfoo.dylib, /opt/pkgname/lib/libfoo.dylib
#[cfg(target_os = "macos")]
fn extract_lib_path_and_resolve(rest: &str, env_root: &Path) -> Option<String> {
    // Try to find lib/ in the path
    if let Some(lib_pos) = rest.find("/lib/") {
        let lib_path = &rest[lib_pos + 1..]; // Skip the leading slash, get "lib/foo.dylib"
        let full_path = env_root.join(lib_path);
        if full_path.exists() {
            return Some(full_path.display().to_string());
        }
    }

    // Fallback: try lib directly under env_root with just the library name
    let lib_name = rest.rsplit('/').next()?;
    let lib_path = env_root.join("lib").join(lib_name);
    if lib_path.exists() {
        return Some(lib_path.display().to_string());
    }

    // If the library doesn't exist yet, still return the expected path
    // (it might be from another package being installed in the same batch)
    if rest.contains("/lib/") {
        let lib_pos = rest.find("/lib/").unwrap();
        let lib_path = &rest[lib_pos + 1..];
        return Some(env_root.join(lib_path).display().to_string());
    }

    None
}
