use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::io::{self, BufRead};

pub struct CutOptions {
    pub delimiter: String,
    pub fields: String,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<CutOptions> {
    let delimiter = matches.get_one::<String>("delimiter")
        .unwrap_or(&"\t".to_string())
        .clone();

    let fields = matches.get_one::<String>("fields")
        .ok_or_else(|| eyre!("cut: missing fields"))?
        .clone();

    Ok(CutOptions { delimiter, fields })
}

pub fn command() -> Command {
    Command::new("cut")
        .about("Extract fields from lines")
        .arg(Arg::new("delimiter")
            .short('d')
            .long("delimiter")
            .help("Field delimiter (default: tab)")
            .value_name("DELIM"))
        .arg(Arg::new("fields")
            .short('f')
            .long("fields")
            .help("Fields to extract")
            .value_name("LIST")
            .required(true))
}

#[derive(Debug, Clone)]
enum FieldSpec {
    Single(usize),
    Range(usize, Option<usize>),
}

fn parse_field_list(field_str: &str) -> Result<Vec<FieldSpec>> {
    let mut fields = Vec::new();

    for part in field_str.split(',') {
        if part.contains('-') {
            // Range like "2-4" or "2-"
            let bounds: Vec<&str> = part.split('-').collect();
            if bounds.len() != 2 {
                return Err(eyre!("cut: invalid field range '{}'", part));
            }

            let start: usize = bounds[0].parse()
                .map_err(|_| eyre!("cut: invalid field number '{}'", bounds[0]))?;

            let end = if bounds[1].is_empty() {
                // Open-ended range like "2-"
                None
            } else {
                Some(bounds[1].parse()
                    .map_err(|_| eyre!("cut: invalid field number '{}'", bounds[1]))?)
            };

            if let Some(end_val) = end {
                if start > end_val {
                    return Err(eyre!("cut: invalid field range '{}'", part));
                }
            }

            fields.push(FieldSpec::Range(start, end));
        } else {
            // Single field
            let field: usize = part.parse()
                .map_err(|_| eyre!("cut: invalid field number '{}'", part))?;
            fields.push(FieldSpec::Single(field));
        }
    }

    // Validate field numbers are >= 1
    for spec in &fields {
        let field_num = match spec {
            FieldSpec::Single(f) => *f,
            FieldSpec::Range(f, _) => *f,
        };
        if field_num == 0 {
            return Err(eyre!("cut: fields are numbered from 1"));
        }
    }

    Ok(fields)
}

pub fn run(options: CutOptions) -> Result<()> {
    let field_specs = parse_field_list(&options.fields)?;
    let delimiter_chars: Vec<char> = options.delimiter.chars().collect();

    let stdin = io::stdin();
    let reader = stdin.lock();

    for line_result in reader.lines() {
        let line = line_result
            .map_err(|e| eyre!("cut: error reading input: {}", e))?;

        // Handle different delimiter lengths
        let fields: Vec<&str> = if delimiter_chars.len() == 1 {
            // Single character delimiter
            line.split(delimiter_chars[0]).collect()
        } else {
            // Multi-character delimiter
            line.split(&options.delimiter).collect()
        };

        let mut field_indices = Vec::new();

        // Expand field specs into actual indices for this line
        for spec in &field_specs {
            match spec {
                FieldSpec::Single(field_num) => {
                    let idx = field_num - 1; // Convert to 0-based
                    if idx < fields.len() {
                        field_indices.push(idx);
                    }
                }
                FieldSpec::Range(start, end_opt) => {
                    let start_idx = start - 1; // Convert to 0-based
                    let end_idx = end_opt.map(|e| e - 1).unwrap_or(fields.len() - 1);

                    for idx in start_idx..=end_idx {
                        if idx < fields.len() {
                            field_indices.push(idx);
                        }
                    }
                }
            }
        }

        // Remove duplicates and sort
        field_indices.sort();
        field_indices.dedup();

        let mut output_parts = Vec::new();
        for &field_idx in &field_indices {
            if field_idx < fields.len() {
                output_parts.push(fields[field_idx]);
            }
        }

        if !output_parts.is_empty() {
            println!("{}", output_parts.join(&options.delimiter));
        }
    }

    Ok(())
}