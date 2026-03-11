use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, Write};
use crate::busybox::comm::open_file_or_stdin;

#[derive(Clone)]
pub struct UniqOptions {
    pub files: Vec<String>,
    pub count: bool,
    pub repeated: bool,
    pub unique: bool,
    pub skip_fields: usize,
    pub skip_chars: usize,
    pub max_chars: Option<usize>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<UniqOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let count = matches.get_flag("count");
    let repeated = matches.get_flag("repeated");
    let unique = matches.get_flag("unique");
    let skip_fields = matches.get_one::<usize>("skip_fields").copied().unwrap_or(0);
    let skip_chars = matches.get_one::<usize>("skip_chars").copied().unwrap_or(0);
    let max_chars = matches.get_one::<usize>("max_chars").copied();

    Ok(UniqOptions {
        files,
        count,
        repeated,
        unique,
        skip_fields,
        skip_chars,
        max_chars,
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
        .arg(Arg::new("skip_fields")
            .short('f')
            .long("skip-fields")
            .value_name("N")
            .num_args(1)
            .value_parser(clap::value_parser!(usize))
            .help("Skip first N fields when comparing"))
        .arg(Arg::new("skip_chars")
            .short('s')
            .long("skip-chars")
            .value_name("N")
            .num_args(1)
            .value_parser(clap::value_parser!(usize))
            .help("Skip first N chars when comparing"))
        .arg(Arg::new("max_chars")
            .short('w')
            .long("check-chars")
            .value_name("N")
            .num_args(1)
            .value_parser(clap::value_parser!(usize))
            .help("Compare at most N chars"))
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files to process (if none, read from stdin; - means stdin"))
}

/// Compute comparison key from line (skip N fields, skip M chars, at most W chars).
fn line_key_owned(line: &str, skip_fields: usize, skip_chars: usize, max_chars: Option<usize>) -> String {
    let mut s = line;
    for _ in 0..skip_fields {
        // Skip leading whitespace
        let first_non_ws = s.find(|c: char| !c.is_whitespace());
        let s1 = match first_non_ws {
            Some(idx) => &s[idx..],
            None => return "".to_string(),
        };
        // Skip the field (non-whitespace)
        let first_ws = s1.find(|c: char| c.is_whitespace());
        s = match first_ws {
            Some(idx) => &s1[idx..], // keep the whitespace delimiter
            None => return "".to_string(),
        };
    }
    let char_count = s.chars().count();
    let skip: usize = std::cmp::min(skip_chars, char_count);
    let start = s.char_indices().nth(skip).map(|(i, _)| i).unwrap_or(s.len());
    s = &s[start..];
    if let Some(n) = max_chars {
        let end = s.char_indices().nth(n).map(|(i, _)| i).unwrap_or(s.len());
        s = &s[..end];
    }
    s.to_string()
}

fn process_lines(reader: &mut dyn BufRead, options: &UniqOptions, out: &mut dyn Write) -> Result<()> {
    if options.repeated && options.unique {
        return Ok(());
    }
    let mut key_counts: HashMap<String, (u64, String)> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for line_result in reader.lines() {
        let line = line_result
            .map_err(|e| eyre!("uniq: error reading input: {}", e))?;
        let key = line_key_owned(&line, options.skip_fields, options.skip_chars, options.max_chars);
        let entry = key_counts.entry(key.clone()).or_insert_with(|| (0, line.clone()));
        entry.0 += 1;
        if entry.0 == 1 {
            order.push(key);
        }
    }

    let mut seen = std::collections::HashSet::new();
    for key in order {
        let (count, line) = key_counts.get(&key).unwrap();
        let should_print = if options.repeated {
            *count > 1
        } else if options.unique {
            *count == 1
        } else {
            true
        };
        if options.repeated || options.unique {
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key.clone());
        }
        if should_print {
            if options.count {
                writeln!(out, "{} {}", count, line)?;
            } else {
                writeln!(out, "{}", line)?;
            }
        }
    }

    Ok(())
}


fn open_output(path: &str) -> Result<Box<dyn Write>> {
    if path == "-" {
        Ok(Box::new(io::stdout()))
    } else {
        let file = File::create(path).map_err(|e| eyre!("uniq: {}: {}", path, e))?;
        Ok(Box::new(file))
    }
}

pub fn run(options: UniqOptions) -> Result<()> {
    let (input_path, output_path) = match options.files.as_slice() {
        [] => (None, None),
        [a] => (Some(a.clone()), None),
        [a, b] => (Some(a.clone()), Some(b.clone())),
        _ => {
            return Err(eyre!("uniq: extra operand '{}'", options.files[2]));
        }
    };
    let mut reader: Box<dyn BufRead> = match &input_path {
        None => open_file_or_stdin("-", "uniq")?,
        Some(p) => open_file_or_stdin(p, "uniq")?,
    };
    let mut out: Box<dyn Write> = match &output_path {
        None => Box::new(io::stdout()),
        Some(p) => open_output(p)?,
    };
    process_lines(&mut reader, &options, &mut out)
}