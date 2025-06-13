use std::io::Write;
use std::env;
use std::path::Path;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use crate::models::dirs;
use crate::store::untar_zst;
use std::fs::{File, OpenOptions};
use tar::Builder;
use zstd::stream::write::Encoder;

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
    let package_txt_path = store_dir.join("info/package.txt");
    let pkgline = store_dir.file_name().and_then(|os_str| os_str.to_str()).unwrap_or("unknown");
    let output_file = Path::new(out_dir).join(&format!("{}.epkg", pkgline));
    append_to_file(&package_txt_path, &format!("originUrl: {}", origin_url))?;
    compress_folder_to_epkg(store_dir, &output_file.to_string_lossy())?;
    println!("{}", output_file.display());
    Ok(())
}

pub fn compress_folder_to_epkg(
    source_dir: &Path,
    output_file: &str,
) -> Result<()> {
    // 创建输出文件
    let output = File::create(output_file)?;

    // 创建 zstd 编码器
    let encoder = Encoder::new(output, 3)?;

    // 创建 tar 构建器
    let mut tar_builder = Builder::new(encoder.auto_finish());

    // 添加目录到 tar
    tar_builder.append_dir_all(".", Path::new(source_dir))?;

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
