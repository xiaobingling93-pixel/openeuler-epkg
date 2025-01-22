use std::fs;
use std::io;
use tar::Archive;
use zstd::stream::read::Decoder;
use anyhow::{Context, Result};
use crate::models::*;

pub fn unpack_package_files_to_store(files: Vec<String>) -> io::Result<()> {
    // Actual unpacking implementation would go here
    for file in files {
        println!("Unpacking {} to /opt/epkg/store/", file);
    }
    Ok(())
}

pub fn garbage_collect() -> io::Result<()> {
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
