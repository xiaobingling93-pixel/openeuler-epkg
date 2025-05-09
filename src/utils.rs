use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, ErrorKind};
use std::path::Path;
use std::path::PathBuf;
use anyhow::Result;
use anyhow::Context;
use sha2::{Sha256, Digest};
use std::fs::File;
use tar::Archive;
use flate2::read::GzDecoder;

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
    AsciiText,
    Binary,
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
            FileType::AsciiText => "ASCII text",
            FileType::Binary => "Binary data",
        }
    }
}

#[allow(dead_code)]
pub fn is_setuid() -> bool {
    true
}

// List package/fs files
pub fn list_package_files(package_fs_dir: &str) -> Result<Vec<PathBuf>> {
    let dir = Path::new(package_fs_dir);
    let mut paths = Vec::new();

    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            paths.push(path.clone());
            paths.extend(list_package_files(path.to_str().unwrap())?);
        } else {
            paths.push(path.clone());
        }
    }

    Ok(paths)
}

// Get file type
pub fn get_file_type(file: &Path) -> Result<FileType> {
    const ELF_MAGIC: &[u8] = &[0x7f, b'E', b'L', b'F'];

    // Check Symbolic link first
    if fs::symlink_metadata(&file).map_or(false, |metadata| metadata.file_type().is_symlink()) {
        return Ok(FileType::Symlink);
    }

    // Read file contents for other checks
    let mut file = fs::File::open(file)?;
    // Check ELF 64-bit LSB
    let mut buffer = vec![0;4];
    if let Ok(_) = file.read_exact(&mut buffer) {
        if buffer.starts_with(ELF_MAGIC) {
            return Ok(FileType::Elf);
        }
    }

    // Use BufReader for reading lines
    // Reset file pointer to the beginning
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();
    let bytes_read = reader.read_line(&mut first_line)?;
    if bytes_read == 0 {
        return Ok(FileType::AsciiText);
    }

    // Check if file starts with shebang
    if first_line.starts_with("#!") {
        // Check for various script types
        if first_line.contains("sh") {
            return Ok(FileType::ShellScript);
        } else if first_line.contains("perl") {
            return Ok(FileType::PerlScript);
        } else if first_line.contains("python") {
            return Ok(FileType::PythonScript);
        } else if first_line.contains("ruby") {
            return Ok(FileType::RubyScript);
        } else if first_line.contains("node") {
            return Ok(FileType::NodeScript);
        } else if first_line.contains("lua") {
            return Ok(FileType::LuaScript);
        }
    }

    // Try to detect if it's ASCII text
    for line in reader.lines() {
        let line = line?;
        if !line.is_ascii() {
            return Ok(FileType::Binary);
        }
    }

    return Ok(FileType::AsciiText);
}

pub fn compute_file_sha256(file_path: &str) -> Result<String> {
    let file = File::open(file_path)?;
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
        return Err(anyhow::anyhow!("Checksum file not found: {}", checksum_file.display()));
    }

    if !file_path.exists() {
        return Err(anyhow::anyhow!("File not found: {}", file_path.display()));
    }

    // Read expected checksum from file
    let binding = fs::read_to_string(checksum_file)?;
    let expected_checksum = binding
        .trim()
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Invalid checksum file format"))?;

    // Compute actual checksum
    let actual_checksum = compute_file_sha256(file_path.to_str().unwrap())?;

    // Compare checksums
    if actual_checksum != expected_checksum {
        return Err(anyhow::anyhow!(
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
        return Err(anyhow::anyhow!("Tar file not found: {}", tar_path.display()));
    }

    // Check if file is empty
    let metadata = fs::metadata(tar_path)?;
    if metadata.len() == 0 {
        return Err(anyhow::anyhow!("Tar file is empty: {}", tar_path.display()));
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
                return Err(anyhow::anyhow!("Error reading tar entry: {}", e));
            }
        };

        let path = match entry.path() {
            Ok(path) => path,
            Err(e) => {
                return Err(anyhow::anyhow!("Error getting entry path: {}", e));
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
                return Err(anyhow::anyhow!("Error creating directory {}: {}", full_path.display(), e));
            }
            continue;
        }

        // only files
        match entry.unpack(&full_path) {
            Ok(_) => {
                // Verify file was created and is readable
                if !full_path.exists() {
                    return Err(anyhow::anyhow!("File was not extracted: {}", full_path.display()));
                }
                if let Err(e) = fs::metadata(&full_path) {
                    return Err(anyhow::anyhow!("Cannot access extracted file {}: {}", full_path.display(), e));
                }
            },
            Err(e) => {
                if e.kind() == ErrorKind::AlreadyExists {
                    fs::remove_file(&full_path)
                        .with_context(|| format!("Error removing existing file {}", full_path.display()))?;
                    entry.unpack(&full_path)
                        .with_context(|| format!("Error extracting {} after removal", full_path.display()))?;
                } else {
                    return Err(anyhow::anyhow!("Error extracting {}: {}", full_path.display(), e))
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