use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs::File;
use std::io::{self, BufRead};

pub struct TacOptions {
    pub files: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<TacOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(TacOptions { files })
}

pub fn command() -> Command {
    Command::new("tac")
        .about("Concatenate and print files in reverse")
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files to process (if none, read from stdin)"))
}

fn print_lines_reversed(reader: &mut dyn BufRead) -> Result<()> {
    let mut lines = Vec::new();

    // Read all lines first
    for line_result in reader.lines() {
        let line = line_result
            .map_err(|e| eyre!("tac: error reading input: {}", e))?;
        lines.push(line);
    }

    // Print in reverse order
    for line in lines.iter().rev() {
        println!("{}", line);
    }

    Ok(())
}

pub fn run(options: TacOptions) -> Result<()> {
    if options.files.is_empty() {
        // Read from stdin
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        print_lines_reversed(&mut reader)?;
    } else {
        // Process files
        for file_path in &options.files {
            let file = File::open(file_path)
                .map_err(|e| eyre!("tac: {}: {}", file_path, e))?;
            let mut reader = io::BufReader::new(file);
            print_lines_reversed(&mut reader)?;
        }
    }

    Ok(())
}