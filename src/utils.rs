use std::fs;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use anyhow::Result;

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

        if file_type.is_file() || file_type.is_symlink() {
            paths.push(path.clone());
        } else if file_type.is_dir() {
            paths.push(path.clone());
            paths.extend(list_package_files(path.to_str().unwrap())?);
        }
    }

    // Remove duplicates
    paths.sort();
    paths.dedup();

    Ok(paths)
}

// Get file type
pub fn get_file_type(file: &Path) -> Result<FileType> {
    const ELF_MAGIC: &[u8] = &[0x7f, b'E', b'L', b'F'];

    // Check Symbolic link first
    if fs::symlink_metadata(&file).map(|metadata| metadata.file_type().is_symlink()).unwrap() {
        return Ok(FileType::Symlink);
    }

    // Read file contents for other checks
    let mut buffer = Vec::new();
    let mut f = fs::File::open(file)?;
    f.read_to_end(&mut buffer)?;

    // Check ELF 64-bit LSB
    if buffer.starts_with(ELF_MAGIC) {
        return Ok(FileType::Elf);
    }

    // Check if file starts with shebang
    if buffer.starts_with(b"#!") {
        let first_line = String::from_utf8_lossy(&buffer[..buffer.iter().position(|&x| x == b'\n').unwrap_or(buffer.len())]);

        // Check for various script types
        if first_line.contains("sh") || first_line.contains("bash") {
            return Ok(FileType::ShellScript);
        } else if first_line.contains("perl") {
            return Ok(FileType::PerlScript);
        } else if first_line.contains("python") {
            return Ok(FileType::PythonScript);
        } else if first_line.contains("ruby") {
            return Ok(FileType::RubyScript);
        } else if first_line.contains("node") || first_line.contains("nodejs") {
            return Ok(FileType::NodeScript);
        } else if first_line.contains("lua") {
            return Ok(FileType::LuaScript);
        }
    }

    // Try to detect if it's ASCII text
    if buffer.iter().all(|&b| b.is_ascii()) {
        return Ok(FileType::AsciiText);
    }

    // If nothing matches, return binary data
    Ok(FileType::Binary)
}
