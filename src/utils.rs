use std::fs;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use anyhow::Result;

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
pub fn get_file_type(file: &Path) -> Result<String> {
    const ELF_MAGIC: &[u8] = &[0x7f, b'E', b'L', b'F'];
    // Check Symbolic link
    if fs::symlink_metadata(&file).map(|metadata| metadata.file_type().is_symlink()).unwrap() {
        return Ok("symbolic link".to_string());
    }

    // Check ELF 64-bit LSB 
    let mut buffer = Vec::new();
    let mut f = fs::File::open(file)?;
    f.read_to_end(&mut buffer)?;
    if buffer.starts_with(ELF_MAGIC) {
        return Ok("ELF 64-bit LSB".to_string());
    }

    // Check if file starts with shebang
    if buffer.starts_with(b"#!") {
        let first_line = String::from_utf8_lossy(&buffer[..buffer.iter().position(|&x| x == b'\n').unwrap_or(buffer.len())]);
        
        if first_line.contains("/bin/bash") || first_line.contains("/bin/sh") {
            return Ok("Bourne-Again shell script, ASCII text executable".to_string());
        } else if first_line.contains("perl") {
            return Ok("Perl script text executable".to_string());
        } else if first_line.contains("python") {
            return Ok("Python script, ASCII text executable".to_string());
        }
    }
    
    // Try to detect if it's ASCII text
    if buffer.iter().all(|&b| b.is_ascii()) {
        return Ok("ASCII text".to_string());
    }
    
    // If nothing matches, return binary data
    Ok("data".to_string())
}
