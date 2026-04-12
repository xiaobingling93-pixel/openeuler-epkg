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

/// Homebrew placeholder prefixes that need to be rewritten in dylib/interpreter paths
const HOMEBREW_PLACEHOLDER_PREFIXES: &[&str] = &[
    "@@HOMEBREW_CELLAR@@",
    "@@HOMEBREW_PREFIX@@",
];

/// Homebrew preferred installation prefixes.
///
/// Homebrew bottles are precompiled binaries that expect to be installed at specific
/// prefixes. Using the correct prefix is required for most bottles to work.
///
/// See: https://docs.brew.sh/Homebrew-on-Linux
/// See: https://docs.brew.sh/Installation
pub mod prefix {
    use std::path::PathBuf;

    /// Linux preferred prefix: /home/linuxbrew/.linuxbrew
    /// This avoids writing to system-owned directories while allowing bottles to work.
    pub const LINUX: &str = "/home/linuxbrew/.linuxbrew";

    /// macOS ARM (Apple Silicon) preferred prefix: /opt/homebrew
    #[cfg(target_os = "macos")]
    pub const MACOS_ARM: &str = "/opt/homebrew";

    /// macOS Intel preferred prefix: /usr/local
    #[cfg(target_os = "macos")]
    pub const MACOS_INTEL: &str = "/usr/local";

    /// Get the preferred HOMEBREW_PREFIX for the current platform.
    pub fn preferred() -> &'static str {
        #[cfg(target_os = "macos")]
        {
            // Detect Apple Silicon vs Intel
            if is_apple_silicon() {
                MACOS_ARM
            } else {
                MACOS_INTEL
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            LINUX
        }
    }

    /// Get the preferred prefix as a PathBuf
    pub fn preferred_path() -> PathBuf {
        PathBuf::from(preferred())
    }

    #[cfg(target_os = "macos")]
    fn is_apple_silicon() -> bool {
        // Check for ARM64 architecture
        std::env::consts::ARCH == "aarch64"
    }
}

/// Global lock to serialize install_name_tool/patchelf rewrites.
/// Parallel package linking can trigger concurrent rewrite passes on the same env files.
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
            crate::dirs::path_join(store_tmp_dir, &["info", "brew", ".brew"]),
            |acc, comp| acc.join(comp.as_os_str())
        )
    } else if is_root_meta_file {
        // Move metadata files to info/brew/ to avoid conflicts
        crate::dirs::path_join(store_tmp_dir, &["info", "brew"]).join(
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
/// Rewrite dylib/interpreter paths for all binaries in the environment.
/// On macOS: rewrite dylib paths in Mach-O files using install_name_tool.
/// On Linux: rewrite ELF interpreter paths using patchelf.
pub fn rewrite_dylib_paths_for_env(env_root: &Path) -> Result<()> {
    let _rewrite_guard = BREW_DYLIB_REWRITE_LOCK
        .lock()
        .map_err(|e| eyre::eyre!("Failed to acquire brew dylib rewrite lock: {}", e))?;

    #[cfg(target_os = "macos")]
    {
        rewrite_mach_o_dylib_paths(env_root)
    }

    #[cfg(target_os = "linux")]
    {
        rewrite_elf_interpreter_paths(env_root)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        // Other platforms: nothing to do
        Ok(())
    }
}

/// Rewrite dylib paths in Mach-O files (macOS).
#[cfg(target_os = "macos")]
fn rewrite_mach_o_dylib_paths(env_root: &Path) -> Result<()> {
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

    // Scan libexec/ directory (gcc internal tools like cc1, cc1plus are here)
    let libexec_dir = env_root.join("libexec");
    if libexec_dir.exists() {
        for entry in walkdir::WalkDir::new(&libexec_dir).into_iter().filter_map(|e| e.ok()) {
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

    // Remove code signature before modifying Mach-O to avoid warnings
    // install_name_tool changes invalidate code signatures
    let _ = Command::new("codesign")
        .arg("--remove-signature")
        .arg(mach_o_path)
        .status();
    // Ignore failure - file may not be signed or may be ad-hoc signed

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

// ============================================================================
// Linux ELF Interpreter Path Rewriting
// ============================================================================

/// Rewrite ELF interpreter paths for all binaries in the environment (Linux).
/// Homebrew Linux bottles use @@HOMEBREW_PREFIX@@ as a placeholder in the
/// ELF interpreter path (PT_INTERP). This function replaces
/// those paths to point to the actual environment location.
#[cfg(target_os = "linux")]
fn rewrite_elf_interpreter_paths(env_root: &Path) -> Result<()> {
    // Collect all potential ELF files
    let mut elf_files: Vec<std::path::PathBuf> = Vec::new();
    let mut seen_real_paths: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();

    // Scan bin/ directory
    let bin_dir = env_root.join("bin");
    if bin_dir.exists() {
        for entry in walkdir::WalkDir::new(&bin_dir).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if entry.file_type().is_symlink() {
                continue;
            }
            if path.is_file() && is_elf_file(path) {
                let real_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                if seen_real_paths.insert(real_path.clone()) {
                    elf_files.push(real_path);
                }
            }
        }
    }

    // Scan libexec/ directory
    let libexec_dir = env_root.join("libexec");
    if libexec_dir.exists() {
        for entry in walkdir::WalkDir::new(&libexec_dir).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if entry.file_type().is_symlink() {
                continue;
            }
            if path.is_file() && is_elf_file(path) {
                let real_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                if seen_real_paths.insert(real_path.clone()) {
                    elf_files.push(real_path);
                }
            }
        }
    }

    if elf_files.is_empty() {
        log::debug!("No ELF files found in {}", env_root.display());
        return Ok(());
    }

    // Scan lib/ directory for libraries (they also have RPATH that needs rewriting)
    let lib_dir = env_root.join("lib");
    if lib_dir.exists() {
        for entry in walkdir::WalkDir::new(&lib_dir).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if entry.file_type().is_symlink() {
                continue;
            }
            if path.is_file() && is_elf_file(path) {
                let real_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                if seen_real_paths.insert(real_path.clone()) {
                    elf_files.push(real_path);
                }
            }
        }
    }

    log::info!("Checking ELF paths in {} files for env {}", elf_files.len(), env_root.display());

    // Homebrew Linux bottles use @@HOMEBREW_PREFIX@@/lib/ld.so as the interpreter path.
    // Replace it with the system's dynamic linker.
    // x86_64 Linux typically uses /lib64/ld-linux-x86-64.so.2
    let new_interpreter = "/lib64/ld-linux-x86-64.so.2";

    for elf_path in &elf_files {
        if let Err(e) = rewrite_elf_interpreter_for_file(elf_path, &new_interpreter) {
            log::warn!("Failed to rewrite ELF interpreter for {}: {}", elf_path.display(), e);
        }
    }

    Ok(())
}

/// Check if a file is an ELF binary.
#[cfg(target_os = "linux")]
fn is_elf_file(path: &Path) -> bool {
    use std::io::Read;

    // Check file extension - skip common non-binary extensions
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ["txt", "md", "json", "xml", "html", "py", "sh", "pl", "rb"].contains(&ext) {
        return false;
    }

    // Check magic number for ELF
    if let Ok(mut file) = std::fs::File::open(path) {
        let mut magic = [0u8; 4];
        if file.read_exact(&mut magic).is_ok() {
            // ELF magic: 0x7F 'E' 'L' 'F'
            return magic == [0x7F, 0x45, 0x4C, 0x46];
        }
    }
    false
}

/// Rewrite ELF interpreter and RPATH for a single file using goblin.
/// Homebrew Linux bottles use @@HOMEBREW_PREFIX@@ as a placeholder in the
/// ELF interpreter path (PT_INTERP) and RPATH. This function replaces them.
#[cfg(target_os = "linux")]
fn rewrite_elf_interpreter_for_file(elf_path: &Path, new_interpreter: &str) -> Result<()> {
    use goblin::elf::Elf;
    use std::io::{Seek, SeekFrom, Read, Write};

    // Read the file
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(elf_path)
        .wrap_err_with(|| format!("Failed to open ELF file {}", elf_path.display()))?;

    let mut content = Vec::new();
    file.read_to_end(&mut content)?;

    // Parse ELF - this borrows from content, so we extract info first
    let elf = match Elf::parse(&content) {
        Ok(elf) => elf,
        Err(e) => {
            log::debug!("Not a valid ELF file {}: {}", elf_path.display(), e);
            return Ok(());
        }
    };

    // Extract RPATH information before modifying content
    let rpath_info = extract_rpath_info(&elf, &content, elf_path);

    // Extract interpreter information
    let interp_info = extract_interp_info(&elf, &content, elf_path);

    // Now modify content - interpreter first
    if let Some((offset, old_str, max_len)) = interp_info {
        if HOMEBREW_PLACEHOLDER_PREFIXES.iter().any(|p| old_str.contains(p)) {
            let new_interp_bytes = new_interpreter.as_bytes();
            if new_interp_bytes.len() + 1 <= max_len {
                let interp_slice = &mut content[offset..offset + max_len];
                interp_slice.fill(0);
                interp_slice[..new_interp_bytes.len()].copy_from_slice(new_interp_bytes);
                interp_slice[new_interp_bytes.len()] = 0;
                log::info!("Rewrote ELF interpreter for {}: {} -> {}",
                    elf_path.display(), old_str, new_interpreter);
            } else {
                log::warn!("New interpreter path too long for {}: need {} bytes, have {} bytes",
                    elf_path.display(), new_interp_bytes.len() + 1, max_len);
            }
        }
    }

    // Then modify RPATH
    for (str_offset, old_rpath, max_len) in rpath_info {
        if HOMEBREW_PLACEHOLDER_PREFIXES.iter().any(|p| old_rpath.contains(p)) {
            let homebrew_prefix = crate::brew_pkg::prefix::preferred();
            let mut new_rpath = old_rpath.clone();
            for placeholder in HOMEBREW_PLACEHOLDER_PREFIXES {
                new_rpath = new_rpath.replace(placeholder, homebrew_prefix);
            }

            let new_len = new_rpath.len() + 1;
            if new_len <= max_len {
                let rpath_slice = &mut content[str_offset..str_offset + max_len];
                rpath_slice.fill(0);
                let new_bytes = new_rpath.as_bytes();
                rpath_slice[..new_bytes.len()].copy_from_slice(new_bytes);
                rpath_slice[new_bytes.len()] = 0;
                log::info!("Rewrote ELF RPATH for {}: {} -> {}",
                    elf_path.display(), old_rpath, new_rpath);
            } else {
                log::warn!("New RPATH too long for {}: need {} bytes, have {} bytes (old: {}, new: {})",
                    elf_path.display(), new_len, max_len, old_rpath, new_rpath);
            }
        }
    }

    // Write back to file
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&content)?;
    file.set_len(content.len() as u64)?;

    Ok(())
}

/// Extract interpreter information from ELF.
/// Returns (offset, current_string, max_length) if PT_INTERP exists.
#[cfg(target_os = "linux")]
fn extract_interp_info(elf: &goblin::elf::Elf, content: &[u8], _elf_path: &Path) -> Option<(usize, String, usize)> {
    let interp_phdr = elf.program_headers.iter().find(|ph| ph.p_type == goblin::elf::program_header::PT_INTERP)?;

    let offset = interp_phdr.p_offset as usize;
    let filesz = interp_phdr.p_filesz as usize;

    if offset == 0 || filesz == 0 {
        return None;
    }

    let current_str = read_null_terminated_string(content, offset)?;
    Some((offset, current_str, filesz))
}

/// Extract RPATH information from ELF.
/// Returns Vec of (string_offset, current_string, max_length) for each RPATH/RUNPATH entry.
#[cfg(target_os = "linux")]
fn extract_rpath_info(elf: &goblin::elf::Elf, content: &[u8], _elf_path: &Path) -> Vec<(usize, String, usize)> {
    let mut result = Vec::new();

    let dyn_section = match elf.dynamic.as_ref() {
        Some(d) => d,
        None => return result,
    };

    // Get dynstr section offset from section headers
    let dynstr_sh = elf.section_headers.iter()
        .find(|sh| elf.shdr_strtab.get_at(sh.sh_name).map(|name| name == ".dynstr").unwrap_or(false));

    let dynstr_offset = match dynstr_sh {
        Some(sh) => sh.sh_offset as usize,
        None => return result,
    };

    // Find all RPATH/RUNPATH entries
    let rpath_entries: Vec<_> = dyn_section.dyns.iter()
        .filter(|e| e.d_tag == goblin::elf::dynamic::DT_RPATH || e.d_tag == goblin::elf::dynamic::DT_RUNPATH)
        .collect();

    for entry in &rpath_entries {
        let str_offset = dynstr_offset + entry.d_val as usize;

        if let Some(current_str) = read_null_terminated_string(content, str_offset) {
            // Calculate max length by finding the next string or end of section
            let max_len = find_string_max_length(content, str_offset);
            result.push((str_offset, current_str, max_len));
        }
    }

    result
}

/// Find the maximum length of a string at the given offset (until next string or end of content).
#[cfg(target_os = "linux")]
fn find_string_max_length(content: &[u8], offset: usize) -> usize {
    if offset >= content.len() {
        return 0;
    }
    // Find the null terminator
    let end = content[offset..].iter().position(|&b| b == 0).unwrap_or(content.len() - offset);
    // The max length includes the null terminator
    end + 1
}

/// Read a null-terminated string from a byte slice at the given offset.
#[cfg(target_os = "linux")]
fn read_null_terminated_string(content: &[u8], offset: usize) -> Option<String> {
    if offset >= content.len() {
        return None;
    }
    let end = content[offset..].iter().position(|&b| b == 0)?;
    Some(String::from_utf8_lossy(&content[offset..offset + end]).into_owned())
}
