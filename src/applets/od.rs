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
    let format_specs = collect_format_specs(matches);

    Ok(OdOptions { files, address_radix, format_specs })
}

fn collect_format_specs(matches: &clap::ArgMatches) -> Vec<String> {
    let mut format_specs = Vec::new();

    // Check for -t/--format options
    if let Some(formats) = matches.get_many::<String>("format") {
        format_specs.extend(formats.cloned());
    }

    // Check for traditional format flags
    if matches.get_flag("named_chars") { format_specs.push("a".to_string()); }
    if matches.get_flag("octal_bytes") { format_specs.push("o1".to_string()); }
    if matches.get_flag("printable_chars") { format_specs.push("c".to_string()); }
    if matches.get_flag("unsigned_decimal_2") { format_specs.push("u2".to_string()); }
    if matches.get_flag("floats") { format_specs.push("f4".to_string()); }
    if matches.get_flag("decimal_ints") { format_specs.push("d4".to_string()); }
    if matches.get_flag("decimal_longs") { format_specs.push("d8".to_string()); }
    if matches.get_flag("octal_2") { format_specs.push("o2".to_string()); }
    if matches.get_flag("decimal_2") { format_specs.push("d2".to_string()); }
    if matches.get_flag("hex_2") { format_specs.push("x2".to_string()); }
    if matches.get_flag("hex_bytes_B") { format_specs.push("o2".to_string()); }
    if matches.get_flag("unsigned_decimal_4_D") { format_specs.push("u4".to_string()); }
    if matches.get_flag("float_e") { format_specs.push("f8".to_string()); }
    if matches.get_flag("float_F") { format_specs.push("f8".to_string()); }
    if matches.get_flag("hex_4_H") { format_specs.push("x4".to_string()); }
    if matches.get_flag("hex_4_X") { format_specs.push("x4".to_string()); }
    if matches.get_flag("hex_2_h") { format_specs.push("x2".to_string()); }
    if matches.get_flag("octal_4_O") { format_specs.push("o4".to_string()); }
    if matches.get_flag("signed_decimal_8_I") { format_specs.push("d8".to_string()); }
    if matches.get_flag("signed_decimal_8_L") { format_specs.push("d8".to_string()); }
    if matches.get_flag("unsigned_decimal_2_u") { format_specs.push("u2".to_string()); }

    // Default to octal if no format specified
    if format_specs.is_empty() {
        format_specs.push("o2".to_string());
    }

    format_specs
}

fn traditional_format_args() -> Vec<Arg> {
    vec![
        Arg::new("named_chars")
            .short('a')
            .help("Same as -t a, select named characters, ignoring high-order bit")
            .action(clap::ArgAction::SetTrue),
        Arg::new("octal_bytes")
            .short('b')
            .help("Same as -t o1, select octal bytes")
            .action(clap::ArgAction::SetTrue),
        Arg::new("printable_chars")
            .short('c')
            .help("Same as -t c, select printable characters or backslash escapes")
            .action(clap::ArgAction::SetTrue),
        Arg::new("unsigned_decimal_2")
            .short('d')
            .help("Same as -t u2, select unsigned decimal 2-byte units")
            .action(clap::ArgAction::SetTrue),
        Arg::new("floats")
            .short('f')
            .help("Same as -t f4, select single-precision floating point")
            .action(clap::ArgAction::SetTrue),
        Arg::new("decimal_ints")
            .short('i')
            .help("Same as -t d4, select signed decimal 4-byte units")
            .action(clap::ArgAction::SetTrue),
        Arg::new("decimal_longs")
            .short('l')
            .help("Same as -t dL, select decimal longs")
            .action(clap::ArgAction::SetTrue),
        Arg::new("octal_2")
            .short('o')
            .help("Same as -t o2, select octal 2-byte units")
            .action(clap::ArgAction::SetTrue),
        Arg::new("decimal_2")
            .short('s')
            .help("Same as -t d2, select decimal 2-byte units")
            .action(clap::ArgAction::SetTrue),
        Arg::new("hex_2")
            .short('x')
            .help("Same as -t x2, select hexadecimal 2-byte units")
            .action(clap::ArgAction::SetTrue),
        Arg::new("hex_bytes_B")
            .short('B')
            .help("Same as -t o2, select octal 2-byte units")
            .action(clap::ArgAction::SetTrue),
        Arg::new("unsigned_decimal_4_D")
            .short('D')
            .help("Same as -t u4, select unsigned decimal 4-byte units")
            .action(clap::ArgAction::SetTrue),
        Arg::new("float_e")
            .short('e')
            .help("Same as -t f8, select double-precision floating point")
            .action(clap::ArgAction::SetTrue),
        Arg::new("float_F")
            .short('F')
            .help("Same as -t f8, select double-precision floating point")
            .action(clap::ArgAction::SetTrue),
        Arg::new("hex_4_H")
            .short('H')
            .help("Same as -t x4, select hexadecimal 4-byte units")
            .action(clap::ArgAction::SetTrue),
        Arg::new("hex_4_X")
            .short('X')
            .help("Same as -t x4, select hexadecimal 4-byte units")
            .action(clap::ArgAction::SetTrue),
        Arg::new("hex_2_h")
            .short('h')
            .help("Same as -t x2, select hexadecimal 2-byte units")
            .action(clap::ArgAction::SetTrue),
        Arg::new("octal_4_O")
            .short('O')
            .help("Same as -t o4, select octal 4-byte units")
            .action(clap::ArgAction::SetTrue),
        Arg::new("signed_decimal_8_I")
            .short('I')
            .help("Same as -t d8, select signed decimal 8-byte units")
            .action(clap::ArgAction::SetTrue),
        Arg::new("signed_decimal_8_L")
            .short('L')
            .help("Same as -t d8, select signed decimal 8-byte units")
            .action(clap::ArgAction::SetTrue),
        Arg::new("unsigned_decimal_2_u")
            .short('u')
            .help("Same as -t u2, select unsigned decimal 2-byte units")
            .action(clap::ArgAction::SetTrue),
    ]
}

pub fn command() -> Command {
    let mut cmd = Command::new("od")
        .disable_help_flag(true)
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
        .arg(Arg::new("help").long("help").action(clap::ArgAction::Help));

    // Traditional format specifications
    for arg in traditional_format_args() {
        cmd = cmd.arg(arg);
    }

    cmd.arg(Arg::new("files")
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

fn format_a(value: u64, unit_size: usize) -> String {
    // Named characters, ignoring high-order bit
    if unit_size == 1 {
        let byte = value as u8;
        // BusyBox buggy behavior for non-DESKTOP tests
        if byte >= 128 {
            // High bytes printed as 2-digit hex, right-aligned to 3 chars
            return format!("{:>3}", format!("{:02x}", byte));
        }
        match byte & 0x7F { // ignore high-order bit
            0 => format!("{:>3}", "nul"),
            1 => format!("{:>3}", "soh"),
            2 => format!("{:>3}", "stx"),
            3 => format!("{:>3}", "etx"),
            4 => format!("{:>3}", "eot"),
            5 => format!("{:>3}", "enq"),
            6 => format!("{:>3}", "ack"),
            7 => format!("{:>3}", "bel"),
            8 => format!("{:>3}", "bs"),
            9 => format!("{:>3}", "ht"),
            10 => format!("{:>3}", "lf"),  // BusyBox uses "lf" not "nl"
            11 => format!("{:>3}", "vt"),
            12 => format!("{:>3}", "ff"),
            13 => format!("{:>3}", "cr"),
            14 => format!("{:>3}", "so"),
            15 => format!("{:>3}", "si"),
            16 => format!("{:>3}", "dle"),
            17 => format!("{:>3}", "dc1"),
            18 => format!("{:>3}", "dc2"),
            19 => format!("{:>3}", "dc3"),
            20 => format!("{:>3}", "dc4"),
            21 => format!("{:>3}", "nak"),
            22 => format!("{:>3}", "syn"),
            23 => format!("{:>3}", "etb"),
            24 => format!("{:>3}", "can"),
            25 => format!("{:>3}", "em"),
            26 => format!("{:>3}", "sub"),
            27 => format!("{:>3}", "esc"),
            28 => format!("{:>3}", "fs"),
            29 => format!("{:>3}", "gs"),
            30 => format!("{:>3}", "rs"),
            31 => format!("{:>3}", "us"),
            32 => format!("{:>3}", "sp"),
            127 => format!("{:>3}", "del"),
            _ => format!("{:>3}", byte as char),
        }
    } else {
        format!("{:>3}", value)
    }
}

fn format_c(value: u64, unit_size: usize) -> String {
    // Printable characters or backslash escapes
    if unit_size == 1 {
        let byte = value as u8;
        match byte {
            0 => " \\0".to_string(),
            7 => " \\a".to_string(),
            8 => " \\b".to_string(),
            9 => " \\t".to_string(),
            10 => " \\n".to_string(),
            11 => " \\v".to_string(),
            12 => " \\f".to_string(),
            13 => " \\r".to_string(),
            27 => " \\e".to_string(),
            32..=126 => format!("  {}", byte as char),
            _ => format!("{:03o}", byte),
        }
    } else {
        format!("{:>3}", value)
    }
}

fn format_d(value: u64, unit_size: usize) -> String {
    // Signed decimal
    match unit_size {
        1 => format!("{:>4}", value as i8),
        2 => format!("{:>6}", value as i16),
        4 => format!("{:>11}", value as i32),
        8 => format!("{:>20}", value as i64),
        _ => format!("{:>11}", value as i32),
    }
}

fn format_o(value: u64, unit_size: usize) -> String {
    // Octal
    match unit_size {
        1 => format!("{:03o}", value),
        2 => format!("{:06o}", value),
        4 => format!("{:011o}", value),
        8 => format!("{:022o}", value),
        _ => format!("{:011o}", value),
    }
}

fn format_u(value: u64, unit_size: usize) -> String {
    // Unsigned decimal
    match unit_size {
        1 => format!("{:>3}", value),
        2 => format!("{:>5}", value),
        4 => format!("{:>10}", value),
        8 => format!("{:>20}", value),
        _ => format!("{:>10}", value),
    }
}

fn format_x(value: u64, unit_size: usize) -> String {
    // Hexadecimal
    match unit_size {
        1 => format!("{:02x}", value),
        2 => format!("{:04x}", value),
        4 => format!("{:08x}", value),
        8 => format!("{:016x}", value),
        _ => format!("{:08x}", value),
    }
}

fn format_f(value: u64, unit_size: usize) -> String {
    // Floating point
    let s = match unit_size {
        4 => {
            let bits = value as u32;
            let f = f32::from_bits(bits);
            format!("{:.7e}", f)
        }
        8 => {
            let f = f64::from_bits(value);
            format!("{:.14e}", f)
        }
        _ => format!("{:e}", value as f64),
    };
    // Ensure exponent has '+' if positive
    let s = if s.contains('e') && !s.contains("e-") {
        s.replace("e", "e+")
    } else {
        s
    };
    // Column widths: double 25, single 16 (including separator space)
    // dump_data adds one space before each value, so pad to width-1
    let col_width = if unit_size == 4 { 16 } else { 25 };
    let pad_width = col_width - 1;
    format!("{:>pad_width$}", s, pad_width = pad_width)
}

fn format_default(value: u64) -> String {
    format!("{:011o}", value)
}

fn format_value(value: u64, format_type: &str, unit_size: usize) -> String {
    match format_type {
        "a" => format_a(value, unit_size),
        "c" => format_c(value, unit_size),
        "d" => format_d(value, unit_size),
        "o" => format_o(value, unit_size),
        "u" => format_u(value, unit_size),
        "x" => format_x(value, unit_size),
        "f" => format_f(value, unit_size),
        _ => format_default(value),
    }
}

fn read_unit(data: &[u8], address: usize, unit_size: usize) -> Option<u64> {
    match unit_size {
        1 => {
            if address < data.len() {
                Some(data[address] as u64)
            } else {
                None
            }
        }
        2 => {
            if address + 1 < data.len() {
                Some(((data[address] as u16) | ((data[address + 1] as u16) << 8)) as u64)
            } else {
                None
            }
        }
        4 => {
            if address + 3 < data.len() {
                Some(((data[address] as u32) |
                     ((data[address + 1] as u32) << 8) |
                     ((data[address + 2] as u32) << 16) |
                     ((data[address + 3] as u32) << 24)) as u64)
            } else {
                None
            }
        }
        8 => {
            if address + 7 < data.len() {
                Some((data[address] as u64) |
                     ((data[address + 1] as u64) << 8) |
                     ((data[address + 2] as u64) << 16) |
                     ((data[address + 3] as u64) << 24) |
                     ((data[address + 4] as u64) << 32) |
                     ((data[address + 5] as u64) << 40) |
                     ((data[address + 6] as u64) << 48) |
                     ((data[address + 7] as u64) << 56))
            } else {
                None
            }
        }
        _ => {
            if address < data.len() {
                Some(data[address] as u64)
            } else {
                None
            }
        }
    }
}

fn print_line_values(data: &[u8], start_address: usize, unit_size: usize, format_type: &str, bytes_per_line: usize) -> usize {
    let mut line_bytes = 0;
    let mut line_address = start_address;

    while line_bytes < bytes_per_line && line_address < data.len() {
        if let Some(value) = read_unit(data, line_address, unit_size) {
            print!(" {}", format_value(value, format_type, unit_size));
            line_address += unit_size;
            line_bytes += unit_size;
        } else {
            break;
        }
    }
    line_bytes
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

        let _line_bytes = print_line_values(data, address, unit_size, &format_type, bytes_per_line);
        println!();
        address += bytes_per_line;
    }
    // Print final offset line
    if address_radix != "n" && data.len() > 0 {
        println!("{}", format_address(data.len(), address_radix));
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
