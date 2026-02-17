use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use md5::{Md5, Digest};
use std::fs::File;
use std::io::{self, BufRead, Read};

pub struct Md5sumOptions {
    pub files: Vec<String>,
    pub check: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<Md5sumOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let check = matches.get_flag("check");

    Ok(Md5sumOptions { files, check })
}

pub fn command() -> Command {
    Command::new("md5sum")
        .about("Compute and check MD5 message digest")
        .arg(Arg::new("check")
            .short('c')
            .long("check")
            .help("Read MD5 sums from the FILEs and check them")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files to process (if none, read from stdin)"))
}

fn compute_md5(file_path: Option<&str>) -> Result<String> {
    let mut hasher = Md5::new();

    if let Some(path) = file_path {
        let mut file = File::open(path)
            .map_err(|e| eyre!("md5sum: {}: {}", path, e))?;

        let mut buffer = [0; 8192];
        loop {
            let bytes_read = file.read(&mut buffer)
                .map_err(|e| eyre!("md5sum: error reading {}: {}", path, e))?;

            if bytes_read == 0 {
                break;
            }

            hasher.update(&buffer[..bytes_read]);
        }
    } else {
        // Read from stdin
        let mut buffer = [0; 8192];
        loop {
            let bytes_read = io::stdin().read(&mut buffer)
                .map_err(|e| eyre!("md5sum: error reading stdin: {}", e))?;

            if bytes_read == 0 {
                break;
            }

            hasher.update(&buffer[..bytes_read]);
        }
    }

    let result = hasher.finalize();
    Ok(format!("{:x}", result))
}

pub fn run(options: Md5sumOptions) -> Result<()> {
    if options.check {
        // Check mode - read checksums from files and verify
        for file_path in &options.files {
            let file = File::open(file_path)
                .map_err(|e| eyre!("md5sum: {}: {}", file_path, e))?;
            let reader = io::BufReader::new(file);

            for line_result in reader.lines() {
                let line = line_result
                    .map_err(|e| eyre!("md5sum: error reading {}: {}", file_path, e))?;

                let parts: Vec<&str> = line.splitn(2, ' ').collect();
                if parts.len() != 2 {
                    eprintln!("md5sum: {}: improperly formatted MD5 checksum line", file_path);
                    continue;
                }

                let expected_md5 = parts[0];
                let filename = parts[1];

                match compute_md5(Some(filename)) {
                    Ok(actual_md5) => {
                        if actual_md5 == expected_md5 {
                            println!("{}: OK", filename);
                        } else {
                            println!("{}: FAILED", filename);
                        }
                    }
                    Err(e) => {
                        eprintln!("md5sum: {}", e);
                    }
                }
            }
        }
    } else {
        // Compute mode
        if options.files.is_empty() {
            // Read from stdin
            let md5 = compute_md5(None)?;
            println!("{}", md5);
        } else {
            // Compute for each file
            for file_path in &options.files {
                let md5 = compute_md5(Some(file_path))?;
                println!("{}  {}", md5, file_path);
            }
        }
    }

    Ok(())
}