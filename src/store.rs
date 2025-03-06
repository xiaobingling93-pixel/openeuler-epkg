use std::fs;
use std::io;
use std::path::Path;
use std::os::unix::fs::PermissionsExt;
use tar::Archive;
use nix::unistd::{chown, User};
use zstd::stream::read::Decoder;
use users::get_effective_uid;
use anyhow::Result;
use walkdir::WalkDir;
use crate::paths;

pub fn unpack_packages(files: Vec<String>) -> Result<()> {
    for file in files {
        let pkgline = file.split('/').last().expect(&format!("invalid package file name {}", file)).strip_suffix(".epkg").unwrap();
        let dir = paths::instance.epkg_store_root.join(pkgline);
        let dir_str = dir.to_string_lossy().to_owned(); // Convert to String

        // println!("untar {} {}", file, dir_str);
        untar_zst(&file, &dir_str)?;

        set_dir_permissions_and_ownership(&dir_str).unwrap();
        // let hash = crate::hash::epkg_store_hash(&dir_str)?;
        // if hash != pkgline[..32] {
        //     eprintln!("Hash mismatch, expect {} for {}", hash, dir_str);
        // }
    }
    Ok(())
}

pub fn garbage_collect() -> Result<()> {
    // Actual garbage collection implementation would go here
    println!("Performing garbage collection");
    Ok(())
}

fn untar_zst(file_path: &str, output_dir: &str) -> io::Result<()> {
    if Path::new(output_dir).exists() {
        return Ok(());
    }

    // Open the compressed file
    let file = fs::File::open(file_path)?;
    let buffered_reader = io::BufReader::new(file);

    // Create a Zstandard decoder
    let zstd_decoder = Decoder::new(buffered_reader)?;

    // Create a tar archive from the Zstandard decoder
    let mut archive = Archive::new(zstd_decoder);

    // Unpack the archive into the output directory
    archive.unpack(output_dir)?;

    Ok(())
}

fn set_dir_permissions_and_ownership(dir_str: &str) -> Result<()> {
    // get uid | gid
    let current_uid = get_effective_uid();
    let user_account = User::from_uid(current_uid.into())?.ok_or(anyhow::anyhow!("当前用户未找到"))?;
    let uid = Some(user_account.uid);
    let gid = Some(user_account.gid);

    // chmod 755, chown USER:USER
    for entry in WalkDir::new(dir_str).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.exists() {
            continue;
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        chown(path, uid, gid).unwrap();
    }
    Ok(())
}
