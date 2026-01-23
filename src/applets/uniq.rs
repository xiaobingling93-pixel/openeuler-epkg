use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead};

pub struct UniqOptions {
    pub files: Vec<String>,
    pub count: bool,
    pub repeated: bool,
    pub unique: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<UniqOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let count = matches.get_flag("count");
    let repeated = matches.get_flag("repeated");
    let unique = matches.get_flag("unique");

    Ok(UniqOptions {
        files,
        count,
        repeated,
        unique,
    })
}

pub fn command() -> Command {
    Command::new("uniq")
        .about("Report or omit repeated lines")
        .arg(Arg::new("count")
            .short('c')
            .long("count")
            .help("Prefix lines with count of occurrences")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("repeated")
            .short('d')
            .long("repeated")
            .help("Only print duplicate lines")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("unique")
            .short('u')
            .long("unique")
            .help("Only print unique lines")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files to process (if none, read from stdin)"))
}

fn process_lines(reader: &mut dyn BufRead, options: &UniqOptions) -> Result<()> {
    let mut line_counts = HashMap::new();
    let mut lines = Vec::new();

    // First pass: count occurrences
    for line_result in reader.lines() {
        let line = line_result
            .map_err(|e| eyre!("uniq: error reading input: {}", e))?;

        let count = line_counts.entry(line.clone()).or_insert(0);
        *count += 1;
        lines.push(line);
    }

    // Second pass: output based on options
    let mut seen = HashMap::new();
    for line in lines {
        let count = *line_counts.get(&line).unwrap_or(&0);

        // Determine if we should print this line
        let should_print = if options.repeated {
            count > 1
        } else if options.unique {
            count == 1
        } else {
            true
        };

        // For repeated/unique modes, only print each line once
        if options.repeated || options.unique {
            if seen.contains_key(&line) {
                continue;
            }
            seen.insert(line.clone(), true);
        }

        if should_print {
            if options.count {
                println!("{:>7} {}", count, line);
            } else {
                println!("{}", line);
            }
        }
    }

    Ok(())
}

pub fn run(options: UniqOptions) -> Result<()> {
    if options.files.is_empty() {
        // Read from stdin
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        process_lines(&mut reader, &options)?;
    } else {
        // Process files
        for file_path in &options.files {
            let file = File::open(file_path)
                .map_err(|e| eyre!("uniq: {}: {}", file_path, e))?;
            let mut reader = io::BufReader::new(file);
            process_lines(&mut reader, &options)?;
        }
    }

    Ok(())
}