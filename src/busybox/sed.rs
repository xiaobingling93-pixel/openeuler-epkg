use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use regex::Regex;
use std::borrow::Cow;
use std::io;
use std::collections::HashMap;
use crate::lfs;

fn sed_read_line_error(file_path: Option<&str>, e: io::Error) -> color_eyre::Report {
    let file_info = file_path.map(|p| format!(" '{}'", p)).unwrap_or_default();
    eyre!("sed: error reading{}: {}", file_info, e)
}

pub struct SedOptions {
    pub scripts: Vec<String>,
    pub inplace: bool,
    pub extended_regex: bool,
    pub quiet: bool,
    pub files: Vec<String>,
    pub version: bool,
}

#[derive(Default)]
struct SedState {
    pattern_space: String,
    hold_space: String,
    test_flag: bool,
    labels: HashMap<String, usize>,
    previous_regex: Option<String>,
    next_command_index: Option<usize>,
    n_command_pending: bool,
    n_append_pending: bool,
    /// When N hits EOF, exit without printing (GNU behavior)
    n_append_hit_eof: bool,
    /// When n hits EOF, exit main loop (already printed current line in n)
    n_hit_eof: bool,
    // Pattern range state
    #[allow(dead_code)] pattern_range_active: bool,
    pattern_range_start_matched: bool,
    pattern_range_end_matched: bool,
    pattern_offset_remaining: i64,
    pattern_offset_target: i64,
    pattern_start_line: u64,
    #[allow(dead_code)] pattern_end_line: u64,
    /// Line number at start of current cycle (before any N in this cycle). Used so that after 1{N;N;d} the next line (4) can match 2,3 (range "skipped").
    cycle_start_line_number: u64,
    /// Last line number we completed a cycle for (printed or discarded). Used for numeric range matching when lines were skipped by N/d.
    last_processed_line_number: u64,
    /// When set, N should resume from within this group (index in the current command list) at next_command_index.
    continue_group_index: Option<usize>,
}

fn make_initial_state(commands: &[AddressedCommand]) -> SedState {
    let mut state = SedState { ..Default::default() };
    for (idx, cmd) in commands.iter().enumerate() {
        if let SedCommand::Label(label) = &cmd.command {
            state.labels.insert(label.clone(), idx);
        }
    }
    state
}

struct ApplyCommandsInput<'a> {
    line: &'a str,
    commands: &'a [AddressedCommand],
    extended_regex: bool,
    quiet: bool,
    line_number: u64,
    total_lines: Option<u64>,
    line_has_newline: bool,
    start_index: usize,
    /// Line number at start of this cycle (set when we first read this line; unchanged by N). 0 when not tracking.
    cycle_start_line_number: u64,
    /// When running inside a group, index of the Group command in the parent list so N can resume within the group.
    parent_group_index: Option<usize>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<SedOptions> {
    let mut scripts = Vec::new();
    let version = matches.get_flag("version");

    // Get scripts from -e flags
    if let Some(script_vals) = matches.get_many::<String>("expression") {
        scripts.extend(script_vals.cloned());
    }

    // Get scripts from -f script files (read as bytes so NULs are preserved in script content)
    if let Some(paths) = matches.get_many::<String>("file") {
        for path in paths {
            let bytes = std::fs::read(path)
                .map_err(|e| eyre!("sed: cannot read {}: {}", path, e))?;
            // Split by newline only; lines may contain NUL
            let content = bytes.split(|&b| b == b'\n')
                .map(|line| String::from_utf8_lossy(line).into_owned())
                .collect::<Vec<_>>()
                .join("\n");
            scripts.push(content);
        }
    }

    // If no -e/-f, check for positional script argument
    if scripts.is_empty() && !version {
        if let Some(script) = matches.get_one::<String>("script") {
            scripts.push(script.clone());
        } else {
            return Err(eyre!("sed: missing script"));
        }
    }

    let inplace = matches.get_flag("inplace");
    let extended_regex = matches.get_flag("extended") || matches.get_flag("regexp-extended");
    let quiet = matches.get_flag("quiet") || matches.get_flag("silent");

    let mut files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();
    // When script came from -e or -f, the first positional (index 1) is a file (BusyBox/GNU: "sed -e 'x' -i input")
    let has_e_or_f = matches.get_many::<String>("expression").map(|v| v.len() > 0).unwrap_or(false)
        || matches.get_many::<String>("file").map(|v| v.len() > 0).unwrap_or(false);
    if has_e_or_f {
        if let Some(s) = matches.get_one::<String>("script") {
            files.insert(0, s.clone());
        }
    }

    Ok(SedOptions { scripts, inplace, extended_regex, quiet, files, version })
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
        .arg(Arg::new("file")
            .short('f')
            .help("Add the contents of script-file to the commands to be executed")
            .value_name("SCRIPT")
            .action(clap::ArgAction::Append))
        .arg(Arg::new("version")
            .long("version")
            .help("Output version information and exit")
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

#[derive(Debug, Clone)]
enum Address {
    LineNumber(u64),
    LastLine,
    Range(u64, u64),
    RangeTo(u64),           // ,end
    RangeFrom(u64),         // start,
    RangeOffset(u64, i64),  // start,+offset or start,-offset
    Pattern(String),
    PatternTo(String, u64), // /pattern/,end
    PatternOffset(String, i64), // /pattern/,+offset
    RangeToPattern(String), // ,/pattern/
    RangePattern(u64, String), // start,/pattern/
    #[allow(dead_code)] PatternRange(String, u64), // /pattern/,end (same as PatternTo, keep for compatibility)
    PatternPattern(String, #[allow(dead_code)] String), // /pattern/,/pattern/
}

#[derive(Debug)]
enum SedCommand {
    Substitution {
        pattern: String,
        replacement: String,
        flags: String,
        delimiter: char,
    },
    Delete,
    Print,
    Quit,
    NoOp,
    Insert(String),
    Append(String),
    Change(#[allow(dead_code)] String),
    Branch(Option<String>),
    TestBranch(Option<String>),
    TestBranchNot(Option<String>),
    Next,
    NextAppend,
    PrintFirst,
    Hold,
    HoldAppend,
    GetHold,
    GetHoldAppend,
    Exchange,
    Write(String),
    Label(String),
    Equals,
    Zap,  // z: delete first line of pattern space (up to and including first newline)
    Group(Vec<AddressedCommand>),
}

#[derive(Debug)]
struct AddressedCommand {
    address: Option<Address>,
    command: SedCommand,
    compiled_pattern: Option<Regex>,
}

fn parse_substitution(s: &str) -> Result<(char, String, String, String, &str)> {
    let delimiter = s.chars().next().ok_or_else(|| eyre!("sed: missing delimiter in substitution command"))?;
    let mut i = 1; // position after delimiter
    let mut bracket_depth = 0;
    let mut escaped = false;
    let chars: Vec<char> = s.chars().collect();
    // Find pattern end
    while i < chars.len() {
        let c = chars[i];
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '[' {
            bracket_depth += 1;
        } else if c == ']' && bracket_depth > 0 {
            bracket_depth -= 1;
        } else if c == delimiter && bracket_depth == 0 {
            break;
        }
        i += 1;
    }
    if i >= chars.len() {
        let mut err_msg = String::from("sed: invalid substitution syntax");
        if bracket_depth > 0 {
            err_msg.push_str(": unclosed bracket in regex pattern");
        } else if escaped {
            err_msg.push_str(": trailing backslash in pattern");
        } else if i == 1 {
            err_msg.push_str(": missing pattern and closing delimiter");
        }
        err_msg.push_str(&format!(" in 's{}'", s));
        return Err(eyre!(err_msg));
    }
    let pattern = s[1..i].to_string();
    // Move past delimiter; replacement is literal (no character-class bracket matching)
    i += 1;
    let start_repl = i;
    escaped = false;
    while i < chars.len() {
        let c = chars[i];
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == delimiter {
            break;
        }
        i += 1;
    }
    let replacement = s[start_repl..i].to_string();
    // Skip optional third delimiter (i is either at third delimiter or end of string)
    if i < chars.len() && chars[i] == delimiter {
        i += 1;
    }
    // Collect flag characters (letters/digits) starting at i
    let flags_start = i;
    let mut flags_end = flags_start;
    while flags_end < chars.len() {
        let c = chars[flags_end];
        if c.is_ascii_alphanumeric() {
            flags_end += 1;
        } else {
            break;
        }
    }
    let flags = s[flags_start..flags_end].to_string();
    let remaining = &s[flags_end..];
    Ok((delimiter, pattern, replacement, flags, remaining))
}

struct SubstitutionFlags {
    global: bool,
    print: bool,
    write_filename: Option<String>,
    occurrence: Option<usize>,
    case_insensitive: bool,
    multiline: bool,
}

/// Unescape text for a/i/c commands (sed.c copy_parsing_escapes then parse_escapes with 0,0).
/// Count backslashes before n/t/r: odd -> expand to newline/tab/cr; even -> output (count/2) backslashes + literal char.
/// So \\\n -> \\ + newline; \\n -> \n; \n -> newline. Same for t and r. \X -> X.
fn unescape_append_insert_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            let start = i;
            while i < bytes.len() && bytes[i] == b'\\' {
                i += 1;
            }
            let count = i - start;
            if i < bytes.len() {
                let c = bytes[i] as char;
                match c {
                    'n' => {
                        i += 1;
                        for _ in 0..(count / 2) {
                            out.push('\\');
                        }
                        if count % 2 == 1 {
                            out.push('\n');
                        } else {
                            out.push('n');
                        }
                    }
                    't' => {
                        i += 1;
                        for _ in 0..(count / 2) {
                            out.push('\\');
                        }
                        if count % 2 == 1 {
                            out.push('\t');
                        } else {
                            out.push('t');
                        }
                    }
                    'r' => {
                        i += 1;
                        for _ in 0..(count / 2) {
                            out.push('\\');
                        }
                        if count % 2 == 1 {
                            out.push('\r');
                        } else {
                            out.push('r');
                        }
                    }
                    _ => {
                        for _ in 0..(count.saturating_sub(1)) {
                            out.push('\\');
                        }
                        if count > 0 {
                            out.push(c);
                            i += 1;
                        } else {
                            out.push('\\');
                        }
                    }
                }
            } else {
                // Trailing backslashes: pairs collapse to one (sed.c parse_escapes); \\ -> \, \\\\ -> \\, etc.
                for _ in 0..((count + 1) / 2) {
                    out.push('\\');
                }
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn parse_substitution_flags(flags: &str) -> SubstitutionFlags {
    let mut global = false;
    let mut print = false;
    let mut write_filename = None;
    let mut occurrence = None;
    let mut case_insensitive = false;
    let mut multiline = false;

    // Find 'w' flag and filename
    let mut i = 0;
    while i < flags.len() {
        let c = flags.chars().nth(i).unwrap();
        match c {
            'g' => global = true,
            'p' => print = true,
            'i' | 'I' => case_insensitive = true,
            'm' | 'M' => multiline = true,
            'w' => {
                // Remaining characters after 'w' are the filename
                let filename = &flags[i+1..];
                if !filename.is_empty() {
                    write_filename = Some(filename.to_string());
                }
                break; // filename consumes the rest of the flag string
            }
            '0'..='9' => {
                // Parse numeric occurrence
                let start = i;
                while i < flags.len() && flags.chars().nth(i).unwrap().is_ascii_digit() {
                    i += 1;
                }
                let digits = &flags[start..i];
                occurrence = digits.parse().ok();
                continue; // i already advanced
            }
            _ => {}
        }
        i += 1;
    }

    SubstitutionFlags {
        global,
        print,
        write_filename,
        occurrence,
        case_insensitive,
        multiline,
    }
}

fn parse_address_component(s: &str) -> Result<Option<(Address, usize)>, color_eyre::eyre::Error> {
    if s.is_empty() {
        return Ok(None);
    }

    if s.starts_with('$') {
        return Ok(Some((Address::LastLine, 1)));
    }

    if s.starts_with('/') {
        let rest = &s[1..];
        let mut i = 0;
        let chars: Vec<char> = rest.chars().collect();
        while i < chars.len() {
            if chars[i] == '\\' && i + 1 < chars.len() {
                i += 2;
                continue;
            }
            if chars[i] == '/' {
                let end_byte = rest.char_indices().nth(i).map(|(o, _)| o).unwrap_or(rest.len());
                let pattern = s[..=1 + end_byte].to_string();
                return Ok(Some((Address::Pattern(pattern), 2 + end_byte)));
            }
            i += 1;
        }
    }

    let mut digit_len = 0;
    while digit_len < s.len() && s.chars().nth(digit_len).unwrap().is_ascii_digit() {
        digit_len += 1;
    }
    if digit_len > 0 {
        if let Ok(line_num) = s[..digit_len].parse::<u64>() {
            return Ok(Some((Address::LineNumber(line_num), digit_len)));
        }
    }

    Ok(None)
}

fn parse_address_offset_suffix(
    first_addr: &Option<Address>,
    after_comma: &str,
    first_len: usize,
) -> Result<Option<(Address, usize)>, color_eyre::eyre::Error> {
    if after_comma.starts_with('+') || after_comma.starts_with('-') {
        let sign = after_comma.chars().next().unwrap();
        let offset_str = &after_comma[1..];
        let mut digit_len = 0;
        while digit_len < offset_str.len() && offset_str.chars().nth(digit_len).unwrap().is_ascii_digit() {
            digit_len += 1;
        }
        if digit_len > 0 {
            if let Ok(offset) = offset_str[..digit_len].parse::<u64>() {
                let offset_i64 = if sign == '+' { offset as i64 } else { -(offset as i64) };
                let total_len = first_len + 1 + (after_comma.len() - offset_str.len()) + digit_len;
                match first_addr {
                    Some(Address::LineNumber(start)) => {
                        return Ok(Some((Address::RangeOffset(*start, offset_i64), total_len)));
                    }
                    Some(Address::Pattern(pattern)) => {
                        return Ok(Some((Address::PatternOffset(pattern.clone(), offset_i64), total_len)));
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(None)
}

fn parse_address_range_suffix(
    first_addr: &Option<Address>,
    after_comma: &str,
    first_len: usize,
) -> Result<Option<(Address, usize)>, color_eyre::eyre::Error> {
    match parse_address_component(after_comma)? {
        Some((second_addr, second_len)) => {
            let total_len = first_len + 1 + second_len;
            match (first_addr, second_addr) {
                (Some(Address::LineNumber(start)), Address::LineNumber(end)) => {
                    return Ok(Some((Address::Range(*start, end), total_len)));
                }
                (Some(Address::Pattern(pattern)), Address::LineNumber(end)) => {
                    return Ok(Some((Address::PatternTo(pattern.clone(), end), total_len)));
                }
                (Some(Address::LineNumber(start)), Address::LastLine) => {
                    return Ok(Some((Address::Range(*start, u64::MAX), total_len)));
                }
                (Some(Address::Pattern(pattern)), Address::LastLine) => {
                    return Ok(Some((Address::PatternTo(pattern.clone(), u64::MAX), total_len)));
                }
                (Some(Address::LineNumber(start)), Address::Pattern(pattern)) => {
                    return Ok(Some((Address::RangePattern(*start, pattern), total_len)));
                }
                (Some(Address::Pattern(pattern1)), Address::Pattern(pattern2)) => {
                    return Ok(Some((Address::PatternPattern(pattern1.clone(), pattern2), total_len)));
                }
                _ => {}
            }
        }
        None => {
            let total_len = first_len + 1;
            match first_addr {
                Some(Address::LineNumber(start)) => {
                    return Ok(Some((Address::RangeFrom(*start), total_len)));
                }
                Some(Address::Pattern(pattern)) => {
                    return Ok(Some((Address::PatternTo(pattern.clone(), u64::MAX), total_len)));
                }
                None => {
                    match parse_address_component(after_comma)? {
                        Some((addr, second_len)) => {
                            let total_len = first_len + 1 + second_len;
                            match addr {
                                Address::LineNumber(end) => {
                                    return Ok(Some((Address::RangeTo(end), total_len)));
                                }
                                Address::LastLine => {
                                    return Ok(Some((Address::RangeTo(u64::MAX), total_len)));
                                }
                                Address::Pattern(pattern) => {
                                    return Ok(Some((Address::RangeToPattern(pattern), total_len)));
                                }
                                _ => {}
                            }
                        }
                        None => {}
                    }
                }
                _ => {}
            }
        }
    }
    Ok(None)
}

fn parse_address(script: &str) -> Result<(Option<Address>, &str)> {
    let script = script.trim_start();

    let (first_addr, first_len) = match parse_address_component(script)? {
        Some((addr, len)) => (Some(addr), len),
        None => (None, 0),
    };

    let after_first = &script[first_len..].trim_start();

    if after_first.starts_with(',') {
        let after_comma = &after_first[1..].trim_start();

        if let Some((addr, total_len)) = parse_address_offset_suffix(&first_addr, after_comma, first_len)? {
            return Ok((Some(addr), &script[total_len..].trim_start()));
        }
        if let Some((addr, total_len)) = parse_address_range_suffix(&first_addr, after_comma, first_len)? {
            return Ok((Some(addr), &script[total_len..].trim_start()));
        }
    }

    if let Some(addr) = &first_addr {
        return Ok((Some(addr.clone()), after_first));
    }

    Ok((None, script))
}

/// Split script by ';' only when brace depth is 0 (so segments respect brace groups).
/// POSIX/sed: command separator is newline or semicolon, not NUL.
fn split_commands_at_semicolon(script: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut depth = 0u32;
    let mut start = 0;
    let mut i = 0;
    let chars: Vec<char> = script.chars().collect();
    while i < chars.len() {
        let c = chars[i];
        match c {
            '{' => depth = depth.saturating_add(1),
            '}' => depth = depth.saturating_sub(1),
            ';' if depth == 0 => {
                // Include delimiter so brace groups get their closing "};" in the same segment
                segments.push(script[start..=i].trim());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start <= chars.len() {
        let s = script[start..].trim();
        if !s.is_empty() {
            segments.push(s);
        }
    }
    segments
}

fn merge_sed_segments_with_braces(source: &str) -> Vec<String> {
    let raw_segments: Vec<&str> = split_commands_at_semicolon(source)
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    let mut segments: Vec<String> = Vec::new();
    let mut j = 0;
    while j < raw_segments.len() {
        let mut merged = raw_segments[j].to_string();
        let mut depth = 0u32;
        for c in merged.chars() {
            match c {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        j += 1;
        while depth > 0 && j < raw_segments.len() {
            merged.push(';');
            merged.push_str(raw_segments[j]);
            for c in raw_segments[j].chars() {
                match c {
                    '{' => depth += 1,
                    '}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
            j += 1;
        }
        if !merged.trim().is_empty() {
            segments.push(merged);
        }
    }
    segments
}

/// Find the position of the matching closing '}' for the first '{' in s (0-indexed char offset).
/// Returns None if no matching brace. The first character of s is assumed to be '{'.
fn find_matching_brace(s: &str) -> Option<usize> {
    let mut depth = 1u32; // inside the opening '{'
    for (i, c) in s.chars().enumerate() {
        if i == 0 {
            continue; // skip the opening '{'
        }
        match c {
            '{' => depth = depth.saturating_add(1),
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn sed_cmd_rest_after(cmd: &str, len: usize) -> &str {
    if cmd.len() <= len {
        ""
    } else {
        cmd[len..].trim_start().trim_start_matches(';').trim_start()
    }
}

fn parse_sed_simple_command(cmd: &str) -> Option<(SedCommand, &str)> {
    if cmd == "d" || cmd.starts_with("d;") || cmd.starts_with("d ") {
        Some((SedCommand::Delete, sed_cmd_rest_after(cmd, 1)))
    } else if cmd == "p" || cmd.starts_with("p;") || cmd.starts_with("p ") {
        Some((SedCommand::Print, sed_cmd_rest_after(cmd, 1)))
    } else if cmd == "q" || cmd.starts_with("q;") || cmd.starts_with("q ") {
        Some((SedCommand::Quit, sed_cmd_rest_after(cmd, 1)))
    } else if cmd == "n" || cmd.starts_with("n;") || cmd.starts_with("n ") {
        Some((SedCommand::Next, sed_cmd_rest_after(cmd, 1)))
    } else if cmd == "N" || cmd.starts_with("N;") || cmd.starts_with("N ") {
        Some((SedCommand::NextAppend, sed_cmd_rest_after(cmd, 1)))
    } else if cmd == "P" || cmd.starts_with("P;") || cmd.starts_with("P ") {
        Some((SedCommand::PrintFirst, sed_cmd_rest_after(cmd, 1)))
    } else if cmd == "=" || cmd.starts_with("=;") || cmd.starts_with("= ") {
        Some((SedCommand::Equals, sed_cmd_rest_after(cmd, 1)))
    } else if cmd == "G" || cmd.starts_with("G;") || cmd.starts_with("G ") {
        Some((SedCommand::GetHoldAppend, sed_cmd_rest_after(cmd, 1)))
    } else if cmd == "g" || cmd.starts_with("g;") || cmd.starts_with("g ") {
        Some((SedCommand::GetHold, sed_cmd_rest_after(cmd, 1)))
    } else if cmd == "h" || cmd.starts_with("h;") || cmd.starts_with("h ") {
        Some((SedCommand::Hold, sed_cmd_rest_after(cmd, 1)))
    } else if cmd == "H" || cmd.starts_with("H;") || cmd.starts_with("H ") {
        Some((SedCommand::HoldAppend, sed_cmd_rest_after(cmd, 1)))
    } else if cmd == "x" || cmd.starts_with("x;") || cmd.starts_with("x ") {
        Some((SedCommand::Exchange, sed_cmd_rest_after(cmd, 1)))
    } else if cmd == "z" || cmd.starts_with("z;") || cmd.starts_with("z ") {
        Some((SedCommand::Zap, sed_cmd_rest_after(cmd, 1)))
    } else if cmd == ";" || cmd.starts_with(";") {
        Some((SedCommand::NoOp, sed_cmd_rest_after(cmd, 1)))
    } else {
        None
    }
}

fn parse_sed_complex_command(cmd: &str) -> Result<Option<(SedCommand, &str)>, color_eyre::eyre::Error> {
    let first_char = cmd.chars().next().unwrap();
    match first_char {
        'b' => {
            let after = cmd[1..].trim_start();
            let label_end = after.find(';').unwrap_or(after.len());
            let label_str = after[..label_end].trim();
            let label = if label_str.is_empty() { None } else { Some(label_str.to_string()) };
            let skip = cmd.len() - after.len() + label_end + if after.find(';').is_some() { 1 } else { 0 };
            Ok(Some((SedCommand::Branch(label), if skip <= cmd.len() { sed_cmd_rest_after(cmd, skip) } else { "" })))
        }
        't' => {
            let after = cmd[1..].trim_start();
            let label_end = after.find(';').unwrap_or(after.len());
            let label_str = after[..label_end].trim();
            let label = if label_str.is_empty() { None } else { Some(label_str.to_string()) };
            let skip = cmd.len() - after.len() + label_end + if after.find(';').is_some() { 1 } else { 0 };
            Ok(Some((SedCommand::TestBranch(label), if skip <= cmd.len() { sed_cmd_rest_after(cmd, skip) } else { "" })))
        }
        'T' => {
            let after = cmd[1..].trim_start();
            let label_end = after.find(';').unwrap_or(after.len());
            let label_str = after[..label_end].trim();
            let label = if label_str.is_empty() { None } else { Some(label_str.to_string()) };
            let skip = cmd.len() - after.len() + label_end + if after.find(';').is_some() { 1 } else { 0 };
            Ok(Some((SedCommand::TestBranchNot(label), if skip <= cmd.len() { sed_cmd_rest_after(cmd, skip) } else { "" })))
        }
        ':' => {
            let after = cmd[1..].trim_start();
            let label_end = after.find(';').unwrap_or(after.len());
            let label_str = after[..label_end].trim();
            if label_str.is_empty() {
                return Err(eyre!("sed: missing label for ':' command"));
            }
            let skip = cmd.len() - after.len() + label_end + if after.find(';').is_some() { 1 } else { 0 };
            Ok(Some((SedCommand::Label(label_str.to_string()), if skip <= cmd.len() { sed_cmd_rest_after(cmd, skip) } else { "" })))
        }
        'w' => {
            let after = cmd[1..].trim_start();
            let end = after.find(';').unwrap_or(after.len());
            let filename = after[..end].trim();
            if filename.is_empty() {
                return Err(eyre!("sed: missing filename for w command"));
            }
            let skip = cmd.len() - after.len() + end + if after.find(';').is_some() { 1 } else { 0 };
            Ok(Some((SedCommand::Write(filename.to_string()), if skip <= cmd.len() { sed_cmd_rest_after(cmd, skip) } else { "" })))
        }
        'c' => {
            let after = cmd[1..].trim_start();
            let end = after.find(';').unwrap_or(after.len());
            let text = after[..end].trim();
            if text.is_empty() {
                return Err(eyre!("sed: missing text for c command"));
            }
            let skip = cmd.len() - after.len() + end + if after.find(';').is_some() { 1 } else { 0 };
            Ok(Some((SedCommand::Change(text.to_string()), if skip <= cmd.len() { sed_cmd_rest_after(cmd, skip) } else { "" })))
        }
        'i' => {
            let mut after = cmd[1..].trim_start_matches(|c| c == ' ' || c == '\t');
            if after.starts_with('\\') {
                after = &after[1..];
            }
            if after.starts_with('\n') {
                after = &after[1..];
            }
            let text = after.trim_end();
            let unescaped = unescape_append_insert_text(text);
            Ok(Some((SedCommand::Insert(unescaped), "")))
        }
        'a' => {
            let mut after = cmd[1..].trim_start_matches(|c| c == ' ' || c == '\t');
            if after.starts_with('\\') {
                after = &after[1..];
            }
            if after.starts_with('\n') {
                after = &after[1..];
            }
            let text = after.trim_end();
            let unescaped = unescape_append_insert_text(text);
            Ok(Some((SedCommand::Append(unescaped), "")))
        }
        _ => Ok(None),
    }
}

/// Returns (command, remainder of script after this command).
fn parse_command(script: &str, extended_regex: bool) -> Result<(AddressedCommand, &str)> {
    let (address, remaining) = parse_address(script)?;
    let remaining = remaining.trim();
    let cmd = remaining.trim_start();

    let (command, rest) = if cmd.starts_with('s') {
        // Parse substitution command
        let script_after_s = &cmd[1..]; // Remove 's'
        let (delimiter, pattern, replacement, flags, rem) = parse_substitution(script_after_s)?;
        (SedCommand::Substitution { pattern, replacement, flags, delimiter }, rem)
    } else if let Some((simple_cmd, simple_rest)) = parse_sed_simple_command(cmd) {
        (simple_cmd, simple_rest)
    } else if cmd.is_empty() {
        (SedCommand::NoOp, "")
    } else if cmd.starts_with('{') {
        // Brace group: find matching } and parse inner content
        let from_brace = cmd;
        if let Some(close_pos) = find_matching_brace(from_brace) {
            let inner = from_brace[1..close_pos].trim();
            let rest_after = from_brace[close_pos + 1..].trim_start().trim_start_matches(';').trim_start();
            let group_commands = parse_script_to_commands(inner, extended_regex)?;
            (SedCommand::Group(group_commands), rest_after)
        } else {
            let snippet: String = cmd.chars().take(30).collect();
            if cmd.len() > 30 {
                return Err(eyre!("sed: unmatched '{{' in '{}...'", snippet));
            } else {
                return Err(eyre!("sed: unmatched '{{' in '{}'", cmd));
            }
        }
    } else {
        // Handle complex commands with arguments: b, t, T, :, w, c, i, a
        match parse_sed_complex_command(cmd)? {
            Some((complex_cmd, complex_rest)) => (complex_cmd, complex_rest),
            None => return Err(eyre!("sed: unsupported command '{}'", cmd)),
        }
    };

    // Compile regex pattern if we have a pattern address (with (?m) for ^/$ line boundaries)
    let compiled_pattern = compile_pattern_for_address(&address, extended_regex)?;

    Ok((AddressedCommand { address, command, compiled_pattern }, rest))
}

/// Unescape sed pattern content: \\ -> \, \/ -> /, \$ -> $, \n -> newline, \t -> tab, etc.
fn unescape_sed_pattern(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                match n {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    _ => out.push(n),
                }
            } else {
                out.push(c);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// When extended_regex is false, escape extended metacharacters so they are literal (basic regex behavior).
fn escape_basic_regex_pattern(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                if n == '$' {
                    // \$ in sed = end anchor; in regex we need literal \ + $, so "\\$"
                    out.push_str("\\$");
                } else {
                    out.push(c);
                    out.push(n);
                }
            } else {
                out.push(c);
            }
            continue;
        }
        if matches!(c, '|' | '(' | ')' | '?' | '+' | '{' | '}') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Compile address pattern. Do NOT use (?m): in sed.c get_address uses xregcomp with
/// G.regex_type only, so $ matches end of string (end-of-line), not after newlines.
/// For pattern \$ (backslash at end), allow optional trailing newline so that "line with \n"
/// matches (BusyBox test: /$_in_regex/ should not match newlines, only end-of-line).
fn compile_address_regex(pattern_content: &str, extended_regex: bool) -> Option<Regex> {
    if pattern_content.is_empty() {
        return None;
    }
    let mut pat = if extended_regex {
        pattern_content.to_string()
    } else {
        escape_basic_regex_pattern(pattern_content)
    };
    // Pattern \$ (backslash at end): match only at absolute end of pattern space (man sed; BusyBox test).
    // Use \z so we don't match $ before embedded newlines (regex crate $ matches before \n).
    if pattern_content.ends_with('$')
        && pattern_content.chars().rev().nth(1) == Some('\\')
    {
        pat = r"\\\n?\z".to_string();
    }
    Regex::new(&pat).ok()
}

fn compile_address_regex_result(pattern_content: &str, pattern: &str, extended_regex: bool) -> Result<Regex> {
    let mut pat = if extended_regex {
        pattern_content.to_string()
    } else {
        escape_basic_regex_pattern(pattern_content)
    };
    if pattern_content.ends_with('$')
        && pattern_content.chars().rev().nth(1) == Some('\\')
    {
        pat = r"\\\n?\z".to_string();
    }
    Regex::new(&pat).map_err(|e| eyre!("sed: invalid regex pattern '{}': {}", pattern, e))
}

fn compile_pattern_for_address(address: &Option<Address>, extended_regex: bool) -> Result<Option<Regex>> {
    match address {
        Some(Address::Pattern(pattern)) |
        Some(Address::PatternTo(pattern, _)) |
        Some(Address::PatternOffset(pattern, _)) |
        Some(Address::RangeToPattern(pattern)) |
        Some(Address::RangePattern(_, pattern)) |
        Some(Address::PatternRange(pattern, _)) => {
            let raw = if pattern.len() >= 2 { &pattern[1..pattern.len()-1] } else { "" };
            let pattern_content = unescape_sed_pattern(raw);
            Ok(Some(compile_address_regex_result(&pattern_content, pattern, extended_regex)?))
        }
        Some(Address::PatternPattern(pattern1, _)) => {
            let raw = if pattern1.len() >= 2 { &pattern1[1..pattern1.len()-1] } else { "" };
            let pattern_content = unescape_sed_pattern(raw);
            Ok(Some(compile_address_regex_result(&pattern_content, pattern1, extended_regex)?))
        }
        _ => Ok(None),
    }
}

fn compile_address_pattern(address: &Address, extended_regex: bool) -> Option<Regex> {
    let pattern_content = match address {
        Address::Pattern(p) | Address::PatternTo(p, _) | Address::PatternOffset(p, _)
        | Address::RangeToPattern(p) | Address::RangePattern(_, p) | Address::PatternRange(p, _) => {
            let raw = if p.len() >= 2 { &p[1..p.len()-1] } else { "" };
            unescape_sed_pattern(raw)
        }
        Address::PatternPattern(p1, _) => {
            let raw = if p1.len() >= 2 { &p1[1..p1.len()-1] } else { "" };
            unescape_sed_pattern(raw)
        }
        _ => return None,
    };
    compile_address_regex(&pattern_content, extended_regex)
}

fn pattern_address_multiline_zero_width_adjust(
    mut matched: bool,
    pattern_space: &str,
    compiled_pattern: Option<&Regex>,
) -> bool {
    if matched && pattern_space.contains('\n') {
        if let Some(regex) = compiled_pattern {
            if let Some(m) = regex.find(pattern_space) {
                let len = pattern_space.len();
                let zero_width_at_end = m.start() == len && m.end() == len;
                let ends_with_backslash_then_newline = len >= 2
                    && pattern_space.as_bytes()[len - 2] == b'\\'
                    && pattern_space.as_bytes()[len - 1] == b'\n';
                if zero_width_at_end && pattern_space.ends_with('\n') && !ends_with_backslash_then_newline {
                    matched = false;
                }
            }
        }
    }
    matched
}

fn address_maybe_set_previous_regex(address: &Address, state: &mut SedState, matches: bool) {
    if !matches {
        return;
    }
    let content = match address {
        Address::Pattern(p) | Address::PatternTo(p, _) | Address::PatternOffset(p, _)
        | Address::RangeToPattern(p) | Address::RangePattern(_, p) | Address::PatternRange(p, _)
        | Address::PatternPattern(p, _) => {
            if p.len() >= 2 { &p[1..p.len()-1] } else { "" }
        }
        _ => "",
    };
    if !content.is_empty() {
        state.previous_regex = Some(content.to_string());
    }
}

/// Check if address matches current line and update pattern range state.
fn address_matches(
    address: &Address,
    line_number: u64,
    total_lines: Option<u64>,
    state: &mut SedState,
    compiled_pattern: Option<&Regex>,
    extended_regex: bool,
) -> bool {
    let matches = match address {
        Address::LineNumber(n) => *n == line_number,
        Address::LastLine => total_lines.map_or(false, |total| line_number == total),
        Address::Range(start, end) => {
            if end < start {
                // Reversed range (e.g. 2,1): BusyBox test expects only line start+1 to match
                line_number == start.saturating_add(1)
            } else {
                let in_range = line_number >= *start && line_number <= *end;
                // After 1{N;N;d} we skip lines 2,3; next line is 4. Range 2,3 should still match line 4 (BusyBox test "sed with N skipping lines past ranges on next cmds").
                let skipped_past = *end < line_number && state.last_processed_line_number < *start;
                in_range || skipped_past
            }
        }
        Address::RangeTo(end) => line_number <= *end,
        Address::RangeFrom(start) => line_number >= *start,
        Address::RangeOffset(start, offset) => {
            if *offset >= 0 {
                let end = *start + *offset as u64;
                line_number >= *start && line_number <= end
            } else {
                // negative offset, range is empty? sed likely treats as start to start-offset?
                // For now, treat as no match
                false
            }
        }
        Address::Pattern(p) => {
            let mut matched = if p == "//" {
                state.previous_regex.as_ref()
                    .and_then(|r| compile_address_regex(r, extended_regex))
                    .map_or(false, |re| re.is_match(&state.pattern_space))
            } else {
                compiled_pattern.map_or(false, |regex| regex.is_match(&state.pattern_space))
            };
            matched = pattern_address_multiline_zero_width_adjust(matched, &state.pattern_space, compiled_pattern);
            if matched {
                state.pattern_range_start_matched = true;
                state.pattern_start_line = line_number;
            }
            matched
        }
        Address::PatternTo(_, end) => {
            // Check if pattern matches now
            let pattern_matched = compiled_pattern.map_or(false, |regex| regex.is_match(&state.pattern_space));
            if pattern_matched {
                state.pattern_range_start_matched = true;
                state.pattern_start_line = line_number;
            }
            // Range is from pattern match to end line
            pattern_matched || (state.pattern_range_start_matched && line_number <= *end && line_number >= state.pattern_start_line)
        }
        Address::PatternOffset(_, offset) => {
            // Check if pattern matches now
            let pattern_matched = compiled_pattern.map_or(false, |regex| regex.is_match(&state.pattern_space));
            if pattern_matched {
                state.pattern_offset_target = *offset;
                state.pattern_offset_remaining = if *offset >= 0 { *offset } else { 0 };
            }
            // Match if pattern matches now, or we're within +N lines after a match (decrement only for the extra lines)
            let in_range = state.pattern_offset_remaining > 0;
            if in_range && !pattern_matched {
                state.pattern_offset_remaining -= 1;
            }
            pattern_matched || in_range
        }
        Address::RangeToPattern(_) => {
            // Check if pattern matches now
            let pattern_matched = compiled_pattern.map_or(false, |regex| regex.is_match(&state.pattern_space));
            if pattern_matched {
                state.pattern_range_end_matched = true;
            }
            // Match lines until pattern matches (including the matching line)
            pattern_matched || !state.pattern_range_end_matched
        }
        Address::RangePattern(start, _) => {
            if line_number < *start {
                false
            } else {
                // Check if pattern matches now
                let pattern_matched = compiled_pattern.map_or(false, |regex| regex.is_match(&state.pattern_space));
                if pattern_matched {
                    state.pattern_range_end_matched = true;
                }
                // Match lines from start until pattern matches (including the matching line)
                pattern_matched || !state.pattern_range_end_matched
            }
        }
        Address::PatternRange(_, end) => {
            // Same as PatternTo
            // Check if pattern matches now
            let pattern_matched = compiled_pattern.map_or(false, |regex| regex.is_match(&state.pattern_space));
            if pattern_matched {
                state.pattern_range_start_matched = true;
                state.pattern_start_line = line_number;
            }
            // Range is from pattern match to end line
            pattern_matched || (state.pattern_range_start_matched && line_number <= *end && line_number >= state.pattern_start_line)
        }
        Address::PatternPattern(_, _) => {
            // TODO: implement pattern range matching with state
            false
        }
    };

    address_maybe_set_previous_regex(address, state, matches);
    matches
}

fn substitution_apply_regex_to_pattern_space(
    regex: &Regex,
    pattern_space: &str,
    processed_replacement: &str,
    parsed_flags: &SubstitutionFlags,
) -> (String, bool) {
    if parsed_flags.global {
        let cow = regex.replace_all(pattern_space, processed_replacement);
        let changed = matches!(cow, Cow::Owned(_));
        (cow.to_string(), changed)
    } else if let Some(n) = parsed_flags.occurrence {
        let mut new_line = String::new();
        let mut last_end = 0;
        let mut count = 0;
        for mat in regex.find_iter(pattern_space) {
            count += 1;
            if count == n {
                new_line.push_str(&pattern_space[last_end..mat.start()]);
                new_line.push_str(processed_replacement);
                last_end = mat.end();
                break;
            }
        }
        if count == n {
            new_line.push_str(&pattern_space[last_end..]);
            (new_line, true)
        } else {
            (pattern_space.to_string(), false)
        }
    } else {
        let cow = regex.replace(pattern_space, processed_replacement);
        let changed = matches!(cow, Cow::Owned(_));
        (cow.to_string(), changed)
    }
}

/// Handle substitution command (s///).
fn handle_substitution(
    state: &mut SedState,
    pattern: &str,
    replacement: &str,
    flags: &str,
    delimiter: char,
    extended_regex: bool,
    prints: &mut Vec<String>,
) -> Result<bool> {
    let parsed_flags = parse_substitution_flags(flags);

    // Empty pattern uses previous regex (from address or last s)
    let pattern_to_use = if pattern.is_empty() {
        state.previous_regex.as_deref().unwrap_or("")
    } else {
        pattern
    };

    let mut regex_builder = String::new();

    // Case insensitive flag
    if parsed_flags.case_insensitive {
        regex_builder.push_str("(?i)");
    }

    // Multiline flag
    if parsed_flags.multiline {
        regex_builder.push_str("(?m)");
    }

    let escaped_pattern = if extended_regex {
        pattern_to_use.to_string()
    } else {
        // BRE: \( \) capture groups; \| alternation (POSIX BRE)
        let mut s = pattern_to_use.to_string();
        s = s.replace("\\(", "(").replace("\\)", ")").replace("\\|", "|");
        s
    };
    regex_builder.push_str(&escaped_pattern);

    let full_pattern = regex_builder;

    let regex = Regex::new(&full_pattern)
        .map_err(|e| eyre!("sed: invalid regex '{}': {}", pattern_to_use, e))?;

    if !pattern_to_use.is_empty() {
        state.previous_regex = Some(pattern_to_use.to_string());
    }

    let processed_replacement = process_replacement(replacement, Some(delimiter));

    let (new_ps, substitution_occurred) = substitution_apply_regex_to_pattern_space(
        &regex,
        &state.pattern_space,
        processed_replacement.as_str(),
        &parsed_flags,
    );
    state.pattern_space = new_ps;

    if substitution_occurred {
        state.test_flag = true;

        // Write to file if w flag present
        if let Some(filename) = &parsed_flags.write_filename {
            use std::fs::OpenOptions;
            use std::io::Write;
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(filename)
                .map_err(|e| eyre!("sed: cannot open '{}' for writing: {}", filename, e))?;
            writeln!(file, "{}", state.pattern_space)
                .map_err(|e| eyre!("sed: write error '{}': {}", filename, e))?;
        }

        // Print if p flag present
        if parsed_flags.print {
            prints.push(state.pattern_space.clone());
        }
    }

    Ok(substitution_occurred)
}

/// Handle branch commands (b, t, T).
/// Returns `Ok(Some(new_index))` if branch should be taken, `Ok(None)` otherwise.
fn handle_branch(
    state: &mut SedState,
    label: Option<&String>,
    commands_len: usize,
    should_branch: bool,
    clear_test_flag: bool,
) -> Result<Option<usize>> {
    if !should_branch {
        return Ok(None);
    }
    let target_idx = if let Some(label) = label {
        match state.labels.get(label) {
            Some(pos) => *pos + 1,
            None => return Err(eyre!("sed: undefined label '{}'", label)),
        }
    } else {
        commands_len
    };
    if clear_test_flag {
        state.test_flag = false;
    }
    Ok(Some(target_idx))
}

/// Parse a single script string into commands (used for brace-group inner script).
/// Applies continuation (trailing \) within the script before parsing.
fn parse_script_to_commands(script: &str, extended_regex: bool) -> Result<Vec<AddressedCommand>> {
    let logical = build_logical_lines(&[script.to_string()]);
    parse_script_to_commands_from_lines(&logical, extended_regex)
}

fn parse_script_to_commands_from_lines(lines: &[String], extended_regex: bool) -> Result<Vec<AddressedCommand>> {
    let mut commands = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].as_str().trim();
        if line.is_empty() {
            i += 1;
            continue;
        }
        let (address, remaining) = parse_address(line)?;
        let remaining_trimmed = remaining.trim_start();
        let mut rest_after_brace: Option<String> = None;

        // Check for brace group (trim_start so "1{ s/s/c/" is recognized)
        if remaining_trimmed.starts_with('{') {
            // Collect lines until matching '}'
            let mut group_content = String::new();
            let mut brace_depth = 1;
            let mut current_line_content = &remaining_trimmed[1..]; // Skip opening brace

            loop {
                // Find matching brace in current line (by depth, not first '}')
                let mut found_close = false;
                let mut end_byte = 0;
                for (byte_idx, ch) in current_line_content.char_indices() {
                    if ch == '{' {
                        brace_depth += 1;
                    } else if ch == '}' {
                        brace_depth -= 1;
                        if brace_depth == 0 {
                            end_byte = byte_idx;
                            found_close = true;
                            break;
                        }
                    }
                }
                if found_close {
                    group_content.push_str(&current_line_content[..end_byte]);
                    let rest = current_line_content[end_byte + 1..].trim();
                    if !rest.is_empty() {
                        rest_after_brace = Some(rest.to_string());
                    }
                }

                if found_close {
                    let group_commands = parse_script_to_commands(&group_content, extended_regex)?;
                    let command = SedCommand::Group(group_commands);
                    let compiled_pattern = compile_pattern_for_address(&address, extended_regex)?;
                    commands.push(AddressedCommand { address: address.clone(), command, compiled_pattern });
                    i += 1;
                    break;
                }

                // Haven't found closing brace yet, add current line content and move to next line
                group_content.push_str(current_line_content);
                group_content.push('\n');
                i += 1;
                if i >= lines.len() {
                    return Err(eyre!("sed: unmatched '{{' before end of script"));
                }
                current_line_content = lines[i].as_str().trim();
            }
            if rest_after_brace.is_none() {
                continue;
            }
        }

        // Segment source: rest after closing brace (e.g. "} p") or full remaining
        let segments_source = rest_after_brace.as_deref().unwrap_or(remaining);

        // Check for i\ and a\ commands: only when line ends with single \ (continuation), like sed.c add_cmd
        let trailing_backslashes = remaining_trimmed.chars().rev().take_while(|&c| c == '\\').count();
        let is_continuation_line = trailing_backslashes % 2 == 1;
        if is_continuation_line && (remaining_trimmed.starts_with('i') || remaining_trimmed.starts_with('a')) {
            let command_char = remaining_trimmed.chars().next().unwrap();
            let raw = if i + 1 < lines.len() { lines[i + 1].to_string() } else { String::new() };
            let text = unescape_append_insert_text(&raw);
            let command = match command_char {
                'i' => SedCommand::Insert(text),
                'a' => SedCommand::Append(text),
                _ => unreachable!(),
            };
            let compiled_pattern = compile_pattern_for_address(&address, extended_regex)?;
            commands.push(AddressedCommand { address: address.clone(), command, compiled_pattern });
            i += 2; // skip text line
        } else {
            // remaining (or rest_after_brace) contains commands (split by ; or NUL only at brace depth 0)
            // Merge segments that have unclosed braces (e.g. " /s/ { s/s/c/ " with " }; p")
            let segments = merge_sed_segments_with_braces(segments_source);
            let mut skip_next_line = false;
            for (idx, segment) in segments.iter().enumerate() {
                let segment = segment.trim();
                if segment.is_empty() {
                    continue;
                }
                // a\ or i\ at end of line: text is on the next line (sed script continuation)
                let seg_trim = segment.trim();
                if seg_trim.len() == 2 && (seg_trim.starts_with('a') || seg_trim.starts_with('i')) && seg_trim.ends_with('\\') && i + 1 < lines.len() {
                    let text = unescape_append_insert_text(&lines[i + 1]);
                    skip_next_line = true;
                    let command = if seg_trim.starts_with('a') {
                        SedCommand::Append(text)
                    } else {
                        SedCommand::Insert(text)
                    };
                    let compiled_pattern = compile_pattern_for_address(&address, extended_regex)?;
                    commands.push(AddressedCommand {
                        address: if idx == 0 { address.clone() } else { None },
                        command,
                        compiled_pattern,
                    });
                    continue;
                }
                let mut seg_str: String = segment.to_string();
                let mut first_cmd_in_segment = true;
                while !seg_str.trim().is_empty() {
                    let (mut cmd, rest) = parse_command(&seg_str, extended_regex)?;
                    if idx == 0 && first_cmd_in_segment {
                        cmd.address = address.clone();
                        cmd.compiled_pattern = address.as_ref().and_then(|a| compile_address_pattern(a, extended_regex));
                        first_cmd_in_segment = false;
                    }
                    commands.push(cmd);
                    seg_str = rest.trim_start().trim_start_matches(';').trim_start().to_string();
                    if seg_str.is_empty() {
                        break;
                    }
                }
            }
            i += 1;
            if skip_next_line {
                i += 1;
            }
        }
    }
    Ok(commands)
}

fn apply_commands(
    input: &ApplyCommandsInput<'_>,
    state: &mut SedState,
    last_puts_char: Option<&mut char>,
    write_file_last: &mut Option<&mut HashMap<String, char>>,
) -> Result<(String, bool, Vec<String>, Vec<String>, Vec<String>, bool, bool)> {
    state.pattern_space = input.line.to_string();
    let mut should_print = true;
    let mut prints = Vec::new();
    let mut inserts = Vec::new();
    let mut appends = Vec::new();
    let mut should_quit = false;
    let mut should_next = false;

    // eprintln!("DEBUG: labels: {:?}", state.labels);
    let mut i = input.start_index;
    while i < input.commands.len() {
        let cmd = &input.commands[i];
        // eprintln!("DEBUG: cmd {:?}", cmd.command);
        i += 1;

        // Check if address matches current line
        if let Some(address) = &cmd.address {
            if !address_matches(address, input.line_number, input.total_lines, state, cmd.compiled_pattern.as_ref(), input.extended_regex) {
                continue;
            }
        }

        #[allow(unreachable_patterns)]
        match &cmd.command {
            SedCommand::Substitution { pattern, replacement, flags, delimiter } => {
                handle_substitution(state, pattern, replacement, flags, *delimiter, input.extended_regex, &mut prints)?;
            }
            SedCommand::Delete => {
                if input.cycle_start_line_number != 0 {
                    state.last_processed_line_number = input.cycle_start_line_number;
                }
                state.continue_group_index = None;
                should_print = false;
                break;
            }
            SedCommand::Print => {
                // p prints the entire pattern space (BusyBox case 'p': sed_puts(pattern_space, ...))
                prints.push(state.pattern_space.clone());
            }
            SedCommand::Equals => {
                // Print line number to stdout
                println!("{}", input.line_number);
            }
            SedCommand::Quit => {
                should_quit = true;
                break;
            }
            SedCommand::NoOp => {},
            SedCommand::Insert(text) => {
                inserts.push(text.clone());
            }
            SedCommand::Append(text) => {
                appends.push(text.clone());
            }
            SedCommand::Change(text) => {
                state.pattern_space = text.clone();
            }
            SedCommand::Branch(label) => {
                if let Some(target_idx) = handle_branch(state, label.as_ref(), input.commands.len(), true, false)? {
                    i = target_idx;
                    continue;
                }
            }
            SedCommand::TestBranch(label) => {
                if let Some(target_idx) = handle_branch(state, label.as_ref(), input.commands.len(), state.test_flag, true)? {
                    i = target_idx;
                    continue;
                }
            }
            SedCommand::TestBranchNot(label) => {
                if let Some(target_idx) = handle_branch(state, label.as_ref(), input.commands.len(), !state.test_flag, false)? {
                    i = target_idx;
                    continue;
                }
            }
            SedCommand::Label(_label) => {
                // eprintln!("DEBUG: label '{}'", label);
            },
            SedCommand::Next => {
                if !input.quiet {
                    if let Some(lpc) = last_puts_char {
                        maybe_newline_before(lpc);
                        if input.line_has_newline {
                            println!("{}", state.pattern_space);
                        } else {
                            print!("{}", state.pattern_space);
                        }
                        *lpc = if input.line_has_newline { '\n' } else { 'x' };
                    } else {
                        if input.line_has_newline {
                            println!("{}", state.pattern_space);
                        } else {
                            print!("{}", state.pattern_space);
                        }
                    }
                }
                state.last_processed_line_number = input.line_number;
                state.test_flag = false; // n resets the substituted/test bit
                state.n_command_pending = true;
                state.next_command_index = Some(i);
                should_next = false;
                should_print = false;
                break;
            }
            SedCommand::NextAppend => {
                state.n_append_pending = true;
                state.next_command_index = Some(i);
                state.continue_group_index = input.parent_group_index;
                should_print = false;
                break;
            }
            SedCommand::PrintFirst => {
                // P: print pattern space up to and including first newline
                let first_line = state.pattern_space.split('\n').next().unwrap_or("");
                let to_print = if state.pattern_space.contains('\n') {
                    format!("{}\n", first_line)
                } else {
                    first_line.to_string()
                };
                prints.push(to_print);
            }
            SedCommand::Zap => {
                // z: delete first line of pattern space (up to and including first newline)
                if let Some(pos) = state.pattern_space.find('\n') {
                    state.pattern_space = state.pattern_space[pos + 1..].to_string();
                } else {
                    state.pattern_space.clear();
                }
            }
            SedCommand::Hold => {
                state.hold_space = state.pattern_space.clone();
            },
            SedCommand::HoldAppend => {
                if state.hold_space.is_empty() {
                    state.hold_space = state.pattern_space.clone();
                } else {
                    state.hold_space.push('\n');
                    state.hold_space.push_str(&state.pattern_space);
                }
            },
            SedCommand::GetHold => {
                state.pattern_space = state.hold_space.clone();
            },
            SedCommand::GetHoldAppend => {
                if state.pattern_space.is_empty() {
                    state.pattern_space = state.hold_space.clone();
                } else {
                    state.pattern_space.push('\n');
                    state.pattern_space.push_str(&state.hold_space);
                }
            },
            SedCommand::Exchange => {
                std::mem::swap(&mut state.pattern_space, &mut state.hold_space);
            },
            SedCommand::Write(filename) => {
                if let Some(ref mut wfl) = *write_file_last {
                    let last = wfl.get(filename).copied().unwrap_or('\n');
                    use std::fs::OpenOptions;
                    let mut file = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(filename)
                        .map_err(|e| eyre!("sed: cannot open '{}' for writing: {}", filename, e))?;
                    use std::io::Write;
                    if last != '\n' && last != '\0' {
                        file.write_all(b"\n").map_err(|e| eyre!("sed: write error '{}': {}", filename, e))?;
                    }
                    file.write_all(state.pattern_space.as_bytes())
                        .map_err(|e| eyre!("sed: write error '{}': {}", filename, e))?;
                    if input.line_has_newline {
                        file.write_all(b"\n").map_err(|e| eyre!("sed: write error '{}': {}", filename, e))?;
                    }
                    let new_last = if input.line_has_newline { '\n' } else { 'x' };
                    wfl.insert(filename.clone(), new_last);
                } else {
                    use std::fs::OpenOptions;
                    let mut file = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(filename)
                        .map_err(|e| eyre!("sed: cannot open '{}' for writing: {}", filename, e))?;
                    use std::io::Write;
                    writeln!(file, "{}", state.pattern_space)
                        .map_err(|e| eyre!("sed: write error '{}': {}", filename, e))?;
                }
            },
            SedCommand::Group(group_commands) => {
                // Apply grouped commands recursively (parent_group_index so N can resume within this group)
                let group_cmd_index = i - 1;
                let (new_line, new_should_print, mut new_prints, mut new_inserts, mut new_appends, new_should_quit, new_should_next) = {
                    let pattern_copy = state.pattern_space.clone();
                    let line_has_nl = pattern_copy.ends_with('\n');
                    let group_input = ApplyCommandsInput {
                        line: &pattern_copy,
                        commands: group_commands,
                        extended_regex: input.extended_regex,
                        quiet: input.quiet,
                        line_number: input.line_number,
                        total_lines: input.total_lines,
                        line_has_newline: line_has_nl,
                        start_index: 0,
                        cycle_start_line_number: input.cycle_start_line_number,
                        parent_group_index: Some(group_cmd_index),
                    };
                    apply_commands(&group_input, state, None, write_file_last)?
                };
                state.pattern_space = new_line;
                if !new_should_print {
                    should_print = false;
                }
                prints.append(&mut new_prints);
                inserts.append(&mut new_inserts);
                appends.append(&mut new_appends);
                if new_should_quit {
                    should_quit = true;
                }
                if new_should_next {
                    should_next = true;
                }
                if should_quit {
                    break;
                }
                if new_should_next || state.n_append_pending {
                    break;
                }
            }
            _ => {}
        }
    }

    Ok((state.pattern_space.clone(), should_print, prints, inserts, appends, should_quit, should_next))
}

/// Split one script segment into lines; newline preceded by backslash (continuation) does not split.
fn split_script_into_lines(s: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
            current.push('\n');
            i += 2;
            continue;
        }
        if bytes[i] == b'\n' {
            lines.push(std::mem::take(&mut current));
            i += 1;
            continue;
        }
        current.push(bytes[i] as char);
        i += 1;
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// Build logical script lines from script segments (-e strings or -f file lines).
/// Like BusyBox add_cmd(): each segment is one "input line"; if a segment ends with
/// unescaped backslash (odd trailing backslashes), it is glued with the next segment
/// (with newline between). This allows -e 'i\' -e '1' to become one logical line "i\n1".
fn build_logical_lines(scripts: &[String]) -> Vec<String> {
    let input_lines: Vec<String> = scripts
        .iter()
        .flat_map(|s| split_script_into_lines(s))
        .collect();

    let mut logical = Vec::new();
    let mut i = 0;
    while i < input_lines.len() {
        let mut line = input_lines[i].clone();
        i += 1;
        loop {
            let n = line.len();
            let trailing = line.chars().rev().take_while(|&c| c == '\\').count();
            if trailing % 2 == 1 && n >= 1 {
                line = line[..n - 1].to_string();
                if i < input_lines.len() {
                    line.push('\n');
                    line.push_str(&input_lines[i]);
                    i += 1;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        logical.push(line);
    }
    logical
}

pub fn run(options: SedOptions) -> Result<()> {
    if options.version {
        println!("GNU sed version 4.8");
        return Ok(());
    }
    if options.inplace && options.files.is_empty() {
        return Err(eyre!("sed: no input files specified"));
    }
    // Build logical lines from script segments (-e / -f). Continuation (trailing \) glues
    // with the next segment (BusyBox add_cmd behavior).
    let logical_lines = build_logical_lines(&options.scripts);
    let all_commands = parse_script_to_commands_from_lines(&logical_lines, options.extended_regex)?;

    // Build label map and validate branch targets (error before reading any input)
    let mut labels: HashMap<String, usize> = HashMap::new();
    for (idx, cmd) in all_commands.iter().enumerate() {
        if let SedCommand::Label(label) = &cmd.command {
            labels.insert(label.clone(), idx);
        }
    }
    fn check_branch_labels(commands: &[AddressedCommand], labels: &HashMap<String, usize>) -> Result<()> {
        for cmd in commands {
            let label = match &cmd.command {
                SedCommand::Branch(l) | SedCommand::TestBranch(l) | SedCommand::TestBranchNot(l) => l.as_ref(),
                SedCommand::Group(group) => {
                    check_branch_labels(group, labels)?;
                    continue;
                }
                _ => continue,
            };
            if let Some(l) = label {
                if !labels.contains_key(l) {
                    return Err(eyre!("sed: undefined label '{}'", l));
                }
            }
        }
        Ok(())
    }
    check_branch_labels(&all_commands, &labels)?;

    run_streams(&options, &all_commands)
}

fn run_streams(options: &SedOptions, all_commands: &[AddressedCommand]) -> Result<()> {
    if options.files.is_empty() {
        run_stdin(all_commands, options, true)
    } else {
        let mut last_puts_char = '\n';
        let mut write_file_last = HashMap::new();
        let mut write_file_last_opt = Some(&mut write_file_last);
        let file_count = options.files.len();
        for (file_index, file_path) in options.files.iter().enumerate() {
            let is_last_file = file_index == file_count - 1;
            if file_path == "-" {
                let mut reader = io::stdin().lock();
                let mut state = make_initial_state(all_commands);
                process_input(&mut reader, all_commands, options, None, &mut state, &mut last_puts_char, &mut write_file_last_opt, is_last_file)?;
                continue;
            }

            let mut state = make_initial_state(all_commands);
            if options.inplace {
                run_inplace(file_path, all_commands, options, &mut state, &mut write_file_last_opt)?;
            } else {
                let file = std::fs::File::open(file_path)
                    .map_err(|e| eyre!("sed: cannot open '{}': {}", file_path, e))?;
                let mut reader = io::BufReader::new(file);
                process_input(&mut reader, all_commands, options, Some(file_path), &mut state, &mut last_puts_char, &mut write_file_last_opt, is_last_file)?;
            }
        }
        Ok(())
    }
}


fn run_stdin(all_commands: &[AddressedCommand], options: &SedOptions, is_last_file: bool) -> Result<()> {
    let mut reader = io::stdin().lock();
    let mut state = make_initial_state(all_commands);
    let mut last_puts_char = '\n';
    let mut write_file_last = HashMap::new();
    let mut write_file_last_opt = Some(&mut write_file_last);
    process_input(&mut reader, all_commands, options, None, &mut state, &mut last_puts_char, &mut write_file_last_opt, is_last_file)
}

fn run_inplace(
    file_path: &str,
    all_commands: &[AddressedCommand],
    options: &SedOptions,
    state: &mut SedState,
    write_file_last_opt: &mut Option<&mut HashMap<String, char>>,
) -> Result<()> {
    let content = std::fs::read_to_string(file_path)
        .map_err(|e| eyre!("sed: cannot read '{}': {}", file_path, e))?;
    let trailing_newline = content.ends_with('\n');

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len() as u64;

    let mut processed_lines = Vec::new();
    let mut last_had_appends = false;
    for (i, line) in lines.iter().enumerate() {
        let line_number = (i + 1) as u64;
        state.cycle_start_line_number = line_number;
        let input = ApplyCommandsInput {
            line: *line,
            commands: all_commands,
            extended_regex: options.extended_regex,
            quiet: options.quiet,
            line_number,
            total_lines: Some(total_lines),
            line_has_newline: true,
            start_index: 0,
            cycle_start_line_number: line_number,
            parent_group_index: None,
        };
        let (processed, should_print, _, inserts, appends, should_quit, _) = apply_commands(&input, state, None, write_file_last_opt)?;
        last_had_appends = !appends.is_empty();
        if should_print {
            for text in &inserts {
                processed_lines.push(text.clone());
            }
            processed_lines.push(processed);
            for text in &appends {
                if !text.is_empty() {
                    processed_lines.push(text.clone());
                }
            }
        }
        if should_quit {
            break;
        }
    }

    let output = processed_lines.join("\n");
    let output = if trailing_newline || last_had_appends {
        format!("{}\n", output)
    } else {
        output
    };
    lfs::write(file_path, output)?;
    Ok(())
}

fn process_replacement(replacement: &str, delimiter: Option<char>) -> String {
    // Handle basic escape sequences and backreferences.
    // When delimiter is a digit (e.g. '1'), \1 in replacement is literal "1" (GNU sed 4.8).
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
                    '&' => {
                        result.push('&');
                        chars.next();
                    }
                    '0' => {
                        // Whole match
                        result.push_str("$0");
                        chars.next();
                    }
                    '1'..='9' => {
                        // When delimiter is this digit, \digit is literal (not backreference)
                        if delimiter == Some(*next_ch) {
                            result.push(*next_ch);
                            chars.next();
                        } else {
                            result.push('$');
                            result.push(*next_ch);
                            chars.next();
                        }
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
        } else if ch == '&' {
            // Unescaped '&' means the whole matched text
            result.push_str("$0");
        } else {
            result.push(ch);
        }
    }

    result
}

/// Emit a newline before next output if last output didn't end with newline (BusyBox puts_maybe_newline).
fn maybe_newline_before(last_puts_char: &mut char) {
    if *last_puts_char != '\n' && *last_puts_char != '\0' {
        print!("\n");
        *last_puts_char = '\n';
    }
}

/// Context for the N (append next line) command loop; groups refs that are only used inside the loop.
struct NAppendLoopCtx<'a> {
    line_number: &'a mut u64,
    next_buffer: &'a mut String,
    next_bytes: &'a mut usize,
    skip_swap_this: &'a mut bool,
    n_append_consumed: &'a mut bool,
    processed: &'a mut String,
    should_print: &'a mut bool,
    prints: &'a mut Vec<String>,
    inserts: &'a mut Vec<String>,
    appends: &'a mut Vec<String>,
    should_quit: &'a mut bool,
    should_next: &'a mut bool,
}

fn handle_n_append_loop(
    reader: &mut dyn std::io::BufRead,
    commands: &[AddressedCommand],
    options: &SedOptions,
    file_path: Option<&str>,
    state: &mut SedState,
    last_puts_char: &mut char,
    write_file_last: &mut Option<&mut HashMap<String, char>>,
    ctx: &mut NAppendLoopCtx<'_>,
) -> Result<()> {
    let NAppendLoopCtx {
        line_number,
        next_buffer,
        next_bytes,
        skip_swap_this,
        n_append_consumed,
        processed,
        should_print,
        prints,
        inserts,
        appends,
        should_quit,
        should_next,
    } = ctx;
    while state.n_append_pending {
        let (next_line, _has_newline2, _bytes_read) = if !next_buffer.is_empty() {
            let has_nl = next_buffer.ends_with('\n');
            let line = if has_nl {
                next_buffer.trim_end_matches('\n')
            } else {
                next_buffer.trim_end_matches('\r')
            }.to_string();
            next_buffer.clear();
            let nb = reader.read_line(next_buffer)
                .map_err(|e| sed_read_line_error(file_path, e))?;
            **next_bytes = nb;
            **skip_swap_this = true;
            **n_append_consumed = true;
            (line, has_nl, 1usize)
        } else {
            let mut read_buf = String::new();
            let bytes_read = reader.read_line(&mut read_buf)
                .map_err(|e| sed_read_line_error(file_path, e))?;
            if bytes_read == 0 {
                state.n_append_pending = false;
                state.n_append_hit_eof = true;
                state.next_command_index = None;
                if !options.quiet {
                    maybe_newline_before(last_puts_char);
                    if state.pattern_space.ends_with('\n') {
                        print!("{}", state.pattern_space);
                        *last_puts_char = '\n';
                    } else {
                        println!("{}", state.pattern_space);
                        *last_puts_char = '\n';
                    }
                }
                break;
            }
            let has_newline2 = read_buf.ends_with('\n');
            let next_line = if has_newline2 {
                read_buf.trim_end_matches('\n')
            } else {
                read_buf.trim_end_matches('\r')
            }.to_string();
            **next_buffer = read_buf;
            **next_bytes = bytes_read;
            **skip_swap_this = true;
            **n_append_consumed = true;
            (next_line, has_newline2, bytes_read)
        };
        **line_number += 1;
        if !state.pattern_space.is_empty() {
            state.pattern_space.push('\n');
        }
        state.pattern_space.push_str(&next_line);
        let next_idx = state.next_command_index.unwrap_or(0);
        state.n_append_pending = false;
        state.next_command_index = None;
        let pattern_copy = state.pattern_space.clone();
        let line_has_nl = pattern_copy.ends_with('\n');
        let (processed2, should_print2, prints2, inserts2, appends2, should_quit2, should_next2) = {
            let (commands_to_use, start_idx, parent_grp) = if let Some(gidx) = state.continue_group_index {
                if let SedCommand::Group(ref inner) = commands[gidx].command {
                    state.continue_group_index = None;
                    (inner.as_slice(), next_idx, Some(gidx))
                } else {
                    (commands, next_idx, None)
                }
            } else {
                (commands, next_idx, None)
            };
            let input = ApplyCommandsInput {
                line: pattern_copy.as_str(),
                commands: commands_to_use,
                extended_regex: options.extended_regex,
                quiet: options.quiet,
                line_number: **line_number,
                total_lines: None,
                line_has_newline: line_has_nl,
                start_index: start_idx,
                cycle_start_line_number: state.cycle_start_line_number,
                parent_group_index: parent_grp,
            };
            apply_commands(&input, state, Some(last_puts_char), write_file_last)?
        };
        prints.extend(prints2);
        inserts.extend(inserts2);
        appends.extend(appends2);
        **processed = processed2;
        **should_print = should_print2;
        **should_quit = should_quit2;
        **should_next = should_next2;
    }
    Ok(())
}

/// Context for the n (next line) command loop; groups refs used only inside the loop.
struct NCommandLoopCtx<'a> {
    buffer: &'a mut String,
    next_buffer: &'a mut String,
    next_bytes: &'a mut usize,
    line_number: &'a mut u64,
    skip_swap_this: &'a mut bool,
    processed: &'a mut String,
    should_print: &'a mut bool,
    prints: &'a mut Vec<String>,
    inserts: &'a mut Vec<String>,
    appends: &'a mut Vec<String>,
    should_quit: &'a mut bool,
    should_next: &'a mut bool,
}

fn handle_n_command_loop(
    reader: &mut dyn std::io::BufRead,
    commands: &[AddressedCommand],
    options: &SedOptions,
    file_path: Option<&str>,
    state: &mut SedState,
    last_puts_char: &mut char,
    write_file_last: &mut Option<&mut HashMap<String, char>>,
    ctx: &mut NCommandLoopCtx<'_>,
) -> Result<()> {
    let NCommandLoopCtx {
        buffer,
        next_buffer,
        next_bytes,
        line_number,
        skip_swap_this,
        processed,
        should_print,
        prints,
        inserts,
        appends,
        should_quit,
        should_next,
    } = ctx;
    while state.n_command_pending {
        let used_peeked = !next_buffer.is_empty();
        let (next_line, has_newline2) = if used_peeked {
            let has_nl = next_buffer.ends_with('\n');
            let line = if has_nl {
                next_buffer.trim_end_matches('\n')
            } else {
                next_buffer.trim_end_matches('\r')
            }.to_string();
            next_buffer.clear();
            let _nb = reader.read_line(next_buffer)
                .map_err(|e| sed_read_line_error(file_path, e))?;
            **next_bytes = _nb;
            **skip_swap_this = true;
            (line, has_nl)
        } else {
            let mut read_buf = String::new();
            let bytes_read = reader.read_line(&mut read_buf)
                .map_err(|e| sed_read_line_error(file_path, e))?;
            if bytes_read == 0 {
                state.n_command_pending = false;
                state.next_command_index = None;
                state.n_hit_eof = true;
                break;
            }
            let has_nl = read_buf.ends_with('\n');
            let line = if has_nl {
                read_buf.trim_end_matches('\n')
            } else {
                read_buf.trim_end_matches('\r')
            }.to_string();
            **buffer = read_buf;
            **skip_swap_this = true;
            (line, has_nl)
        };
        if used_peeked {
            buffer.clear();
            buffer.push_str(&next_line);
            if has_newline2 {
                buffer.push('\n');
            }
        }
        state.pattern_space = next_line;
        let idx = state.next_command_index.unwrap_or(0);
        state.n_command_pending = false;
        state.next_command_index = None;
        **line_number += 1;
        state.cycle_start_line_number = **line_number;
        let pattern_copy = state.pattern_space.clone();
        let (processed2, should_print2, prints2, inserts2, appends2, should_quit2, should_next2) = {
            let input = ApplyCommandsInput {
                line: pattern_copy.as_str(),
                commands,
                extended_regex: options.extended_regex,
                quiet: options.quiet,
                line_number: **line_number,
                total_lines: None,
                line_has_newline: has_newline2,
                start_index: idx,
                cycle_start_line_number: state.cycle_start_line_number,
                parent_group_index: None,
            };
            apply_commands(&input, state, Some(last_puts_char), write_file_last)?
        };
        prints.extend(prints2);
        inserts.extend(inserts2);
        appends.extend(appends2);
        **processed = processed2;
        **should_print = should_print2;
        **should_quit = should_quit2;
        **should_next = should_next2;
    }
    Ok(())
}

/// Emit one line's output: inserts, prints, main line (or pattern space if quitting), appends.
fn emit_line_output(
    last_puts_char: &mut char,
    state: &mut SedState,
    options: &SedOptions,
    has_newline: bool,
    inserts: &[String],
    prints: &[String],
    processed: &str,
    appends: &[String],
    should_output: bool,
    should_quit: bool,
    should_print: bool,
    should_next: bool,
    line_number: u64,
) {
    for text in inserts {
        maybe_newline_before(last_puts_char);
        if has_newline {
            print!("{}\n", text);
            *last_puts_char = '\n';
        } else {
            print!("{}", text);
            *last_puts_char = 'x';
        }
    }
    for text in prints {
        maybe_newline_before(last_puts_char);
        if text.ends_with('\n') {
            print!("{}", text);
            *last_puts_char = '\n';
        } else if has_newline {
            print!("{}\n", text);
            *last_puts_char = '\n';
        } else {
            print!("{}", text);
            *last_puts_char = 'x';
        }
    }
    if should_next && !options.quiet {
        maybe_newline_before(last_puts_char);
        if has_newline {
            println!("{}", processed);
            *last_puts_char = '\n';
        } else {
            print!("{}", processed);
            *last_puts_char = 'x';
        }
    }
    if should_output || (should_quit && should_print && !options.quiet) {
        state.last_processed_line_number = line_number;
        maybe_newline_before(last_puts_char);
        let to_print = if should_quit { &state.pattern_space } else { processed };
        if has_newline {
            println!("{}", to_print);
            *last_puts_char = '\n';
        } else {
            print!("{}", to_print);
            *last_puts_char = 'x';
            if !appends.is_empty() {
                print!("\n");
                *last_puts_char = '\n';
            }
        }
    }
    for text in appends {
        if text.is_empty() {
            // Empty append (e.g. "$a\") only ensures line/newline; do not print an extra line.
            continue;
        }
        maybe_newline_before(last_puts_char);
        println!("{}", text);
        *last_puts_char = '\n';
    }
}

fn process_input(
    reader: &mut dyn std::io::BufRead,
    commands: &[AddressedCommand],
    options: &SedOptions,
    file_path: Option<&str>,
    state: &mut SedState,
    last_puts_char: &mut char,
    write_file_last: &mut Option<&mut HashMap<String, char>>,
    is_last_file: bool,
) -> Result<()> {
    let mut line_number = 0u64;
    let mut buffer = String::new();
    let mut next_buffer = String::new();
    // Read first line
    let bytes_read = reader.read_line(&mut buffer)
        .map_err(|e| sed_read_line_error(file_path, e))?;
    if bytes_read == 0 {
        return Ok(());
    }
    let mut skip_swap = false; // true after n/N consumed line(s): don't clear next_buffer at top of loop
    let mut next_bytes = 1usize; // so first iteration doesn't think we're at EOF
    let mut swapped_then_read_zero = false; // true after swap then read 0: process buffer before break
    loop {
        // When we swapped, buffer already holds the next line; don't increment again (BusyBox test: line 5).
        if !swapped_then_read_zero {
            line_number += 1;
        }
        state.test_flag = false;
        // Peek next line (unless we already have it from previous n/N)
        if !skip_swap {
            next_buffer.clear();
            next_bytes = reader.read_line(&mut next_buffer)
                .map_err(|e| sed_read_line_error(file_path, e))?;
        }
        // $ matches only last line of last file (sed.c: get_next_line spans files)
        let total_lines = if is_last_file && next_bytes == 0 {
            Some(line_number)
        } else {
            None
        };
        let has_newline = buffer.ends_with('\n');
        let line = if has_newline {
            buffer.trim_end_matches('\n')
        } else {
            buffer.trim_end_matches('\r')
        }.to_string();

        state.cycle_start_line_number = line_number;
        let input = ApplyCommandsInput {
            line: line.as_str(),
            commands,
            extended_regex: options.extended_regex,
            quiet: options.quiet,
            line_number,
            total_lines,
            line_has_newline: has_newline,
            start_index: 0,
            cycle_start_line_number: state.cycle_start_line_number,
            parent_group_index: None,
        };
        let (mut processed, mut should_print, mut prints, mut inserts, mut appends, mut should_quit, mut should_next) = apply_commands(&input, state, Some(last_puts_char), write_file_last)?;

        let broke_for_n_append = state.n_append_pending; // don't output unmerged line when we're about to do N
        let mut skip_swap_this = false; // n: don't swap (buffer already set). N: swap so buffer = next line
        let mut n_append_consumed = false; // true if N loop consumed from next_buffer (then we swap after)
        let mut ctx = NAppendLoopCtx {
            line_number: &mut line_number,
            next_buffer: &mut next_buffer,
            next_bytes: &mut next_bytes,
            skip_swap_this: &mut skip_swap_this,
            n_append_consumed: &mut n_append_consumed,
            processed: &mut processed,
            should_print: &mut should_print,
            prints: &mut prints,
            inserts: &mut inserts,
            appends: &mut appends,
            should_quit: &mut should_quit,
            should_next: &mut should_next,
        };
        handle_n_append_loop(reader, commands, options, file_path, state, last_puts_char, write_file_last, &mut ctx)?;
        // After N loop only: next iteration's line is in next_buffer (or we read 0 in N loop).
        // Swap so buffer has the next line to process; if next_buffer was left empty (N read 0),
        // read the next line into buffer so we don't reprocess the same line forever.
        if n_append_consumed {
            if next_buffer.is_empty() {
                next_buffer.clear();
                next_bytes = reader.read_line(&mut buffer)
                    .map_err(|e| sed_read_line_error(file_path, e))?;
            } else {
                std::mem::swap(&mut buffer, &mut next_buffer);
                next_buffer.clear();
                next_bytes = reader.read_line(&mut next_buffer)
                    .map_err(|e| sed_read_line_error(file_path, e))?;
            }
        }

        if state.n_append_hit_eof {
            state.n_append_hit_eof = false;
            break;
        }

        // Handle n command (read next line, replace pattern space)
        let mut n_ctx = NCommandLoopCtx {
            buffer: &mut buffer,
            next_buffer: &mut next_buffer,
            next_bytes: &mut next_bytes,
            line_number: &mut line_number,
            skip_swap_this: &mut skip_swap_this,
            processed: &mut processed,
            should_print: &mut should_print,
            prints: &mut prints,
            inserts: &mut inserts,
            appends: &mut appends,
            should_quit: &mut should_quit,
            should_next: &mut should_next,
        };
        handle_n_command_loop(reader, commands, options, file_path, state, last_puts_char, write_file_last, &mut n_ctx)?;
        if state.n_hit_eof {
            state.n_hit_eof = false;
            break;
        }
        skip_swap = skip_swap_this;

        let should_output = !options.quiet && should_print && (n_append_consumed || !broke_for_n_append) && !(skip_swap_this && !n_append_consumed);
        emit_line_output(
            last_puts_char,
            state,
            options,
            has_newline,
            &inserts,
            &prints,
            &processed,
            &appends,
            should_output,
            should_quit,
            should_print,
            should_next,
            line_number,
        );

        if next_bytes == 0 {
            swapped_then_read_zero = false; // so we break on next iteration after processing this line
        }
        if should_quit {
            break;
        }
        // Exit if no more input. When n_append_consumed we swapped so buffer has the next line
        // (e.g. last line "c"); don't break yet so we process it and N can hit EOF and print it (GNU behavior).
        // When we swapped and then read 0 at start of next iteration, don't break so we process (BusyBox test).
        if next_bytes == 0 && !n_append_consumed && !swapped_then_read_zero {
            break;
        }
        // Next iteration: use peeked line as current. When skip_swap we already have it in buffer.
        // When next_buffer is empty (e.g. N read 0 and we read next line into buffer) don't swap
        // or we'd overwrite buffer and lose the line we just read.
        if !skip_swap && !next_buffer.is_empty() {
            std::mem::swap(&mut buffer, &mut next_buffer);
            // Set for next iteration's read in N-handling and should_output
            #[allow(unused_assignments)]
            {
                n_append_consumed = true;
            }
            swapped_then_read_zero = true; // so if next read gets 0 we don't break before processing buffer
            line_number += 1; // buffer now holds the next line
        }
    }

    Ok(())
}
