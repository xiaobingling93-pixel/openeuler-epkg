use clap::{Arg, Command};
use color_eyre::Result;

#[derive(Default)]
pub(crate) enum EscapeMode {
    #[default]
    Disabled,
    Enabled,
}

pub struct EchoOptions {
    pub text: Vec<String>,
    pub no_newline: bool,
    pub escape_mode: EscapeMode,
}

enum EscapeResult {
    Continue,
    Stop,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<EchoOptions> {
    let mut text: Vec<String> = matches.get_many::<String>("text")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let mut no_newline = false;
    let mut escape_mode = EscapeMode::Disabled;
    let mut iter = text.drain(..);
    let mut processed_text = Vec::new();

    while let Some(arg) = iter.next() {
        if arg == "--" {
            // End of options, remaining arguments are text
            processed_text.extend(iter);
            break;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            let mut chars = arg[1..].chars();
            let mut temp_no_newline = false;
            let mut temp_escape_mode = None;
            let mut all_valid = true;
            while let Some(c) = chars.next() {
                match c {
                    'n' => temp_no_newline = true,
                    'e' => temp_escape_mode = Some(EscapeMode::Enabled),
                    'E' => temp_escape_mode = Some(EscapeMode::Disabled),
                    _ => {
                        // Unknown option character, treat the whole argument as text
                        all_valid = false;
                        break;
                    }
                }
            }
            if all_valid {
                // Apply options from this argument
                no_newline = temp_no_newline;
                if let Some(mode) = temp_escape_mode {
                    escape_mode = mode;
                }
                // Continue to next argument
            } else {
                // This argument (including leading '-') is text, and so are all following args
                processed_text.push(arg);
                processed_text.extend(iter);
                break;
            }
        } else {
            // Not an option argument, treat as text and stop option processing
            processed_text.push(arg);
            processed_text.extend(iter);
            break;
        }
    }

    Ok(EchoOptions {
        text: processed_text,
        no_newline,
        escape_mode,
    })
}

pub fn command() -> Command {
    Command::new("echo")
        .about("Display a line of text")
        .ignore_errors(true)
        .allow_hyphen_values(true)
        .arg(Arg::new("text")
            .num_args(0..)
            .allow_hyphen_values(true)
            .help("Text to display"))
}

fn handle_octal_escape(chars: &mut std::iter::Peekable<std::str::Chars>, first_digit: char) -> String {
    let max_extra = if first_digit == '0' { 3 } else { 2 };
    let mut octal = String::new();
    octal.push(first_digit);
    for _ in 0..max_extra {
        match chars.peek() {
            Some('0'..='7') => octal.push(chars.next().unwrap()),
            _ => break,
        }
    }
    match u32::from_str_radix(&octal, 8) {
        Ok(val) if val <= 0xFF => {
            char::from(val as u8).to_string()
        }
        _ => {
            // Should not happen, but fallback: push backslash and digits
            format!("\\{}", octal)
        }
    }
}

fn handle_hex_escape(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut hex = String::new();
    for _ in 0..2 {
        match chars.peek() {
            Some('0'..='9') | Some('a'..='f') | Some('A'..='F') => hex.push(chars.next().unwrap()),
            _ => break,
        }
    }
    if hex.is_empty() {
        "x".to_string()
    } else {
        match u32::from_str_radix(&hex, 16) {
            Ok(val) if val <= 0xFF => {
                char::from(val as u8).to_string()
            }
            _ => {
                // Invalid hex, treat as literal x plus digits?
                format!("x{}", hex)
            }
        }
    }
}

fn handle_escape(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    result: &mut String,
) -> EscapeResult {
    match chars.next() {
        Some('\\') => result.push('\\'),
        Some('a') => result.push('\x07'),
        Some('b') => result.push('\x08'),
        Some('c') => return EscapeResult::Stop,
        Some('e') => result.push('\x1b'),
        Some('f') => result.push('\x0c'),
        Some('n') => result.push('\n'),
        Some('r') => result.push('\r'),
        Some('t') => result.push('\t'),
        Some('v') => result.push('\x0b'),
        Some(d) if d.is_digit(8) => result.push_str(&handle_octal_escape(chars, d)),
        Some('x') => result.push_str(&handle_hex_escape(chars)),
        Some(other) => {
            result.push('\\');
            result.push(other);
        }
        None => {
            result.push('\\');
        }
    }
    EscapeResult::Continue
}

fn process_escapes(s: &str) -> (String, bool) {
    let mut result = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        if let EscapeResult::Stop = handle_escape(&mut chars, &mut result) {
            return (result, true);
        }
    }
    (result, false)
}

pub fn run(options: EchoOptions) -> Result<()> {
    let text = options.text.join(" ");
    let (output, cancel_newline) = match options.escape_mode {
        EscapeMode::Enabled => process_escapes(&text),
        EscapeMode::Disabled => (text, false),
    };
    let no_newline = options.no_newline || cancel_newline;
    if no_newline {
        print!("{}", output);
    } else {
        println!("{}", output);
    }
    Ok(())
}

