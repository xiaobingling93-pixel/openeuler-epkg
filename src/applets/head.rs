use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs::File;
use std::io::{self, BufRead};

pub struct HeadOptions {
    pub files: Vec<String>,
    pub lines: Option<usize>,
    pub bytes: Option<usize>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<HeadOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let lines = matches.get_one::<String>("lines")
        .and_then(|s| s.parse().ok());

    let bytes = matches.get_one::<String>("bytes")
        .and_then(|s| s.parse().ok());

    // If neither is specified, default to 10 lines
    let lines = if lines.is_none() && bytes.is_none() {
        Some(10)
    } else {
        lines
    };

    Ok(HeadOptions { files, lines, bytes })
}

pub fn command() -> Command {
    Command::new("head")
        .about("Output the first part of files")
        .arg(Arg::new("lines")
            .short('n')
            .long("lines")
            .help("Print the first N lines (default 10)")
            .value_name("NUM")
            .conflicts_with("bytes"))
        .arg(Arg::new("bytes")
            .short('c')
            .long("bytes")
            .help("Print the first N bytes")
            .value_name("NUM")
            .conflicts_with("lines"))
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files to process (if none, read from stdin)"))
}

fn print_first_lines(reader: &mut dyn BufRead, num_lines: usize) -> Result<()> {
    for (line_num, line_result) in reader.lines().enumerate() {
        if line_num >= num_lines {
            break;
        }

        let line = line_result
            .map_err(|e| eyre!("head: error reading input: {}", e))?;
        println!("{}", line);
    }

    Ok(())
}

fn print_first_bytes(reader: &mut dyn BufRead, num_bytes: usize) -> Result<()> {
    let mut buffer = Vec::new();
    let bytes_read = reader.read_to_end(&mut buffer)
        .map_err(|e| eyre!("head: error reading input: {}", e))?;

    let bytes_to_print = std::cmp::min(num_bytes, bytes_read);
    let output = &buffer[..bytes_to_print];

    // Write directly to stdout to preserve binary data
    use std::io::Write;
    std::io::stdout().write_all(output)
        .map_err(|e| eyre!("head: error writing output: {}", e))?;

    Ok(())
}

pub fn run(options: HeadOptions) -> Result<()> {
    if options.files.is_empty() {
        // Read from stdin
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        if let Some(num_lines) = options.lines {
            print_first_lines(&mut reader, num_lines)?;
        } else if let Some(num_bytes) = options.bytes {
            print_first_bytes(&mut reader, num_bytes)?;
        }
    } else {
        // Process files
        let mut first_file = true;
        for file_path in &options.files {
            if options.files.len() > 1 {
                if !first_file {
                    println!();
                }
                println!("==> {} <==", file_path);
                first_file = false;
            }

            let file = File::open(file_path)
                .map_err(|e| eyre!("head: cannot open '{}': {}", file_path, e))?;
            let mut reader = io::BufReader::new(file);
            if let Some(num_lines) = options.lines {
                print_first_lines(&mut reader, num_lines)?;
            } else if let Some(num_bytes) = options.bytes {
                print_first_bytes(&mut reader, num_bytes)?;
            }
        }
    }

    Ok(())
}