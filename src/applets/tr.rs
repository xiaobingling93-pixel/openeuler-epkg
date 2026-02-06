use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};

/// Expand character classes like [:upper:], [:lower:], etc. into byte sets
fn expand_character_class(class_name: &str) -> Result<Vec<u8>> {
    match class_name {
        "alnum" => {
            let mut bytes = Vec::new();
            // ASCII alphanumeric: 0-9, A-Z, a-z
            bytes.extend(48..=57); // 0-9
            bytes.extend(65..=90); // A-Z
            bytes.extend(97..=122); // a-z
            Ok(bytes)
        }
        "alpha" => {
            let mut bytes = Vec::new();
            // ASCII alphabetic: A-Z, a-z
            bytes.extend(65..=90); // A-Z
            bytes.extend(97..=122); // a-z
            Ok(bytes)
        }
        "blank" => {
            // Tab and space (GNU order)
            Ok(vec![9, 32]) // tab, space
        }
        "cntrl" => {
            let mut bytes = Vec::new();
            // Control characters: 0-31, 127
            bytes.extend(0..=31);
            bytes.push(127);
            Ok(bytes)
        }
        "digit" => {
            // ASCII digits: 0-9
            Ok((48..=57).collect()) // 0-9
        }
        "graph" => {
            let mut bytes = Vec::new();
            // Visible characters: 33-126
            bytes.extend(33..=126);
            Ok(bytes)
        }
        "lower" => {
            // ASCII lowercase: a-z
            Ok((97..=122).collect()) // a-z
        }
        "print" => {
            let mut bytes = Vec::new();
            // Printable characters: 32-126
            bytes.extend(32..=126);
            Ok(bytes)
        }
        "punct" => {
            let mut bytes = Vec::new();
            // Punctuation: 33-47, 58-64, 91-96, 123-126
            bytes.extend(33..=47);
            bytes.extend(58..=64);
            bytes.extend(91..=96);
            bytes.extend(123..=126);
            Ok(bytes)
        }
        "space" => {
            // Whitespace: tab, newline, vertical tab, form feed, carriage return, space (GNU order)
            Ok(vec![9, 10, 11, 12, 13, 32]) // tab, \n, \v, \f, \r, space
        }
        "upper" => {
            // ASCII uppercase: A-Z
            Ok((65..=90).collect()) // A-Z
        }
        "xdigit" => {
            let mut bytes = Vec::new();
            // Hexadecimal digits: 0-9, A-F, a-f
            bytes.extend(48..=57); // 0-9
            bytes.extend(65..=70); // A-F
            bytes.extend(97..=102); // a-f
            Ok(bytes)
        }
        _ => Err(eyre!("tr: invalid character class '{}'", class_name)),
    }
}

/// Parse escape sequences like \n, \t, \0, etc.
fn parse_escape_sequence(s: &str) -> Result<(u8, usize)> {
    if s.is_empty() {
        return Err(eyre!("tr: empty escape sequence"));
    }

    let ch = s.as_bytes()[0];
    match ch {
        b'a' => Ok((7, 1)),   // \a - bell
        b'b' => Ok((8, 1)),   // \b - backspace
        b'f' => Ok((12, 1)),  // \f - form feed
        b'n' => Ok((10, 1)),  // \n - newline
        b'r' => Ok((13, 1)),  // \r - carriage return
        b't' => Ok((9, 1)),   // \t - tab
        b'v' => Ok((11, 1)),  // \v - vertical tab
        b'\\' => Ok((92, 1)), // \\ - backslash
        b'0' => {
            // \0 - null byte
            Ok((0, 1))
        }
        b'1'..=b'7' => {
            // Octal escape sequence: \OOO
            let mut octal = 0u8;
            let mut consumed = 0;
            for &digit in s.as_bytes().iter().take(3) {
                if digit >= b'0' && digit <= b'7' {
                    octal = octal * 8 + (digit - b'0');
                    consumed += 1;
                } else {
                    break;
                }
            }
            if consumed == 0 {
                return Err(eyre!("tr: invalid octal escape sequence"));
            }
            Ok((octal, consumed))
        }
        b'x' => {
            // Hex escape sequence: \xHH
            if s.len() < 3 {
                return Err(eyre!("tr: incomplete hex escape sequence"));
            }
            let hex_str = &s[1..3];
            match u8::from_str_radix(hex_str, 16) {
                Ok(val) => Ok((val, 3)),
                Err(_) => Err(eyre!("tr: invalid hex escape sequence '{}'", hex_str)),
            }
        }
        _ => {
            // Any other escaped character is literal
            Ok((ch, 1))
        }
    }
}

/// Parse a string that may contain character classes and escape sequences into a set of bytes
fn parse_set(input: &str) -> Result<Vec<u8>> {
    let bytes = parse_set_inner(input)?;
    expand_ranges(bytes)
}

/// Parse a character class sequence [:...:] and expand it
fn parse_character_class(chars: &[char], i: &mut usize, bytes: &mut Vec<u8>) -> Result<()> {
    // i points to '['; caller ensures i+2 < chars.len() and chars[i+1] == ':'
    *i += 2; // Skip [:
    let mut class_name = String::new();

    // Collect class name until :]
    while *i + 1 < chars.len() && !(chars[*i] == ':' && chars[*i + 1] == ']') {
        class_name.push(chars[*i]);
        *i += 1;
    }

    // If we didn't find :], treat the whole thing as literal characters
    if *i + 1 >= chars.len() || chars[*i] != ':' || chars[*i + 1] != ']' {
        // Push the literal '[' and ':' we skipped, plus collected chars
        bytes.push(b'[');
        bytes.push(b':');
        for c in class_name.chars() {
            bytes.push(c as u8);
        }
        // i already at position after collected chars, continue parsing from here
        return Ok(());
    }

    *i += 2; // Skip :]
    bytes.extend(expand_character_class(&class_name)?);
    Ok(())
}

/// Parse an equivalence class sequence [=...=] and expand it
fn parse_equivalence_class(chars: &[char], i: &mut usize, bytes: &mut Vec<u8>) -> Result<()> {
    // i points to '['; caller ensures i+2 < chars.len() and chars[i+1] == '='
    *i += 2; // Skip [=
    let mut class_char = String::new();

    // Collect until =]
    while *i + 1 < chars.len() && !(chars[*i] == '=' && chars[*i + 1] == ']') {
        class_char.push(chars[*i]);
        *i += 1;
    }

    // If we didn't find =], treat as literal '[=' plus collected chars
    if *i + 1 >= chars.len() || chars[*i] != '=' || chars[*i + 1] != ']' {
        bytes.push(b'[');
        bytes.push(b'=');
        for c in class_char.chars() {
            bytes.push(c as u8);
        }
        // i already at position after collected chars, continue parsing from here
        return Ok(());
    }

    *i += 2; // Skip =]
    // Equivalence class expands to the single character (first char of class)
    if let Some(c) = class_char.chars().next() {
        bytes.push(c as u8);
    }
    Ok(())
}

/// Parse a collating symbol sequence [....] and expand it
fn parse_collating_symbol(chars: &[char], i: &mut usize, bytes: &mut Vec<u8>) -> Result<()> {
    // i points to '['; caller ensures i+2 < chars.len() and chars[i+1] == '.'
    *i += 2; // Skip [.
    let mut symbol = String::new();

    // Collect until .]
    while *i + 1 < chars.len() && !(chars[*i] == '.' && chars[*i + 1] == ']') {
        symbol.push(chars[*i]);
        *i += 1;
    }

    // If we didn't find .], treat as literal '[.' plus collected chars
    if *i + 1 >= chars.len() || chars[*i] != '.' || chars[*i + 1] != ']' {
        bytes.push(b'[');
        bytes.push(b'.');
        for c in symbol.chars() {
            bytes.push(c as u8);
        }
        // i already at position after collected chars, continue parsing from here
        return Ok(());
    }

    *i += 2; // Skip .]
    // Collating symbol expands to the single character (first char of symbol)
    if let Some(c) = symbol.chars().next() {
        bytes.push(c as u8);
    }
    Ok(())
}

/// Inner parser that expands character classes, escapes, equivalence classes, and collating symbols
fn parse_set_inner(input: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];

        if ch == '[' && i + 2 < chars.len() {
            let next = chars[i + 1];
            if next == ':' {
                parse_character_class(&chars, &mut i, &mut bytes)?;
                continue;
            } else if next == '=' {
                parse_equivalence_class(&chars, &mut i, &mut bytes)?;
                continue;
            } else if next == '.' {
                parse_collating_symbol(&chars, &mut i, &mut bytes)?;
                continue;
            }
        }

        // If we get here, '[' is not part of a special sequence, treat as literal
        if ch == '[' {
            bytes.push(b'[');
            i += 1;
        } else if ch == '\\' {
            // Escape sequence
            i += 1;
            if i >= chars.len() {
                return Err(eyre!("tr: incomplete escape sequence"));
            }

            let remaining: String = chars[i..].iter().collect();
            let (byte, consumed) = parse_escape_sequence(&remaining)?;
            bytes.push(byte);
            i += consumed;
        } else {
            // Regular character - take first byte of UTF-8 representation
            let ch_str = ch.to_string();
            if let Some(&byte) = ch_str.as_bytes().first() {
                bytes.push(byte);
            }
            i += 1;
        }
    }

    Ok(bytes)
}

/// Expand ranges like a-z and A-Z in a byte sequence
fn expand_ranges(bytes: Vec<u8>) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if i + 2 < bytes.len() && bytes[i + 1] == b'-' {
            let start = bytes[i];
            let end = bytes[i + 2];
            // Check if start and end are both digits, both lowercase, or both uppercase
            let is_valid_range = (b'0' <= start && start <= b'9' && b'0' <= end && end <= b'9')
                || (b'a' <= start && start <= b'z' && b'a' <= end && end <= b'z')
                || (b'A' <= start && start <= b'Z' && b'A' <= end && end <= b'Z');
            if is_valid_range && start <= end {
                // Expand range
                for c in start..=end {
                    result.push(c);
                }
                i += 3;
                continue;
            }
        }
        // Not a range, push byte as is
        result.push(bytes[i]);
        i += 1;
    }
    Ok(result)
}

pub struct TrOptions {
    #[allow(dead_code)]
    pub set1: String, // Original string for error messages
    #[allow(dead_code)]
    pub set2: Option<String>, // Original string for error messages
    pub set1_bytes: Vec<u8>,
    pub set2_bytes: Option<Vec<u8>>,
    pub delete: bool,
    pub complement: bool,
    pub squeeze: Option<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<TrOptions> {
    let delete = matches.get_flag("delete");
    let complement = matches.get_flag("complement");
    let squeeze = matches.get_one::<String>("squeeze").cloned();

    let args: Vec<String> = matches.get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    if delete && args.len() != 1 {
        return Err(eyre!("tr: missing operand"));
    }

    if !delete && args.len() != 2 {
        return Err(eyre!("tr: missing operand"));
    }

    let set1_bytes = parse_set(&args[0])?;
    let set2_bytes = if args.len() > 1 { Some(parse_set(&args[1])?) } else { None };

    Ok(TrOptions {
        set1: args[0].clone(), // Keep original for error messages
        set2: set2_bytes.as_ref().map(|_| args[1].clone()), // Keep original for error messages
        set1_bytes,
        set2_bytes,
        delete,
        complement,
        squeeze,
    })
}

pub fn command() -> Command {
    Command::new("tr")
        .about("Translate or delete characters")
        .arg(Arg::new("delete")
            .short('d')
            .long("delete")
            .help("Delete characters in SET1")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("complement")
            .short('c')
            .long("complement")
            .help("Complement the set of characters in SET1")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("squeeze")
            .short('s')
            .long("squeeze-repeats")
            .help("Replace each sequence of a repeated character with a single occurrence")
            .value_name("SET"))
        .arg(Arg::new("args")
            .num_args(1..3)
            .help("Character sets")
            .required(true))
}

fn build_translation_map(options: &TrOptions) -> HashMap<u8, Option<u8>> {
    let mut translation_map = HashMap::new();

    // Compute effective set1 bytes (complement if needed)
    let effective_set1 = if options.complement {
        let set1_set: std::collections::HashSet<u8> = options.set1_bytes.iter().cloned().collect();
        (0..=255u8).filter(|b| !set1_set.contains(b)).collect::<Vec<_>>()
    } else {
        options.set1_bytes.clone()
    };

    if options.delete {
        // Delete bytes in effective set1
        for &byte in &effective_set1 {
            translation_map.insert(byte, None);
        }
    } else if let Some(ref set2_bytes) = options.set2_bytes {
        // Translate bytes from effective set1 to set2
        for (i, &byte1) in effective_set1.iter().enumerate() {
            if i < set2_bytes.len() {
                translation_map.insert(byte1, Some(set2_bytes[i]));
            } else {
                // If set2 is shorter, last byte of set2 is repeated
                translation_map.insert(byte1, Some(*set2_bytes.last().unwrap()));
            }
        }
    }

    translation_map
}

fn build_squeeze_set(options: &TrOptions) -> Result<HashSet<u8>> {
    let mut squeeze_set = HashSet::new();
    if let Some(squeeze_str) = &options.squeeze {
        let squeeze_bytes = parse_set(squeeze_str)?;
        for &byte in &squeeze_bytes {
            squeeze_set.insert(byte);
        }
    }
    Ok(squeeze_set)
}

fn process_chunk(chunk: &[u8], translation_map: &HashMap<u8, Option<u8>>, squeeze_set: &HashSet<u8>, delete: bool, last_byte: &mut Option<u8>) -> Vec<u8> {
    let mut output = Vec::new();

    for &byte in chunk {
        let translated_byte = if delete {
            if translation_map.contains_key(&byte) {
                None
            } else {
                Some(byte)
            }
        } else {
            translation_map.get(&byte).cloned().unwrap_or(Some(byte))
        };

        if let Some(translated) = translated_byte {
            // Handle squeezing
            if squeeze_set.contains(&translated) {
                if Some(translated) != *last_byte {
                    output.push(translated);
                    *last_byte = Some(translated);
                }
            } else {
                output.push(translated);
                *last_byte = Some(translated);
            }
        } else {
            *last_byte = None;
        }
    }

    output
}

pub fn run(options: TrOptions) -> Result<()> {
    let translation_map = build_translation_map(&options);
    let squeeze_set = build_squeeze_set(&options)?;

    let mut stdin = io::stdin();
    let mut buffer = [0u8; 8192];
    let mut last_byte: Option<u8> = None;

    loop {
        let bytes_read = stdin.read(&mut buffer)
            .map_err(|e| eyre!("tr: error reading input: {}", e))?;

        if bytes_read == 0 {
            break;
        }

        let output = process_chunk(&buffer[..bytes_read], &translation_map, &squeeze_set, options.delete, &mut last_byte);

        // Write output as bytes
        io::stdout().write_all(&output)
            .map_err(|e| eyre!("tr: error writing output: {}", e))?;
    }

    Ok(())
}