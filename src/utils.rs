use std::collections::HashMap;
use std::fs;
use std::io;
use std::io::{BufRead, BufReader, Read, Write, Seek, SeekFrom, ErrorKind};
use std::path::Path;
use std::path::PathBuf;
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre;
use sha2::{Sha256, Digest};
use tar::Archive;
use flate2::read::GzDecoder;
use liblzma;
use zstd;
use std::os::unix::fs::PermissionsExt; // For checking execute permissions
use nix::unistd;
use nix::sys::signal::kill;
use nix::sys::signal::Signal;
#[cfg(unix)]
use users::{get_current_uid, get_effective_uid};
use crate::models;
use crate::userdb;
use crate::lfs;
use crate::mtree::{self, MtreeFileInfo};

#[derive(Debug, PartialEq)]
pub enum FileType {
    Elf,
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


/// Get all files/dirs from a package as relative paths
/// Returns relative paths (without the fs/ directory prefix)
pub fn get_package_files(
    store_root: &Path,
    pkgline: &str,
) -> Result<Vec<String>> {
    let store_fs_dir = store_root.join(pkgline).join("fs");
    if !store_fs_dir.exists() {
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
pub fn list_package_files_with_info(package_fs_dir: &str) -> Result<Vec<MtreeFileInfo>> {
    let package_fs_path = Path::new(package_fs_dir);

    // Check if the package_fs_dir itself exists first
    if !package_fs_path.exists() {
        return Err(eyre::eyre!("Package filesystem directory does not exist: {}", package_fs_dir));
    }

    // Check if it's actually a directory
    if !package_fs_path.is_dir() {
        return Err(eyre::eyre!("Package filesystem path is not a directory: {}", package_fs_dir));
    }

    let store_dir = package_fs_path.parent()
        .ok_or_else(|| eyre::eyre!("Cannot get parent directory of {}", package_fs_dir))?;
    let filelist_path = store_dir.join("info/filelist.txt");

    // If filelist.txt doesn't exist, return an error
    if !filelist_path.exists() {
        return Err(eyre::eyre!("filelist.txt not found at {}", filelist_path.display()));
    }

    // Read and parse filelist.txt
    let content = fs::read_to_string(&filelist_path)
        .wrap_err_with(|| format!("Failed to read filelist.txt from {}", filelist_path.display()))?;

    mtree::parse_simplified_mtree(&content)
}

/// Normalize a file path from package filelist: remove leading '.' and trailing '/'
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
pub fn truncate_display(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}


// Get file type
pub fn get_file_type(file: &Path) -> Result<(FileType, String)> {
    const ELF_MAGIC: &[u8] = &[0x7f, b'E', b'L', b'F'];

    // Check Symbolic link first
    if lfs::symlink_metadata(&file).map_or(false, |metadata| metadata.file_type().is_symlink()) {
        return Ok((FileType::Symlink, String::new()));
    }

    // Read file contents for other checks
    let mut file = fs::File::open(file)?;
    // Check ELF 64-bit LSB
    let mut buffer = vec![0;4];
    if let Ok(_) = file.read_exact(&mut buffer) {
        if buffer.starts_with(ELF_MAGIC) {
            return Ok((FileType::Elf, String::new()));
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

pub fn compute_file_sha256(file_path: &str) -> Result<String> {
    let file = fs::File::open(file_path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0; 4096];

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
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

    if !checksum_file.exists() {
        return Err(eyre::eyre!("Checksum file not found: {}", checksum_file.display()));
    }

    if !file_path.exists() {
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
    let file_path_str = file_path.to_str()
        .ok_or_else(|| eyre::eyre!("File path contains invalid UTF-8: {}", file_path.display()))?;
    let actual_checksum = compute_file_sha256(file_path_str)?;

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
    if !tar_path.exists() {
        return Err(eyre::eyre!("Tar file not found: {}", tar_path.display()));
    }

    // Check if file is empty
    let metadata = fs::metadata(tar_path)?;
    if metadata.len() == 0 {
        return Err(eyre::eyre!("Tar file is empty: {}", tar_path.display()));
    }

    // Open and extract tar.gz file
    let tar_gz = fs::File::open(tar_path)
        .context("Failed to open tar file")?;
    let tar = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar);

    // Extract all entries
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

        let full_path = dest_dir.join(path.clone());

        // Create parent directories if needed
        if path.is_dir() {
            lfs::create_dir_all(&full_path)?;
            continue;
        }

        // only files
        match entry.unpack(&full_path) {
            Ok(_) => {
                // Verify file was created and is readable
                if !full_path.exists() {
                    return Err(eyre::eyre!("File was not extracted: {}", full_path.display()));
                }
                if let Err(e) = fs::metadata(&full_path) {
                    return Err(eyre::eyre!("Cannot access extracted file {}: {}", full_path.display(), e));
                }
            },
            Err(e) => {
                if e.kind() == ErrorKind::AlreadyExists {
                    lfs::remove_file(&full_path)?;
                    entry.unpack(&full_path)
                        .with_context(|| format!("Error extracting {} after removal", full_path.display()))?;
                } else {
                    return Err(eyre::eyre!("Error extracting {}: {}", full_path.display(), e))
                }
            }
        }
    }

    Ok(())
}

pub fn is_running_as_root() -> bool {
    unistd::geteuid().is_root()
}

/// Determine shared_store mode based on the decision sequence:
/// 1. private if !is_running_as_root
/// 2. private if current_exe starts with /home/
/// 3. public  if current_exe starts with /opt/epkg/
/// 4. public  if running as root and /opt/epkg/store/ exists
/// 5. private if $HOME/.epkg/store/ exists
/// 6. public  if /opt/epkg/store/ exists
/// 7. error and abort otherwise
pub fn determine_shared_store() -> Result<bool> {
    use std::env;
    use std::path::Path;
    use crate::dirs::get_home;

    let is_root = is_running_as_root();

    // Rule 1: If !is_running_as_root, set to private
    if !is_root {
        return Ok(false);
    }

    // Rule 2: If current_exe starts with /home/, set to private
    let current_exe = env::current_exe()
        .wrap_err("Failed to get current executable path")?;
    let current_exe_str = current_exe.to_string_lossy();
    if current_exe_str.starts_with("/home/") {
        return Ok(false);
    }

    // Rule 3: If current_exe starts with /opt/epkg/, set to public
    if current_exe_str.starts_with("/opt/epkg/") {
        return Ok(true);
    }

    // Rule 4: If running as root and /opt/epkg/envs/ exists, set to public
    let opt_envs = Path::new("/opt/epkg/envs");
    let has_opt_envs = opt_envs.exists();
    if is_root && has_opt_envs {
        return Ok(true);
    }

    // Rule 5: If $HOME/.epkg/envs/ exists, set to private
    let home = get_home()?;
    let home_envs = Path::new(&home).join(".epkg/envs");
    if home_envs.exists() {
        return Ok(false);
    }

    // Rule 6: If /opt/epkg/envs/ exists, set to public
    if has_opt_envs {
        return Ok(true);
    }

    // Rule 7: Otherwise: neither envs exists, default to private (false)
    Ok(false)
}

/// Check if the process is running with setuid privileges
/// Returns true if effective UID differs from real UID
#[cfg(unix)]
pub fn is_suid() -> bool {
    get_current_uid() != get_effective_uid()
}

#[cfg(not(unix))]
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
pub fn get_home_from_uid() -> Result<String> {
    Err(color_eyre::eyre::eyre!("get_home_from_uid() not supported on this platform"))
}

pub fn command_exists(command_name: &str) -> bool {
    find_command_in_paths(command_name).is_some()
}

/// Searches for an executable command in a predefined list of common paths.
/// Returns the full path to the command if found and executable, otherwise None.
pub fn find_command_in_paths(command_name: &str) -> Option<PathBuf> {
    let common_paths = [
        "/usr/local/sbin",
        "/usr/local/bin",
        "/usr/sbin",
        "/usr/bin",
        "/sbin",
        "/bin",
        // Add other paths if necessary, e.g., from $HOME/.local/bin
    ];

    for path_dir in common_paths.iter() {
        let mut full_path = PathBuf::from(path_dir);
        full_path.push(command_name);
        if full_path.exists() {
            if let Ok(metadata) = fs::metadata(&full_path) {
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
    if part_path.exists() {
        lfs::remove_file(&part_path)?;
    }

    // If the original file no longer exists, treat this as a no-op.
    // This can happen if another component (or an earlier attempt) has
    // already renamed the file to .bad or otherwise removed it.
    if !file_path.exists() {
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
    if bad_path.exists() {
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

pub fn force_symlink<P: AsRef<Path>, Q: AsRef<Path>>(file_path: P, symlink_path: Q) -> Result<()> {
    let file_path = file_path.as_ref();
    let symlink_path = symlink_path.as_ref();

    // Remove existing symlink or file if it exists
    if lfs::symlink_metadata(symlink_path).is_ok() {
        lfs::remove_file(symlink_path)?;
    }

    // Create the symlink
    log::debug!("Creating symlink: {} -> {}", symlink_path.display(), file_path.display());
    lfs::symlink(file_path, symlink_path)?;

    Ok(())
}

/// Resolve a symlink (or regular file) to its target within the environment root.
///
/// This function handles both absolute and relative symlinks, ensuring the resolved
/// target exists within the environment root. It also handles regular files (non-symlinks)
/// by returning the input path unchanged.
///
/// Returns Some(target_path) where target_path is the resolved absolute path within the environment
/// that actually contains the executable, or None if the path is invalid or the target doesn't exist.
///
/// # Examples
/// - Regular file: `~/.epkg/envs/alpine/usr/bin/bash` exists → `Some(~/.epkg/envs/alpine/usr/bin/bash)`
/// - Absolute symlink: `~/.epkg/envs/alpine/usr/bin/sh -> /usr/bin/bash` → `Some(~/.epkg/envs/alpine/usr/bin/bash)`
/// - Relative symlink: `~/.epkg/envs/alpine/usr/bin/sh -> bash` → `Some(~/.epkg/envs/alpine/usr/bin/bash)`
/// - Invalid symlink: symlink points to non‑existent target → `None`
///
/// # Security
/// The resolved path is guaranteed to be within `env_root` (or `None`). Relative symlinks
/// containing `..` components that would escape the environment root are rejected.
pub fn resolve_symlink_in_env(symlink_path: &std::path::Path, env_root: &std::path::Path) -> Option<std::path::PathBuf> {
    // First check if the symlink file itself exists (as a regular file or symlink)
    if symlink_path.exists() && !symlink_path.is_symlink() {
        // It's a regular file, not a symlink
        // Example: ~/.epkg/envs/alpine/usr/bin/bash is a regular executable file
        // Return: Some(~/.epkg/envs/alpine/usr/bin/bash) - the resolved target path (same as input)
        return Some(symlink_path.to_path_buf());
    }

    // If it's a symlink, read the target and check if the target exists within the environment
    if let Ok(link_target) = std::fs::read_link(symlink_path) {

        if link_target.is_absolute() {
            // For system paths, map them into the environment root
            // This avoids checking host system paths that might coincidentally exist
            // Example: ~/.epkg/envs/alpine/usr/bin/sh -> /usr/bin/bash -> Some(~/.epkg/envs/alpine/usr/bin/bash)
            let is_system_path = link_target.starts_with("/usr") ||
                                 link_target.starts_with("/bin") ||
                                 link_target.starts_with("/sbin") ||
                                 link_target.starts_with("/lib") ||
                                 link_target.starts_with("/lib64") ||
                                 link_target.starts_with("/lib32") ||
                                 link_target.starts_with("/libx32");
            let is_mounted_path = link_target.starts_with("/etc");
            if is_system_path || is_mounted_path {
                let target_in_env = env_root.join(link_target.strip_prefix("/").unwrap_or(&link_target));
                if target_in_env.exists() {
                    return Some(target_in_env);
                }
            }

            // Allow symlinks pointing within the same environment root
            // if link_target.starts_with(env_root) && link_target.exists() {
            //     return Some(link_target);
            // }

            // Allow symlinks pointing into the epkg store
            // if link_target.starts_with(&dirs().epkg_store) && link_target.exists() {
            //     return Some(link_target);
            // }

            // Special case: symlink pointing to the current executable (epkg binary)
            // if let Ok(current_exe) = std::env::current_exe() {
            //     if link_target == current_exe && link_target.exists() {
            //         return Some(link_target);
            //     }
            // }

            // For other absolute paths, assume env fs is the same with host, so detect in host
            if link_target.exists() {
                return Some(link_target);
            }
        } else {
            // Relative symlink: resolve relative to the symlink's directory
            // Example: ~/.epkg/envs/alpine/usr/bin/sh -> bash -> Some(~/.epkg/envs/alpine/usr/bin/bash)
            let symlink_dir = symlink_path.parent()?;
            let resolved_path = symlink_dir.join(&link_target);
            if resolved_path.exists() {
                return Some(resolved_path);
            }
        }
    }

    // Return: None - symlink_path doesn't exist on host, symlink target doesn't exist in environment, or symlink couldn't be read
    None
}

/// Safely create a directory, handling cases where a file with the same name already exists
///
/// This function will:
/// 1. Check if the path already exists
/// 2. If it's a file, remove it first
/// 3. If it's a directory, return success
/// 4. If it's something else (symlink, etc.), remove it first
/// 5. Create the directory with all parent directories
pub fn safe_mkdir_p(path: &Path) -> Result<()> {
    // Check if path already exists and handle it appropriately
    if path.exists() {
        let metadata = fs::metadata(path)
            .map_err(|e| eyre::eyre!("Failed to get metadata for existing path '{}': {}", path.display(), e))?;

        if metadata.is_file() {
            log::debug!("Path exists as a file, removing it: {}", path.display());
            remove_any_existing_file(path, false)?;
        } else if metadata.is_dir() {
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
    if let Ok(metadata) = fs::metadata(source) {
        lfs::set_permissions(target, metadata.permissions())?;
    }
    Ok(())
}

/// Fix up file permissions to ensure files are readable for hash calculation
#[cfg(unix)]
pub fn fixup_file_permissions(target_path: &Path) {
    if let Ok(metadata) = fs::metadata(target_path) {
        if metadata.is_dir() {
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
    if let Ok(metadata) = fs::metadata(target_path) {
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
pub fn fixup_file_permissions(_target_path: &Path) {
    // No-op on non-Unix systems
}

#[cfg(not(unix))]
fn ensure_owner_permissions(_target_path: &Path, _required_mask: u32, _file_type: &str) {
    // No-op on non-Unix systems
}


/// Fix up environment links and remove system directories
pub fn fixup_env_links(env_root: &Path) -> Result<()> {
    // Prevent running and stalling on `systemctl --system daemon-reload`
    let _ = lfs::remove_dir(env_root.join("run/systemd/system"));

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
        let full_symlink_path = env_root.join(
            symlink_path.strip_prefix("/")
            .unwrap_or(symlink_path)  // Fallback to original if no prefix
        );

        if full_symlink_path.exists() && full_symlink_path.is_symlink() {
            // Resolve the symlink to get the actual target file path
            let target_path = std::fs::canonicalize(&full_symlink_path)
                .map_err(|e| {
                    log::warn!("Failed to resolve symlink {}: {}", full_symlink_path.display(), e);
                    e
                })?;

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
    // List of symlinks to create: [(symlink, [possible_targets])]
    let symlinks = [
        ("bin/sh", ["bash", "dash"]),
        ("usr/bin/awk", ["mawk", "gawk"]),

        // These are optional and will fail due to no "dpkg -L" output
        ("usr/local/bin/py3compile", ["/usr/bin/true", "/bin/true"]),
        ("usr/local/bin/py3clean", ["/usr/bin/true", "/bin/true"]),

        // Pacman-style dbus reload hook expects this helper. Many minimal
        // Arch-like environments don't ship it, so we point it to a no-op
        // true(1) to avoid hard failures while still allowing the
        // transaction to complete.
        ("usr/share/libalpm/scripts/systemd-hook", ["/usr/bin/true", "/bin/true"]),
    ];

    for (link_name, possible_targets) in &symlinks {
        let link_path = env_root.join(link_name);

        // Skip if symlink already exists
        if link_path.is_symlink() || link_path.exists() {
            continue;
        }

        // Try each possible target until we find one that exists
        for target in possible_targets.iter() {
            // Check if target exists within env_root, not host rootfs
            let target_check_path = if target.starts_with('/') {
                // Absolute path: check in env_root
                env_root.join(&target[1..]) // Remove leading slash
            } else {
                // Relative path: relative to symlink's parent directory within env_root
                env_root.join(link_name).parent().unwrap().join(target)
            };

            if target_check_path.exists() {
                if let Some(parent) = link_path.parent() {
                    lfs::create_dir_all(parent)?;
                }
                // Use the original target string for the symlink (relative or absolute as specified)
                lfs::symlink(target, &link_path)?;
                break;
            }
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

    let conf_dir = env_root.join("etc/makepkg.conf.d");
    if !conf_dir.exists() {
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
            env_root.join(&pattern[1..]) // Remove leading slash
        } else {
            env_root.join(pattern)
        };

        // Use glob to find matching files
        match glob::glob(absolute_pattern.to_str().unwrap()) {
            Ok(paths) => {
                for path_result in paths {
                    match path_result {
                        Ok(path) => {
                            if path.exists() {
                                log::debug!("Removing file matching pattern '{}': {}", pattern, path.display());
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
    let mut perms = fs::metadata(path)
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
    // No-op on non-Unix systems
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

/// Process scriptlets from a mapping, copying them from source directory to target directory
/// This is a common pattern used by deb_pkg, conda_pkg, and apk_pkg
///
/// # Arguments
/// * `mapping` - HashMap mapping package-specific scriptlet names to common scriptlet names
/// * `source_dir` - Directory containing the source scriptlet files
/// * `target_dir` - Directory where common scriptlet files should be written
/// * `enable_logging` - If true, log when scriptlets are found and created
pub fn copy_scriptlets_by_mapping<P: AsRef<Path>>(
    mapping: &std::collections::HashMap<&str, &str>,
    source_dir: P,
    target_dir: P,
    enable_logging: bool,
) -> Result<()> {
    let source_dir = source_dir.as_ref();
    let target_dir = target_dir.as_ref();

    for (package_script, common_script) in mapping {
        let source_path = source_dir.join(package_script);
        if source_path.exists() {
            if enable_logging {
                log::debug!("Found scriptlet: {}", package_script);
            }
            let target_path = target_dir.join(common_script);
            copy_scriptlet_file(&source_path, &target_path)?;
            if enable_logging {
                log::debug!("Created script: {} -> {}", package_script, common_script);
            }
        }
    }

    Ok(())
}

macro_rules! signal_map {
    ($map:expr, $(($name:expr, $signal:expr)),* $(,)?) => {
        $(
            $map.insert($name, $signal);
        )*
    };
}

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
            ("PWR",     Signal::SIGPWR),
            ("SYS",     Signal::SIGSYS),
        );
        map
    };
}

/// Parse a signal string into a Signal enum value
///
/// Supports standard signal names (HUP, INT, TERM, etc. - with or without SIG prefix),
/// numeric values, and real-time signals (RTMIN+x, RTMAX-x, SIGRTMIN+x, SIGRTMAX-x formats)
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

    // Handle real-time signals like RTMIN+x or RTMAX-x (SIG prefix already stripped)
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
pub fn is_signal_name(name: &str) -> bool {
    // Try parsing - if it succeeds, it's a valid signal
    parse_signal(name).is_ok()
}

/// Send a signal to a process by PID
pub fn kill_process(pid: i32, signal: Signal, command_name: &str) -> Result<()> {
    kill(unistd::Pid::from_raw(pid), signal)
        .map_err(|e| color_eyre::eyre::eyre!("{}: ({}) - {}: {}", command_name, pid, signal as i32, e))?;
    Ok(())
}

/// Get process name from /proc/<pid>/comm
///
/// Uses the comm file instead of cmdline because comm is visible to all users,
/// while cmdline may not be readable due to process permissions (e.g., setuid).
/// Note that comm is limited to 16 bytes (including null terminator).
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

/// Get full command line from /proc/<pid>/cmdline
///
/// Returns the complete command line with null bytes replaced by spaces.
/// This may fail if cmdline is not readable (e.g., due to process permissions).
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

/// Get the executable path from /proc/<pid>/exe symlink
pub fn get_process_exe(pid: u32) -> Option<String> {
    let exe_path = format!("/proc/{}/exe", pid);
    if let Ok(target) = std::fs::read_link(&exe_path) {
        target.to_str().map(|s| s.to_string())
    } else {
        None
    }
}

/// Check if a process exists by checking /proc/<pid> directory
pub fn process_exists(pid: u32) -> bool {
    Path::new(&format!("/proc/{}", pid)).exists()
}

/// Iterator over all processes in /proc
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
            let mut cp_options = crate::applets::cp::CpOptions::default();
            cp_options.archive = true; // cp -a
            cp_options.force = true; // force overwrite
            cp_options.compute_derived();
            crate::applets::cp::copy_directory_recursive(src, dst, &cp_options)
                .wrap_err_with(|| format!("Failed to copy directory from {} to {}", src.display(), dst.display()))?;
            lfs::remove_dir_all(src)?;
            log::debug!("Successfully copied and removed directory {} -> {}", src.display(), dst.display());
            Ok(())
        }
        Err(e) => {
            Err(eyre::eyre!("Failed to rename directory from {} to {}: {}", 
                           src.display(), dst.display(), e))
                .wrap_err_with(|| format!("Failed to rename directory from {} to {}", src.display(), dst.display()))
        }
    }
}
