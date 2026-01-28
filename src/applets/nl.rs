use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs::File;
use std::io::{self, BufRead, BufReader};

pub struct NlOptions {
    pub files: Vec<String>,
    pub number_format: String,
    pub number_width: usize,
    pub number_separator: String,
    pub starting_line_number: usize,
    pub increment: usize,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<NlOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let number_format = matches.get_one::<String>("number-format")
        .cloned()
        .unwrap_or_else(|| "rn".to_string());

    let number_width = matches.get_one::<String>("number-width")
        .and_then(|s| s.parse().ok())
        .unwrap_or(6);

    let number_separator = matches.get_one::<String>("number-separator")
        .cloned()
        .unwrap_or_else(|| "\t".to_string());

    let starting_line_number = matches.get_one::<String>("starting-line-number")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let increment = matches.get_one::<String>("increment")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    Ok(NlOptions {
        files,
        number_format,
        number_width,
        number_separator,
        starting_line_number,
        increment,
    })
}

pub fn command() -> Command {
    Command::new("nl")
        .about("Number lines of files")
        .arg(Arg::new("number-format")
            .short('v')
            .long("number-format")
            .help("Line numbering format (ln, rn, rz)")
            .value_name("FORMAT"))
        .arg(Arg::new("number-width")
            .short('w')
            .long("number-width")
            .help("Use NUMBER columns for line numbers")
            .value_name("NUMBER"))
        .arg(Arg::new("number-separator")
            .short('s')
            .long("number-separator")
            .help("Add STRING after (possible) line number")
            .value_name("STRING"))
        .arg(Arg::new("starting-line-number")
            .short('b')
            .long("starting-line-number")
            .help("Start line numbering with NUMBER")
            .value_name("NUMBER"))
        .arg(Arg::new("increment")
            .short('i')
            .long("increment")
            .help("Increment line numbers by NUMBER")
            .value_name("NUMBER"))
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files to number (if none, read from stdin)"))
}

fn format_line_number(num: usize, format: &str, width: usize) -> String {
    match format {
        "ln" => format!("{:width$}", num, width = width),
        "rn" => format!("{:>width$}", num, width = width),
        "rz" => format!("{:0>width$}", num, width = width),
        _ => format!("{:>width$}", num, width = width),
    }
}

pub fn run(options: NlOptions) -> Result<()> {
    let mut line_number = options.starting_line_number;

    let mut process_file = |reader: Box<dyn BufRead>| -> Result<()> {
        for line_result in reader.lines() {
            let line = line_result
                .map_err(|e| eyre!("nl: error reading: {}", e))?;

            let formatted_num = format_line_number(line_number, &options.number_format, options.number_width);
            println!("{}{}{}", formatted_num, options.number_separator, line);

            line_number += options.increment;
        }
        Ok(())
    };

    if options.files.is_empty() {
        // Read from stdin
        let stdin = io::stdin();
        let reader = BufReader::new(stdin.lock());
        process_file(Box::new(reader))?;
    } else {
        // Read from files
        for file_path in &options.files {
            let file = File::open(file_path)
                .map_err(|e| eyre!("nl: cannot open '{}': {}", file_path, e))?;
            let reader = BufReader::new(file);
            process_file(Box::new(reader))?;
        }
    }

    Ok(())
}
