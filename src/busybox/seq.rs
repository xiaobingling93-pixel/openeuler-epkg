use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::io::Write;

pub struct SeqOptions {
    pub first: f64,
    pub increment: f64,
    pub last: f64,
    pub precision: usize,  // Maximum decimal precision from input strings
    pub format: Option<String>,
    pub separator: String,
    pub equal_width: bool,
}

/// Parse a number string (integer or floating-point)
fn parse_number(s: &str) -> Result<f64> {
    s.parse::<f64>()
        .map_err(|e| eyre!("seq: invalid floating point argument '{}': {}", s, e))
}

/// Count decimal places in a number string
fn count_decimal_places(s: &str) -> usize {
    // Handle scientific notation by converting first
    let expanded = if s.contains('e') || s.contains('E') {
        // For scientific notation, we need to expand it
        if let Ok(val) = s.parse::<f64>() {
            format!("{}", val)
        } else {
            s.to_string()
        }
    } else {
        s.to_string()
    };

    if let Some(pos) = expanded.find('.') {
        let decimal_part = &expanded[pos + 1..];
        // Count all digits after decimal point (including trailing zeros)
        // This matches GNU seq behavior
        decimal_part.len()
    } else {
        0
    }
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<SeqOptions> {
    let numbers: Vec<String> = matches.get_many::<String>("numbers")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    // Parse arguments: seq [FIRST [INCREMENT]] LAST
    let (first, increment, last, precision) = match numbers.len() {
        1 => {
            let last_str = &numbers[0];
            let last = parse_number(last_str)?;
            let prec = count_decimal_places(last_str);
            (1.0, 1.0, last, prec)
        }
        2 => {
            let first_str = &numbers[0];
            let last_str = &numbers[1];
            let first = parse_number(first_str)?;
            let last = parse_number(last_str)?;
            let increment = if first <= last { 1.0 } else { -1.0 };
            let prec = count_decimal_places(first_str).max(count_decimal_places(last_str));
            (first, increment, last, prec)
        }
        3 => {
            let first_str = &numbers[0];
            let incr_str = &numbers[1];
            let last_str = &numbers[2];
            let first = parse_number(first_str)?;
            let increment = parse_number(incr_str)?;
            let last = parse_number(last_str)?;
            if increment == 0.0 {
                return Err(eyre!("seq: zero increment"));
            }
            let prec = count_decimal_places(first_str)
                .max(count_decimal_places(incr_str))
                .max(count_decimal_places(last_str));
            (first, increment, last, prec)
        }
        _ => {
            return Err(eyre!("seq: too many arguments"));
        }
    };

    let format = matches.get_one::<String>("format")
        .cloned();

    let separator = matches.get_one::<String>("separator")
        .cloned()
        .unwrap_or_else(|| "\n".to_string());

    let equal_width = matches.get_flag("equal-width");

    Ok(SeqOptions {
        first,
        increment,
        last,
        precision,
        format,
        separator,
        equal_width,
    })
}

pub fn command() -> Command {
    Command::new("seq")
        .about("Print numbers from FIRST to LAST, in steps of INCREMENT")
        .long_about("Print numbers from FIRST to LAST, in steps of INCREMENT.\n\
                     If FIRST or INCREMENT is omitted, it defaults to 1.\n\
                     FIRST, INCREMENT, and LAST are interpreted as floating point values.")
        .allow_hyphen_values(true)
        .arg(Arg::new("format")
            .short('f')
            .long("format")
            .help("Use printf style floating-point FORMAT"))
        .arg(Arg::new("separator")
            .short('s')
            .long("separator")
            .help("Use STRING to separate numbers (default: \\n)"))
        .arg(Arg::new("equal-width")
            .short('w')
            .long("equal-width")
            .action(clap::ArgAction::SetTrue)
            .help("Equalize width by padding with leading zeroes"))
        .arg(Arg::new("numbers")
            .required(true)
            .num_args(1..=3)
            .allow_hyphen_values(true)
            .help("FIRST [INCREMENT] LAST"))
}

/// Format a number using printf-style format string
/// This is a simplified version that handles common floating-point formats
fn format_with_printf(format: &str, num: f64) -> Result<String> {
    // Parse format specifier: %[flags][width][.precision]f|g|e|E
    let mut chars = format.chars().peekable();

    // Skip any leading non-% characters (they would be output literally in printf,
    // but seq format strings should only have the format specifier)
    if chars.peek() != Some(&'%') {
        return Err(eyre!("seq: invalid format '{}'", format));
    }
    chars.next(); // consume %

    // Check for %%
    if chars.peek() == Some(&'%') {
        return Err(eyre!("seq: format must be suitable for 'double' argument"));
    }

    // Parse flags: -, +, space, #, 0
    let mut flags = String::new();
    while let Some(&f) = chars.peek() {
        if f == '-' || f == '+' || f == ' ' || f == '#' || f == '0' {
            flags.push(chars.next().unwrap());
        } else {
            break;
        }
    }

    // Parse width
    let mut width: Option<usize> = None;
    let mut w = 0usize;
    while let Some(&wc) = chars.peek() {
        if wc.is_ascii_digit() {
            w = w * 10 + (chars.next().unwrap() as usize - '0' as usize);
            width = Some(w);
        } else {
            break;
        }
    }

    // Parse precision
    let mut precision: Option<usize> = None;
    if chars.peek() == Some(&'.') {
        chars.next();
        let mut p = 0usize;
        let mut has_digit = false;
        while let Some(&pc) = chars.peek() {
            if pc.is_ascii_digit() {
                p = p * 10 + (chars.next().unwrap() as usize - '0' as usize);
                precision = Some(p);
                has_digit = true;
            } else {
                break;
            }
        }
        if !has_digit {
            precision = Some(0);
        }
    }

    // Parse type specifier
    let type_char = chars.next();

    // Check for trailing characters
    if chars.next().is_some() {
        return Err(eyre!("seq: format '{}' has trailing characters", format));
    }

    match type_char {
        Some('f') | Some('F') => {
            let prec = precision.unwrap_or(6);
            let num_str = format!("{:.prec$}", num, prec = prec);
            apply_flags(&num_str, &flags, width, prec, num >= 0.0)
        }
        Some('e') => {
            let prec = precision.unwrap_or(6);
            let num_str = format!("{:.prec$e}", num, prec = prec);
            apply_flags(&num_str, &flags, width, prec, num >= 0.0)
        }
        Some('E') => {
            let prec = precision.unwrap_or(6);
            let num_str = format!("{:.prec$E}", num, prec = prec);
            apply_flags(&num_str, &flags, width, prec, num >= 0.0)
        }
        Some('g') => {
            let prec = precision.unwrap_or(6);
            // %g chooses between %e and %f based on exponent
            let num_str = if num.abs() < 1e-4 || num.abs() >= 10f64.powi(prec as i32) {
                format!("{:.prec$e}", num, prec = prec)
            } else {
                let fixed = format!("{:.prec$}", num, prec = prec);
                fixed.trim_end_matches('0').trim_end_matches('.').to_string()
            };
            apply_flags(&num_str, &flags, width, prec, num >= 0.0)
        }
        Some('G') => {
            let prec = precision.unwrap_or(6);
            let num_str = if num.abs() < 1e-4 || num.abs() >= 10f64.powi(prec as i32) {
                format!("{:.prec$E}", num, prec = prec)
            } else {
                let fixed = format!("{:.prec$}", num, prec = prec);
                fixed.trim_end_matches('0').trim_end_matches('.').to_string()
            };
            apply_flags(&num_str, &flags, width, prec, num >= 0.0)
        }
        Some('d') | Some('i') | Some('u') | Some('o') | Some('x') | Some('X') => {
            // Integer formats - round to nearest integer
            let int_val = num.round() as i64;
            let num_str = match type_char {
                Some('d') | Some('i') | Some('u') => format!("{}", int_val.abs()),
                Some('o') => format!("{:o}", int_val.abs() as u64),
                Some('x') => format!("{:x}", int_val.abs() as u64),
                Some('X') => format!("{:X}", int_val.abs() as u64),
                _ => unreachable!(),
            };
            apply_flags_int(&num_str, &flags, width, int_val >= 0, type_char.unwrap())
        }
        None => {
            return Err(eyre!("seq: format '{}' missing type specifier", format));
        }
        Some(c) => {
            return Err(eyre!("seq: format '{}' has unknown type '{}'", format, c));
        }
    }
}

/// Apply flags to formatted floating-point number string
fn apply_flags(num_str: &str, flags: &str, width: Option<usize>, _precision: usize, is_positive: bool) -> Result<String> {
    let left_align = flags.contains('-');
    let show_sign = flags.contains('+');
    let space_sign = flags.contains(' ') && !show_sign;
    let zero_pad = flags.contains('0') && !left_align;

    let mut result = num_str.to_string();

    // Handle sign for positive numbers
    if is_positive && (show_sign || space_sign) {
        if result.starts_with('-') {
            // Already has sign
        } else if show_sign {
            result.insert(0, '+');
        } else if space_sign {
            result.insert(0, ' ');
        }
    }

    // Apply width padding
    if let Some(w) = width {
        if result.len() < w {
            if left_align {
                result = format!("{:<width$}", result, width = w);
            } else if zero_pad {
                // Zero padding after sign
                if result.starts_with('-') || result.starts_with('+') || result.starts_with(' ') {
                    let sign = result.chars().next().unwrap();
                    let rest = &result[1..];
                    result = format!("{}{:0>width$}", sign, rest, width = w - 1);
                } else {
                    result = format!("{:0>width$}", result, width = w);
                }
            } else {
                result = format!("{:>width$}", result, width = w);
            }
        }
    }

    Ok(result)
}

/// Apply flags to formatted integer number string
fn apply_flags_int(num_str: &str, flags: &str, width: Option<usize>, is_positive: bool, type_char: char) -> Result<String> {
    let left_align = flags.contains('-');
    let show_sign = flags.contains('+');
    let space_sign = flags.contains(' ') && !show_sign && (type_char == 'd' || type_char == 'i');
    let zero_pad = flags.contains('0') && !left_align;
    let alt_form = flags.contains('#');

    let mut result = num_str.to_string();

    // Add alternate form prefix
    if alt_form {
        match type_char {
            'o' => {
                if !result.starts_with('0') {
                    result.insert(0, '0');
                }
            }
            'x' => result = format!("0x{}", result),
            'X' => result = format!("0X{}", result),
            _ => {}
        }
    }

    // Handle sign for signed formats
    if is_positive && (type_char == 'd' || type_char == 'i') {
        if show_sign {
            result.insert(0, '+');
        } else if space_sign {
            result.insert(0, ' ');
        }
    } else if !is_positive && (type_char == 'd' || type_char == 'i') {
        result.insert(0, '-');
    }

    // Apply width padding
    if let Some(w) = width {
        if result.len() < w {
            if left_align {
                result = format!("{:<width$}", result, width = w);
            } else if zero_pad {
                if result.starts_with('-') || result.starts_with('+') || result.starts_with(' ') {
                    let sign = result.chars().next().unwrap();
                    let rest = &result[1..];
                    result = format!("{}{:0>width$}", sign, rest, width = w - 1);
                } else if result.starts_with("0x") || result.starts_with("0X") {
                    let prefix = &result[..2];
                    let rest = &result[2..];
                    result = format!("{}{:0>width$}", prefix, rest, width = w - 2);
                } else {
                    result = format!("{:0>width$}", result, width = w);
                }
            } else {
                result = format!("{:>width$}", result, width = w);
            }
        }
    }

    Ok(result)
}

/// Format a number with given precision (matching GNU seq behavior)
fn format_number_with_precision(num: f64, precision: usize) -> String {
    if precision == 0 {
        // Integer output
        format!("{}", num.round() as i64)
    } else {
        // Fixed-point with precision
        format!("{:.prec$}", num, prec = precision)
    }
}

/// Check if sequence iteration is done
fn is_done(current: f64, last: f64, increment: f64) -> bool {
    if increment > 0.0 {
        current > last
    } else {
        current < last
    }
}

pub fn run(options: SeqOptions) -> Result<()> {
    let first = options.first;
    let increment = options.increment;
    let last = options.last;
    let separator = options.separator;
    let default_precision = options.precision;

    // Determine if the sequence is valid (increment should lead toward last)
    if increment > 0.0 && first > last {
        return Ok(()); // Empty sequence
    }
    if increment < 0.0 && first < last {
        return Ok(()); // Empty sequence
    }

    // Handle -w (equal width) option
    if options.equal_width {
        // Calculate maximum width needed
        let precision = if let Some(ref fmt) = options.format {
            // Extract precision from format string
            if let Some(pos) = fmt.find('.') {
                let after_dot = &fmt[pos + 1..];
                let mut p = 0usize;
                for c in after_dot.chars() {
                    if c.is_ascii_digit() {
                        p = p * 10 + (c as usize - '0' as usize);
                    } else {
                        break;
                    }
                }
                p
            } else {
                0
            }
        } else {
            default_precision
        };

        // Calculate max width: find the largest absolute value and its integer part width
        let first_abs = first.abs();
        let last_abs = last.abs();
        let max_abs = first_abs.max(last_abs);

        // Integer part width
        let max_int_part = if precision == 0 {
            max_abs.round() as i64
        } else {
            max_abs.floor() as i64
        };
        let base_int_width = if max_int_part == 0 { 1 } else { max_int_part.to_string().len() };

        // Check if sequence includes negative numbers
        let has_negative = (increment > 0.0 && first < 0.0) ||
                          (increment < 0.0 && last < 0.0) ||
                          (first < 0.0 && last < 0.0);

        // Width for positive numbers (including potential sign space)
        let int_width = if has_negative {
            base_int_width + 1  // Extra space for potential sign
        } else {
            base_int_width
        };

        // Generate sequence
        let mut output = String::new();
        let mut current = first;

        while !is_done(current, last, increment) {
            let formatted = if let Some(ref fmt) = options.format {
                format_with_printf(fmt, current)?
            } else {
                if current < 0.0 {
                    // Negative: keep sign, pad remaining with zeros
                    if precision == 0 {
                        let int_val = current.round() as i64;
                        let abs_val = int_val.abs();
                        format!("-{:0>width$}", abs_val, width = int_width - 1)
                    } else {
                        let abs_val = current.abs();
                        let total_width = int_width + precision;  // int_width includes space for '-'
                        format!("-{:0>width$.prec$}", abs_val, width = total_width - 1, prec = precision)
                    }
                } else {
                    // Positive or zero: pad with leading zeros
                    if precision == 0 {
                        let int_val = current.round() as i64;
                        format!("{:0>width$}", int_val, width = int_width)
                    } else {
                        let total_width = int_width + 1 + precision;
                        format!("{:0>width$.prec$}", current, width = total_width, prec = precision)
                    }
                }
            };

            if output.is_empty() {
                output.push_str(&formatted);
            } else {
                output.push_str(&separator);
                output.push_str(&formatted);
            }

            current += increment;

            // Avoid infinite loop due to floating-point precision issues
            if increment > 0.0 && current > last + increment.abs() {
                break;
            }
            if increment < 0.0 && current < last - increment.abs() {
                break;
            }
        }

        print!("{}", output);
        if separator != "\n" {
            println!();
        }
        return Ok(());
    }

    // Handle -f (format) option without -w
    if let Some(ref fmt) = options.format {
        let mut output = String::new();
        let mut current = first;

        while !is_done(current, last, increment) {
            let formatted = format_with_printf(fmt, current)?;

            if output.is_empty() {
                output.push_str(&formatted);
            } else {
                output.push_str(&separator);
                output.push_str(&formatted);
            }

            current += increment;

            // Avoid infinite loop due to floating-point precision issues
            if increment > 0.0 && current > last + increment.abs() {
                break;
            }
            if increment < 0.0 && current < last - increment.abs() {
                break;
            }
        }

        print!("{}", output);
        if separator != "\n" {
            println!();
        }
        return Ok(());
    }

    // Default output: use precision from input strings
    let mut output = String::new();
    let mut current = first;
    let mut first_output = true;

    while !is_done(current, last, increment) {
        let formatted = format_number_with_precision(current, default_precision);

        if first_output {
            output.push_str(&formatted);
            first_output = false;
        } else {
            output.push_str(&separator);
            output.push_str(&formatted);
        }

        current += increment;

        // Avoid infinite loop due to floating-point precision issues
        if increment > 0.0 && current > last + increment.abs() {
            break;
        }
        if increment < 0.0 && current < last - increment.abs() {
            break;
        }
    }

    print!("{}", output);
    if separator != "\n" {
        println!();
    }

    std::io::stdout().flush()?;
    Ok(())
}