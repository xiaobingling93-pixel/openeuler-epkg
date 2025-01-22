use std::fs;
use std::io;
use dirs::home_dir;
use tar::Archive;
use zstd::stream::read::Decoder;
use anyhow::Result;

pub fn unpack_packages(files: Vec<String>) -> Result<()> {

    // Get the home directory
    let home = home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;

    for file in files {
        let pkgline = file.split('/').last().expect(&format!("invalid package file name {}", file)).strip_suffix(".epkg").unwrap();
        let dir = home
            .join(".epkg")
            .join("store")
            .join(pkgline);
        let dir_str = dir.to_string_lossy().to_owned(); // Convert to String

        println!("untar {} {}", file, dir_str);
        untar_zst(&file, &dir_str)?;

        let hash = crate::hash::epkg_store_hash(&dir_str)?;
        if hash != pkgline[..32] {
            eprintln!("Hash mismatch, expect {} for {}", hash, dir_str);
        }
    }
    Ok(())
}

pub fn garbage_collect() -> Result<()> {
    // Actual garbage collection implementation would go here
    println!("Performing garbage collection");
    Ok(())
}

fn untar_zst(file_path: &str, output_dir: &str) -> io::Result<()> {
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
