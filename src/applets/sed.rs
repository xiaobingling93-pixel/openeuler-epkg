use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use regex::Regex;
use std::io::{self, BufRead};

pub struct SedOptions {
    pub scripts: Vec<String>,
    pub inplace: bool,
    pub extended_regex: bool,
    pub quiet: bool,
    pub files: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<SedOptions> {
    let mut scripts = Vec::new();

    // Get scripts from -e flags
    if let Some(script_vals) = matches.get_many::<String>("expression") {
        scripts.extend(script_vals.cloned());
    }

    // If no -e flags, check for positional script argument
    if scripts.is_empty() {
        if let Some(script) = matches.get_one::<String>("script") {
            scripts.push(script.clone());
        } else {
            return Err(eyre!("sed: missing script"));
        }
    }

    let inplace = matches.get_flag("inplace");
    let extended_regex = matches.get_flag("extended") || matches.get_flag("regexp-extended");
    let quiet = matches.get_flag("quiet") || matches.get_flag("silent");

    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(SedOptions { scripts, inplace, extended_regex, quiet, files })
}

pub fn command() -> Command {
    Command::new("sed")
        .about("Stream editor")
        .arg(Arg::new("expression")
            .short('e')
            .long("expression")
            .help("Add the script to the commands to be executed")
            .value_name("SCRIPT")
            .action(clap::ArgAction::Append))
        .arg(Arg::new("inplace")
            .short('i')
            .long("in-place")
            .help("Edit files in place")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("extended")
            .short('E')
            .long("regexp-extended")
            .help("Use extended regular expressions")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("regexp-extended")
            .short('r')
            .help("Use extended regular expressions (deprecated, use -E)")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("quiet")
            .short('n')
            .long("quiet")
            .help("Suppress automatic printing of pattern space")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("silent")
            .long("silent")
            .help("Suppress automatic printing of pattern space")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("script")
            .help("Script to execute (can be used without -e for single script)")
            .value_name("SCRIPT")
            .index(1))
        .arg(Arg::new("files")
            .help("Files to process (if none, read from stdin)")
            .index(2)
            .num_args(0..))
}

#[derive(Debug)]
enum Address {
    LineNumber(u64),
    LastLine,
    Range(u64, u64),
    Pattern(String),
}

#[derive(Debug)]
enum SedCommand {
    Substitution {
        pattern: String,
        replacement: String,
        flags: String,
    },
    Delete,
    Print,
    Quit,
}

#[derive(Debug)]
struct AddressedCommand {
    address: Option<Address>,
    command: SedCommand,
    compiled_pattern: Option<Regex>,
}

fn parse_address(script: &str) -> Result<(Option<Address>, &str)> {
    // Check for regex pattern address like /pattern/
    if script.starts_with('/') {
        if let Some(end_slash) = script[1..].find('/') {
            let pattern = script[1..=end_slash].to_string();
            return Ok((Some(Address::Pattern(pattern)), &script[end_slash + 2..]));
        }
    }

    // Check for line number or range before command
    if let Some(comma_pos) = script.find(',') {
        // Check if it's a range like "1,10d" or "/pattern/,10d"
        let before_comma = &script[..comma_pos];
        let _after_comma = &script[comma_pos + 1..];

        // Try line number range first
        if let Ok(start) = before_comma.parse::<u64>() {
            // Find where the address ends (first non-digit after comma)
            let mut addr_end = comma_pos + 1;
            while addr_end < script.len() && script.chars().nth(addr_end).unwrap().is_ascii_digit() {
                addr_end += 1;
            }

            if let Ok(end_num) = script[comma_pos + 1..addr_end].parse::<u64>() {
                return Ok((Some(Address::Range(start, end_num)), &script[addr_end..]));
            }
        }
    }

    // Check for single line number
    if let Some(digit_end) = script.chars().position(|c| !c.is_ascii_digit()) {
        if digit_end > 0 {
            if let Ok(line_num) = script[..digit_end].parse::<u64>() {
                return Ok((Some(Address::LineNumber(line_num)), &script[digit_end..]));
            }
        }
    }

    // Check for $ (last line)
    if script.starts_with('$') {
        return Ok((Some(Address::LastLine), &script[1..]));
    }

    // No address found
    Ok((None, script))
}

fn parse_command(script: &str) -> Result<AddressedCommand> {
    // Parse address if present
    let (address, remaining) = parse_address(script)?;

    let command = if remaining.starts_with('s') {
        // Parse substitution command
        let script_after_s = &remaining[1..]; // Remove 's'

        // Find the delimiter (first character after 's')
        let delimiter = script_after_s.chars().next()
            .ok_or_else(|| eyre!("sed: missing delimiter"))?;

        let parts: Vec<&str> = script_after_s.split(delimiter).collect();
        if parts.len() < 3 {
            return Err(eyre!("sed: invalid substitution syntax"));
        }

        let pattern = parts[1].to_string();
        let replacement = parts[2].to_string();
        let flags = if parts.len() > 3 { parts[3].to_string() } else { String::new() };

        SedCommand::Substitution { pattern, replacement, flags }
    } else if remaining == "d" {
        SedCommand::Delete
    } else if remaining == "p" {
        SedCommand::Print
    } else if remaining == "q" {
        SedCommand::Quit
    } else {
        return Err(eyre!("sed: unsupported command '{}'", remaining));
    };

    // Compile regex pattern if we have a pattern address
    let compiled_pattern = if let Some(Address::Pattern(ref pattern)) = address {
        Some(Regex::new(&pattern[1..pattern.len()-1])
            .map_err(|e| eyre!("sed: invalid regex pattern '{}': {}", pattern, e))?)
    } else {
        None
    };

    Ok(AddressedCommand { address, command, compiled_pattern })
}

fn apply_commands(line: &str, commands: &[AddressedCommand], extended_regex: bool, line_number: u64, total_lines: Option<u64>) -> Result<(String, bool, bool, bool)> {
    let mut current_line = line.to_string();
    let mut should_print = true;
    let mut force_print = false;
    let mut should_quit = false;

    for cmd in commands {
        // Check if address matches current line
        if let Some(address) = &cmd.address {
            let matches = match address {
                Address::LineNumber(n) => *n == line_number,
                Address::LastLine => total_lines.map_or(false, |total| line_number == total),
                Address::Range(start, end) => line_number >= *start && line_number <= *end,
                Address::Pattern(_) => {
                    // Use pre-compiled regex
                    cmd.compiled_pattern.as_ref().map_or(false, |regex| regex.is_match(line))
                }
            };
            if !matches {
                continue;
            }
        }

        match &cmd.command {
            SedCommand::Substitution { pattern, replacement, flags } => {
                let mut regex_builder = String::new();

                // Case insensitive flag
                if flags.contains('i') || flags.contains('I') {
                    regex_builder.push_str("(?i)");
                }

                // Multiline flag (though not commonly used in sed)
                if flags.contains('m') || flags.contains('M') {
                    regex_builder.push_str("(?m)");
                }

                let escaped_pattern = if extended_regex {
                    pattern.clone()
                } else {
                    regex::escape(pattern)
                };
                regex_builder.push_str(&escaped_pattern);

                let full_pattern = regex_builder;

                let regex = Regex::new(&full_pattern)
                    .map_err(|e| eyre!("sed: invalid regex '{}': {}", pattern, e))?;

                let processed_replacement = process_replacement(replacement);

                if flags.contains('g') {
                    current_line = regex.replace_all(&current_line, processed_replacement.as_str()).to_string();
                } else {
                    current_line = regex.replace(&current_line, processed_replacement.as_str()).to_string();
                }
            }
            SedCommand::Delete => {
                should_print = false;
                break;
            }
            SedCommand::Print => {
                force_print = true;
            }
            SedCommand::Quit => {
                should_quit = true;
                break;
            }
        }
    }

    Ok((current_line, should_print, force_print, should_quit))
}

pub fn run(options: SedOptions) -> Result<()> {
    // Parse all scripts into commands
    let mut all_commands = Vec::new();
    for script in &options.scripts {
        let cmd = parse_command(script)?;
        all_commands.push(cmd);
    }

    // Process input
    if options.files.is_empty() {
        // Read from stdin
        let mut lines = io::stdin().lock().lines();
        process_input(&mut lines, &all_commands, &options, None)?;
    } else {
        // Process files
        for file_path in &options.files {
            if options.inplace {
                // Read entire file for in-place editing
                let content = std::fs::read_to_string(file_path)
                    .map_err(|e| eyre!("sed: cannot read '{}': {}", file_path, e))?;

                let lines: Vec<&str> = content.lines().collect();
                let total_lines = lines.len() as u64;

                let mut processed_lines = Vec::new();
                for (i, line) in lines.iter().enumerate() {
                    let line_number = (i + 1) as u64;
                    let (processed, _, _, should_quit) = apply_commands(line, &all_commands, options.extended_regex, line_number, Some(total_lines))?;
                    // For in-place editing, always include the processed line
                    processed_lines.push(processed);
                    if should_quit {
                        break;
                    }
                }

                // Write back to file
                std::fs::write(file_path, processed_lines.join("\n") + "\n")
                    .map_err(|e| eyre!("sed: cannot write '{}': {}", file_path, e))?;
            } else {
                // Read and process line by line
                let file = std::fs::File::open(file_path)
                    .map_err(|e| eyre!("sed: cannot open '{}': {}", file_path, e))?;
                let reader = io::BufReader::new(file);
                let mut lines = reader.lines();
                process_input(&mut lines, &all_commands, &options, Some(file_path))?;
            }
        }
    }

    Ok(())
}

fn process_replacement(replacement: &str) -> String {
    // Handle basic escape sequences and backreferences
    let mut result = String::new();
    let mut chars = replacement.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next_ch) = chars.peek() {
                match next_ch {
                    '\\' => {
                        result.push('\\');
                        chars.next();
                    }
                    '1'..='9' => {
                        // Backreference - let regex crate handle this
                        result.push('\\');
                        result.push(*next_ch);
                        chars.next();
                    }
                    'n' => {
                        result.push('\n');
                        chars.next();
                    }
                    't' => {
                        result.push('\t');
                        chars.next();
                    }
                    'r' => {
                        result.push('\r');
                        chars.next();
                    }
                    _ => {
                        // Other escape sequences - just keep the backslash and character
                        result.push('\\');
                        result.push(*next_ch);
                        chars.next();
                    }
                }
            } else {
                result.push('\\');
            }
        } else {
            result.push(ch);
        }
    }

    result
}

fn process_input(
    lines: &mut dyn Iterator<Item = Result<String, std::io::Error>>,
    commands: &[AddressedCommand],
    options: &SedOptions,
    file_path: Option<&str>,
) -> Result<()> {
    let mut line_number = 0;
    for line_result in lines {
        line_number += 1;
        let line = line_result
            .map_err(|e| {
                let file_info = file_path.map(|p| format!(" '{}'", p)).unwrap_or_default();
                eyre!("sed: error reading{}: {}", file_info, e)
            })?;

        let (processed, should_print, force_print, should_quit) = apply_commands(&line, commands, options.extended_regex, line_number, None)?;

        let should_output = if options.quiet { force_print } else { should_print };
        if should_output {
            println!("{}", processed);
        }

        if should_quit {
            break;
        }
    }

    Ok(())
}