use std::fs::File;
use std::io::{self, BufReader};
use tar::Archive;
use zstd::stream::read::Decoder;
use anyhow::{Context, Result};
use crate::models::*;

fn untar_zst(file_path: &str, output_dir: &str) -> io::Result<()> {
    // Open the compressed file
    let file = File::open(file_path)?;
    let buffered_reader = BufReader::new(file);

    // Create a Zstandard decoder
    let zstd_decoder = Decoder::new(buffered_reader)?;

    // Create a tar archive from the Zstandard decoder
    let mut archive = Archive::new(zstd_decoder);

    // Unpack the archive into the output directory
    archive.unpack(output_dir)?;

    Ok(())
}
