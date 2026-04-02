#[cfg(unix)]
use std::collections::HashMap;
use std::fs;
use std::io;
use std::io::{BufRead, BufReader, Read, Write, Seek, SeekFrom};
use std::path::Path;
use std::path::PathBuf;
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre;
use color_eyre::eyre::eyre;
use base64::Engine;
use sha1::Sha1;
use sha2::digest::Digest;
use sha2::Sha256;
use tar::Archive;
use flate2::read::GzDecoder;
use liblzma;
use zstd;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt; // For checking execute permissions
#[cfg(unix)]
use nix::unistd;
#[cfg(unix)]
use nix::sys::signal::kill;
#[cfg(unix)]
use nix::sys::signal::Signal;
#[cfg(unix)]
use users::{get_current_uid, get_effective_uid};
use crate::models;
#[cfg(unix)]
use crate::userdb;
use crate::lfs;
use crate::mtree::{self, MtreeFileInfo};
use clap::error::{ErrorKind as ClapErrorKind, ContextKind, ContextValue};

#[derive(Debug, PartialEq)]
pub enum FileType {
    Elf,
    MachO,      // macOS Mach-O binary
    Symlink,
    ShellScript,
    PerlScript,
    PythonScript,
    RubyScript,
    NodeScript,
    LuaScript,
    Others,
}


impl FileType {
    #[allow(dead_code)]
    pub fn as_str(&self) -> &'static str {
        match self {
            FileType::Elf => "ELF 64-bit LSB executable",
            FileType::MachO => "Mach-O executable",
            FileType::Symlink => "Symbolic link",
            FileType::ShellScript => "Shell script, ASCII text executable",
            FileType::PerlScript => "Perl script, ASCII text executable",
            FileType::PythonScript => "Python script, ASCII text executable",
            FileType::RubyScript => "Ruby script, ASCII text executable",
            FileType::NodeScript => "Node.js script, ASCII text executable",
            FileType::LuaScript => "Lua script, ASCII text executable",
            FileType::Others => "Other file type",
        }
    }
}

#[allow(dead_code)]
pub fn is_setuid() -> bool {
    true
}

/// True when e2e runs inside the QEMU microVM guest (`E2E_BACKEND=vm` from `tests/e2e/vm.sh`).
#[inline]
pub fn e2e_backend_is_vm() -> bool {
    matches!(std::env::var("E2E_BACKEND").as_deref(), Ok("vm"))
}


/// Get all files/dirs from a package as relative paths
/// Returns relative paths (without the fs/ directory prefix)
pub fn get_package_files(
    store_root: &Path,
    pkgline: &str,
) -> Result<Vec<String>> {
    let store_fs_dir = store_root.join(pkgline).join("fs");
    if !lfs::exists_on_host(&store_fs_dir) {
        return Ok(Vec::new());
    }

    let file_infos = list_package_files_with_info(store_fs_dir.to_str()
        .ok_or_else(|| eyre::eyre!("Invalid store fs path"))?)?;

    // Return both files and dirs
    // Archlinux need matched dirs for case kmod/trunk/depmod.hook:Target = usr/lib/modules/*/
    let files: Vec<String> = file_infos
        .into_iter()
        .map(|info| info.path)
        .collect();

    Ok(files)
}

// New function that reads from filelist.txt and provides type information
// Accepts either the store path (containing fs/ and info/) or the fs/ directory path
pub fn list_package_files_with_info(package_fs_dir: &str) -> Result<Vec<MtreeFileInfo>> {
    let package_fs_path = Path::new(package_fs_dir);

    // Determine the store_dir: if input is fs/ dir, get parent; otherwise use input directly
    let store_dir: &Path = if package_fs_path.file_name()
        .and_then(|n| n.to_str()) == Some("fs") {
        // Input is the fs/ directory, parent is store_dir
        package_fs_path.parent()
            .ok_or_else(|| eyre::eyre!("Cannot get parent directory of {}", package_fs_dir))?
    } else if package_fs_path.join("fs").exists() {
        // Input is the store directory, has fs/ subdir
        package_fs_path
    } else if crate::dirs::path_join(package_fs_path, &["info", "filelist.txt"]).exists() {
        // Input is the store directory, fs/ doesn't exist but filelist.txt does
        // This happens for consumed stores after LinkType::Move
        package_fs_path
    } else {
        return Err(eyre::eyre!("Cannot determine store directory structure for {}", package_fs_dir));
    };

    let filelist_path = crate::dirs::path_join(store_dir, &["info", "filelist.txt"]);

    // If filelist.txt doesn't exist, return an error
    if !lfs::exists_on_host(&filelist_path) {
        return Err(eyre::eyre!("filelist.txt not found at {}", filelist_path.display()));
    }

    // Read and parse filelist.txt
    let content = fs::read_to_string(&filelist_path)
        .wrap_err_with(|| format!("Failed to read filelist.txt from {}", filelist_path.display()))?;

    mtree::parse_simplified_mtree(&content)
}

/// Normalize a file path from package filelist: remove leading '.' and trailing '/'
#[cfg(target_os = "linux")]
pub fn normalize_file_path(path: &str) -> &str {
    let normalized = if path.starts_with('.') {
        &path[1..]
    } else {
        path
    };
    normalized.trim_end_matches('/')
}

/// Get normalized file paths for a package store directory (package root).
/// Reads info/filelist.txt via the "fs" subdir convention.
#[cfg(target_os = "linux")]
pub fn list_package_file_paths_normalized(store_root: &Path) -> Result<Vec<String>> {
    let fs_dir = store_root.join("fs");
    let fs_dir_str = fs_dir
        .to_str()
        .ok_or_else(|| eyre::eyre!("Package store path is not valid UTF-8"))?;
    let file_infos = list_package_files_with_info(fs_dir_str)?;
    Ok(file_infos
        .into_iter()
        .map(|info| normalize_file_path(&info.path).to_string())
        .collect())
}

/// Truncate a string for display, appending "..." if longer than max_len.
#[cfg(target_os = "linux")]
pub fn truncate_display(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}


// Get file type
pub fn get_file_type(file: &Path) -> Result<(FileType, String)> {
    const ELF_MAGIC:       &[u8] = &[0x7f, b'E', b'L', b'F'];
    // Mach-O magic numbers (little-endian byte order)
    const MACHO_MAGIC_64:  &[u8] = &[0xcf, 0xfa, 0xed, 0xfe]; // MH_MAGIC_64 (0xFEEDFACF)
    const MACHO_MAGIC_32:  &[u8] = &[0xfe, 0xed, 0xfa, 0xce]; // MH_MAGIC (0xFEEDFACE)
    const MACHO_CIGAM_64:  &[u8] = &[0xfe, 0xed, 0xcf, 0xfa]; // MH_CIGAM_64 (big-endian 64-bit)
    const MACHO_CIGAM_32:  &[u8] = &[0xce, 0xfa, 0xed, 0xfe]; // MH_CIGAM (big-endian 32-bit)

    // For symlinks, follow the link and check the target's type
    let file_to_check = if lfs::is_symlink(&file) {
        match fs::canonicalize(file) {
            Ok(target) => target,
            Err(_) => return Ok((FileType::Symlink, String::new())),
        }
    } else {
        file.to_path_buf()
    };

    // Read file contents for other checks
    let mut file = fs::File::open(&file_to_check)?;
    // Check ELF 64-bit LSB
    let mut buffer = vec![0;4];
    if let Ok(_) = file.read_exact(&mut buffer) {
        if buffer.starts_with(ELF_MAGIC) {
            return Ok((FileType::Elf, String::new()));
        }
        // Check Mach-O binaries (macOS)
        if buffer.starts_with(MACHO_MAGIC_64) ||
           buffer.starts_with(MACHO_MAGIC_32) ||
           buffer.starts_with(MACHO_CIGAM_64) ||
           buffer.starts_with(MACHO_CIGAM_32) {
            return Ok((FileType::MachO, String::new()));
        }
    }

    // Use BufReader for reading lines
    // Reset file pointer to the beginning
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();
    let _bytes_read = match reader.read_line(&mut first_line) {
        Ok(n) => n,
        Err(_e) => {
            0
        }
    };
    if _bytes_read == 0 {
        return Ok((FileType::Others, String::new()));
    }

    // Check if file starts with shebang
    if first_line.starts_with("#!") {
        let script_line0 = first_line.trim_end().to_string();
        // Check for various script types
        if script_line0.contains("sh")              { return Ok((FileType::ShellScript,  script_line0));
        } else if script_line0.contains("perl")     { return Ok((FileType::PerlScript,   script_line0));
        } else if script_line0.contains("python")   { return Ok((FileType::PythonScript, script_line0));
        } else if script_line0.contains("ruby")     { return Ok((FileType::RubyScript,   script_line0));
        } else if script_line0.contains("node")     { return Ok((FileType::NodeScript,   script_line0));
        } else if script_line0.contains("lua")      { return Ok((FileType::LuaScript,    script_line0));
        }
    }

    Ok((FileType::Others, String::new()))
}

/// Compute hash of file with any Digest implementation (shared implementation).
fn compute_file_hash<D>(file_path: &str) -> Result<String>
where
    D: Digest,
{
    let file = fs::File::open(file_path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = D::new();
    let mut buffer = [0; 4096];

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(hex::encode(hasher.finalize().as_ref()))
}

pub fn compute_file_sha256(file_path: &str) -> Result<String> {
    compute_file_hash::<Sha256>(file_path)
}

pub fn compute_file_sha1(file_path: &str) -> Result<String> {
    compute_file_hash::<Sha1>(file_path)
}

/// Normalize SHA1 to hex for comparison. Accepts hex (40 chars) or
/// base64. Alpine APK: use first 28 chars (multiple of 4), decode to 21 bytes, take first 20.
pub fn normalize_sha1(base64_or_hex: &str) -> Result<String> {
    let s = base64_or_hex.trim().trim_end_matches('=');
    if s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(s.to_lowercase());
    }
    // Truncate to 28 chars (multiple of 4) so decoder accepts; 28 chars -> 21 bytes, we use first 20.
    const SHA1_B64_LEN: usize = 28;
    let b64 = if s.len() >= SHA1_B64_LEN {
        &s[..SHA1_B64_LEN]
    } else {
        s
    };
    let decoded = base64::engine::general_purpose::STANDARD.decode(b64.as_bytes())?;
    if decoded.len() >= 20 {
        Ok(hex::encode(&decoded[..20]))
    } else {
        Err(eyre::eyre!("SHA1 expected value: base64 decoded length {} (expected >= 20) for {}", decoded.len(), base64_or_hex))
    }
}

/// rust version of `sha256sum -c $checksum_file`
///
/// Verify a file's SHA-256 checksum against its .sha256 file
///
/// Takes a path to a .sha256 checksum file and verifies that the corresponding file
/// (same path without .sha256 extension) matches the expected checksum.
///
/// The .sha256 file should contain the checksum as the first word on a single line.
/// This matches the format produced by sha256sum(1).
///
/// # Arguments
/// * `checksum_file` - Path to the .sha256 file containing the expected checksum
///
/// # Returns
/// * `Ok(())` if the checksum matches
/// * `Err` if the checksum doesn't match or there are any I/O errors
pub fn verify_sha256sum(checksum_file: &Path) -> Result<()> {
    let file_path = checksum_file.with_extension("");

    if !lfs::exists_on_host(checksum_file) {
        return Err(eyre::eyre!("Checksum file not found: {}", checksum_file.display()));
    }

    if !lfs::exists_on_host(&file_path) {
        return Err(eyre::eyre!("File not found: {}", file_path.display()));
    }

    // Read expected checksum from file
    let binding = fs::read_to_string(checksum_file)?;
    let expected_checksum = binding
        .trim()
        .split_whitespace()
        .next()
        .ok_or_else(|| eyre::eyre!("Invalid checksum file format"))?;

    // Compute actual checksum
    // Avoid failing on non-UTF-8 paths (common on Windows); checksum computation
    // only needs a lossily-rendered OS path.
    let file_path_str = file_path.to_string_lossy().to_string();
    let actual_checksum = compute_file_sha256(&file_path_str)?;

    // Compare checksums
    if actual_checksum != expected_checksum {
        return Err(eyre::eyre!(
            "Checksum verification failed for {}: expected {}, got {}",
            file_path.display(),
            expected_checksum,
            actual_checksum
        ));
    }

    Ok(())
}

/// Extract a tar.gz file to a destination directory
///
/// # Arguments
/// * `tar_path` - Path to the .tar.gz file
/// * `dest_dir` - Directory to extract to
///
/// # Returns
/// * `Ok(())` if extraction succeeds
/// * `Err` if there are any I/O errors or the archive is invalid
pub fn extract_tar_gz(tar_path: &Path, dest_dir: &Path) -> Result<()> {
    // Verify tar file exists and is readable
    if !lfs::exists_on_host(tar_path) {
        return Err(eyre::eyre!("Tar file not found: {}", tar_path.display()));
    }

    // Check if file is empty
    let metadata = lfs::metadata_on_host(tar_path)?;
    if metadata.len() == 0 {
        return Err(eyre::eyre!("Tar file is empty: {}", tar_path.display()));
    }

    // Open and extract tar.gz file
    let tar_gz = fs::File::open(tar_path)
        .context("Failed to open tar file")?;
    let tar = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar);

    #[cfg(windows)]
    {
        return crate::tar_extract::unpack_tar_archive(&mut archive, dest_dir, None);
    }

    // Extract all entries (Unix: per-entry sanitize; Windows uses unpack_tar_archive above)
    #[cfg(not(windows))]
    {
        for entry in archive.entries()? {
            let mut entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    return Err(eyre::eyre!("Error reading tar entry: {}", e));
                }
            };

            let path = match entry.path() {
                Ok(path) => path,
                Err(e) => {
                    return Err(eyre::eyre!("Error getting entry path: {}", e));
                }
            };

            // Skip pax_global_header file
            if path.file_name().map_or(false, |name| name == "pax_global_header") {
                continue;
            }

            // Sanitize path for Windows (handles `::` in Perl man pages, etc.)
            let sanitized_path = lfs::sanitize_path_for_windows(&path);
            if path != sanitized_path {
                log::debug!("Sanitized tar entry path: '{}' -> '{}'",
                           path.display(), sanitized_path.display());
            }

            let full_path = dest_dir.join(&sanitized_path);

            // Create parent directories if needed
            if path.is_dir() {
                lfs::create_dir_all(&full_path)?;
                continue;
            }

            // only files
            match entry.unpack(&full_path) {
                Ok(_) => {
                    // Verify file was created - use symlink_metadata to handle symlinks
                    // (exists() returns false for broken symlinks whose targets don't exist yet)
                    if let Err(e) = fs::symlink_metadata(&full_path) {
                        return Err(eyre::eyre!("Cannot access extracted file {}: {}", full_path.display(), e));
                    }
                },
                Err(e) => {
                    if e.kind() == io::ErrorKind::AlreadyExists {
                        lfs::remove_file(&full_path)?;
                        entry.unpack(&full_path)
                            .with_context(|| format!("Error extracting {} after removal", full_path.display()))?;
                    } else {
                        return Err(eyre::eyre!("Error extracting {}: {}", full_path.display(), e))
                    }
                }
            }
        }
    }

    #[cfg(not(windows))]
    Ok(())
}

/// Returns `true` if the process is running with effective root privileges.
/// This checks the effective user ID (euid), which determines the process's
/// current permissions. For the real user ID, see `get_current_uid()`.
#[cfg(unix)]
pub fn is_running_as_root() -> bool {
    unistd::geteuid().is_root()
}

#[cfg(not(unix))]
pub fn is_running_as_root() -> bool {
    false
}

/// Returns true if /opt/epkg should be mounted read-only in VM sandbox.
/// When we're not root on host, /opt/epkg/cache is not writable (host permissions apply),
/// so mounting it read-only prevents EPERM errors and forces fallback to user cache.
#[cfg(target_os = "linux")]
pub(crate) fn should_mount_opt_epkg_readonly() -> bool {
    !is_running_as_root()
}

/// Determine shared_store mode based on the decision sequence:
/// 1. private if !is_running_as_root
/// 2. public  if running as root and /opt/epkg/envs/ exists AND /opt/epkg/cache is writable
/// 3. private if $HOME/.epkg/envs/ exists
/// 4. public  if /opt/epkg/envs/ exists AND /opt/epkg/cache is writable
/// 5. otherwise: neither envs exists, default to private (false)
pub fn determine_shared_store() -> Result<bool> {
    use std::path::Path;
    use crate::dirs::get_home;

    let is_root = is_running_as_root();
    let opt_envs = Path::new("/opt/epkg/envs");
    let opt_cache = Path::new("/opt/epkg/cache");
    let has_opt_envs = lfs::exists_on_host(opt_envs);

    // Helper: check if /opt/epkg/cache is actually writable by attempting to create a temp file.
    // This is more reliable than checking permissions, especially for virtiofs which may
    // report writable permissions but actually reject writes.
    let is_opt_cache_writable = || -> bool {
        // First ensure the directory exists
        if !lfs::exists_on_host(opt_cache) {
            // Try to create it, or check if parent is writable
            if let Some(parent) = opt_cache.parent() {
                if !lfs::exists_on_host(parent) {
                    return false;
                }
                // Try to create the cache directory
                if std::fs::create_dir_all(opt_cache).is_err() {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Try to create a temporary file to verify actual write access
        let test_file = opt_cache.join(".write_test_tmp");
        match std::fs::File::create(&test_file) {
            Ok(_) => {
                // Clean up the test file
                let _ = std::fs::remove_file(&test_file);
                log::trace!("is_opt_cache_writable: write test succeeded");
                true
            }
            Err(e) => {
                log::trace!("is_opt_cache_writable: write test failed: {}", e);
                false
            }
        }
    };

    // Rule 1: If no root permission, set to private
    if !is_root {
        log::trace!("determine_shared_store: rule 1 -> private (not root)");
        return Ok(false);
    }

    // Rule 2: If running as root and /opt/epkg/envs/ exists, check cache writability
    if has_opt_envs {
        if is_opt_cache_writable() {
            log::trace!("determine_shared_store: rule 2 -> public (root and /opt/epkg/envs exists, cache writable)");
            return Ok(true);
        } else {
            // Cache not writable (e.g., virtiofs with restricted permissions), fall through
            log::trace!("determine_shared_store: /opt/epkg/cache not writable, falling through");
        }
    }

    // Rule 3: If $HOME/.epkg/envs/ exists, set to private
    let home = get_home()?;
    let home_envs = crate::dirs::path_join(Path::new(&home), &[".epkg", "envs"]);
    let home_envs_exists = lfs::exists_on_host(&home_envs);
    if home_envs_exists {
        log::trace!("determine_shared_store: rule 3 -> private ({} exists)", home_envs.display());
        return Ok(false);
    }

    // Rule 4: If /opt/epkg/envs/ exists AND cache is writable, set to public
    if has_opt_envs && is_opt_cache_writable() {
        log::trace!("determine_shared_store: rule 4 -> public (/opt/epkg/envs exists, cache writable)");
        return Ok(true);
    }

    // Rule 5: Otherwise: neither envs exists or cache not writable, default to private (false)
    log::trace!("determine_shared_store: rule 5 -> private (neither envs exists or cache not writable)");
    Ok(false)
}

/// Check if the process is running with setuid privileges
/// Returns true if effective UID differs from real UID
#[cfg(unix)]
pub fn is_suid() -> bool {
    get_current_uid() != get_effective_uid()
}

#[cfg(not(unix))]
#[allow(dead_code)]
pub fn is_suid() -> bool {
    false
}

/// Get username from real UID (for setuid security)
/// In statically linked binaries, getpwuid() may not work properly due to NSS limitations,
/// so we parse /etc/passwd directly.
#[cfg(unix)]
pub fn get_username_from_uid() -> Result<String> {
    let uid = get_current_uid();
    userdb::get_username_by_uid(uid, None)
}

#[cfg(not(unix))]
#[allow(dead_code)]
pub fn get_username_from_uid() -> Result<String> {
    Err(color_eyre::eyre::eyre!("get_username_from_uid() not supported on this platform"))
}

/// Get home directory from real UID (for setuid security)
/// In statically linked binaries, getpwuid() may not work properly due to NSS limitations,
/// so we parse /etc/passwd directly.
#[cfg(unix)]
pub fn get_home_from_uid() -> Result<String> {
    let uid = get_current_uid();
    userdb::get_home_by_uid(uid, None)
}

#[cfg(not(unix))]
#[allow(dead_code)]
pub fn get_home_from_uid() -> Result<String> {
    Err(color_eyre::eyre::eyre!("get_home_from_uid() not supported on this platform"))
}

/// Check if a command exists in PATH
/// Only used on Linux (idmap, rpm_verify)
#[cfg(target_os = "linux")]
pub fn command_exists(command_name: &str) -> bool {
    find_command_in_paths(command_name).is_some()
}

/// Searches for an executable command in a predefined list of common paths.
/// Returns the full path to the command if found and executable, otherwise None.
#[cfg(unix)]
pub fn find_command_in_paths(command_name: &str) -> Option<PathBuf> {
    // If command contains a slash, treat it as a direct path
    if command_name.contains('/') {
        let path = Path::new(command_name);
        if lfs::exists_on_host(path) {
            if let Ok(metadata) = lfs::metadata_on_host(path) {
                if metadata.is_file() && (metadata.permissions().mode() & 0o111 != 0) {
                    return Some(path.to_path_buf());
                }
            }
        }
        return None;
    }

    let common_paths = [
        "/usr/local/sbin",
        "/usr/local/bin",
        "/usr/sbin",
        "/usr/bin",
        "/usr/libexec",
        "/sbin",
        "/bin",
        // Add other paths if necessary, e.g., from $HOME/.local/bin
    ];

    for path_dir in common_paths.iter() {
        let mut full_path = PathBuf::from(path_dir);
        full_path.push(command_name);
        if lfs::exists_on_host(&full_path) {
            if let Ok(metadata) = lfs::metadata_on_host(&full_path) {
                if metadata.is_file() && (metadata.permissions().mode() & 0o111 != 0) {
                    return Some(full_path);
                }
            }
        }
    }
    None
}

/// Decompress a file based on its extension
///
/// # Arguments
/// * `input_path` - Path to the compressed input file
/// * `output_path` - Path where the decompressed file should be written
/// * `extension` - The file extension (e.g. "gz", "xz", "zst")
///
/// # Returns
/// * `Result<()>` - Ok if decompression was successful, Err otherwise
#[allow(dead_code)]
pub fn decompress_file(input_path: &Path, output_path: &Path, extension: &str) -> Result<()> {
    if let Some(parent) = output_path.parent() {
        lfs::create_dir_all(parent)?;
    }

    let input_file = fs::File::open(input_path)
        .with_context(|| format!("Failed to open input file: {}", input_path.display()))?;
    let mut output = lfs::file_create(output_path)?;

    match extension {
        "gz" => {
            let mut decoder = GzDecoder::new(input_file);
            std::io::copy(&mut decoder, &mut output)
                .with_context(|| format!("Failed to decompress gz file from {} to {}", input_path.display(), output_path.display()))?;
        }
        "xz" => {
            let mut decoder = liblzma::read::XzDecoder::new(input_file);
            std::io::copy(&mut decoder, &mut output)
                .with_context(|| format!("Failed to decompress xz file from {} to {}", input_path.display(), output_path.display()))?;
        }
        "zst" => {
            let mut decoder = zstd::stream::read::Decoder::new(input_file)?;
            std::io::copy(&mut decoder, &mut output)
                .with_context(|| format!("Failed to decompress zst file from {} to {}", input_path.display(), output_path.display()))?;
        }
        _ => return Err(eyre::eyre!("Unsupported compression format for file: {}", input_path.display())),
    }

    Ok(())
}

/// Rename a file by appending .bad to its name
///
/// This will:
/// 1. Remove any existing .bad file with the same name
/// 2. Rename the file to have a .bad extension
///
/// # Arguments
/// * `file_path` - Path to the file to be marked as bad
///
/// # Returns
/// * `Ok(PathBuf)` - The new path of the file
/// * `Err` - If renaming fails
pub fn mark_file_bad<P: AsRef<Path>>(file_path: P) -> Result<PathBuf> {
    let file_path = file_path.as_ref();
    let bad_path = append_suffix(file_path, "bad");
    let part_path = append_suffix(file_path, "part");

    // Remove existing .part file
    if lfs::exists_on_host(&part_path) {
        lfs::remove_file(&part_path)?;
    }

    // If the original file no longer exists, treat this as a no-op.
    // This can happen if another component (or an earlier attempt) has
    // already renamed the file to .bad or otherwise removed it.
    if !lfs::exists_on_host(file_path) {
        log::warn!(
            "mark_file_bad: file already missing, skipping rename: {}",
            file_path.display()
        );
        eprintln!(
            "Corrupted file already handled or missing: {}",
            file_path.display()
        );
        eprintln!("Please retry, the file should be auto redownloaded.");
        return Ok(bad_path);
    }

    // Remove existing .bad file
    if lfs::exists_on_host(&bad_path) {
        lfs::remove_file(&bad_path)?;
    }

    // Rename the file
    lfs::rename(file_path, &bad_path)?;

    eprintln!("Renamed corrupted file {} to {}", file_path.display(), bad_path.display());
    eprintln!("Please retry, the file should be auto redownloaded.");

    Ok(bad_path)
}

/// General helper function to append a suffix to a path instead of using with_extension
pub fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}.{}", path.display(), suffix))
}


pub fn user_prompt_and_confirm() -> Result<bool> {
    if models::config().common.dry_run {
        println!("\nDry run: No changes will be made to the system.");
        return Ok(false);
    }

    if models::config().common.assume_no {
        return Ok(false);
    }

    if models::config().common.assume_yes {
        return Ok(true);
    }

    // Auto-assume-yes when stdin is not a TTY (e.g., running from script)
    let stdin_is_tty = unsafe { libc::isatty(libc::STDIN_FILENO) != 0 };
    if !stdin_is_tty {
        return Ok(true);
    }

    print!("\nDo you want to continue? [Y/n] ");
    io::stdout().flush()?;
    let mut user_input = String::new();
    io::stdin().read_line(&mut user_input)?;
    let trimmed = user_input.trim().to_lowercase();
    if trimmed != "y" && trimmed != "yes" && trimmed != "" {
        println!("{:?} cancelled by user.", models::config().subcommand);
        return Ok(false);
    }
    Ok(true)
}

/// Remove any existing link at `symlink_path`, then create a **directory** symlink for virtiofs.
/// See [`lfs::symlink_dir_for_virtiofs`].
pub fn force_symlink_dir_for_virtiofs<P: AsRef<Path>, Q: AsRef<Path>>(file_path: P, symlink_path: Q) -> Result<()> {
    let file_path = file_path.as_ref();
    let symlink_path = symlink_path.as_ref();

    if lfs::symlink_metadata(symlink_path).is_ok() {
        // Check if it's a symlink (can be removed with remove_file) or a real directory
        if lfs::is_symlink(symlink_path) {
            lfs::remove_file(symlink_path)?;
        } else if symlink_path.is_dir() {
            // It's a real directory, not a symlink. Remove it if empty.
            // This handles the case where environment is being recreated with different config.
            lfs::remove_dir(symlink_path)?;
        } else {
            // It's a regular file
            lfs::remove_file(symlink_path)?;
        }
    }

    log::debug!(
        "Creating directory symlink for virtiofs: {} -> {}",
        symlink_path.display(),
        file_path.display()
    );
    lfs::symlink_dir_for_virtiofs(file_path, symlink_path)?;

    Ok(())
}

/// Remove any existing link at `symlink_path`, then create a **file** symlink for virtiofs.
/// See [`lfs::symlink_file_for_virtiofs`].
pub fn force_symlink_file_for_virtiofs<P: AsRef<Path>, Q: AsRef<Path>>(file_path: P, symlink_path: Q) -> Result<()> {
    let file_path = file_path.as_ref();
    let symlink_path = symlink_path.as_ref();

    if lfs::symlink_metadata(symlink_path).is_ok() {
        lfs::remove_file(symlink_path)?;
    }

    log::debug!(
        "Creating file symlink for virtiofs: {} -> {}",
        symlink_path.display(),
        file_path.display()
    );
    lfs::symlink_file_for_virtiofs(file_path, symlink_path)?;

    Ok(())
}

/// Remove any existing link at `symlink_path`, then create a **directory** symlink for native Windows.
/// See [`lfs::symlink_dir_for_native`].
#[cfg(windows)]
pub fn force_symlink_dir_for_native<P: AsRef<Path>, Q: AsRef<Path>>(file_path: P, symlink_path: Q) -> Result<()> {
    let file_path = file_path.as_ref();
    let symlink_path = symlink_path.as_ref();

    if lfs::symlink_metadata(symlink_path).is_ok() {
        // Check if it's a symlink (can be removed with remove_file) or a real directory
        if lfs::is_symlink(symlink_path) {
            lfs::remove_file(symlink_path)?;
        } else if symlink_path.is_dir() {
            lfs::remove_dir(symlink_path)?;
        } else {
            lfs::remove_file(symlink_path)?;
        }
    }

    log::debug!(
        "Creating directory symlink for native: {} -> {}",
        symlink_path.display(),
        file_path.display()
    );
    lfs::symlink_dir_for_native(file_path, symlink_path)?;

    Ok(())
}

/// Remove any existing link at `symlink_path`, then create a **file** symlink for native Windows.
/// See [`lfs::symlink_file_for_native`].
#[cfg(windows)]
pub fn force_symlink_file_for_native<P: AsRef<Path>, Q: AsRef<Path>>(file_path: P, symlink_path: Q) -> Result<()> {
    let file_path = file_path.as_ref();
    let symlink_path = symlink_path.as_ref();

    if lfs::symlink_metadata(symlink_path).is_ok() {
        lfs::remove_file(symlink_path)?;
    }

    log::debug!(
        "Creating file symlink for native: {} -> {}",
        symlink_path.display(),
        file_path.display()
    );
    lfs::symlink_file_for_native(file_path, symlink_path)?;

    Ok(())
}

// Unix versions that delegate to virtiofs variants (on Unix, both are the same)
#[cfg(unix)]
pub use force_symlink_dir_for_virtiofs as force_symlink_dir_for_native;
#[cfg(unix)]
pub use force_symlink_file_for_virtiofs as force_symlink_file_for_native;

/// Convert a path (relative or absolute) to an absolute path string.
/// If the path is already absolute, returns it unchanged.
/// If the path is relative, joins it with the current working directory.
/// Returns the original string if unable to get current working directory.
pub fn to_absolute_path(dir: &str) -> String {
    if dir.is_empty() {
        return dir.to_string();
    }

    let path = std::path::Path::new(dir);
    if path.is_absolute() {
        dir.to_string()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path).to_string_lossy().to_string(),
            Err(_) => dir.to_string(), // fallback
        }
    }
}

/// Safely create a directory, handling cases where a file with the same name already exists
///
/// This function will:
/// 1. Check if the path already exists
/// 2. If it's a file, remove it first
/// 3. If it's a directory, return success
/// 4. If it's something else (symlink, etc.), remove it first
/// 5. Create the directory with all parent directories
#[cfg(target_os = "linux")]
pub fn safe_mkdir_p(path: &Path) -> Result<()> {
    // Check if path already exists and handle it appropriately
    if lfs::exists_or_any_symlink(path) {
        let metadata = lfs::symlink_metadata(path)
            .map_err(|e| eyre::eyre!("Failed to get metadata for existing path '{}': {}", path.display(), e))?;

        if metadata.file_type().is_file() {
            log::debug!("Path exists as a file, removing it: {}", path.display());
            remove_any_existing_file(path, false)?;
        } else if metadata.file_type().is_dir() {
            log::trace!("Path exists as a directory: {}", path.display());
            // Directory already exists, we can proceed
            return Ok(());
        } else {
            log::debug!("Path exists as something else (symlink, etc.), removing it: {}", path.display());
            remove_any_existing_file(path, false)?;
        }
    }

    // Create the directory with all parent directories
    lfs::create_dir_all(path)
}

/// Remove any existing file, symlink, or directory at the given path
///
/// This function will attempt to remove the file regardless of its type
/// (regular file, symlink, etc.) and optionally directories.
/// Handles dead symlinks by using symlink_metadata instead of path.exists().
///
/// # Arguments
/// * `path` - Path to the file/directory to remove
/// * `rm_dir` - If true, remove directories; if false, return error for directories
///
/// # Returns
/// * `Result<()>` - Ok if removal was successful, Err otherwise
pub fn remove_any_existing_file(path: &Path, rm_dir: bool) -> Result<()> {
    // Use symlink_metadata to handle dead symlinks properly
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.is_dir() {
                if rm_dir {
                    lfs::remove_dir_all(path)?;
                } else {
                    return Err(eyre::eyre!("Cannot remove directory '{}' with remove_any_existing_file()", path.display()));
                }
            } else {
                lfs::remove_file(path)?;
            }
        }
        Err(e) => {
            // If symlink_metadata fails, the path doesn't exist (not even as a dead symlink)
            if e.kind() == std::io::ErrorKind::NotFound {
                // Path doesn't exist, nothing to remove
                return Ok(());
            } else {
                return Err(eyre::eyre!("Failed to get metadata for path '{}': {}", path.display(), e));
            }
        }
    }

    Ok(())
}

/// Format bytes into human-readable size string
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    match bytes {
        0..KB => format!("{} B", bytes),
        KB..MB => format!("{:.1} KB", bytes as f64 / KB as f64),
        MB..GB => format!("{:.1} MB", bytes as f64 / MB as f64),
        _ => format!("{:.1} GB", bytes as f64 / GB as f64),
    }
}

/// Preserve file permissions from source to target
pub fn preserve_file_permissions<P: AsRef<Path>>(source: P, target: P) -> Result<()> {
    let source = source.as_ref();
    let target = target.as_ref();
    if let Ok(metadata) = lfs::symlink_metadata(source) {
        lfs::set_permissions(target, metadata.permissions())?;
    }
    Ok(())
}

/// Fix up file permissions using mode from tar header, avoiding extra disk I/O.
/// This is the preferred function when mode and entry type are already available.
#[cfg(unix)]
pub fn fixup_file_permissions_with_mode(target_path: &Path, mode: u32, is_dir: bool) {
    let required_mask = if is_dir { 0o700 } else { 0o600 };

    if mode & required_mask != required_mask {
        let new_mode = mode | required_mask;
        let perms = std::fs::Permissions::from_mode(new_mode);
        if let Err(e) = lfs::set_permissions(target_path, perms) {
            let file_type = if is_dir { "directory" } else { "file" };
            log::warn!("Failed to set {} permissions for {}: {}", file_type, target_path.display(), e);
        }
    }
}

/// Fix up file permissions by reading metadata from disk.
/// Prefer fixup_file_permissions_with_mode() when mode is already available.
///
/// This is only used during cleanup (deinit) to make directories/files
/// removable by ensuring owner write permission. NTFS EA is not set here
/// since cleanup is about deletion, not preserving permissions for VM use.
#[cfg(unix)]
pub fn fixup_file_permissions(target_path: &Path) {
    if let Ok(metadata) = lfs::symlink_metadata(target_path) {
        if metadata.file_type().is_dir() {
            // Ensure directories are writable by owner so they can be removed later
            // This prevents issues with read-only directories like /usr/lib (dr-xr-xr-x)
            ensure_owner_permissions(target_path, 0o700, "directory");
        } else {
            // Ensure files are readable by owner for hash calculation and other operations
            ensure_owner_permissions(target_path, 0o600, "file");
        }
    }
}

/// Ensure the file/directory has the specified owner permissions
#[cfg(unix)]
fn ensure_owner_permissions(target_path: &Path, required_mask: u32, file_type: &str) {
    if let Ok(metadata) = lfs::symlink_metadata(target_path) {
        let mut perms = metadata.permissions();
        let current_mode = perms.mode();

        if current_mode & required_mask != required_mask {
            let new_mode = current_mode | required_mask;
            perms.set_mode(new_mode);
            if let Err(e) = lfs::set_permissions(target_path, perms) {
                log::warn!("Failed to set {} permissions for {}: {}", file_type, target_path.display(), e);
            }
        }
    }
}

#[cfg(not(unix))]
pub fn fixup_file_permissions_with_mode(target_path: &Path, mode: u32, is_dir: bool) {
    // On Windows, store non-trivial permissions in NTFS EA for POSIX compatibility
    // This is used when running Linux packages in a VM environment
    //
    // IMPORTANT: The mode from tar header only contains permission bits (0o755, 0o644, etc.)
    // We MUST add the file type bits (S_IFREG/S_IFDIR) so virtiofs can correctly identify
    // files vs directories. Without this, all files appear as directories in the VM!
    #[cfg(windows)]
    {
        // Linux file type bits (from libc)
        const S_IFREG: u32 = 0o100000;  // Regular file
        const S_IFDIR: u32 = 0o040000;  // Directory

        // Add the file type bit to the mode
        let full_mode = if is_dir {
            S_IFDIR | (mode & 0o7777)  // Directory with permissions
        } else {
            S_IFREG | (mode & 0o7777)   // Regular file with permissions
        };

        if let Err(e) = crate::ntfs_ea::set_posix_mode(target_path, full_mode, is_dir) {
            log::warn!(
                "Skipping POSIX mode for {}: {}",
                target_path.display(),
                e
            );
        }
    }

    // On non-Windows, non-Unix platforms (e.g., macOS), this is a no-op
    #[cfg(not(windows))]
    {
        let _ = (target_path, mode, is_dir);
    }
}

#[cfg(not(unix))]
#[allow(dead_code)]
pub fn fixup_file_permissions(_target_path: &Path) {
    // No-op on non-Unix systems.
    // This is only called during cleanup (deinit) to make files removable.
    // Setting NTFS EA here is meaningless - cleanup is about deletion, not preserving permissions.
}

#[cfg(not(unix))]
#[allow(dead_code)]
fn ensure_owner_permissions(_target_path: &Path, _required_mask: u32, _file_type: &str) {
    // No-op on non-Unix systems
}


/// Fix up environment links and remove system directories
pub fn fixup_env_links(env_root: &Path) -> Result<()> {
    log::trace!("fixup_env_links called for {}", env_root.display());
    // Prevent running and stalling on `systemctl --system daemon-reload`
    let _ = lfs::remove_dir(crate::dirs::path_join(env_root, &["run", "systemd", "system"]));

    // Replace symlinks with their target file content
    replace_symlinks_with_content(env_root)?;

    // Create common symlinks for shells and utilities
    create_common_symlinks(env_root)?;

    // Create quiet makepkg DLAGENTS config
    create_makepkg_download_conf(env_root)?;

    // Remove files based on glob patterns
    remove_files_by_patterns(env_root)?;

    Ok(())
}

/// Replace symlinks with their target file content
fn replace_symlinks_with_content(env_root: &Path) -> Result<()> {
    let symlink_replace_list = [
        // Fixes:
        //      /usr/share/debconf/confmodule: line 28: /usr/lib/cdebconf/debconf: No such file or directory
        // Root cause: that script relies on this being normal file
        //      elif [ -x /usr/share/debconf/frontend ] && \
        //           [ ! -h /usr/share/debconf/frontend ]; then
        //              _DEBCONF_IMPL=debconf
        "/usr/share/debconf/frontend",

        // Fixes script search path
        "/usr/bin/python3",
        "/usr/bin/python",
    ];

    for symlink_path in &symlink_replace_list {
        let relative_path = symlink_path.strip_prefix("/")
            .unwrap_or(symlink_path);  // Fallback to original if no prefix
        let full_symlink_path = lfs::normalize_path_separators(&env_root.join(relative_path));

        if lfs::exists_in_env(&full_symlink_path) && lfs::is_symlink(&full_symlink_path) {
            // Resolve the symlink to get the actual target file path
            let target_path = match std::fs::canonicalize(&full_symlink_path) {
                Ok(path) => path,
                Err(e) => {
                    // Symlink is broken (target doesn't exist) - skip it
                    log::debug!("Skipping broken symlink {}: {}", full_symlink_path.display(), e);
                    continue;
                }
            };

            // Remove the symlink
            lfs::remove_file(&full_symlink_path)?;

            // Try to hardlink the target file to the symlink location, fall back to copy
            if let Err(hardlink_err) = lfs::hard_link(&target_path, &full_symlink_path) {
                log::debug!("Hardlink not work for {} -> {}: {}, falling back to copy",
                           target_path.display(), full_symlink_path.display(), hardlink_err);

                // If hardlink fails, copy the file
                log::debug!("Copying file from {} to {}", target_path.display(), full_symlink_path.display());
                lfs::copy(&target_path, &full_symlink_path)?;
            } else {
                log::debug!("Successfully created hardlink from {} to {}",
                           target_path.display(), full_symlink_path.display());
            }
        }
    }
    Ok(())
}

/// Create common symlinks for shell and utilities if they don't exist
fn create_common_symlinks(env_root: &Path) -> Result<()> {
    log::trace!("Creating common symlinks in {}", env_root.display());

    // For MSYS2 on Windows: create lib -> usr/lib symlink for Python to find its stdlib
    // Python with PYTHONHOME looks for $PYTHONHOME/lib/python3.x, but MSYS2 puts it in usr/lib
    #[cfg(windows)]
    {
        let ch = crate::models::channel_config();
        if ch.format == crate::models::PackageFormat::Pacman && ch.distro == "msys2" {
            let lib_link = env_root.join("lib");
            if !lfs::exists_or_any_symlink(&lib_link) {
                log::debug!("Creating lib -> usr/lib symlink for MSYS2 Python");
                lfs::symlink_dir_for_virtiofs("usr/lib", &lib_link)?;
            }
        }
    }

    // List of symlinks to create: [(symlink, [possible_targets])]
    let symlinks: &[(&str, &[&str])] = &[
        ("usr/bin/sh", &["bash", "dash", "yash", "busybox"]),
        ("bin/sh",     &["bash", "dash", "yash", "busybox"]),
        ("usr/bin/awk", &["mawk", "gawk"]),

        // These are optional and will fail due to no "dpkg -L" output
        ("usr/local/bin/py3compile", &["/usr/bin/true", "/bin/true"]),
        ("usr/local/bin/py3clean", &["/usr/bin/true", "/bin/true"]),

        // Pacman-style dbus reload hook expects this helper. Many minimal
        // Arch-like environments don't ship it, so we point it to a no-op
        // true(1) to avoid hard failures while still allowing the
        // transaction to complete.
        ("usr/share/libalpm/scripts/systemd-hook", &["/usr/bin/true", "/bin/true"]),
    ];

    for (link_name, possible_targets) in symlinks {
        log::trace!("Checking symlink: {}", link_name);
        let link_path = lfs::normalize_path_separators(&env_root.join(link_name));

        // Skip if symlink already exists
        if lfs::exists_or_any_symlink(&link_path) {
            log::debug!("Skipping {}: file already exists", link_path.display());
            continue;
        }

        // Try each possible target until we find one that exists
        let mut found = false;
        for target in *possible_targets {
            log::trace!("  Trying target: {}", target);
            // Check if target exists within env_root, not host rootfs
            let target_check_path = if target.starts_with('/') {
                // Absolute path: check in env_root
                lfs::normalize_path_separators(&env_root.join(&target[1..])) // Remove leading slash
            } else {
                // Relative path: relative to symlink's parent directory within env_root
                lfs::normalize_path_separators(&env_root.join(link_name).parent().unwrap().join(target))
            };
            log::trace!("  Target check path: {}", target_check_path.display());

            if lfs::exists_or_any_symlink(&target_check_path) {
                log::trace!("  Target found at {}", target_check_path.display());
                found = true;
                if let Some(parent) = link_path.parent() {
                    lfs::create_dir_all(parent)?;
                }
                // Use the original target string for the symlink (relative or absolute as specified)
                // All symlinks in this function point to executables (files), use symlink_file_for_virtiofs
                log::debug!("  Creating symlink {} -> {}", link_path.display(), target);
                lfs::symlink_file_for_virtiofs(target, &link_path)?;
                break;
            }
        }
        if !found {
            log::trace!("  No suitable target found for {}", link_name);
        }
    }
    Ok(())
}

/// Create a quiet `makepkg` DLAGENTS configuration
fn create_makepkg_download_conf(env_root: &Path) -> Result<()> {
    // Only relevant for Pacman-style channels
    if crate::models::channel_config().format != crate::models::PackageFormat::Pacman {
        return Ok(());
    }

    let conf_dir = crate::dirs::path_join(env_root, &["etc", "makepkg.conf.d"]);
    if !lfs::exists_in_env(&conf_dir) {
        return Ok(());
    }

    let conf_path = conf_dir.join("download.conf");
    let content = r#"DLAGENTS=('file::/usr/bin/curl -sS -qgC - -o %o %u'
          'ftp::/usr/bin/curl -sS -qgfC - --ftp-pasv --retry 3 --retry-delay 3 -o %o %u'
          'http::/usr/bin/curl -sS -qgb "" -fLC - --retry 3 --retry-delay 3 -o %o %u'
          'https::/usr/bin/curl -sS -qgb "" -fLC - --retry 3 --retry-delay 3 -o %o %u'
          'rsync::/usr/bin/rsync --no-motd -z %u %o'
          'scp::/usr/bin/scp -C %u %o')
"#;

    lfs::write(&conf_path, content)?;
    Ok(())
}

/// Remove files based on glob patterns
fn remove_files_by_patterns(env_root: &Path) -> Result<()> {
    let remove_patterns = [
        "/usr/lib/python3.*/EXTERNALLY-MANAGED",
    ];

    for pattern in &remove_patterns {
        // Convert relative pattern to absolute path within env_root
        let absolute_pattern = if pattern.starts_with('/') {
            lfs::normalize_path_separators(&env_root.join(&pattern[1..])) // Remove leading slash
        } else {
            lfs::normalize_path_separators(&env_root.join(pattern))
        };

        // Use glob to find matching files
        match glob::glob(absolute_pattern.to_str().unwrap()) {
            Ok(paths) => {
                for path_result in paths {
                    match path_result {
                        Ok(path) => {
                            if lfs::exists_in_env(&path) {
                                if let Err(e) = lfs::remove_file(&path) {
                                    log::warn!("Failed to remove file {}: {}", path.display(), e);
                                }
                            }
                        }
                        Err(e) => {
                            log::warn!("Failed to process glob result for pattern '{}': {}", pattern, e);
                        }
                    }
                }
            }
            Err(e) => {
                log::warn!("Failed to process glob pattern '{}': {}", pattern, e);
            }
        }
    }

    Ok(())
}

/// Set executable permissions on a file (Unix only)
/// This is a common helper used when creating executable scripts
/// Gets existing permissions first and then sets the mode, preserving other bits
#[cfg(unix)]
pub fn set_executable_permissions<P: AsRef<Path>>(path: P, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = path.as_ref();
    let mut perms = lfs::symlink_metadata(path)
        .wrap_err_with(|| format!("Failed to get metadata for {}", path.display()))?.permissions();
    perms.set_mode(mode);
    lfs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
pub fn set_executable_permissions<P: AsRef<Path>>(_path: P, _mode: u32) -> Result<()> {
    // No-op on non-Unix systems
    Ok(())
}

/// Set exact permissions on a file from a mode (Unix only)
/// This sets the exact mode without reading existing permissions first
/// Useful when you want to set a specific mode value directly
#[cfg(unix)]
pub fn set_permissions_from_mode<P: AsRef<Path>>(path: P, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = path.as_ref();
    let perms = fs::Permissions::from_mode(mode);
    lfs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
pub fn set_permissions_from_mode<P: AsRef<Path>>(_path: P, _mode: u32) -> Result<()> {
    Ok(())
}

/// Copy a scriptlet file from source to target and make it executable
/// This is a common pattern used by deb_pkg, conda_pkg, and apk_pkg
pub fn copy_scriptlet_file<P: AsRef<Path>>(source: P, target: P) -> Result<()> {
    let source = source.as_ref();
    let target = target.as_ref();

    // Copy the script content
    let content = fs::read(source)
        .wrap_err_with(|| format!("Failed to read scriptlet file: {}", source.display()))?;
    lfs::write(target, &content)?;

    // Make it executable on Unix systems
    set_executable_permissions(target, 0o755)?;

    Ok(())
}

/// Write scriptlet content to a file and make it executable
/// This is used by rpm_pkg when writing scriptlet content directly (not from files)
pub fn write_scriptlet_content<P: AsRef<Path>>(target: P, content: &[u8]) -> Result<()> {
    let target = target.as_ref();

    // Write the script content
    lfs::write(target, content)?;

    // Make it executable on Unix systems
    set_executable_permissions(target, 0o755)?;

    Ok(())
}

/// Process scriptlets from a mapping, copying or symlinking them from source directory to target directory.
/// Used by deb_pkg (symlink so debconf frontend finds templates), conda_pkg, and apk_pkg (copy).
///
/// # Arguments
/// * `mapping` - HashMap mapping package-specific scriptlet names to common scriptlet names
/// * `source_dir` - Directory containing the source scriptlet files
/// * `target_dir` - Directory where common scriptlet files should be written
/// * `use_symlink` - If true, create symlinks (target -> relative path to source); if false, copy files
pub fn copy_scriptlets_by_mapping<P: AsRef<Path>>(
    mapping: &std::collections::HashMap<&str, &str>,
    source_dir: P,
    target_dir: P,
    use_symlink: bool,
) -> Result<()> {
    let source_dir = source_dir.as_ref();
    let target_dir = target_dir.as_ref();

    for (package_script, common_script) in mapping {
        let package_script = lfs::sanitize_path_for_windows(std::path::Path::new(package_script));
        let common_script = lfs::sanitize_path_for_windows(std::path::Path::new(common_script));
        let source_path = source_dir.join(&package_script);
        if source_path.exists() {
            log::debug!("Found scriptlet: {}", package_script.display());
            let target_path = target_dir.join(&common_script);
            if use_symlink {
                let rel = pathdiff::diff_paths(&source_path, target_dir).unwrap_or_else(|| {
                    let base = source_dir.file_name().and_then(|o| o.to_str()).unwrap_or("deb");
                    PathBuf::from(format!("../{}/{}", base, package_script.display()))
                });
                let rel = lfs::sanitize_path_for_windows(&rel);
                if lfs::symlink_metadata(&target_path).is_ok() {
                    lfs::remove_file(&target_path)?;
                }
                log::debug!("Creating symlink: {} -> {}", target_path.display(), rel.display());
                lfs::symlink_file_for_virtiofs(&rel, &target_path)?;
                log::debug!(
                    "Created script symlink: {} -> {}",
                    common_script.display(),
                    package_script.display()
                );
            } else {
                copy_scriptlet_file(&source_path, &target_path)?;
                log::debug!(
                    "Created script: {} -> {}",
                    package_script.display(),
                    common_script.display()
                );
            }
        }
    }

    Ok(())
}

#[cfg(unix)]
macro_rules! signal_map {
    ($map:expr, $(($name:expr, $signal:expr)),* $(,)?) => {
        $(
            $map.insert($name, $signal);
        )*
    };
}

#[cfg(unix)]
lazy_static::lazy_static! {
    static ref SIGNAL_NAME_MAP: HashMap<&'static str, Signal> = {
        let mut map = HashMap::new();
        signal_map!(map,
            ("HUP",     Signal::SIGHUP),
            ("INT",     Signal::SIGINT),
            ("QUIT",    Signal::SIGQUIT),
            ("ILL",     Signal::SIGILL),
            ("TRAP",    Signal::SIGTRAP),
            ("ABRT",    Signal::SIGABRT),
            ("BUS",     Signal::SIGBUS),
            ("FPE",     Signal::SIGFPE),
            ("KILL",    Signal::SIGKILL),
            ("USR1",    Signal::SIGUSR1),
            ("SEGV",    Signal::SIGSEGV),
            ("USR2",    Signal::SIGUSR2),
            ("PIPE",    Signal::SIGPIPE),
            ("ALRM",    Signal::SIGALRM),
            ("TERM",    Signal::SIGTERM),
            ("CHLD",    Signal::SIGCHLD),
            ("CONT",    Signal::SIGCONT),
            ("STOP",    Signal::SIGSTOP),
            ("TSTP",    Signal::SIGTSTP),
            ("TTIN",    Signal::SIGTTIN),
            ("TTOU",    Signal::SIGTTOU),
            ("URG",     Signal::SIGURG),
            ("XCPU",    Signal::SIGXCPU),
            ("XFSZ",    Signal::SIGXFSZ),
            ("VTALRM",  Signal::SIGVTALRM),
            ("PROF",    Signal::SIGPROF),
            ("WINCH",   Signal::SIGWINCH),
            ("IO",      Signal::SIGIO),
        );
        // Linux-only signals
        #[cfg(target_os = "linux")]
        {
            map.insert("PWR", Signal::SIGPWR);
            map.insert("SYS", Signal::SIGSYS);
        }
        map
    };
}

/// Parse a signal string into a Signal enum value
///
/// Supports standard signal names (HUP, INT, TERM, etc. - with or without SIG prefix),
/// numeric values, and real-time signals (RTMIN+x, RTMAX-x, SIGRTMIN+x, SIGRTMAX-x formats)
#[cfg(unix)]
pub fn parse_signal(signal_str: &str) -> Result<Signal> {
    let mut lookup_str = signal_str.to_uppercase();

    // Strip SIG prefix if present for lookup
    if lookup_str.starts_with("SIG") {
        lookup_str = lookup_str[3..].to_string();
    }

    // Try to parse as signal name
    if let Some(&signal) = SIGNAL_NAME_MAP.get(lookup_str.as_str()) {
        return Ok(signal);
    }

    // Handle real-time signals like RTMIN+x or RTMAX-x (SIG prefix already stripped) - Linux only
    #[cfg(target_os = "linux")]
    if let Some(rt_signal) = parse_realtime_signal(&lookup_str) {
        return Ok(rt_signal);
    }

    // Try to parse as number directly
    signal_str.parse::<i32>()
        .map_err(|_| color_eyre::eyre::eyre!("invalid signal: {}", signal_str))
        .and_then(|num| Signal::try_from(num)
            .map_err(|_| color_eyre::eyre::eyre!("invalid signal: {}", signal_str)))
}

/// Parse real-time signal specifications (RTMIN+x, RTMAX-x, etc.)
#[cfg(target_os = "linux")]
fn parse_realtime_signal(signal_str: &str) -> Option<Signal> {
    let upper = signal_str.to_uppercase();

    // Handle RTMIN+x format
    if let Some(offset_str) = upper.strip_prefix("RTMIN+") {
        if let Ok(offset) = offset_str.parse::<i32>() {
            // SIGRTMIN is typically 34 on Linux
            let sig_num = 34 + offset;
            return Signal::try_from(sig_num).ok();
        }
    }

    // Handle RTMAX-x format
    if let Some(offset_str) = upper.strip_prefix("RTMAX-") {
        if let Ok(offset) = offset_str.parse::<i32>() {
            // SIGRTMAX is typically 64 on Linux
            let sig_num = 64 - offset;
            return Signal::try_from(sig_num).ok();
        }
    }

    None
}

/// Check if a string represents a valid signal name
#[cfg(unix)]
pub fn is_signal_name(name: &str) -> bool {
    // Try parsing - if it succeeds, it's a valid signal
    parse_signal(name).is_ok()
}

/// Send a signal to a process by PID
#[cfg(unix)]
pub fn kill_process(pid: i32, signal: Signal, command_name: &str) -> Result<()> {
    kill(unistd::Pid::from_raw(pid), signal)
        .map_err(|e| color_eyre::eyre::eyre!("{}: ({}) - {}: {}", command_name, pid, signal as i32, e))?;
    Ok(())
}

#[cfg(not(unix))]
#[allow(dead_code)]
pub fn kill_process(_pid: i32, _signal: i32, command_name: &str) -> Result<()> {
    Err(color_eyre::eyre::eyre!("kill_process() not supported on this platform: {}", command_name))
}

/// Get process name from /proc/<pid>/comm
///
/// Uses the comm file instead of cmdline because comm is visible to all users,
/// while cmdline may not be readable due to process permissions (e.g., setuid).
/// Note that comm is limited to 16 bytes (including null terminator).
#[cfg(unix)]
pub fn get_process_name(pid: u32) -> Option<String> {
    let comm_path = format!("/proc/{}/comm", pid);
    if let Ok(content) = fs::read_to_string(&comm_path) {
        let trimmed = content.trim_end_matches('\n');
        if !trimmed.is_empty() {
            Some(trimmed.to_string())
        } else {
            None
        }
    } else {
        None
    }
}

#[cfg(not(unix))]
#[allow(dead_code)]
pub fn get_process_name(_pid: u32) -> Option<String> {
    None
}

/// Get full command line from /proc/<pid>/cmdline
///
/// Returns the complete command line with null bytes replaced by spaces.
/// This may fail if cmdline is not readable (e.g., due to process permissions).
#[cfg(unix)]
pub fn get_process_cmdline(pid: u32) -> Option<String> {
    let cmdline_path = format!("/proc/{}/cmdline", pid);
    if let Ok(content) = fs::read_to_string(&cmdline_path) {
        let trimmed = content.trim_end_matches('\0');
        if !trimmed.is_empty() {
            Some(trimmed.replace('\0', " "))
        } else {
            None
        }
    } else {
        None
    }
}

#[cfg(not(unix))]
#[allow(dead_code)]
pub fn get_process_cmdline(_pid: u32) -> Option<String> {
    None
}

/// Get the executable path from /proc/<pid>/exe symlink
#[cfg(unix)]
pub fn get_process_exe(pid: u32) -> Option<String> {
    let exe_path = format!("/proc/{}/exe", pid);
    if let Ok(target) = std::fs::read_link(&exe_path) {
        target.to_str().map(|s| s.to_string())
    } else {
        None
    }
}

#[cfg(not(unix))]
#[allow(dead_code)]
pub fn get_process_exe(_pid: u32) -> Option<String> {
    None
}

/// Check if a process exists by checking /proc/<pid> directory
#[cfg(unix)]
pub fn process_exists(pid: u32) -> bool {
    Path::new(&format!("/proc/{}", pid)).exists()
}

/// Check if a process exists on Windows using OpenProcess API
#[cfg(windows)]
pub fn process_exists(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: OpenProcess returns a handle that we immediately check and close
    unsafe {
        let handle = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => h,
            Err(_) => return false,
        };
        if handle.is_invalid() {
            return false;
        }
        let _ = CloseHandle(handle);
        true
    }
}

#[cfg(not(any(unix, windows)))]
#[allow(dead_code)]
pub fn process_exists(_pid: u32) -> bool {
    false
}

/// Iterator over all processes in /proc
#[cfg(unix)]
pub fn iterate_processes() -> Result<impl Iterator<Item = Result<u32>>> {
    let proc_dir = Path::new("/proc");
    if !proc_dir.exists() {
        return Err(color_eyre::eyre::eyre!("/proc directory not found"));
    }

    let entries = fs::read_dir(proc_dir)
        .map_err(|e| color_eyre::eyre::eyre!("error reading /proc: {}", e))?
        .filter_map(|entry| {
            match entry {
                Ok(entry) => {
                    let file_name = entry.file_name();
                    let pid_str = file_name.to_str().unwrap_or("");
                    match pid_str.parse::<u32>() {
                        Ok(pid) => Some(Ok(pid)),
                        Err(_) => None, // Skip non-numeric directory names
                    }
                }
                Err(e) => Some(Err(color_eyre::eyre::eyre!("error reading /proc entry: {}", e))),
            }
        });

    Ok(entries)
}

#[cfg(not(unix))]
#[allow(dead_code)]
pub fn iterate_processes() -> Result<impl Iterator<Item = Result<u32>>> {
    Ok(std::iter::empty())
}

/// Rename a directory, falling back to copy+remove on cross-device errors
///
/// Attempts to use `fs::rename()` first (atomic and fast when on same filesystem).
/// If rename fails with EXDEV (Invalid cross-device link), falls back to
/// recursively copying the directory and then removing the source.
///
/// Parameters:
/// - src: Source directory path
/// - dst: Destination directory path
///
/// Returns:
/// - Ok(()) on success
/// - Err on failure (including non-EXDEV rename errors)
pub fn rename_or_copy_dir(src: &Path, dst: &Path) -> Result<()> {
    // Try rename first (atomic and fast)
    match fs::rename(src, dst) {
        Ok(()) => {
            log::trace!("Renamed directory {} to {}", src.display(), dst.display());
            Ok(())
        }
        Err(e) if e.raw_os_error() == Some(18) => {
            // EXDEV: Invalid cross-device link - fall back to copy + remove
            log::debug!("Cross-device rename not work, using copy+remove fallback: {} -> {}",
                       src.display(), dst.display());
            #[cfg(unix)]
            {
                let mut cp_options = crate::busybox::cp::CpOptions::default();
                cp_options.archive = true; // cp -a
                cp_options.force = true; // force overwrite
                cp_options.compute_derived();
                crate::busybox::cp::copy_directory_recursive(src, dst, &cp_options)
                    .wrap_err_with(|| format!("Failed to copy directory from {} to {}", src.display(), dst.display()))?;
                lfs::remove_dir_all(src)?;
                log::debug!("Successfully copied and removed directory {} -> {}", src.display(), dst.display());
                Ok(())
            }
            #[cfg(not(unix))]
            {
                Err(eyre::eyre!("Cross-device move not supported on this platform"))
            }
        }
        Err(e) => {
            Err(eyre::eyre!("Failed to rename directory from {} to {}: {}",
                           src.display(), dst.display(), e))
                .wrap_err_with(|| format!("Failed to rename directory from {} to {}", src.display(), dst.display()))
        }
    }
}

/// Print detailed clap error message without extra tip/usage/help lines.
/// This function extracts context from the error to provide specific messages.
pub fn print_clap_error_detail(e: &clap::Error) {
    match e.kind() {
        ClapErrorKind::UnknownArgument => {
            if let Some(ContextValue::String(arg)) = e.get(ContextKind::InvalidArg) {
                eprintln!("error: unexpected argument '{}' found", arg);
            } else {
                eprintln!("error: unexpected argument found");
            }
        }
        ClapErrorKind::InvalidSubcommand => {
            if let Some(ContextValue::String(sub)) = e.get(ContextKind::InvalidSubcommand) {
                eprintln!("error: unrecognized subcommand '{}'", sub);
            } else {
                eprintln!("error: unrecognized subcommand");
            }
        }
        ClapErrorKind::MissingRequiredArgument => {
            if let Some(ContextValue::Strings(args_list)) = e.get(ContextKind::InvalidArg) {
                eprintln!("error: the following required arguments were not provided:");
                for arg in args_list {
                    eprintln!("  {}", arg);
                }
            } else {
                eprintln!("error: missing required argument");
            }
        }
        ClapErrorKind::InvalidValue => {
            if let Some(ContextValue::String(arg)) = e.get(ContextKind::InvalidArg) {
                if let Some(ContextValue::String(value)) = e.get(ContextKind::InvalidValue) {
                    eprintln!("error: invalid value '{}' for '{}'", value, arg);
                } else {
                    eprintln!("error: invalid value for '{}'", arg);
                }
            } else {
                eprintln!("error: invalid value");
            }
        }
        _ => {
            // Fallback: use the first line of clap's error message
            let error_msg = e.to_string();
            if error_msg.starts_with("error:") {
                let first_line = error_msg.lines().next().unwrap_or("");
                eprintln!("{}", first_line);
            } else {
                eprintln!("{}", error_msg);
            }
        }
    }
}

/// Handle clap error with custom formatting, printing the given command line prefix.
/// This function never returns (exits the process).
pub fn handle_clap_error_with_cmdline(e: clap::Error, cmdline: String) -> ! {
    match e.kind() {
        ClapErrorKind::DisplayHelp | ClapErrorKind::DisplayVersion | ClapErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            eprintln!("{}", e);
            std::process::exit(0);
        }
        _ => {
            eprintln!("Failed to parse command line: {}", cmdline);
            print_clap_error_detail(&e);
            std::process::exit(2);
        }
    }
}

/// Split a size string into numeric part and suffix (if any).
/// Returns (number_str, suffix_option).
/// Supports suffixes: K, M, G, T, P, E, Z, Y, R, Q and their binary counterparts (KiB, MiB, etc.)
pub(crate) fn split_number_and_suffix(size_part: &str) -> (&str, Option<&str>) {
    // Parse suffix (K, M, G, etc. or KB, MB, etc.) case-insensitively.
    // Check for three-letter binary suffixes first (KiB, MiB, etc.)
    if size_part.len() >= 3 {
        let last_three = &size_part[size_part.len() - 3..];
        let last_three_upper = last_three.to_uppercase();
        match last_three_upper.as_str() {
            "KIB" | "MIB" | "GIB" | "TIB" | "PIB" | "EIB" | "ZIB" | "YIB" => {
                let num_str = &size_part[..size_part.len() - last_three.len()];
                return (num_str, Some(last_three));
            }
            _ => {}
        }
    }
    // Check for two-letter decimal suffixes (KB, MB, etc.)
    if size_part.len() >= 2 {
        let last_two = &size_part[size_part.len() - 2..];
        let last_two_upper = last_two.to_uppercase();
        match last_two_upper.as_str() {
            "KB" | "MB" | "GB" | "TB" | "PB" | "EB" | "ZB" | "YB" | "RB" | "QB" => {
                let num_str = &size_part[..size_part.len() - last_two.len()];
                return (num_str, Some(last_two));
            }
            _ => {}
        }
    }
    // Check for single-letter suffixes (K, M, G, etc.)
    if let Some(last_char) = size_part.chars().last() {
        let last_char_upper = last_char.to_ascii_uppercase();
        match last_char_upper {
            'K' | 'M' | 'G' | 'T' | 'P' | 'E' | 'Z' | 'Y' | 'R' | 'Q' => {
                let num_str = &size_part[..size_part.len() - 1];
                return (num_str, Some(&size_part[size_part.len() - 1..]));
            }
            _ => {}
        }
    }
    // No suffix
    (size_part, None)
}

/// Apply a size suffix to a numeric value, returning bytes.
/// Supports both binary (KiB, MiB, etc.) and decimal (KB, MB, etc.) suffixes.
/// Suffix matching is case-insensitive.
pub(crate) fn apply_suffix(number: i64, suffix: Option<&str>, size_str: &str) -> Result<i64> {
    let bytes = match suffix {
        Some(s) => {
            let s_upper = s.to_uppercase();
            match s_upper.as_str() {
                "K" | "KIB" => number * 1024,
                "M" | "MIB" => number * 1024 * 1024,
                "G" | "GIB" => number * 1024 * 1024 * 1024,
                "T" | "TIB" => number * 1024_i64.pow(4),
                "P" | "PIB" => number * 1024_i64.pow(5),
                "E" | "EIB" => number * 1024_i64.pow(6),
                "Z" | "ZIB" => number * 1024_i64.pow(7),
                "Y" | "YIB" => number * 1024_i64.pow(8),
                "R" => number * 1024_i64.pow(9),
                "Q" => number * 1024_i64.pow(10),
                "KB" => number * 1000,
                "MB" => number * 1000 * 1000,
                "GB" => number * 1000_i64.pow(3),
                "TB" => number * 1000_i64.pow(4),
                "PB" => number * 1000_i64.pow(5),
                "EB" => number * 1000_i64.pow(6),
                "ZB" => number * 1000_i64.pow(7),
                "YB" => number * 1000_i64.pow(8),
                "RB" => number * 1000_i64.pow(9),
                "QB" => number * 1000_i64.pow(10),
                _ => return Err(eyre!("invalid size suffix in '{}'", size_str)),
            }
        }
        None => number,
    };
    Ok(bytes)
}

/// Parse a human-readable size string into bytes.
/// Supports suffixes: K, M, G, T, P, E, Z, Y, R, Q and their binary counterparts (KiB, MiB, etc.)
/// Returns the number of bytes as i64.
#[allow(dead_code)]
pub fn parse_size_bytes(size_str: &str) -> Result<i64> {
    if size_str.is_empty() {
        return Err(eyre!("invalid size '{}'", size_str));
    }
    let (number_str, suffix) = split_number_and_suffix(size_str);
    let number = number_str.parse::<i64>()
        .map_err(|e| eyre!("invalid size '{}': {}", size_str, e))?;
    apply_suffix(number, suffix, size_str)
}

/// Parse a human-readable size string into bytes, returning None on error.
/// This is a convenience wrapper around parse_size_bytes for callers that prefer Option.
#[allow(dead_code)]
pub fn parse_size_bytes_opt(size_str: &str) -> Option<u64> {
    parse_size_bytes(size_str).ok().and_then(|n| {
        if n >= 0 {
            Some(n as u64)
        } else {
            None
        }
    })
}


#[cfg(test)]
mod tests {
    use super::*;

    /// coreutils-9.8-r1.apk: package.txt sha1 (Alpine APKINDEX C field, base64) and actual file sha1sum.
    /// Alpine C field is the control-segment hash; decoding it gives 43519c99...
    /// The file from the mirror has sha1sum e0ee1f77... (whole-file hash). Do not use C for file validation.
    #[test]
    fn normalize_sha1_apk_coreutils() {
        let package_txt_base64 = "Q1GcmRHJUquDM+0k+j5f32+S9NSFw=";
        let _file_sha1sum = "e0ee1f77c60e8d4a2076372273a513a9f8564f52"; // sha1sum coreutils-9.8-r1.apk

        let hex = normalize_sha1(package_txt_base64).expect("normalize_sha1");
        assert_eq!(hex.len(), 40, "SHA1 hex must be 40 chars");
        assert_eq!(hex, "43519c9911c952ab8333ed24fa3e5fdf6f92f4d4");
    }

    #[test]
    fn test_parse_size_bytes() {
        // Test basic suffixes
        assert_eq!(parse_size_bytes("1024").unwrap(), 1024);
        assert_eq!(parse_size_bytes("1K").unwrap(), 1024);
        assert_eq!(parse_size_bytes("1KB").unwrap(), 1000);
        assert_eq!(parse_size_bytes("1KiB").unwrap(), 1024);
        assert_eq!(parse_size_bytes("1M").unwrap(), 1024*1024);
        assert_eq!(parse_size_bytes("1MB").unwrap(), 1000*1000);
        assert_eq!(parse_size_bytes("1MiB").unwrap(), 1024*1024);
        // Test case-insensitivity
        assert_eq!(parse_size_bytes("1k").unwrap(), 1024);
        assert_eq!(parse_size_bytes("1kb").unwrap(), 1000);
        assert_eq!(parse_size_bytes("1kib").unwrap(), 1024);
        // Test large suffixes
        assert_eq!(parse_size_bytes("1G").unwrap(), 1024*1024*1024);
        assert_eq!(parse_size_bytes("1T").unwrap(), 1024_i64.pow(4));
        // Test error cases
        assert!(parse_size_bytes("").is_err());
        assert!(parse_size_bytes("abc").is_err());
        assert!(parse_size_bytes("1X").is_err());
    }
}
