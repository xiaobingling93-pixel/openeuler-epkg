use clap::Command;
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::io::{BufRead, Read};
use crate::applets::head::{open_file_as_bufread, open_file_as_read, print_file_header, parse_head_tail_options, head_tail_command};

pub struct TailOptions {
    pub files: Vec<String>,
    pub lines: Option<i64>, // Can be negative for 'last N lines' or positive for 'from line N'
    pub bytes: Option<i64>, // Can be negative for "last N bytes" or positive for "from byte N"
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<TailOptions> {
    let (files, lines, bytes) = parse_head_tail_options(matches, true, -10)?;
    Ok(TailOptions { files, lines, bytes })
}


pub fn command() -> Command {
    head_tail_command(
        "tail",
        "Output the last part of files",
        "Number of lines (positive for start from line, negative for last N lines)",
        "Number of bytes (positive for start from byte, negative for last N bytes)",
    )
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
fn print_from_line(reader: &mut dyn BufRead, start_line: usize) -> Result<()> {
    let mut lines = Vec::new();
    for line_result in reader.lines() {
        let line = line_result
            .map_err(|e| eyre!("tail: error reading input: {}", e))?;
        lines.push(line);
    }
    // start_line is 1-indexed, convert to 0-indexed
    let start_idx = if start_line == 0 { 0 } else { start_line - 1 };
    if start_idx < lines.len() {
        for line in &lines[start_idx..] {
            println!("{}", line);
        }
    }
    Ok(())
}

pub fn run(options: TailOptions) -> Result<()> {

    let mut first_file = true;
    for file_path in &options.files {
        if options.files.len() > 1 {
            print_file_header(file_path, &mut first_file);
        }

        if let Some(lines_val) = options.lines {
            let mut reader = open_file_as_bufread(file_path)
                .map_err(|e| eyre!("tail: {}", e))?;
            if lines_val > 0 {
                print_from_line(&mut *reader, lines_val as usize)?;
            } else {
                print_last_lines(&mut *reader, (-lines_val) as usize)?;
            }
        } else if let Some(bytes_val) = options.bytes {
            if bytes_val > 0 {
                // +N: start from byte N
                let mut reader = open_file_as_read(file_path)
                    .map_err(|e| eyre!("tail: {}", e))?;
                print_from_byte(&mut *reader, bytes_val as usize)?;
            } else {
                // -N: last N bytes
                let mut reader = open_file_as_read(file_path)
                    .map_err(|e| eyre!("tail: {}", e))?;
                print_last_bytes(&mut *reader, (-bytes_val) as usize)?;
            }
        }
    }

    Ok(())
}