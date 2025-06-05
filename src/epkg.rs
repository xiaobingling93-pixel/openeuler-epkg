use std::path::Path;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use crate::models::dirs;
use crate::store::untar_zst;

/// Legacy function for unpacking .epkg files (original implementation)
/// This function is kept for backward compatibility with existing .epkg packages
pub fn unpack_package<P: AsRef<Path>>(epkg_file: P, store_tmp_dir: P) -> Result<()> {
    let epkg_file = epkg_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    untar_zst(
        epkg_file.to_str().unwrap(),
        store_tmp_dir.to_str().unwrap(),
        false
    )
}

/// Legacy function for unpacking multiple .epkg files
/// This maintains the original behavior for .epkg packages
pub fn unpack_packages(files: Vec<String>) -> Result<()> {
    for file in files {
        let filename = file.split('/').last()
            .ok_or_else(|| eyre::eyre!("Invalid package file name: {}", file))?;

        let pkgline = filename.strip_suffix(".epkg")
            .ok_or_else(|| eyre::eyre!("File does not have .epkg extension: {}", file))?;

        let dir = dirs().epkg_store.join(pkgline);
        let dir_str = dir.to_string_lossy().to_owned(); // Convert to String

        untar_zst(&file, &dir_str, true)
            .wrap_err_with(|| format!("Failed to unpack package: {}", file))?;
        // let hash = crate::hash::epkg_store_hash(&dir_str)?;
        // if hash != pkgline[..32] {
        //     eprintln!("Hash mismatch, expect {} for {}", hash, dir_str);
        // }
    }
    Ok(())
}
