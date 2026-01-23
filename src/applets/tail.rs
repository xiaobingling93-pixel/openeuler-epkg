use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs::File;
use std::io::{self, BufRead, Read};

pub struct TailOptions {
    pub files: Vec<String>,
    pub lines: Option<usize>,
    pub bytes: Option<i64>, // Can be negative for "last N bytes" or positive for "from byte N"
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<TailOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let lines = matches.get_one::<String>("lines")
        .and_then(|s| s.parse().ok());

    let bytes_str = matches.get_one::<String>("bytes");

    let bytes = if let Some(bytes_str) = bytes_str {
        if bytes_str.starts_with('+') {
            // +N means start from byte N (1-indexed)
            bytes_str[1..].parse::<i64>().ok().map(|n| n)
        } else {
            // N means last N bytes (negative to indicate this)
            bytes_str.parse::<i64>().ok().map(|n| -n)
        }
    } else {
        None
    };

    // If neither is specified, default to 10 lines
    let lines = if lines.is_none() && bytes.is_none() {
        Some(10)
    } else {
        lines
    };

    Ok(TailOptions { files, lines, bytes })
}

pub fn command() -> Command {
    Command::new("tail")
        .about("Output the last part of files")
        .arg(Arg::new("lines")
            .short('n')
            .long("lines")
            .help("Print the last N lines (default 10)")
            .value_name("NUM")
            .conflicts_with("bytes"))
        .arg(Arg::new("bytes")
            .short('c')
            .long("bytes")
            .help("Print the last N bytes, or +N to start from byte N")
            .value_name("NUM")
            .conflicts_with("lines"))
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files to process (if none, read from stdin)"))
}

fn print_last_lines(reader: &mut dyn BufRead, num_lines: usize) -> Result<()> {
    let mut lines = Vec::new();

    // Read all lines
    for line_result in reader.lines() {
        let line = line_result
            .map_err(|e| eyre!("tail: error reading input: {}", e))?;
        lines.push(line);
    }

    // Print the last num_lines lines
    let start_idx = if lines.len() > num_lines {
        lines.len() - num_lines
    } else {
        0
    };

    for line in &lines[start_idx..] {
        println!("{}", line);
    }

    Ok(())
}

fn print_last_bytes(reader: &mut dyn Read, num_bytes: usize) -> Result<()> {
    let mut buffer = Vec::new();
    reader.read_to_end(&mut buffer)
        .map_err(|e| eyre!("tail: error reading input: {}", e))?;

    let bytes_to_print = std::cmp::min(num_bytes, buffer.len());
    let start_idx = buffer.len().saturating_sub(bytes_to_print);
    let output = &buffer[start_idx..];

    // Write directly to stdout to preserve binary data
    use std::io::Write;
    std::io::stdout().write_all(output)
        .map_err(|e| eyre!("tail: error writing output: {}", e))?;

    Ok(())
}

fn print_from_byte(reader: &mut dyn Read, start_byte: usize) -> Result<()> {
    let mut buffer = Vec::new();
    reader.read_to_end(&mut buffer)
        .map_err(|e| eyre!("tail: error reading input: {}", e))?;

    // start_byte is 1-indexed, convert to 0-indexed
    let start_idx = if start_byte == 0 { 0 } else { start_byte - 1 };
    let output = if start_idx < buffer.len() {
        &buffer[start_idx..]
    } else {
        &[]
    };

    // Write directly to stdout to preserve binary data
    use std::io::Write;
    std::io::stdout().write_all(output)
        .map_err(|e| eyre!("tail: error writing output: {}", e))?;

    Ok(())
}

pub fn run(options: TailOptions) -> Result<()> {
    if options.files.is_empty() {
        // Read from stdin
        let stdin = io::stdin();
        if let Some(num_lines) = options.lines {
            let mut reader = stdin.lock();
            print_last_lines(&mut reader, num_lines)?;
        } else if let Some(bytes_val) = options.bytes {
            if bytes_val > 0 {
                // +N: start from byte N
                let mut reader = stdin;
                print_from_byte(&mut reader, bytes_val as usize)?;
            } else {
                // -N: last N bytes
                let mut reader = stdin;
                print_last_bytes(&mut reader, (-bytes_val) as usize)?;
            }
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
                .map_err(|e| eyre!("tail: cannot open '{}': {}", file_path, e))?;

            if let Some(num_lines) = options.lines {
                let mut reader = io::BufReader::new(&file);
                print_last_lines(&mut reader, num_lines)?;
            } else if let Some(bytes_val) = options.bytes {
                if bytes_val > 0 {
                    // +N: start from byte N
                    let mut reader = &file;
                    print_from_byte(&mut reader, bytes_val as usize)?;
                } else {
                    // -N: last N bytes
                    let mut reader = &file;
                    print_last_bytes(&mut reader, (-bytes_val) as usize)?;
                }
            }
        }
    }

    Ok(())
}