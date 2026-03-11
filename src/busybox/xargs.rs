use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::io::{self, BufRead, Read};

const DEFAULT_MAX_SIZE: usize = 128 * 1024; // 128KB default command line limit

#[derive(Clone)]
pub struct XargsOptions {
    pub command: Vec<String>,
    pub max_args: Option<usize>,
    pub delimiter: Option<String>,
    pub null_delimiter: bool,
    pub no_run_if_empty: bool,
    pub eof_str: Option<String>,
    pub replace_str: Option<String>,
    pub max_size: Option<usize>,
    pub verbose: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<XargsOptions> {
    let mut command: Vec<String> = matches.get_many::<String>("command")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();
    // Default command is "echo"
    if command.is_empty() {
        command = vec!["echo".to_string()];
    }

    let max_args = matches.get_one::<String>("max-args")
        .and_then(|s| s.parse().ok());

    let delimiter = matches.get_one::<String>("delimiter")
        .map(|s| s.clone());

    let null_delimiter = matches.get_flag("null");

    let no_run_if_empty = matches.get_flag("no-run-if-empty");

    // Handle -E and -e options (synonyms for end-of-file string)
    let eof_str = matches.get_one::<String>("eof-str-E")
        .map(|s| s.clone())
        .or_else(|| {
            // Check -e option: if present with value, use value; if present without value, empty string (disables eof)
            if matches.contains_id("eof-str-e") {
                matches.get_one::<String>("eof-str-e")
                    .map(|s| s.clone())
                    .or(Some("".to_string()))
            } else {
                None
            }
        })
        .and_then(|s| if s.is_empty() { None } else { Some(s) });

    let replace_str = matches.get_one::<String>("replace-str").map(|s| s.clone());

    let max_size = matches.get_one::<String>("max-size")
        .and_then(|s| s.parse().ok());

    let verbose = matches.get_flag("verbose");

    Ok(XargsOptions {
        command,
        max_args,
        delimiter,
        null_delimiter,
        no_run_if_empty,
        eof_str,
        replace_str,
        max_size,
        verbose,
    })
}

pub fn command() -> Command {
    Command::new("xargs")
        .about("Build and execute command lines from standard input")
        .arg(Arg::new("max-args")
            .short('n')
            .long("max-args")
            .help("Maximum arguments per command line")
            .value_name("MAX"))
        .arg(Arg::new("delimiter")
            .short('d')
            .long("delimiter")
            .help("Input delimiter (default: whitespace)")
            .value_name("DELIM"))
        .arg(Arg::new("null")
            .short('0')
            .long("null")
            .help("Input items are terminated by null character")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("no-run-if-empty")
            .short('r')
            .long("no-run-if-empty")
            .help("Do not run command if input is empty")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("eof-str-E")
            .short('E')
            .long("eof")
            .help("Set end-of-file string (synonym for -e)")
            .value_name("EOF-STR"))
        .arg(Arg::new("eof-str-e")
            .short('e')
            .long("eof-str")
            .help("Set end-of-file string (if omitted, default is underscore)")
            .value_name("EOF-STR")
            .num_args(0..=1))
        .arg(Arg::new("max-size")
            .short('s')
            .long("max-chars")
            .help("Maximum size of command line in bytes")
            .value_name("SIZE"))
        .arg(Arg::new("verbose")
            .short('t')
            .long("verbose")
            .help("Print commands before executing")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("replace-str")
            .short('I')
            .long("replace-str")
            .help("Replace STR within PROG ARGS with input line")
            .value_name("STR")
            .num_args(0..=1)
            .default_missing_value("{}"))
        .arg(Arg::new("command")
            .num_args(0..)
            .help("Command to execute (default: echo)")
            .required(false))
}

pub fn run(options: XargsOptions) -> Result<()> {
    if let Some(replace_str) = options.replace_str.clone() {
        return process_replace_mode(&options, &replace_str);
    }

    let args = read_input_with_eof(&options)?;

    // Check if we should run the command
    if options.no_run_if_empty && args.is_empty() {
        return Ok(());
    }

    // Apply max_size constraint (use default if not specified)
    let max_size = options.max_size.unwrap_or(DEFAULT_MAX_SIZE);
    let batches = batch_args_by_size(&options.command, &args, max_size);

    // Execute commands with max_args constraint
    for batch in batches {
        if let Some(max_args) = options.max_args {
            for chunk in batch.chunks(max_args) {
                execute_command(&options.command, chunk, options.verbose)?;
            }
        } else {
            if !batch.is_empty() {
                execute_command(&options.command, &batch, options.verbose)?;
            }
        }
    }

    Ok(())
}

fn process_replace_mode(options: &XargsOptions, replace_str: &str) -> Result<()> {
    let stdin = io::stdin();
    let reader = stdin.lock();
    let mut lines = Vec::new();

    for line_result in reader.lines() {
        let line = line_result.map_err(|e| eyre!("xargs: error reading input: {}", e))?;

        // Check for eof_str
        if let Some(eof_str) = &options.eof_str {
            if line == *eof_str {
                break;
            }
        }

        // Trim leading whitespace
        let line = line.trim_start_matches(|c: char| c.is_whitespace());
        // Skip empty lines
        if line.is_empty() {
            continue;
        }
        lines.push(line.to_string());
    }

    if options.no_run_if_empty && lines.is_empty() {
        return Ok(());
    }

    for line in lines {
        // Replace replace_str in each command argument
        let mut command = options.command.clone();
        for arg in &mut command {
            *arg = arg.replace(replace_str, &line);
        }
        execute_command(&command, &[], options.verbose)?;
    }

    Ok(())
}
fn tokenize_with_quotes(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escape_next = false;

    for ch in input.chars() {
        if escape_next {
            current.push(ch);
            escape_next = false;
            continue;
        }

        match ch {
            '\\' => {
                escape_next = true;
                continue;
            }
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
                continue;
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
                continue;
            }
            ch if ch.is_whitespace() && !in_single_quote && !in_double_quote => {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
                continue;
            }
            _ => {
                current.push(ch);
            }
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

fn read_input_with_eof(options: &XargsOptions) -> Result<Vec<String>> {
    let mut args = Vec::new();

    // For null-delimited input, we need to read raw bytes
    if options.null_delimiter {
        let mut input = Vec::new();
        io::stdin().read_to_end(&mut input)
            .map_err(|e| eyre!("xargs: error reading input: {}", e))?;
        let input_str = String::from_utf8_lossy(&input);
        args = input_str.split('\0')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
    } else {
        // Read line by line to support eof_str
        let stdin = io::stdin();
        let reader = stdin.lock();

        for line_result in reader.lines() {
            let line = line_result.map_err(|e| eyre!("xargs: error reading input: {}", e))?;

            // Check for eof_str
            if let Some(eof_str) = &options.eof_str {
                if line == *eof_str {
                    break; // Stop processing rest of input
                }
            }

            // Split line based on delimiter
            if let Some(delimiter) = &options.delimiter {
                // Split on custom delimiter
                for part in line.split(delimiter) {
                    if !part.is_empty() {
                        args.push(part.to_string());
                    }
                }
            } else {
                // Split on whitespace with quote support (default behavior)
                args.extend(tokenize_with_quotes(&line));
            }
        }
    }

    Ok(args)
}

fn batch_args_by_size(command: &[String], args: &[String], max_size: usize) -> Vec<Vec<String>> {
    // Calculate size of base command (including spaces between command parts and trailing null)
    let base_size: usize = command.iter().map(|s| s.len()).sum::<usize>() + command.len().saturating_sub(1) + 1;

    let mut batches = Vec::new();
    let mut current_batch = Vec::new();
    let mut current_size = base_size;

    for arg in args {
        let arg_size = arg.len() + 1; // +1 for space separator

        if current_size + arg_size > max_size && !current_batch.is_empty() {
            // Start new batch
            batches.push(current_batch);
            current_batch = Vec::new();
            current_size = base_size;
        }

        current_batch.push(arg.clone());
        current_size += arg_size;
    }

    if !current_batch.is_empty() {
        batches.push(current_batch);
    }

    batches
}

fn execute_command(base_command: &[String], args: &[String], verbose: bool) -> Result<()> {
    if base_command.is_empty() {
        return Ok(());
    }

    let mut cmd = std::process::Command::new(&base_command[0]);
    cmd.args(&base_command[1..]);
    cmd.args(args);

    if verbose {
        eprintln!("{} {}", base_command[0], base_command[1..].iter().chain(args).map(|s| s.as_str()).collect::<Vec<_>>().join(" "));
    }

    let status = cmd.status()
        .map_err(|e| eyre!("xargs: failed to execute command: {}", e))?;

    if !status.success() {
        // xargs typically continues even if commands fail, but we can exit on failure
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_with_quotes() {
        // Basic whitespace splitting
        assert_eq!(tokenize_with_quotes("a b c"), vec!["a", "b", "c"]);

        // Single quotes
        assert_eq!(tokenize_with_quotes("'a b' c"), vec!["a b", "c"]);

        // Double quotes
        assert_eq!(tokenize_with_quotes(r#""a b" c"#), vec!["a b", "c"]);

        // Mixed quotes
        assert_eq!(tokenize_with_quotes(r#"a 'b c' "d e""#), vec!["a", "b c", "d e"]);

        // Escaped backslash
        assert_eq!(tokenize_with_quotes(r#"a\\ b"#), vec!["a\\", "b"]);

        // Escaped quote inside quotes
        assert_eq!(tokenize_with_quotes(r#""a\"b" c"#), vec!["a\"b", "c"]);

        // Single quotes inside double quotes
        assert_eq!(tokenize_with_quotes(r#""a'b" c"#), vec!["a'b", "c"]);

        // Empty string
        assert_eq!(tokenize_with_quotes(""), Vec::<String>::new());

        // Only whitespace
        assert_eq!(tokenize_with_quotes("   "), Vec::<String>::new());
    }
}