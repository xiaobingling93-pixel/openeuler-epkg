use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::path::PathBuf;
use anyhow::Result;
use sha2::{Sha256, Digest};
use std::fs::File;

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
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}
