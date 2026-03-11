use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use sha2::{Sha256, Digest};
use std::fs::File;
use std::io::{self, BufRead, Read};

pub struct Sha256sumOptions {
    pub files: Vec<String>,
    pub check: bool,
    pub binary: bool,
    pub zero: bool,
    pub tag: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<Sha256sumOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let check = matches.get_flag("check");
    let binary = matches.get_flag("binary");
    let zero = matches.get_flag("zero");
    let tag = matches.get_flag("tag");

    Ok(Sha256sumOptions {
        files,
        check,
        binary,
        zero,
        tag,
    })
}

pub fn command() -> Command {
    Command::new("sha256sum")
        .about("Compute and check SHA256 message digest")
        .arg(Arg::new("check")
            .short('c')
            .long("check")
            .help("Read SHA256 sums from the FILEs and check them")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("binary")
            .short('b')
            .long("binary")
            .help("Read in binary mode")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("text")
            .short('t')
            .long("text")
            .help("Read in text mode (default)")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("zero")
            .short('z')
            .long("zero")
            .help("End each output line with NUL, not newline")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("tag")
            .long("tag")
            .help("Create a BSD-style checksum")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files to process (if none, read from stdin)"))
}

fn compute_sha256(file_path: Option<&str>) -> Result<String> {
    let mut hasher = Sha256::new();

    if let Some(path) = file_path {
        let mut file = File::open(path)
            .map_err(|e| eyre!("sha256sum: {}: {}", path, e))?;

        let mut buffer = [0; 8192];
        loop {
            let bytes_read = file.read(&mut buffer)
                .map_err(|e| eyre!("sha256sum: error reading {}: {}", path, e))?;

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
                .map_err(|e| eyre!("sha256sum: error reading stdin: {}", e))?;

            if bytes_read == 0 {
                break;
            }

            hasher.update(&buffer[..bytes_read]);
        }
    }

    let result = hasher.finalize();
    Ok(format!("{:x}", result))
}

fn format_output(sha256: &str, file_path: Option<&str>, options: &Sha256sumOptions) {
    let delimiter = if options.zero { "\0" } else { "\n" };

    if options.tag {
        let name = file_path.unwrap_or("stdin");
        print!("SHA256 ({}) = {}", name, sha256);
    } else {
        let mode_char = if options.binary { "*" } else { " " };
        if let Some(path) = file_path {
            print!("{}{}  {}", sha256, mode_char, path);
        } else {
            print!("{}", sha256);
        }
    }
    print!("{}", delimiter);
}

fn check_checksums(file_path: &str) -> Result<()> {
    let file = File::open(file_path)
        .map_err(|e| eyre!("sha256sum: {}: {}", file_path, e))?;
    let reader = io::BufReader::new(file);

    for line_result in reader.lines() {
        let line = line_result
            .map_err(|e| eyre!("sha256sum: error reading {}: {}", file_path, e))?;

        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        if parts.len() != 2 {
            eprintln!("sha256sum: {}: improperly formatted SHA256 checksum line", file_path);
            continue;
        }

        let expected_sha256 = parts[0];
        let filename = parts[1];

        match compute_sha256(Some(filename)) {
            Ok(actual_sha256) => {
                if actual_sha256 == expected_sha256 {
                    println!("{}: OK", filename);
                } else {
                    println!("{}: FAILED", filename);
                }
            }
            Err(e) => {
                eprintln!("sha256sum: {}", e);
            }
        }
    }
    Ok(())
}

fn compute_checksums(files: &[String], options: &Sha256sumOptions) -> Result<()> {
    if files.is_empty() {
        // Read from stdin
        let sha256 = compute_sha256(None)?;
        format_output(&sha256, None, options);
    } else {
        // Compute for each file
        for file_path in files {
            let sha256 = compute_sha256(Some(file_path))?;
            format_output(&sha256, Some(file_path), options);
        }
    }
    Ok(())
}

pub fn run(options: Sha256sumOptions) -> Result<()> {
    if options.check {
        // Check mode - read checksums from files and verify
        for file_path in &options.files {
            check_checksums(file_path)?;
        }
    } else {
        // Compute mode
        compute_checksums(&options.files, &options)?;
    }

    Ok(())
}
