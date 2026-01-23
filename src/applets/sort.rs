use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::io::{self, BufRead};

#[derive(Debug)]
pub struct KeySpec {
    pub field: usize, // 1-based field number
}

fn parse_key_spec(key_str: &str) -> Result<KeySpec> {
    // For now, just parse a simple field number like "2"
    let field: usize = key_str.parse()
        .map_err(|_| eyre!("sort: invalid key specification '{}'", key_str))?;

    if field == 0 {
        return Err(eyre!("sort: field number must be greater than 0"));
    }

    Ok(KeySpec { field })
}

fn get_sort_key(line: &str, key_spec: &KeySpec, separator: Option<&str>) -> String {
    if let Some(sep) = separator {
        // Split by custom separator
        let fields: Vec<&str> = line.split(sep).collect();
        if key_spec.field <= fields.len() {
            fields[key_spec.field - 1].to_string()
        } else {
            "".to_string()
        }
    } else {
        // Split by whitespace (default behavior)
        let fields: Vec<&str> = line.split_whitespace().collect();
        if key_spec.field <= fields.len() {
            fields[key_spec.field - 1].to_string()
        } else {
            "".to_string()
        }
    }
}

pub struct SortOptions {
    pub files: Vec<String>,
    pub reverse: bool,
    pub key: Option<KeySpec>,
    pub separator: Option<String>,
    pub stable: bool,
    pub unique: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<SortOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let reverse = matches.get_flag("reverse");
    let key = if let Some(key_str) = matches.get_one::<String>("key") {
        Some(parse_key_spec(key_str)?)
    } else {
        None
    };
    let separator = matches.get_one::<String>("separator").cloned();
    let stable = matches.get_flag("stable");
    let unique = matches.get_flag("unique");

    Ok(SortOptions { files, reverse, key, separator, stable, unique })
}

pub fn command() -> Command {
    Command::new("sort")
        .about("Sort lines of text files")
        .arg(Arg::new("reverse")
            .short('r')
            .long("reverse")
            .help("Reverse the sort order")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("key")
            .short('k')
            .long("key")
            .help("Sort by key/field")
            .value_name("POS")
            .action(clap::ArgAction::Set))
        .arg(Arg::new("separator")
            .short('t')
            .long("field-separator")
            .help("Use SEP instead of non-blank to blank transition")
            .value_name("SEP")
            .action(clap::ArgAction::Set))
        .arg(Arg::new("stable")
            .short('s')
            .long("stable")
            .help("Stabilize sort by disabling last-resort comparison")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("unique")
            .short('u')
            .long("unique")
            .help("Output only the first of an equal run")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files to sort (if none, read from stdin)"))
}

pub fn run(options: SortOptions) -> Result<()> {
    let mut lines = Vec::new();

    if options.files.is_empty() {
        // Read from stdin
        let stdin = io::stdin();
        let reader = stdin.lock();
        for line_result in reader.lines() {
            let line = line_result
                .map_err(|e| eyre!("sort: error reading stdin: {}", e))?;
            lines.push(line);
        }
    } else {
        // Read from files
        for file_path in &options.files {
            let file = std::fs::File::open(file_path)
                .map_err(|e| eyre!("sort: {}: {}", file_path, e))?;
            let reader = io::BufReader::new(file);
            for line_result in reader.lines() {
                let line = line_result
                    .map_err(|e| eyre!("sort: error reading {}: {}", file_path, e))?;
                lines.push(line);
            }
        }
    }

    // Sort the lines
    if let Some(key_spec) = &options.key {
        // Sort by key
        let mut keyed_lines: Vec<(String, String)> = lines.into_iter()
            .map(|line| {
                let key = get_sort_key(&line, key_spec, options.separator.as_deref());
                (key, line)
            })
            .collect();

        // Use stable sort when --stable flag is set, unstable otherwise
        if options.stable {
            keyed_lines.sort_by(|a, b| a.0.cmp(&b.0));
        } else {
            keyed_lines.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        }

        if options.reverse {
            keyed_lines.reverse();
        }

        if options.unique {
            // Remove duplicates based on the sort key
            keyed_lines.dedup_by(|a, b| a.0 == b.0);
        }

        // Extract the sorted lines
        lines = keyed_lines.into_iter().map(|(_, line)| line).collect();
    } else {
        // Regular sort: use stable sort when --stable flag is set, unstable otherwise
        if options.stable {
            lines.sort();
        } else {
            lines.sort_unstable();
        }

        if options.reverse {
            lines.reverse();
        }

        if options.unique {
            lines.dedup();
        }
    }

    // Output the sorted lines
    for line in lines {
        println!("{}", line);
    }

    Ok(())
}