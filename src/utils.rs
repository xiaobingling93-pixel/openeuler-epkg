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

    // Create GzDecoder - this will fail if the file is not a valid gzip
    let tar = GzDecoder::new(tar_gz);

    // Create Archive - this will fail if the file is not a valid tar
    let mut archive = Archive::new(tar);

    // Track extracted files to verify extraction was successful
    let mut extracted_files = Vec::new();
    let mut has_errors = false;

    // Extract all entries
    for entry in archive.entries()? {
        let mut entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                eprintln!("Error reading tar entry: {}", e);
                has_errors = true;
                continue;
            }
        };

        let path = match entry.path() {
            Ok(path) => path,
            Err(e) => {
                eprintln!("Error getting entry path: {}", e);
                has_errors = true;
                continue;
            }
        };

        // Skip pax_global_header file
        if path.file_name().map_or(false, |name| name == "pax_global_header") {
            continue;
        }

        let full_path = dest_dir.join(path);
        extracted_files.push(full_path.clone());

        // Create parent directories if needed
        if let Some(parent) = full_path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                eprintln!("Error creating directory {}: {}", parent.display(), e);
                has_errors = true;
                continue;
            }
        }

        // Extract the file, handling errors more gracefully
        match entry.unpack(&full_path) {
            Ok(_) => {},
            Err(e) => {
                // If error is "file exists", try to remove and retry
                if e.kind() == ErrorKind::AlreadyExists {
                    if full_path.is_file() {
                        if let Err(remove_err) = fs::remove_file(&full_path) {
                            eprintln!("Error removing existing file {}: {}", full_path.display(), remove_err);
                            has_errors = true;
                            continue;
                        }

                        // Retry unpacking after removing the file
                        if let Err(retry_err) = entry.unpack(&full_path) {
                            eprintln!("Error extracting {} after removal: {}", full_path.display(), retry_err);
                            has_errors = true;
                        }
                    } else {
                        eprintln!("Error extracting {}: file exists and is not a regular file", full_path.display());
                        has_errors = true;
                    }
                } else {
                    eprintln!("Error extracting {}: {}", full_path.display(), e);
                    has_errors = true;
                }
            }
        }
    }

    // Verify extraction was successful
    if has_errors {
        return Err(anyhow::anyhow!("Some files failed to extract from {}", tar_path.display()));
    }

    // Verify all extracted files exist and are readable
    for file in extracted_files {
        if !file.exists() {
            return Err(anyhow::anyhow!("File was not extracted: {}", file.display()));
        }
        if let Err(e) = fs::metadata(&file) {
            return Err(anyhow::anyhow!("Cannot access extracted file {}: {}", file.display(), e));
        }
    }

    Ok(())
}
