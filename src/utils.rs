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

    for entry in fs::read_dir(dir)? {
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

    // Check ASCII text executable || Perl script text executable
    let mime_type = tree_magic::from_u8(&buffer);
    match mime_type.as_str() {
        "application/x-executable" => Ok("ASCII text executable".to_string()),
        "text/x-perl" => Ok("Perl script text executable".to_string()),
        _ => Ok("Unknown file type".to_string()),
    }
}

// Copy directory (cp -R)
pub fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> std::io::Result<()> {
    fs::create_dir_all(&dst).unwrap();

    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let ty = entry.file_type().unwrap();
        if ty.is_dir() {
            copy_dir_all(entry.path(), dst.as_ref().join(entry.file_name())).unwrap();
        } else if ty.is_file() {
            fs::copy(entry.path(), dst.as_ref().join(entry.file_name())).unwrap();
        }
    }
    Ok(())
}

