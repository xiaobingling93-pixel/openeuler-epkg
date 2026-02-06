use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::io::{self, BufRead};

#[derive(Debug, Clone)]
pub enum CutMode {
    Bytes(String),
    Characters(String),
    Fields(String),
    Unset,
}

#[derive(Clone)]
pub struct CutOptions {
    pub delimiter: String,
    pub mode: CutMode,
    pub suppress_no_delim: bool,
    pub files: Vec<String>,
}

fn unquote_delim(s: &str) -> String {
    let mut s = s;
    while s.len() >= 2 {
        let c = s.chars().next().unwrap();
        if (c == '"' && s.ends_with('"')) || (c == '\'' && s.ends_with('\'')) {
            let inner = &s[1..s.len() - 1];
            if inner.is_empty() || inner == "\"" || inner == "'" {
                return String::new();
            }
            s = inner;
            continue;
        }
        break;
    }
    s.to_string()
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<CutOptions> {
    let delimiter = matches.get_one::<String>("delimiter")
        .map(|s| unquote_delim(s))
        .unwrap_or_else(|| "\t".to_string());

    let bytes = matches.get_one::<String>("bytes").cloned();
    let chars = matches.get_one::<String>("characters").cloned();
    let fields = matches.get_one::<String>("fields").cloned();

    let mode = match (bytes, chars, fields) {
        (Some(list), None, None) => CutMode::Bytes(list),
        (None, Some(list), None) => CutMode::Characters(list),
        (None, None, Some(list)) => CutMode::Fields(list),
        (None, None, None) => CutMode::Unset,
        _ => return Err(eyre!("cut: only one of -b, -c, -f may be specified")),
    };

    let suppress_no_delim = matches.get_flag("suppress");
    eprintln!("parse_options: suppress={}", suppress_no_delim);
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    eprintln!("parse_options: files={:?}", files);
    Ok(CutOptions { delimiter, mode, suppress_no_delim, files })
}

pub fn command() -> Command {
    Command::new("cut")
        .about("Extract bytes, characters or fields from lines")
        .allow_hyphen_values(true)
        .arg(Arg::new("fields")
            .short('f')
            .long("fields")
            .help("Select fields")
            .value_name("LIST")
            .num_args(1)
            .allow_negative_numbers(true))
        .arg(Arg::new("bytes")
            .short('b')
            .long("bytes")
            .help("Select bytes")
            .value_name("LIST")
            .num_args(1)
            .allow_negative_numbers(true))
        .arg(Arg::new("characters")
            .short('c')
            .long("characters")
            .help("Select characters")
            .value_name("LIST")
            .num_args(1)
            .allow_negative_numbers(true))
        .arg(Arg::new("delimiter")
            .short('d')
            .long("delimiter")
            .help("Field delimiter (default: tab)")
            .value_name("DELIM")
            .num_args(0..=1))
        .arg(Arg::new("suppress")
            .short('s')
            .long("only-delimited")
            .help("Do not print lines not containing delimiter")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files to cut (if none, read from stdin; - means stdin)")
            .allow_hyphen_values(true))
}

#[derive(Debug, Clone)]
enum RangeSpec {
    Single(usize),
    Range(usize, Option<usize>),
}

fn parse_list(list_str: &str) -> Result<Vec<RangeSpec>> {
    let mut specs = Vec::new();
    for part in list_str.split(',') {
        if part.is_empty() {
            continue;
        }
        if part.contains('-') {
            let bounds: Vec<&str> = part.splitn(2, '-').collect();
            if bounds.len() != 2 {
                return Err(eyre!("cut: invalid range '{}'", part));
            }
            let start = if bounds[0].is_empty() {
                1
            } else {
                bounds[0].parse()
                    .map_err(|_| eyre!("cut: invalid number '{}'", bounds[0]))?
            };
            let end = if bounds[1].is_empty() {
                None
            } else {
                Some(bounds[1].parse()
                    .map_err(|_| eyre!("cut: invalid number '{}'", bounds[1]))?)
            };
            if let Some(end_val) = end {
                if start > end_val {
                    return Err(eyre!("cut: invalid decreasing range '{}'", part));
                }
            }
            specs.push(RangeSpec::Range(start, end));
        } else {
            let n: usize = part.parse()
                .map_err(|_| eyre!("cut: invalid number '{}'", part))?;
            specs.push(RangeSpec::Single(n));
        }
    }
    Ok(specs)
}

fn extract_by_specs(specs: &[RangeSpec], len: usize, one_based: bool) -> Vec<(usize, usize)> {
    let mut segments = Vec::new();
    for spec in specs {
        match spec {
            RangeSpec::Single(n) => {
                let idx = if one_based { n.saturating_sub(1) } else { *n };
                if idx < len {
                    segments.push((idx, idx + 1));
                }
            }
            RangeSpec::Range(start, end_opt) => {
                let s = if one_based { start.saturating_sub(1) } else { *start };
                let e = end_opt
                    .map(|v| if one_based { v } else { v + 1 })
                    .unwrap_or(len);
                let e = std::cmp::min(e, len);
                if s < e {
                    let s_0 = if one_based { s } else { s };
                    let e_0 = if one_based { e } else { std::cmp::min(e, len) };
                    segments.push((s_0, e_0));
                }
            }
        }
    }
    segments.sort_by_key(|(s, _)| *s);
    merge_overlapping(segments)
}

fn merge_overlapping(mut segments: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    if segments.is_empty() {
        return segments;
    }
    segments.sort_by_key(|(s, _)| *s);
    let mut out = vec![segments[0]];
    for (s, e) in segments.into_iter().skip(1) {
        let last = out.last_mut().unwrap();
        if s <= last.1 {
            last.1 = std::cmp::max(last.1, e);
        } else {
            out.push((s, e));
        }
    }
    out
}

fn cut_bytes(line: &str, specs: &[RangeSpec]) -> String {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let segments = extract_by_specs(specs, len, true);
    let mut out = Vec::new();
    for (s, e) in segments {
        out.extend_from_slice(&bytes[s..e]);
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn cut_chars(line: &str, specs: &[RangeSpec]) -> String {
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();
    let segments = extract_by_specs(specs, len, true);
    let mut out = String::new();
    for (s, e) in segments {
        out.extend(chars[s..e].iter());
    }
    out
}

fn cut_fields(line: &str, delimiter: &str, delim_char: Option<char>, specs: &[RangeSpec], suppress: bool) -> Option<String> {
    if delimiter.is_empty() {
        if suppress {
            return None;
        }
        return Some(line.to_string());
    }
    let fields: Vec<&str> = if let Some(c) = delim_char {
        line.split(c).collect()
    } else {
        line.split(delimiter).collect()
    };
    if suppress && fields.len() <= 1 && !line.contains(delimiter) {
        return None;
    }
    if fields.len() <= 1 && !line.contains(delimiter) {
        return Some(line.to_string());
    }
    let mut indices = Vec::new();
    for spec in specs {
        match spec {
            RangeSpec::Single(n) => {
                let idx = n.saturating_sub(1);
                if idx < fields.len() {
                    indices.push(idx);
                }
            }
            RangeSpec::Range(start, end_opt) => {
                let s = start.saturating_sub(1);
                let e = end_opt.map(|v| std::cmp::min(v, fields.len())).unwrap_or(fields.len());
                for i in s..e {
                    if i < fields.len() {
                        indices.push(i);
                    }
                }
            }
        }
    }
    indices.sort();
    indices.dedup();
    if indices.is_empty() {
        return Some(String::new());
    }
    let parts: Vec<&str> = indices.iter().map(|&i| fields[i]).collect();
    Some(parts.join(delimiter))
}

fn process<R: BufRead>(reader: R, options: &CutOptions, specs: &[RangeSpec], delim_char: Option<char>, delimiter: &str) -> Result<()> {
    if matches!(&options.mode, CutMode::Fields(_)) && delimiter == "\n" {
        let mut content = String::new();
        let mut r = reader;
        r.read_to_string(&mut content).map_err(|e| eyre!("cut: error reading input: {}", e))?;
        let fields: Vec<&str> = content.split('\n').collect();
        let mut indices = Vec::new();
        for spec in specs {
            match spec {
                RangeSpec::Single(n) => {
                    let idx = n.saturating_sub(1);
                    if idx < fields.len() {
                        indices.push(idx);
                    }
                }
                RangeSpec::Range(start, end_opt) => {
                    let s = start.saturating_sub(1);
                    let e = end_opt.map(|v| std::cmp::min(v, fields.len())).unwrap_or(fields.len());
                    for i in s..e {
                        if i < fields.len() {
                            indices.push(i);
                        }
                    }
                }
            }
        }
        indices.sort();
        indices.dedup();
        let parts: Vec<&str> = indices.iter().map(|&i| fields[i]).collect();
        println!("{}", parts.join("\n"));
        return Ok(());
    }
    for line_result in reader.lines() {
        let line = line_result.map_err(|e| eyre!("cut: error reading input: {}", e))?;
        let out = match &options.mode {
            CutMode::Bytes(_) => cut_bytes(&line, specs),
            CutMode::Characters(_) => cut_chars(&line, specs),
            CutMode::Fields(_) => {
                match cut_fields(&line, delimiter, delim_char, specs, options.suppress_no_delim) {
                    Some(s) => s,
                    None => continue,
                }
            }
            CutMode::Unset => unreachable!(),
        };
        println!("{}", out);
    }
    Ok(())
}

fn filter_option_like_from_files(files: &[String]) -> Vec<String> {
    files.iter()
        .filter(|f| {
            *f != "-s" && *f != "-d"
                && !f.starts_with("-d") && !f.starts_with("-f") && !f.starts_with("-b") && !f.starts_with("-c")
        })
        .cloned()
        .collect()
}

fn extract_flag_value(
    rest: &mut Vec<String>,
    flag_prefix: &str,
) -> Result<String> {
    let pos = rest.iter().position(|f| f.starts_with(flag_prefix))
        .ok_or_else(|| eyre!("cut: missing -b, -c, or -f"))?;
    let value = if rest[pos].len() > flag_prefix.len() {
        rest.remove(pos)[flag_prefix.len()..].to_string()
    } else if pos + 1 < rest.len() {
        let value = rest.remove(pos + 1);
        rest.remove(pos);
        value
    } else {
        return Err(eyre!("cut: missing -b, -c, or -f"));
    };
    Ok(value)
}

fn extract_delimiter_override(
    rest: &mut Vec<String>,
) -> Option<String> {
    let pos = rest.iter().position(|f| f.starts_with("-d"))?;
    let item = rest[pos].clone();
    if item.len() > 2 {
        // Attached delimiter: keep the flag in rest (will be filtered later)
        Some(unquote_delim(item.get(2..).unwrap_or("")))
    } else if pos + 1 < rest.len() {
        let delim = unquote_delim(&rest[pos + 1]);
        rest.remove(pos + 1);
        rest.remove(pos);
        Some(delim)
    } else {
        // -d alone: no delimiter override
        None
    }
}

fn extract_suppress_flag(
    rest: &mut Vec<String>,
    current_suppress: bool,
) -> bool {
    let mut suppress = current_suppress;
    if let Some(pos) = rest.iter().position(|f| f == "-s") {
        suppress = true;
        rest.remove(pos);
    }
    suppress
}

fn resolve_unset_mode(options: &CutOptions, mut rest: Vec<String>) -> Result<(CutMode, Vec<String>, Option<String>, bool)> {
    let mode = if rest.iter().any(|f| f.starts_with("-f")) {
        let list = extract_flag_value(&mut rest, "-f")?;
        CutMode::Fields(list)
    } else if rest.iter().any(|f| f.starts_with("-b")) {
        let list = extract_flag_value(&mut rest, "-b")?;
        CutMode::Bytes(list)
    } else if rest.iter().any(|f| f.starts_with("-c")) {
        let list = extract_flag_value(&mut rest, "-c")?;
        CutMode::Characters(list)
    } else {
        return Err(eyre!("cut: missing -b, -c, or -f"));
    };
    let delim_override = extract_delimiter_override(&mut rest);
    let suppress = extract_suppress_flag(&mut rest, options.suppress_no_delim);
    let files = filter_option_like_from_files(&rest);
    Ok((mode, files, delim_override, suppress))
}

fn resolve_mode_and_files(options: &CutOptions) -> Result<(CutMode, Vec<String>, Option<String>, bool)> {
    let (mode, files, delimiter_override, suppress) = match &options.mode {
        CutMode::Bytes(_) | CutMode::Characters(_) | CutMode::Fields(_) => {
            let files = filter_option_like_from_files(&options.files);
            (options.mode.clone(), files, None, options.suppress_no_delim)
        }
        CutMode::Unset => resolve_unset_mode(options, options.files.clone())?,
    };
    Ok((mode, files, delimiter_override, suppress))
}

pub fn run(options: CutOptions) -> Result<()> {
    let (mode, files, delimiter_override, suppress) = resolve_mode_and_files(&options)?;

    let specs = match &mode {
        CutMode::Bytes(list) | CutMode::Characters(list) | CutMode::Fields(list) => parse_list(list)?,
        CutMode::Unset => return Err(eyre!("cut: missing -b, -c, or -f")),
    };
    let delimiter = delimiter_override.as_ref().unwrap_or(&options.delimiter);
    let delim_char = if delimiter.chars().count() == 1 {
        delimiter.chars().next()
    } else {
        None
    };

    let resolved_opts = CutOptions {
        mode: mode.clone(),
        files: files.clone(),
        delimiter: delimiter.clone(),
        suppress_no_delim: suppress,
        ..options.clone()
    };


    if files.is_empty() {
        process(io::stdin().lock(), &resolved_opts, &specs, delim_char, delimiter)?;
    } else {
        for path in &files {
            if path == "-" {
                process(io::stdin().lock(), &resolved_opts, &specs, delim_char, delimiter)?;
            } else {
                match std::fs::File::open(path) {
                    Ok(f) => process(io::BufReader::new(f), &resolved_opts, &specs, delim_char, delimiter)?,
                    Err(e) => eprintln!("cut: {}: {}", path, e),
                }
            }
        }
    }

    Ok(())
}
