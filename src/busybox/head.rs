use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs::File;
use std::io::{self, BufRead, Read};

// Common functions for head and tail
pub fn process_file_args(files: Vec<String>, allow_plus: bool) -> (Vec<String>, Option<i64>, Option<i64>) {
    let mut actual_files = Vec::new();
    let mut lines_from_files = None;
    let bytes_from_files = None;

    for file in files {
        if file == "-" {
            // Keep as stdin marker
            actual_files.push(file);
            continue;
        }
        // Check for -NUM pattern (traditional syntax)
        if let Some(num_str) = file.strip_prefix('-') {
            if num_str.chars().all(|c| c.is_digit(10)) {
                if let Ok(num) = num_str.parse::<i64>() {
                    if lines_from_files.is_none() && bytes_from_files.is_none() {
                        lines_from_files = Some(-num); // negative for -NUM
                    }
                    continue;
                }
            }
        }
        // Check for +NUM pattern (start from line N)
        if allow_plus {
            if let Some(num_str) = file.strip_prefix('+') {
                if num_str.chars().all(|c| c.is_digit(10)) {
                    if let Ok(num) = num_str.parse::<i64>() {
                        if lines_from_files.is_none() && bytes_from_files.is_none() {
                            lines_from_files = Some(num); // positive for +NUM
                        }
                        continue;
                    }
                }
            }
        }
        // Normal file
        actual_files.push(file);
    }
    (actual_files, lines_from_files, bytes_from_files)
}

pub fn open_file_as_bufread(file_path: &str) -> Result<Box<dyn BufRead>> {
    let reader: Box<dyn BufRead> = if file_path == "-" {
        Box::new(io::stdin().lock())
    } else {
        let file = File::open(file_path)
            .map_err(|e| eyre!("cannot open '{}': {}", file_path, e))?;
        Box::new(io::BufReader::new(file))
    };
    Ok(reader)
}

pub fn open_file_as_read(file_path: &str) -> Result<Box<dyn Read>> {
    let reader: Box<dyn Read> = if file_path == "-" {
        Box::new(io::stdin())
    } else {
        let file = File::open(file_path)
            .map_err(|e| eyre!("cannot open '{}': {}", file_path, e))?;
        Box::new(file)
    };
    Ok(reader)
}

pub fn print_file_header(file_path: &str, first_file: &mut bool) {
    if !*first_file {
        println!();
    }
    println!("==> {} <==", file_path);
    *first_file = false;
}

/// Parse command-line matches into (files, lines, bytes) for head/tail.
///
/// # Parameters
/// - `matches`: Clap argument matches
/// - `allow_plus`: Whether to treat `+NUM` file arguments as start‑from‑line specifiers
/// - `default_lines`: Default line count when neither `-n` nor `-c` is given.
///   Positive means start from that line (tail), negative means last N lines (head/tail).
pub fn parse_head_tail_options(
    matches: &clap::ArgMatches,
    allow_plus: bool,
    default_lines: i64,
) -> Result<(Vec<String>, Option<i64>, Option<i64>)> {
    let files: Vec<String> = matches
        .get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let lines_from_clap = matches
        .get_one::<String>("lines")
        .and_then(|s| s.parse::<i64>().ok());

    let bytes_from_clap = matches
        .get_one::<String>("bytes")
        .and_then(|s| s.parse::<i64>().ok());

    // Process files for special patterns: '-' stdin, '-NUM' lines, '+NUM' lines (if allowed)
    let (actual_files, lines_from_files, bytes_from_files) = process_file_args(files, allow_plus);

    // If no files specified, default to stdin ("-")
    let actual_files = if actual_files.is_empty() {
        vec!["-".to_string()]
    } else {
        actual_files
    };

    // Combine options: command-line flags take precedence over file patterns
    let mut lines = lines_from_clap.or(lines_from_files);
    let bytes = bytes_from_clap.or(bytes_from_files);

    // Default if neither lines nor bytes specified
    if lines.is_none() && bytes.is_none() {
        lines = Some(default_lines);
    }

    Ok((actual_files, lines, bytes))
}

/// Build a clap Command for head or tail.
///
/// # Parameters
/// - `name`: Command name ("head" or "tail")
/// - `about`: Short description (e.g., "Output the first part of files")
/// - `lines_help`: Help text for the `-n` / `--lines` option
/// - `bytes_help`: Help text for the `-c` / `--bytes` option
pub fn head_tail_command(
    name: &'static str,
    about: &'static str,
    lines_help: &'static str,
    bytes_help: &'static str,
) -> Command {
    Command::new(name)
        .about(about)
        .arg(
            Arg::new("lines")
                .short('n')
                .long("lines")
                .help(lines_help)
                .value_name("NUM")
                .allow_negative_numbers(true)
                .conflicts_with("bytes"),
        )
        .arg(
            Arg::new("bytes")
                .short('c')
                .long("bytes")
                .help(bytes_help)
                .value_name("NUM")
                .allow_negative_numbers(true)
                .conflicts_with("lines"),
        )
        .arg(
            Arg::new("files")
                .num_args(0..)
                .help("Files to process (if none, read from stdin)")
                .allow_negative_numbers(true),
        )
}

pub struct HeadOptions {
    pub files: Vec<String>,
    pub lines: Option<usize>,
    pub bytes: Option<usize>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<HeadOptions> {
    let (files, lines_i64, bytes_i64) = parse_head_tail_options(matches, false, -10)?;
    let lines = lines_i64.map(|n| n.unsigned_abs() as usize);
    let bytes = bytes_i64.map(|n| n.unsigned_abs() as usize);
    Ok(HeadOptions { files, lines, bytes })
}

pub fn command() -> Command {
    head_tail_command(
        "head",
        "Output the first part of files",
        "Print the first N lines (default 10)",
        "Print the first N bytes",
    )
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

    let mut first_file = true;
    for file_path in &options.files {
        if options.files.len() > 1 {
            print_file_header(file_path, &mut first_file);
        }

        let mut reader = open_file_as_bufread(file_path)
            .map_err(|e| eyre!("head: {}", e))?;
        if let Some(num_lines) = options.lines {
            print_first_lines(&mut *reader, num_lines)?;
        } else if let Some(num_bytes) = options.bytes {
            print_first_bytes(&mut *reader, num_bytes)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_traditional_minus_num() -> Result<()> {
        let matches = command().get_matches_from(vec!["head", "-1", "file.txt"]);
        let options = parse_options(&matches)?;
        assert_eq!(options.lines, Some(1));
        assert_eq!(options.files, vec!["file.txt"]);
        Ok(())
    }

    #[test]
    fn test_parse_n_joined_negative() -> Result<()> {
        let matches = command().get_matches_from(vec!["head", "-n-5", "file.txt"]);
        let options = parse_options(&matches)?;
        // Negative numbers become absolute value
        assert_eq!(options.lines, Some(5));
        assert_eq!(options.files, vec!["file.txt"]);
        Ok(())
    }

    #[test]
    fn test_parse_n_separate_negative() -> Result<()> {
        let matches = command().get_matches_from(vec!["head", "-n", "-5", "file.txt"]);
        let options = parse_options(&matches)?;
        assert_eq!(options.lines, Some(5));
        assert_eq!(options.files, vec!["file.txt"]);
        Ok(())
    }

    #[test]
    fn test_parse_minus_stdin() -> Result<()> {
        let matches = command().get_matches_from(vec!["head", "-5", "-"]);
        let options = parse_options(&matches)?;
        assert_eq!(options.lines, Some(5));
        assert_eq!(options.files, vec!["-"]);
        Ok(())
    }

    #[test]
    fn test_parse_bytes() -> Result<()> {
        let matches = command().get_matches_from(vec!["head", "-c", "10", "file.txt"]);
        let options = parse_options(&matches)?;
        assert_eq!(options.bytes, Some(10));
        assert_eq!(options.files, vec!["file.txt"]);
        Ok(())
    }
}