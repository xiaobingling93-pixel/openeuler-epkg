use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs::File;
use std::io::{self, Read};

pub struct CksumOptions {
    pub files: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<CksumOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(CksumOptions { files })
}

pub fn command() -> Command {
    Command::new("cksum")
        .about("Print CRC checksum and byte counts")
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files to process (if none, read from stdin)"))
}

// CRC-32 algorithm (IEEE 802.3)
fn crc32(data: &[u8]) -> u32 {
    const CRC32_TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut crc = i as u32;
            let mut j = 0;
            while j < 8 {
                if crc & 1 != 0 {
                    crc = (crc >> 1) ^ 0xEDB88320;
                } else {
                    crc >>= 1;
                }
                j += 1;
            }
            table[i] = crc;
            i += 1;
        }
        table
    };

    let mut crc = 0xFFFFFFFFu32;
    for &byte in data {
        crc = CRC32_TABLE[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFFFFFF
}

fn compute_cksum(file_path: Option<&str>) -> Result<(u32, u64)> {
    let mut data = Vec::new();

    if let Some(path) = file_path {
        let mut file = File::open(path)
            .map_err(|e| eyre!("cksum: {}: {}", path, e))?;
        file.read_to_end(&mut data)
            .map_err(|e| eyre!("cksum: error reading {}: {}", path, e))?;
    } else {
        io::stdin().read_to_end(&mut data)
            .map_err(|e| eyre!("cksum: error reading stdin: {}", e))?;
    }

    let crc = crc32(&data);
    let size = data.len() as u64;

    Ok((crc, size))
}

pub fn run(options: CksumOptions) -> Result<()> {
    if options.files.is_empty() {
        // Read from stdin
        let (crc, size) = compute_cksum(None)?;
        println!("{} {}", crc, size);
    } else {
        // Compute for each file
        for file_path in &options.files {
            let (crc, size) = compute_cksum(Some(file_path))?;
            println!("{} {} {}", crc, size, file_path);
        }
    }

    Ok(())
}
