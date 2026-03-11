use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

pub struct ZcatOptions {
    pub files: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<ZcatOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(ZcatOptions { files })
}

pub fn command() -> Command {
    Command::new("zcat")
        .about("Expand and concatenate compressed files")
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Compressed files to expand (if none, read from stdin)"))
}

fn decompress_file(file_path: &str) -> Result<()> {
    let path = Path::new(file_path);
    let extension = path.extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");

    let file = File::open(file_path)
        .map_err(|e| eyre!("zcat: {}: {}", file_path, e))?;

    let stdout = io::stdout();
    let mut stdout_handle = stdout.lock();

    match extension {
        "gz" | "gzip" => {
            let mut decoder = flate2::read::GzDecoder::new(file);
            io::copy(&mut decoder, &mut stdout_handle)
                .map_err(|e| eyre!("zcat: error decompressing {}: {}", file_path, e))?;
        }
        "bz2" | "bzip2" => {
            let mut decoder = bzip2::read::BzDecoder::new(file);
            io::copy(&mut decoder, &mut stdout_handle)
                .map_err(|e| eyre!("zcat: error decompressing {}: {}", file_path, e))?;
        }
        "xz" => {
            let mut decoder = liblzma::read::XzDecoder::new(file);
            io::copy(&mut decoder, &mut stdout_handle)
                .map_err(|e| eyre!("zcat: error decompressing {}: {}", file_path, e))?;
        }
        _ => {
            // Try gzip first, then fallback to just copying
            let file_clone = File::open(file_path)
                .map_err(|e| eyre!("zcat: {}: {}", file_path, e))?;

            let mut decoder = flate2::read::GzDecoder::new(file_clone);
            if io::copy(&mut decoder, &mut stdout_handle).is_ok() {
                return Ok(());
            }

            // If gzip failed, just copy the file as-is
            let mut file_copy = File::open(file_path)
                .map_err(|e| eyre!("zcat: {}: {}", file_path, e))?;
            io::copy(&mut file_copy, &mut stdout_handle)
                .map_err(|e| eyre!("zcat: error reading {}: {}", file_path, e))?;
        }
    }

    Ok(())
}

pub fn run(options: ZcatOptions) -> Result<()> {
    if options.files.is_empty() {
        // Read from stdin - assume gzip compressed
        let stdin = io::stdin();
        let mut decoder = flate2::read::GzDecoder::new(stdin);
        let stdout = io::stdout();
        let mut stdout_handle = stdout.lock();

        io::copy(&mut decoder as &mut dyn Read, &mut stdout_handle)
            .map_err(|e| eyre!("zcat: error decompressing stdin: {}", e))?;
    } else {
        // Process files
        for file_path in &options.files {
            decompress_file(file_path)?;
        }
    }

    Ok(())
}