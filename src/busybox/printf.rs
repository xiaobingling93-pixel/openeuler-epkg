use clap::{Arg, Command};
use color_eyre::Result;
use std::io::Write;

pub struct PrintfOptions {
    pub format: String,
    pub arguments: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<PrintfOptions> {
    let format = matches.get_one::<String>("format")
        .cloned()
        .unwrap_or_default();
    let arguments: Vec<String> = matches.get_many::<String>("arguments")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();
    Ok(PrintfOptions { format, arguments })
}

pub fn command() -> Command {
    Command::new("printf")
        .about("Format and print data")
        .ignore_errors(true)
        .allow_hyphen_values(true)
        .arg(Arg::new("format")
            .required(true)
            .allow_hyphen_values(true)
            .help("Format string"))
        .arg(Arg::new("arguments")
            .num_args(0..)
            .allow_hyphen_values(true)
            .help("Arguments for format string"))
}

/// Parse escape sequences in a string (like \n, \t, \xHH, \0NNN)
fn parse_escapes(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }

        match chars.next() {
            Some('\\') => result.push('\\'),
            Some('a') => result.push('\x07'),  // alert/bell
            Some('b') => result.push('\x08'),  // backspace
            Some('f') => result.push('\x0c'),  // form feed
            Some('n') => result.push('\n'),
            Some('r') => result.push('\r'),
            Some('t') => result.push('\t'),
            Some('v') => result.push('\x0b'),  // vertical tab
            Some('e') => result.push('\x1b'),  // escape (busybox extension)
            Some('0') => {
                // Octal escape: \0NNN (up to 3 digits)
                let mut octal = String::new();
                for _ in 0..3 {
                    match chars.peek() {
                        Some(&c) if ('0'..='7').contains(&c) => {
                            octal.push(chars.next().unwrap());
                        }
                        _ => break,
                    }
                }
                let val = u32::from_str_radix(&octal, 8).unwrap_or(0);
                if val <= 0xFF {
                    result.push(char::from(val as u8));
                }
            }
            Some('x') => {
                // Hex escape: \xHH (up to 2 hex digits)
                let mut hex = String::new();
                for _ in 0..2 {
                    match chars.peek() {
                        Some(&c) if c.is_ascii_hexdigit() => {
                            hex.push(chars.next().unwrap());
                        }
                        _ => break,
                    }
                }
                if !hex.is_empty() {
                    let val = u32::from_str_radix(&hex, 16).unwrap_or(0);
                    if val <= 0xFF {
                        result.push(char::from(val as u8));
                    }
                } else {
                    // Invalid \x without digits, keep as is
                    result.push_str("\\x");
                }
            }
            Some('c') => {
                // \c stops output (like echo -e)
                break;
            }
            Some(other) => {
                // Unknown escape, output backslash and character
                result.push('\\');
                result.push(other);
            }
            None => {
                // Trailing backslash
                result.push('\\');
            }
        }
    }
    result
}

/// Format a string with printf-style format specifiers
fn format_printf(format: &str, arguments: &[String]) -> Result<String> {
    let mut output = String::new();
    let mut chars = format.chars().peekable();
    let mut arg_index = 0;

    fn get_arg(arguments: &[String], arg_index: &mut usize) -> String {
        if *arg_index < arguments.len() {
            let arg = arguments[*arg_index].clone();
            *arg_index += 1;
            arg
        } else {
            String::new()
        }
    }

    while let Some(c) = chars.next() {
        if c != '%' {
            output.push(c);
            continue;
        }

        // Check for %%
        if chars.peek() == Some(&'%') {
            chars.next();
            output.push('%');
            continue;
        }

        // Parse format specifier: %[flags][width][.precision][length]type
        let mut flags = String::new();
        let mut width = 0usize;
        let mut precision: Option<usize> = None;
        let mut length = String::new();

        // Flags: -, +, space, #, 0
        while let Some(&f) = chars.peek() {
            if f == '-' || f == '+' || f == ' ' || f == '#' || f == '0' {
                flags.push(chars.next().unwrap());
            } else {
                break;
            }
        }

        // Width: number or *
        while let Some(&w) = chars.peek() {
            if w == '*' {
                chars.next();
                // Use argument for width
                width = get_arg(arguments, &mut arg_index).parse().unwrap_or(0);
                break;
            } else if w.is_ascii_digit() {
                width = width * 10 + (chars.next().unwrap() as usize - '0' as usize);
            } else {
                break;
            }
        }

        // Precision: .number or .*
        if chars.peek() == Some(&'.') {
            chars.next();
            let mut p = 0usize;
            while let Some(&pc) = chars.peek() {
                if pc == '*' {
                    chars.next();
                    // Use argument for precision
                    p = get_arg(arguments, &mut arg_index).parse().unwrap_or(6);
                    precision = Some(p);
                    break;
                } else if pc.is_ascii_digit() {
                    p = p * 10 + (chars.next().unwrap() as usize - '0' as usize);
                    precision = Some(p);
                } else {
                    break;
                }
            }
            if precision.is_none() {
                precision = Some(0);  // . without number means precision 0
            }
        }

        // Length modifier: l, ll, h, hh
        while let Some(&l) = chars.peek() {
            if l == 'l' || l == 'h' {
                length.push(chars.next().unwrap());
            } else {
                break;
            }
        }

        // Type specifier
        let type_char = chars.next().unwrap_or('s');

        // Handle %b specially - it interprets escapes in the argument
        if type_char == 'b' {
            let arg = get_arg(arguments, &mut arg_index);
            output.push_str(&parse_escapes(&arg));
            continue;
        }

        // Get argument for other format specifiers
        let arg = get_arg(arguments, &mut arg_index);

        // Format the value
        match type_char {
            's' => {
                // String
                let left_align = flags.contains('-');
                if width > 0 && arg.len() < width {
                    if left_align {
                        output.push_str(&format!("{:<width$}", arg, width = width));
                    } else {
                        output.push_str(&format!("{:>width$}", arg, width = width));
                    }
                } else {
                    output.push_str(&arg);
                }
            }
            'd' | 'i' => {
                // Signed integer
                let val: i64 = arg.parse().unwrap_or(0);
                let left_align = flags.contains('-');
                let show_sign = flags.contains('+');
                let space_sign = flags.contains(' ') && !show_sign;

                let num_str = if show_sign && val >= 0 {
                    format!("+{}", val)
                } else if space_sign && val >= 0 {
                    format!(" {}", val)
                } else {
                    format!("{}", val)
                };

                if width > 0 && num_str.len() < width {
                    if left_align {
                        output.push_str(&format!("{:<width$}", num_str, width = width));
                    } else if flags.contains('0') {
                        // Zero padding
                        if num_str.starts_with('-') || num_str.starts_with('+') || num_str.starts_with(' ') {
                            let sign = num_str.chars().next().unwrap();
                            let digits = &num_str[1..];
                            output.push(sign);
                            output.push_str(&format!("{:0>width$}", digits, width = width - 1));
                        } else {
                            output.push_str(&format!("{:0>width$}", num_str, width = width));
                        }
                    } else {
                        output.push_str(&format!("{:>width$}", num_str, width = width));
                    }
                } else {
                    output.push_str(&num_str);
                }
            }
            'u' => {
                // Unsigned integer
                let val: u64 = arg.parse().unwrap_or(0);
                let num_str = format!("{}", val);
                let left_align = flags.contains('-');

                if width > 0 && num_str.len() < width {
                    if left_align {
                        output.push_str(&format!("{:<width$}", num_str, width = width));
                    } else if flags.contains('0') {
                        output.push_str(&format!("{:0>width$}", num_str, width = width));
                    } else {
                        output.push_str(&format!("{:>width$}", num_str, width = width));
                    }
                } else {
                    output.push_str(&num_str);
                }
            }
            'x' | 'X' => {
                // Hex
                let val: u64 = arg.parse().unwrap_or(0);
                let prefix = flags.contains('#');
                let upper = type_char == 'X';
                let left_align = flags.contains('-');

                let num_str = if upper {
                    if prefix { format!("0x{:X}", val) } else { format!("{:X}", val) }
                } else {
                    if prefix { format!("0x{:x}", val) } else { format!("{:x}", val) }
                };

                if width > 0 && num_str.len() < width {
                    if left_align {
                        output.push_str(&format!("{:<width$}", num_str, width = width));
                    } else {
                        output.push_str(&format!("{:>width$}", num_str, width = width));
                    }
                } else {
                    output.push_str(&num_str);
                }
            }
            'o' => {
                // Octal
                let val: u64 = arg.parse().unwrap_or(0);
                let prefix = flags.contains('#');
                let left_align = flags.contains('-');

                let num_str = if prefix { format!("0{:o}", val) } else { format!("{:o}", val) };

                if width > 0 && num_str.len() < width {
                    if left_align {
                        output.push_str(&format!("{:<width$}", num_str, width = width));
                    } else {
                        output.push_str(&format!("{:>width$}", num_str, width = width));
                    }
                } else {
                    output.push_str(&num_str);
                }
            }
            'f' | 'F' => {
                // Float with fixed point
                let val: f64 = arg.parse().unwrap_or(0.0);
                let prec = precision.unwrap_or(6);
                let num_str = format!("{:.prec$}", val, prec = prec);
                let left_align = flags.contains('-');

                if width > 0 && num_str.len() < width {
                    if left_align {
                        output.push_str(&format!("{:<width$}", num_str, width = width));
                    } else {
                        output.push_str(&format!("{:>width$}", num_str, width = width));
                    }
                } else {
                    output.push_str(&num_str);
                }
            }
            'e' | 'E' => {
                // Float with exponential
                let val: f64 = arg.parse().unwrap_or(0.0);
                let prec = precision.unwrap_or(6);
                let num_str = if type_char == 'E' {
                    format!("{:.prec$E}", val, prec = prec)
                } else {
                    format!("{:.prec$e}", val, prec = prec)
                };
                let left_align = flags.contains('-');

                if width > 0 && num_str.len() < width {
                    if left_align {
                        output.push_str(&format!("{:<width$}", num_str, width = width));
                    } else {
                        output.push_str(&format!("{:>width$}", num_str, width = width));
                    }
                } else {
                    output.push_str(&num_str);
                }
            }
            'g' | 'G' => {
                // Float with automatic format (use shortest representation)
                let val: f64 = arg.parse().unwrap_or(0.0);
                let prec = precision.unwrap_or(6);
                // For g/G format, choose between exponential and fixed based on magnitude
                let num_str = if val.abs() < 1e-4 || val.abs() >= 10f64.powi(prec as i32) {
                    if type_char == 'G' {
                        format!("{:.prec$E}", val, prec = prec)
                    } else {
                        format!("{:.prec$e}", val, prec = prec)
                    }
                } else {
                    // Use fixed format, strip trailing zeros
                    let fixed = format!("{:.prec$}", val, prec = prec);
                    fixed.trim_end_matches('0').trim_end_matches('.').to_string()
                };
                let left_align = flags.contains('-');

                if width > 0 && num_str.len() < width {
                    if left_align {
                        output.push_str(&format!("{:<width$}", num_str, width = width));
                    } else {
                        output.push_str(&format!("{:>width$}", num_str, width = width));
                    }
                } else {
                    output.push_str(&num_str);
                }
            }
            'c' => {
                // Character
                let ch = arg.chars().next().unwrap_or('\0');
                output.push(ch);
            }
            '%' => {
                output.push('%');
            }
            _ => {
                // Unknown specifier, output as-is
                output.push('%');
                output.push_str(&flags);
                if width > 0 {
                    output.push_str(&width.to_string());
                }
                if let Some(p) = precision {
                    output.push('.');
                    output.push_str(&p.to_string());
                }
                output.push_str(&length);
                output.push(type_char);
            }
        }
    }

    Ok(output)
}

pub fn run(options: PrintfOptions) -> Result<()> {
    // First, process escape sequences in the format string
    // Note: printf treats escape sequences differently than %b
    // The format string escapes are processed, but %b argument escapes are also processed
    let format = parse_escapes(&options.format);

    // Process format specifiers
    let output = format_printf(&format, &options.arguments)?;

    // Print without trailing newline (printf never adds newline automatically)
    print!("{}", output);

    // Flush stdout to ensure output appears immediately
    std::io::stdout().flush()?;

    Ok(())
}