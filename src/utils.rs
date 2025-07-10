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
use std::os::unix::fs::symlink;
use nix::unistd;
use crate::models;

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

#[derive(Debug, Clone)]
pub struct MtreeFileInfo {
    pub path: PathBuf,
    pub file_type: MtreeFileType,
    pub mode: Option<u32>,
    #[allow(dead_code)]
    pub sha256: Option<String>,
    #[allow(dead_code)]
    pub link_target: Option<String>,
    #[allow(dead_code)]
    pub uname: Option<String>,
    #[allow(dead_code)]
    pub gname: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MtreeFileType {
    File,
    Dir,
    Link,
    Unknown,
}

impl MtreeFileInfo {
    pub fn is_dir(&self) -> bool {
        self.file_type == MtreeFileType::Dir
    }

    #[allow(dead_code)]
    pub fn is_file(&self) -> bool {
        self.file_type == MtreeFileType::File
    }

    #[allow(dead_code)]
    pub fn is_link(&self) -> bool {
        self.file_type == MtreeFileType::Link
    }
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

// List package/fs files - improved version that reads from filelist.txt
pub fn list_package_files(package_fs_dir: &str) -> Result<Vec<PathBuf>> {
    // For backwards compatibility, still return Vec<PathBuf>
    let file_infos = list_package_files_with_info(package_fs_dir)?;
    Ok(file_infos.into_iter().map(|info| info.path).collect())
}

// New function that reads from filelist.txt and provides type information
pub fn list_package_files_with_info(package_fs_dir: &str) -> Result<Vec<MtreeFileInfo>> {
    let package_fs_path = Path::new(package_fs_dir);
    let store_dir = package_fs_path.parent()
        .ok_or_else(|| eyre::eyre!("Cannot get parent directory of {}", package_fs_dir))?;
    let filelist_path = store_dir.join("info/filelist.txt");

    // If filelist.txt doesn't exist, fall back to filesystem walking
    if !filelist_path.exists() {
        log::debug!("filelist.txt not found at {}, falling back to filesystem walking", filelist_path.display());
        return list_package_files_fallback(package_fs_dir);
    }

    // Read and parse filelist.txt
    let content = fs::read_to_string(&filelist_path)
        .wrap_err_with(|| format!("Failed to read filelist.txt from {}", filelist_path.display()))?;

    let mut file_infos = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        match parse_mtree_line(line, package_fs_path) {
            Ok(Some(file_info)) => file_infos.push(file_info),
            Ok(None) => continue, // Skip this line
            Err(e) => {
                log::debug!("Failed to parse mtree line '{}': {}", line, e);
                continue; // Skip malformed lines
            }
        }
    }

    Ok(file_infos)
}

// Parse a single mtree format line
fn parse_mtree_line(line: &str, package_fs_path: &Path) -> Result<Option<MtreeFileInfo>> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.is_empty() {
        return Ok(None);
    }

    let relative_path = parts[0];
    let full_path = package_fs_path.join(relative_path);

    let mut file_type = MtreeFileType::Unknown;
    let mut mode = None;
    let mut sha256 = None;
    let mut link_target = None;
    let mut uname = None;
    let mut gname = None;

    // Parse key=value pairs
    for part in &parts[1..] {
        if let Some((key, value)) = part.split_once('=') {
            match key {
                "type" => {
                    file_type = match value {
                        "file" => MtreeFileType::File,
                        "dir" => MtreeFileType::Dir,
                        "link" => MtreeFileType::Link,
                        _ => MtreeFileType::Unknown,
                    };
                }
                "mode" => {
                    mode = u32::from_str_radix(value, 8).ok();
                }
                "sha256" => {
                    sha256 = Some(value.to_string());
                }
                "link" => {
                    link_target = Some(value.to_string());
                }
                "uname" => {
                    uname = Some(value.to_string());
                }
                "gname" => {
                    gname = Some(value.to_string());
                }
                _ => {
                    // Skip unknown attributes
                }
            }
        }
    }

    Ok(Some(MtreeFileInfo {
        path: full_path,
        file_type,
        mode,
        sha256,
        link_target,
        uname,
        gname,
    }))
}

// Fallback to filesystem walking if filelist.txt doesn't exist
fn list_package_files_fallback(package_fs_dir: &str) -> Result<Vec<MtreeFileInfo>> {
    let dir = Path::new(package_fs_dir);
    let mut file_infos = Vec::new();

    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        let mtree_type = if file_type.is_dir() {
            MtreeFileType::Dir
        } else if file_type.is_symlink() {
            MtreeFileType::Link
        } else {
            MtreeFileType::File
        };

        let metadata = fs::symlink_metadata(&path)?;
        let mode = Some(metadata.permissions().mode());
        let link_target = if file_type.is_symlink() {
            fs::read_link(&path).ok().map(|p| p.to_string_lossy().to_string())
        } else {
            None
        };

        file_infos.push(MtreeFileInfo {
            path: path.clone(),
            file_type: mtree_type,
            mode,
            sha256: None,
            link_target,
            uname: None,
            gname: None,
        });

        if file_type.is_dir() {
            file_infos.extend(list_package_files_fallback(path.to_str().unwrap())?);
        }
    }

    Ok(file_infos)
}

// Get file type
pub fn get_file_type(file: &Path) -> Result<(FileType, String)> {
    const ELF_MAGIC: &[u8] = &[0x7f, b'E', b'L', b'F'];

    // Check Symbolic link first
    if fs::symlink_metadata(&file).map_or(false, |metadata| metadata.file_type().is_symlink()) {
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
    let actual_checksum = compute_file_sha256(file_path.to_str().unwrap())?;

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
            if let Err(e) = fs::create_dir_all(&full_path) {
                return Err(eyre::eyre!("Error creating directory {}: {}", full_path.display(), e));
            }
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
                    fs::remove_file(&full_path)
                        .with_context(|| format!("Error removing existing file {}", full_path.display()))?;
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

/// 递归复制源路径下的所有内容到目标路径，支持文件和目录的复制。
///
/// 若源路径是目录，该函数会递归复制目录下的所有文件和子目录到目标路径；
/// 若源路径是文件，则直接将该文件复制到目标路径。
///
/// # 参数
/// * `src` - 源路径，可以是文件或目录，实现了 `AsRef<Path>` 特征。
/// * `dst` - 目标路径，复制操作的目的地，实现了 `AsRef<Path>` 特征。
///
/// # 返回值
/// * `Ok(())` - 复制操作成功完成。
/// * `Err` - 复制过程中出现 I/O 错误，如无法获取元数据、创建目录失败或复制文件失败等。
#[allow(dead_code)]
pub fn copy_all<P: AsRef<Path>>(src: P, dst: P) -> Result<()> {
    let metadata = fs::metadata(&src)
        .with_context(|| format!("Failed to get metadata for {}", src.as_ref().display()))?;

    if metadata.is_dir() {
        // 若源路径是目录，则创建目标目录（如果不存在），然后递归复制目录下的所有文件和子目录。
        fs::create_dir_all(&dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let src_path = entry.path();
            let dst_path = dst.as_ref().join(entry.file_name());
            copy_all(src_path, dst_path)?;
        }
    } else {
        fs::copy(src, dst)?;
    }
    Ok(())
}

pub fn is_running_as_root() -> bool {
    unistd::geteuid().is_root()
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
        fs::create_dir_all(parent)?;
    }

    let input_file = fs::File::open(input_path)
        .with_context(|| format!("Failed to open input file: {}", input_path.display()))?;
    let mut output = fs::File::create(output_path)
        .with_context(|| format!("Failed to create output file: {}", output_path.display()))?;

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
        std::fs::remove_file(&part_path)
            .with_context(|| format!("Failed to remove existing bad file: {}", part_path.display()))?;
    }

    if !file_path.exists() {
        return Err(eyre::eyre!("File not exist: {}", file_path.display()));
    }

    // Remove existing .bad file
    if bad_path.exists() {
        std::fs::remove_file(&bad_path)
            .with_context(|| format!("Failed to remove existing bad file: {}", bad_path.display()))?;
    }

    // Rename the file
    std::fs::rename(file_path, &bad_path)
        .with_context(|| format!("Failed to rename {} to {}", file_path.display(), bad_path.display()))?;

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
    if symlink_path.exists() {
        fs::remove_file(symlink_path)
            .with_context(|| format!("Failed to remove existing file/symlink at {}", symlink_path.display()))?;
    }

    // Create the symlink
    log::debug!("Creating symlink: {} -> {}", symlink_path.display(), file_path.display());
    symlink(file_path, symlink_path)
        .with_context(|| format!("Failed to create symlink from {} to {}",
            file_path.display(), symlink_path.display()))?;

    Ok(())
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
            remove_any_existing_file(path)?;
        } else if metadata.is_dir() {
            log::debug!("Path exists as a directory: {}", path.display());
            // Directory already exists, we can proceed
            return Ok(());
        } else {
            log::debug!("Path exists as something else (symlink, etc.), removing it: {}", path.display());
            remove_any_existing_file(path)?;
        }
    }

    // Create the directory with all parent directories
    fs::create_dir_all(path)
        .map_err(|e| {
            let context = match e.kind() {
                io::ErrorKind::PermissionDenied => {
                    format!("Permission denied - check if you have write access to {}",
                           path.parent().unwrap_or_else(|| Path::new("")).display())
                }
                io::ErrorKind::NotFound => {
                    format!("Parent directory not found - check if {} exists",
                           path.parent().unwrap_or_else(|| Path::new("")).display())
                }
                io::ErrorKind::AlreadyExists => {
                    "Directory already exists".to_string()
                }
                _ => format!("Unknown error: {}", e)
            };
            eyre::eyre!("Failed to create directory '{}': {}\nContext: {}", path.display(), e, context)
        })?;

    Ok(())
}

/// Remove any existing file, symlink, or other non-directory at the given path
///
/// This function will attempt to remove the file regardless of its type
/// (regular file, symlink, etc.) but will not remove directories.
/// Handles dead symlinks by using symlink_metadata instead of path.exists().
pub fn remove_any_existing_file(path: &Path) -> Result<()> {
    // Use symlink_metadata to handle dead symlinks properly
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.is_dir() {
                return Err(eyre::eyre!("Cannot remove directory '{}' with remove_any_existing_file()", path.display()));
            }

            fs::remove_file(path)
                .map_err(|e| eyre::eyre!("Failed to remove existing file at path '{}': {}", path.display(), e))?;
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
