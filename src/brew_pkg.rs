use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use tar::Archive;
use log;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use flate2::read::GzDecoder;
use crate::lfs;
use crate::tar_extract::{create_package_dirs, ExtractConfig, extract_archive_with_policy};

/// Homebrew placeholder prefixes that need to be rewritten in dylib paths
#[cfg(target_os = "macos")]
const HOMEBREW_PLACEHOLDER_PREFIXES: &[&str] = &[
    "@@HOMEBREW_CELLAR@@",
    "@@HOMEBREW_PREFIX@@",
];

/// Global lock to serialize install_name_tool rewrites.
/// Parallel package linking can trigger concurrent rewrite passes on the same env files.
#[cfg(target_os = "macos")]
static BREW_DYLIB_REWRITE_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

/// Common metadata file prefixes (base names) that brew packages include at root level
/// These should be moved to info/brew/ to avoid conflicts between packages
const BREW_META_FILE_PREFIXES: &[&str] = &[
    "AUTHORS",
    "CHANGELOG",
    "CHANGES",
    "COPYING",
    "HISTORY",
    "LICENSE",
    "NEWS",
    "README",
    "RELEASE",
    "TODO",
];

/// Specific metadata file names to check
const BREW_META_FILE_NAMES: &[&str] = &[
    "ChangeLog",
    "RELEASE_NOTES",
    "sbom.spdx.json",
];

/// Check if a path component is a brew metadata file
/// Matches files at root level with ALL CAPS names (possibly with extensions)
fn is_brew_meta_file(name: &str) -> bool {
    // Check specific file names
    if BREW_META_FILE_NAMES.iter().any(|&meta| name == meta) {
        return true;
    }

    // Check if name starts with one of the known prefixes
    // Handles: LICENSE, LICENSE.md, README.rst, etc.
    for prefix in BREW_META_FILE_PREFIXES {
        if name == *prefix {
            return true;
        }
        // Check with extension: PREFIX.ext (e.g., README.md, CHANGELOG.md)
        if name.starts_with(prefix) && name.len() > prefix.len() {
            let rest = &name[prefix.len()..];
            // Match .ext patterns where ext is lowercase letters
            if rest.starts_with('.') && rest[1..].chars().all(|c| c.is_ascii_lowercase()) {
                return true;
            }
        }
    }

    // Check for ALL CAPS names (possibly with extensions)
    // e.g., AUTHORS, CONTRIBUTORS, PATENTS, etc.
    let base_name = name.split('.').next().unwrap_or(name);
    if base_name.chars().all(|c| c.is_ascii_uppercase() || c == '_' || c == '-') && base_name.len() > 2 {
        // Must have at least some uppercase letters (not just ___ or ---)
        if base_name.chars().any(|c| c.is_ascii_uppercase()) {
            return true;
        }
    }

    false
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
    create_package_dirs(store_tmp_dir, "brew")?;

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
    let archive = Archive::new(decoder);

    // Use policy-based extraction for Brew bottles
    extract_brew_contents(archive, store_tmp_dir)?;

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

/// Path policy for Brew bottles
///
/// Brew bottles have a top-level directory like "package_name/version/"
/// - Skip top-level entries (package_name/, package_name/version/)
/// - .brew/ directory goes to info/brew/.brew/
/// - Root-level metadata files go to info/brew/
/// - Regular files go to fs/
fn brew_path_policy(path: &Path, _is_hard_link: bool, store_tmp_dir: &Path) -> Option<PathBuf> {
    // Strip the top-level directory (package_name/version/)
    // Path looks like: "jq/1.7.1/bin/jq" -> we want "bin/jq"
    let components: Vec<_> = path.components().collect();
    if components.len() < 3 {
        // Skip top-level entries (package_name/, package_name/version/)
        return None;
    }

    // Reconstruct path without first two components (package_name and version)
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

    Some(target_path)
}

/// Extract Brew bottle contents using policy-based extraction
fn extract_brew_contents<R: Read>(
    archive: Archive<R>,
    store_tmp_dir: &Path,
) -> Result<usize> {
    let config = ExtractConfig::new(store_tmp_dir)
        .handle_hard_links(true);

    let policy: crate::tar_extract::PathPolicy = Box::new(brew_path_policy);
    let mut archive = archive;
    extract_archive_with_policy(&mut archive, &config, policy)
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
    let _rewrite_guard = BREW_DYLIB_REWRITE_LOCK
        .lock()
        .map_err(|e| eyre::eyre!("Failed to acquire brew dylib rewrite lock: {}", e))?;

    // Collect all potential Mach-O files (binaries and dylibs)
    let mut mach_o_files: Vec<std::path::PathBuf> = Vec::new();
    let mut seen_real_paths: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();

    // Scan bin/ directory
    let bin_dir = env_root.join("bin");
    if bin_dir.exists() {
        for entry in walkdir::WalkDir::new(&bin_dir).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if entry.file_type().is_symlink() {
                continue;
            }
            if path.is_file() && is_mach_o_file(path) {
                let real_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                if seen_real_paths.insert(real_path.clone()) {
                    mach_o_files.push(real_path);
                }
            }
        }
    }

    // Scan lib/ directory
    let lib_dir = env_root.join("lib");
    if lib_dir.exists() {
        for entry in walkdir::WalkDir::new(&lib_dir).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if entry.file_type().is_symlink() {
                continue;
            }
            if path.is_file() && is_mach_o_file(path) {
                let real_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                if seen_real_paths.insert(real_path.clone()) {
                    mach_o_files.push(real_path);
                }
            }
        }
    }

    // Scan Frameworks/ directory (macOS Python framework, etc.)
    let frameworks_dir = env_root.join("Frameworks");
    if frameworks_dir.exists() {
        for entry in walkdir::WalkDir::new(&frameworks_dir).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if entry.file_type().is_symlink() {
                continue;
            }
            if path.is_file() && is_mach_o_file(path) {
                let real_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                if seen_real_paths.insert(real_path.clone()) {
                    mach_o_files.push(real_path);
                }
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
/// Handles paths like /lib/libfoo.dylib, /opt/pkgname/lib/libfoo.dylib, /Frameworks/...
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

    // Try to find Frameworks/ in the path (for macOS Python framework, etc.)
    if let Some(fw_pos) = rest.find("/Frameworks/") {
        let fw_path = &rest[fw_pos + 1..]; // Get "Frameworks/..."
        let full_path = env_root.join(fw_path);
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

    // Same for Frameworks - return expected path even if doesn't exist yet
    if rest.contains("/Frameworks/") {
        let fw_pos = rest.find("/Frameworks/").unwrap();
        let fw_path = &rest[fw_pos + 1..];
        return Some(env_root.join(fw_path).display().to_string());
    }

    None
}
