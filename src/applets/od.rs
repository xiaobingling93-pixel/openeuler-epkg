use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs::File;
use std::io::{self, Read};

pub struct OdOptions {
    pub files: Vec<String>,
    pub address_radix: String, // d, o, x, n for decimal, octal, hex, none
    pub format_specs: Vec<String>, // format specifications like "x1", "o2", "c", etc.
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<OdOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    // Parse address radix
    let address_radix = matches.get_one::<String>("address-radix")
        .map(|s| s.as_str())
        .unwrap_or("o") // default to octal
        .to_string();

    // Parse format specifications
    let mut format_specs = Vec::new();

    // Check for -t/--format options
    if let Some(formats) = matches.get_many::<String>("format") {
        format_specs.extend(formats.cloned());
    }

    // Check for traditional format flags
    if matches.get_flag("named_chars") {
        format_specs.push("a".to_string());
    }
    if matches.get_flag("octal_bytes") {
        format_specs.push("o1".to_string());
    }
    if matches.get_flag("printable_chars") {
        format_specs.push("c".to_string());
    }
    if matches.get_flag("unsigned_decimal_2") {
        format_specs.push("u2".to_string());
    }
    if matches.get_flag("floats") {
        format_specs.push("fF".to_string());
    }
    if matches.get_flag("decimal_ints") {
        format_specs.push("dI".to_string());
    }
    if matches.get_flag("decimal_longs") {
        format_specs.push("dL".to_string());
    }
    if matches.get_flag("octal_2") {
        format_specs.push("o2".to_string());
    }
    if matches.get_flag("decimal_2") {
        format_specs.push("d2".to_string());
    }
    if matches.get_flag("hex_2") {
        format_specs.push("x2".to_string());
    }

    // Backward compatibility: if no format specs but old flags are used
    if format_specs.is_empty() {
        if matches.get_flag("characters") {
            format_specs.push("c".to_string());
        } else if matches.get_flag("hex") {
            format_specs.push("x".to_string());
        } else if matches.get_flag("octal") {
            format_specs.push("o".to_string());
        } else {
            format_specs.push("o".to_string()); // default to octal
        }
    }

    Ok(OdOptions { files, address_radix, format_specs })
}

pub fn command() -> Command {
    Command::new("od")
        .about("Dump files in octal and other formats")
        .arg(Arg::new("address-radix")
            .short('A')
            .long("address-radix")
            .value_name("RADIX")
            .help("Output format for file offsets; RADIX is one of [doxn] for Decimal, Octal, Hex or None")
            .value_parser(["d", "o", "x", "n"]))
        .arg(Arg::new("format")
            .short('t')
            .long("format")
            .value_name("TYPE")
            .help("Select output format or formats")
            .action(clap::ArgAction::Append))
        // Traditional format specifications
        .arg(Arg::new("named_chars")
            .short('a')
            .help("Same as -t a, select named characters, ignoring high-order bit")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("octal_bytes")
            .short('b')
            .help("Same as -t o1, select octal bytes")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("printable_chars")
            .short('c')
            .help("Same as -t c, select printable characters or backslash escapes")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("unsigned_decimal_2")
            .short('d')
            .help("Same as -t u2, select unsigned decimal 2-byte units")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("floats")
            .short('f')
            .help("Same as -t fF, select floats")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("decimal_ints")
            .short('i')
            .help("Same as -t dI, select decimal ints")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("decimal_longs")
            .short('l')
            .help("Same as -t dL, select decimal longs")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("octal_2")
            .short('o')
            .help("Same as -t o2, select octal 2-byte units")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("decimal_2")
            .short('s')
            .help("Same as -t d2, select decimal 2-byte units")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("hex_2")
            .short('x')
            .help("Same as -t x2, select hexadecimal 2-byte units")
            .action(clap::ArgAction::SetTrue))
        // Backward compatibility flags
        .arg(Arg::new("characters")
            .help("Print as characters (deprecated, use -c)")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("hex")
            .help("Print in hexadecimal (deprecated, use -x)")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("octal")
            .help("Print in octal (deprecated, use -o)")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files to dump (if none, read from stdin)"))
}

fn format_address(address: usize, radix: &str) -> String {
    match radix {
        "d" => format!("{:>7}", address), // decimal
        "o" => format!("{:07o}", address), // octal
        "x" => format!("{:07x}", address), // hex
        "n" => "".to_string(), // none
        _ => format!("{:07o}", address), // default to octal
    }
}

fn parse_format_spec(spec: &str) -> (String, usize) {
    // Parse format specification like "x1", "o2", "c", etc.
    if spec.len() == 1 {
        // Single character format (like "c", "a")
        (spec.to_string(), 1)
    } else if spec.len() == 2 {
        // Format with size (like "x1", "o2")
        let format_type = spec.chars().nth(0).unwrap().to_string();
        let size_char = spec.chars().nth(1).unwrap();
        let size = size_char.to_digit(10).unwrap_or(1) as usize;
        (format_type, size)
    } else {
        // Default
        ("o".to_string(), 1)
    }
}

fn format_value(value: u64, format_type: &str, unit_size: usize) -> String {
    match format_type {
        "a" => {
            // Named characters, ignoring high-order bit
            if unit_size == 1 {
                let byte = value as u8;
                match byte & 0x7F { // ignore high-order bit
                    0 => "nul".to_string(),
                    1 => "soh".to_string(),
                    2 => "stx".to_string(),
                    3 => "etx".to_string(),
                    4 => "eot".to_string(),
                    5 => "enq".to_string(),
                    6 => "ack".to_string(),
                    7 => "bel".to_string(),
                    8 => "bs".to_string(),
                    9 => "ht".to_string(),
                    10 => "nl".to_string(),
                    11 => "vt".to_string(),
                    12 => "ff".to_string(),
                    13 => "cr".to_string(),
                    14 => "so".to_string(),
                    15 => "si".to_string(),
                    16 => "dle".to_string(),
                    17 => "dc1".to_string(),
                    18 => "dc2".to_string(),
                    19 => "dc3".to_string(),
                    20 => "dc4".to_string(),
                    21 => "nak".to_string(),
                    22 => "syn".to_string(),
                    23 => "etb".to_string(),
                    24 => "can".to_string(),
                    25 => "em".to_string(),
                    26 => "sub".to_string(),
                    27 => "esc".to_string(),
                    28 => "fs".to_string(),
                    29 => "gs".to_string(),
                    30 => "rs".to_string(),
                    31 => "us".to_string(),
                    32 => "sp".to_string(),
                    127 => "del".to_string(),
                    _ => format!("{:>3}", byte as char),
                }
            } else {
                format!("{:>3}", value)
            }
        }
        "c" => {
            // Printable characters or backslash escapes
            if unit_size == 1 {
                let byte = value as u8;
                match byte {
                    0 => "\\0".to_string(),
                    7 => "\\a".to_string(),
                    8 => "\\b".to_string(),
                    9 => "\\t".to_string(),
                    10 => "\\n".to_string(),
                    11 => "\\v".to_string(),
                    12 => "\\f".to_string(),
                    13 => "\\r".to_string(),
                    27 => "\\e".to_string(),
                    32..=126 => format!("  {}", byte as char),
                    _ => format!("{:03o}", byte),
                }
            } else {
                format!("{:>3}", value)
            }
        }
        "d" => {
            // Signed decimal
            match unit_size {
                1 => format!("{:>4}", value as i8),
                2 => format!("{:>6}", value as i16),
                4 => format!("{:>11}", value as i32),
                8 => format!("{:>20}", value as i64),
                _ => format!("{:>11}", value as i32),
            }
        }
        "o" => {
            // Octal
            match unit_size {
                1 => format!("{:03o}", value),
                2 => format!("{:06o}", value),
                4 => format!("{:011o}", value),
                8 => format!("{:022o}", value),
                _ => format!("{:011o}", value),
            }
        }
        "u" => {
            // Unsigned decimal
            match unit_size {
                1 => format!("{:>3}", value),
                2 => format!("{:>5}", value),
                4 => format!("{:>10}", value),
                8 => format!("{:>20}", value),
                _ => format!("{:>10}", value),
            }
        }
        "x" => {
            // Hexadecimal
            match unit_size {
                1 => format!("{:02x}", value),
                2 => format!("{:04x}", value),
                4 => format!("{:08x}", value),
                8 => format!("{:016x}", value),
                _ => format!("{:08x}", value),
            }
        }
        "f" => {
            // Floating point (simplified - treating as float/double)
            if unit_size >= 4 {
                if unit_size == 4 {
                    format!("{:>14.7e}", value as f32)
                } else {
                    format!("{:>21.14e}", value as f64)
                }
            } else {
                format!("{:>14.7e}", value as f32)
            }
        }
        _ => format!("{:011o}", value), // default to octal
    }
}

fn dump_data(data: &[u8], address_radix: &str, format_specs: &[String]) {
    if format_specs.is_empty() {
        return;
    }

    // Use the first format spec for now (simplified implementation)
    let (format_type, unit_size) = parse_format_spec(&format_specs[0]);

    let bytes_per_line = 16;
    let mut address = 0;

    while address < data.len() {
        // Print address if not "none"
        if address_radix != "n" {
            print!("{}", format_address(address, address_radix));
        }

        let mut line_bytes = 0;
        let mut line_address = address;

        // Print values in the selected format
        while line_bytes < bytes_per_line && line_address < data.len() {
            // Read the appropriate number of bytes for the unit size
            let value = match unit_size {
                1 => {
                    if line_address < data.len() {
                        data[line_address] as u64
                    } else {
                        break;
                    }
                }
                2 => {
                    if line_address + 1 < data.len() {
                        ((data[line_address] as u16) | ((data[line_address + 1] as u16) << 8)) as u64
                    } else {
                        break;
                    }
                }
                4 => {
                    if line_address + 3 < data.len() {
                        ((data[line_address] as u32) |
                         ((data[line_address + 1] as u32) << 8) |
                         ((data[line_address + 2] as u32) << 16) |
                         ((data[line_address + 3] as u32) << 24)) as u64
                    } else {
                        break;
                    }
                }
                8 => {
                    if line_address + 7 < data.len() {
                        (data[line_address] as u64) |
                         ((data[line_address + 1] as u64) << 8) |
                         ((data[line_address + 2] as u64) << 16) |
                         ((data[line_address + 3] as u64) << 24) |
                         ((data[line_address + 4] as u64) << 32) |
                         ((data[line_address + 5] as u64) << 40) |
                         ((data[line_address + 6] as u64) << 48) |
                         ((data[line_address + 7] as u64) << 56)
                    } else {
                        break;
                    }
                }
                _ => {
                    if line_address < data.len() {
                        data[line_address] as u64
                    } else {
                        break;
                    }
                }
            };

            print!(" {}", format_value(value, &format_type, unit_size));
            line_address += unit_size;
            line_bytes += unit_size;
        }

        // For character format, also print the characters at the end
        if format_type == "c" && unit_size == 1 {
            print!("  ");
            let end_addr = std::cmp::min(address + bytes_per_line, data.len());
            for i in address..end_addr {
                let ch = if data[i] >= 32 && data[i] <= 126 {
                    data[i] as char
                } else {
                    '.'
                };
                print!("{}", ch);
            }
        }

        println!();
        address += bytes_per_line;
    }
}

pub fn run(options: OdOptions) -> Result<()> {
    let mut data = Vec::new();

    if options.files.is_empty() {
        // Read from stdin
        io::stdin()
            .read_to_end(&mut data)
            .map_err(|e| eyre!("od: error reading stdin: {}", e))?;
    } else {
        // Read from files
        for file_path in &options.files {
            let mut file = File::open(file_path)
                .map_err(|e| eyre!("od: {}: {}", file_path, e))?;

            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer)
                .map_err(|e| eyre!("od: error reading {}: {}", file_path, e))?;

            data.extend(buffer);
        }
    }

    dump_data(&data, &options.address_radix, &options.format_specs);
    Ok(())
}