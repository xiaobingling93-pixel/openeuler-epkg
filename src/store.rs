use std::fs;
use std::io::{self, BufReader, BufWriter};
use std::path::Path;
use std::os::unix::fs::PermissionsExt;
use tar::Archive;
use nix::unistd::{chown, User};
use zstd::stream::Decoder;
use users::get_effective_uid;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use walkdir::WalkDir;
use crate::models::dirs;

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
        set_perm_and_owner(&dir_str)
            .wrap_err_with(|| format!("Failed to set permissions for package: {}", file))?;
        // let hash = crate::hash::epkg_store_hash(&dir_str)?;
        // if hash != pkgline[..32] {
        //     eprintln!("Hash mismatch, expect {} for {}", hash, dir_str);
        // }
    }
    Ok(())
}

pub fn untar_zst(file_path: &str, output_dir: &str, package_flag: bool) -> Result<()> {
    if package_flag && Path::new(output_dir).exists() {
        return Ok(());
    }

    // Open the compressed file
    let file = fs::File::open(file_path)
        .wrap_err_with(|| format!("Failed to open compressed file: {}", file_path))?;
    let buffered_reader = io::BufReader::new(file);

    // Create a Zstandard decoder
    let zstd_decoder = Decoder::new(buffered_reader)
        .wrap_err_with(|| format!("Failed to create Zstandard decoder for file: {}", file_path))?;

    // Create a tar archive from the Zstandard decoder
    let mut archive = Archive::new(zstd_decoder);

    // Unpack the archive into the output directory
    archive.unpack(output_dir)
        .wrap_err_with(|| format!("Failed to unpack archive to directory: {}", output_dir))?;

    Ok(())
}

#[allow(dead_code)]
pub fn unzst(input_path: &str, output_path: &str) -> Result<()> {
    let input_file = fs::File::open(input_path)
        .wrap_err_with(|| format!("Failed to open input file: {}", input_path))?;
    let reader = BufReader::new(input_file);

    let parent_dir = Path::new(output_path).parent()
        .ok_or_else(|| eyre::eyre!("Cannot determine parent directory for: {}", output_path))?;
    fs::create_dir_all(parent_dir)
        .wrap_err_with(|| format!("Failed to create directory: {}", parent_dir.display()))?;

    let output_file = fs::File::create(output_path)
        .wrap_err_with(|| format!("Failed to create output file: {}", output_path))?;
    let mut writer = BufWriter::new(output_file);

    let mut decoder = Decoder::new(reader)
        .wrap_err_with(|| format!("Failed to create Zstandard decoder for file: {}", input_path))?;
    io::copy(&mut decoder, &mut writer)
        .wrap_err_with(|| format!("Failed to decompress {} to {}", input_path, output_path))?;

    Ok(())
}

pub fn set_perm_and_owner(dir_str: &str) -> Result<()> {
    // get uid | gid
    let current_uid = get_effective_uid();
    let user_account = User::from_uid(current_uid.into())
        .wrap_err("Failed to get user information from UID")?;
    let user_account = user_account
        .ok_or_else(|| eyre::eyre!("Current user not found"))?;
    let uid = Some(user_account.uid);
    let gid = Some(user_account.gid);

    // chmod 755, chown USER:USER
    for entry_result in WalkDir::new(dir_str) {
        let entry = entry_result
            .wrap_err_with(|| format!("Failed to access entry in directory: {}", dir_str))?;
        let path = entry.path();

        if !path.exists() || path.is_symlink() {
            continue;
        }

        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .wrap_err_with(|| format!("Failed to set permissions on: {}", path.display()))?;

        chown(path, uid, gid)
            .wrap_err_with(|| format!("Failed to change ownership of: {}", path.display()))?;
    }
    Ok(())
}
