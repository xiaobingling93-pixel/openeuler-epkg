use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs::File;
use std::io::{self, Read, Write};

pub struct Base64Options {
    pub files: Vec<String>,
    pub decode: bool,
    pub wrap: Option<usize>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<Base64Options> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let decode = matches.get_flag("decode");
    let wrap = matches.get_one::<String>("wrap")
        .and_then(|s| s.parse().ok());

    Ok(Base64Options { files, decode, wrap })
}

pub fn command() -> Command {
    Command::new("base64")
        .about("Base64 encode or decode data")
        .arg(Arg::new("decode")
            .short('d')
            .long("decode")
            .help("Decode data")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("wrap")
            .short('w')
            .long("wrap")
            .help("Wrap encoded lines after COLS character")
            .value_name("COLS"))
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files to process (if none, read from stdin)"))
}

const BASE64_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(input: &[u8]) -> String {
    let mut output = Vec::new();
    let mut i = 0;

    while i < input.len() {
        let mut chunk = 0u32;
        let mut bits = 0;

        for j in 0..3 {
            if i + j < input.len() {
                chunk |= (input[i + j] as u32) << (16 - j * 8);
                bits += 8;
            }
        }

        for j in 0..4 {
            if bits >= 6 {
                let index = (chunk >> (18 - j * 6)) & 0x3F;
                output.push(BASE64_CHARS[index as usize]);
                bits -= 6;
            } else {
                output.push(b'=');
            }
        }

        i += 3;
    }

    String::from_utf8(output).unwrap()
}

fn base64_decode(input: &str) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0;

    for &byte in input.as_bytes() {
        if byte == b'=' {
            break;
        }

        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b' ' | b'\t' | b'\n' | b'\r' => continue, // Skip whitespace
            _ => return Err(eyre!("base64: invalid base64 character '{}'", byte as char)),
        };

        buffer = (buffer << 6) | (value as u32);
        bits += 6;

        if bits >= 8 {
            bits -= 8;
            let byte = (buffer >> bits) & 0xFF;
            output.push(byte as u8);
        }
    }

    Ok(output)
}

fn wrap_lines(input: &str, width: usize) -> String {
    if width == 0 {
        return input.to_string();
    }

    let mut result = String::new();
    let mut line_len = 0;

    for ch in input.chars() {
        if ch == '\n' {
            line_len = 0;
        } else if line_len >= width {
            result.push('\n');
            line_len = 0;
        }
        result.push(ch);
        line_len += 1;
    }

    result
}

pub fn run(options: Base64Options) -> Result<()> {
    let mut input_data = Vec::new();

    if options.files.is_empty() {
        // Read from stdin
        io::stdin()
            .read_to_end(&mut input_data)
            .map_err(|e| eyre!("base64: error reading stdin: {}", e))?;
    } else {
        // Read from files
        for file_path in &options.files {
            let mut file = File::open(file_path)
                .map_err(|e| eyre!("base64: {}: {}", file_path, e))?;

            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer)
                .map_err(|e| eyre!("base64: error reading {}: {}", file_path, e))?;

            input_data.extend(buffer);
        }
    }

    if options.decode {
        // Decode mode
        let input_str = String::from_utf8_lossy(&input_data);
        let decoded = base64_decode(&input_str)?;
        io::stdout()
            .write_all(&decoded)
            .map_err(|e| eyre!("base64: error writing output: {}", e))?;
    } else {
        // Encode mode
        let encoded = base64_encode(&input_data);
        let output = if let Some(width) = options.wrap {
            wrap_lines(&encoded, width)
        } else {
            encoded
        };
        println!("{}", output);
    }

    Ok(())
}