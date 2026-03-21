use std::io::Write;
use std::path::Path;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use crate::models::dirs;
use crate::store::untar_zst;
use crate::lfs;
use std::fs::{File, OpenOptions};
use tar::Builder;
use zstd::stream::write::Encoder;
use walkdir::WalkDir;
use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};


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
#[allow(dead_code)]
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

pub fn compress_packages(store_dir: &std::path::PathBuf, out_dir: &str, origin_url: &str) -> Result<()> {
    let package_txt_path = crate::dirs::path_join(store_dir, &["info", "package.txt"]);
    let pkgline = store_dir.file_name().and_then(|os_str| os_str.to_str()).unwrap_or("unknown");
    let output_file = Path::new(out_dir).join(&format!("{}.epkg", pkgline));
    append_to_file(&package_txt_path, &format!("originUrl: {}", origin_url))?;
    compress_folder_to_epkg(store_dir, &output_file.to_string_lossy())?;
    println!("{}", output_file.display());
    Ok(())
}

// Requirements:
// 1) preserve dead symlink
// 2) preserve reproducibility
// - ordered file list
// - default zstd compression level
// - 0 uid/gid (special file's ownership shall be specified in some config file)
// - 0 timestamp
pub fn compress_folder_to_epkg(
    source_dir: &Path,
    output_file: &str,
) -> Result<()> {
    let output = File::create(output_file)?;
    let encoder = Encoder::new(output, 3)?;
    let mut tar_builder = Builder::new(encoder.auto_finish());

    // Manual walk to preserve dead symlinks - collect and sort for reproducibility
    let mut entries: Vec<_> = WalkDir::new(source_dir)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;

    // Sort entries by path for reproducible output
    entries.sort_by(|a, b| a.path().cmp(b.path()));

    for entry in entries {
        let path = entry.path();
        let rel_path = path.strip_prefix(source_dir)?;

        // Skip the root directory itself (empty path)
        if rel_path.as_os_str().is_empty() {
            continue;
        }

        if entry.file_type().is_symlink() {
            // Get symlink target (even if dead)
            let target = fs::read_link(path)?;
            // Forcefully add the symlink to the tar, even if target doesn't exist
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o755);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            tar_builder.append_link(&mut header, rel_path, &target)?;
        } else if entry.file_type().is_file() {
            let metadata = lfs::metadata_on_host(path)?;
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(metadata.len());
            header.set_mode(metadata.permissions().mode());
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            tar_builder.append_data(&mut header, rel_path, &mut File::open(path)?)?;
        } else if entry.file_type().is_dir() {
            let metadata = lfs::metadata_on_host(path)?;
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(metadata.permissions().mode());
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            tar_builder.append_data(&mut header, rel_path, std::io::empty())?;
        } else {
            // Handle special file types (character devices, block devices, fifos, sockets)
            let metadata = lfs::metadata_on_host(path)?;
            let mut header = tar::Header::new_gnu();

            if metadata.file_type().is_char_device() {
                header.set_entry_type(tar::EntryType::Char);
                header.set_device_major((metadata.rdev() >> 8) as u32)?;
                header.set_device_minor((metadata.rdev() & 0xff) as u32)?;
            } else if metadata.file_type().is_block_device() {
                header.set_entry_type(tar::EntryType::Block);
                header.set_device_major((metadata.rdev() >> 8) as u32)?;
                header.set_device_minor((metadata.rdev() & 0xff) as u32)?;
            } else if metadata.file_type().is_fifo() {
                header.set_entry_type(tar::EntryType::Fifo);
            } else if metadata.file_type().is_socket() {
                // Sockets cannot be archived, skip them
                continue;
            } else {
                // Unknown file type, skip
                continue;
            }

            header.set_size(0);
            header.set_mode(metadata.permissions().mode());
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);

            tar_builder.append_data(&mut header, rel_path, std::io::empty())?;
        }
    }

    // 完成 tar 构建
    tar_builder.finish()?;

    Ok(())
}

pub fn append_to_file(file_path: &Path, content: &str) -> Result<()> {
    // 以追加模式打开文件（如果不存在则创建）
    let mut file = OpenOptions::new()
        .append(true)   // 追加模式
        .create(true)    // 如果文件不存在则创建
        .open(file_path)?;

    // 写入内容（添加换行符）
    writeln!(file, "{}", content)?;

    Ok(())
}
