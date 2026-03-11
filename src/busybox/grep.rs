use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use regex::Regex;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use crate::lfs;

fn add_match_mode_args(cmd: Command) -> Command {
    cmd
        .arg(Arg::new("ignore-case")
            .short('i')
            .long("ignore-case")
            .help("Ignore case distinctions")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("invert-match")
            .short('v')
            .long("invert-match")
            .help("Invert the sense of matching")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("word-regexp")
            .short('w')
            .long("word-regexp")
            .help("Match only whole words")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("line-regexp")
            .short('x')
            .long("line-regexp")
            .help("Match only whole lines")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("fixed-strings")
            .short('F')
            .long("fixed-strings")
            .help("Interpret pattern as fixed strings")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("extended-regexp")
            .short('E')
            .long("extended-regexp")
            .help("Interpret pattern as extended regular expressions")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("perl-regexp")
            .short('P')
            .long("perl-regexp")
            .help("Interpret pattern as Perl regular expressions")
            .action(clap::ArgAction::SetTrue))
}

fn add_output_args(cmd: Command) -> Command {
    cmd
        .arg(Arg::new("line-number")
            .short('n')
            .long("line-number")
            .help("Print line numbers")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("quiet")
            .short('q')
            .long("quiet")
            .help("Suppress all output")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("silent")
            .long("silent")
            .help("Suppress all output")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("count")
            .short('c')
            .long("count")
            .help("Print only a count of matching lines per file")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files_with_matches")
            .short('l')
            .long("files-with-matches")
            .help("Print only names of files with matches")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files_without_match")
            .short('L')
            .long("files-without-match")
            .help("Print only names of files with no matches")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("no-messages")
            .short('s')
            .long("no-messages")
            .help("Suppress error messages")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("only-matching")
            .short('o')
            .long("only-matching")
            .help("Print only the matched parts of lines")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("text")
            .short('a')
            .long("text")
            .help("Treat binary files as text")
            .action(clap::ArgAction::SetTrue))
}

fn add_recursion_args(cmd: Command) -> Command {
    cmd
        .arg(Arg::new("recursive")
            .short('r')
            .long("recursive")
            .help("Recursively search subdirectories")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("dereference-recursive")
            .short('R')
            .long("dereference-recursive")
            .help("Recursively search subdirectories (follow symlinks)")
            .action(clap::ArgAction::SetTrue))
}

fn add_pattern_source_args(cmd: Command) -> Command {
    cmd
        .arg(Arg::new("regexp")
            .short('e')
            .long("regexp")
            .value_name("PATTERN")
            .num_args(1)
            .action(clap::ArgAction::Append)
            .help("Use PATTERN as the pattern"))
        .arg(Arg::new("file_patterns")
            .short('f')
            .long("file")
            .value_name("FILE")
            .num_args(1)
            .help("Take patterns from FILE"))
        .arg(Arg::new("pattern")
            .help("Pattern to search for"))
}

fn add_file_args(cmd: Command) -> Command {
    cmd
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files to search (if none, read from stdin; - means stdin)"))
}

/// Build shared command arguments used by all grep variants
pub fn build_shared_args(cmd: Command) -> Command {
    add_file_args(add_pattern_source_args(add_recursion_args(add_output_args(add_match_mode_args(cmd)))))
}

#[derive(Clone)]
pub enum MatchMode {
    Basic,      // Basic regex (default)
    Extended,   // Extended regex (-E)
    Fixed,      // Fixed strings (-F)
    Perl,       // Perl regex (-P)
}

#[derive(Clone)]
pub enum Matcher {
    Regex(Regex),
    Fixed(Vec<String>, bool), // patterns, ignore_case
}

pub struct GrepOptions {
    pub patterns: Vec<String>,
    pub files: Vec<String>,
    pub ignore_case: bool,
    pub line_number: bool,
    pub invert_match: bool,
    pub word_match: bool,
    pub line_match: bool,
    pub quiet: bool,
    pub count: bool,
    pub files_with_matches: bool,
    pub files_without_match: bool,
    pub no_messages: bool,
    pub recursive: bool,
    pub only_matching: bool,
    #[allow(dead_code)]
    pub text: bool,
    pub match_mode: MatchMode,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<GrepOptions> {
    parse_shared_options(matches, MatchMode::Basic)
}

fn collect_patterns_from_regexp(matches: &clap::ArgMatches, patterns: &mut Vec<String>) -> bool {
    let mut patterns_from_options = false;
    for val in matches.get_many::<String>("regexp").into_iter().flatten() {
        patterns_from_options = true;
        for line in val.split('\n') {
            let line = line.trim_end_matches('\r').to_string();
            if !line.is_empty() {
                patterns.push(line);
            }
        }
    }
    patterns_from_options
}

fn collect_patterns_from_file(matches: &clap::ArgMatches, patterns: &mut Vec<String>) -> Result<bool> {
    let mut had_pattern_file = false;
    if let Some(path) = matches.get_one::<String>("file_patterns") {
        had_pattern_file = true;
        let content = if path == "-" {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).map_err(|e| eyre!("grep: -: {}", e))?;
            s
        } else {
            std::fs::read_to_string(path).map_err(|e| eyre!("grep: {}: {}", path, e))?
        };
        for line in content.lines() {
            let line = line.trim_end_matches('\n').trim_end_matches('\r');
            if !line.is_empty() {
                patterns.push(line.to_string());
            }
        }
    }
    Ok(had_pattern_file)
}

fn collect_patterns_from_positional(matches: &clap::ArgMatches, patterns: &mut Vec<String>) {
    if patterns.is_empty() {
        if let Some(p) = matches.get_one::<String>("pattern") {
            for line in p.split('\n') {
                let mut line = line.trim_end_matches('\r').to_string();
                // Strip one layer of surrounding double quotes (e.g. from eval 'grep -o "[^/]*$"')
                if line.len() >= 2 && line.starts_with('"') && line.ends_with('"') {
                    line = line[1..line.len() - 1].to_string();
                }
                patterns.push(line); // allow empty pattern (e.g. grep -o "")
            }
        }
    }
}

fn extract_flags_from_matches(matches: &clap::ArgMatches, default_match_mode: MatchMode) -> (bool, bool, bool, bool, bool, bool, bool, bool, bool, bool, bool, bool, bool, MatchMode) {
    let ignore_case = matches.get_flag("ignore-case");
    let line_number = matches.get_flag("line-number");
    let invert_match = matches.get_flag("invert-match");
    let word_match = matches.get_flag("word-regexp");
    let line_match = matches.get_flag("line-regexp");
    let quiet = matches.get_flag("quiet") || matches.get_flag("silent");
    let count = matches.get_flag("count");
    let files_with_matches = matches.get_flag("files_with_matches");
    let files_without_match = matches.get_flag("files_without_match");
    let no_messages = matches.get_flag("no-messages");
    let only_matching = matches.get_flag("only-matching");
    let recursive = matches.get_flag("recursive") || matches.get_flag("dereference-recursive");
    let text = matches.get_flag("text");

    // Determine match mode - explicit flags override default
    let match_mode = if matches.get_flag("fixed-strings") {
        MatchMode::Fixed
    } else if matches.get_flag("extended-regexp") {
        MatchMode::Extended
    } else if matches.get_flag("perl-regexp") {
        MatchMode::Perl
    } else {
        default_match_mode
    };

    (ignore_case, line_number, invert_match, word_match, line_match, quiet,
     count, files_with_matches, files_without_match, no_messages, recursive,
     only_matching, text, match_mode)
}

fn build_files_vector(matches: &clap::ArgMatches, patterns_from_options: bool, had_pattern_file: bool) -> Vec<String> {
    let mut files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();
    // When patterns came from -e/-f, the single positional may have been taken as "pattern"; treat it as a file.
    if patterns_from_options || had_pattern_file {
        if let Some(p) = matches.get_one::<String>("pattern") {
            files.insert(0, p.clone());
        }
    }
    files
}

pub fn parse_shared_options(matches: &clap::ArgMatches, default_match_mode: MatchMode) -> Result<GrepOptions> {
    let mut patterns: Vec<String> = Vec::new();
    let patterns_from_options = collect_patterns_from_regexp(matches, &mut patterns);
    let had_pattern_file = collect_patterns_from_file(matches, &mut patterns)?;
    collect_patterns_from_positional(matches, &mut patterns);
    if patterns.is_empty() && !had_pattern_file {
        return Err(eyre!("grep: missing pattern"));
    }

    let files = build_files_vector(matches, patterns_from_options, had_pattern_file);
    let (ignore_case, line_number, invert_match, word_match, line_match, quiet,
         count, files_with_matches, files_without_match, no_messages, recursive,
         only_matching, text, match_mode) = extract_flags_from_matches(matches, default_match_mode);

    Ok(GrepOptions {
        patterns,
        files,
        ignore_case,
        line_number,
        invert_match,
        word_match,
        line_match,
        quiet,
        count,
        files_with_matches,
        files_without_match,
        no_messages,
        recursive,
        only_matching,
        text,
        match_mode,
    })
}

pub fn command() -> Command {
    build_shared_args(Command::new("grep")
        .about("Search for patterns in files"))
}

fn display_name(path: &str) -> &str {
    if path == "-" {
        "(standard input)"
    } else {
        path
    }
}

fn expand_path(path: &std::path::Path, follow_symlinks: bool) -> Vec<String> {
    let mut result = Vec::new();
    let metadata = match lfs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return result,
    };
    let is_symlink = metadata.file_type().is_symlink();
    let target_is_dir = if is_symlink && follow_symlinks {
        // Follow symlink to see if target is a directory
        std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
    } else {
        metadata.is_dir()
    };
    if metadata.is_file() {
        result.push(path.to_string_lossy().into_owned());
    } else if target_is_dir {
        if is_symlink && !follow_symlinks {
            // treat symlink as file (skip recursion)
            result.push(path.to_string_lossy().into_owned());
        } else {
            // Determine directory to read: if symlink and following, read resolved directory
            let dir_to_read = if is_symlink && follow_symlinks {
                match std::fs::canonicalize(path) {
                    Ok(resolved) => resolved,
                    Err(_) => return result,
                }
            } else {
                path.to_path_buf()
            };
            // Recurse into directory
            match std::fs::read_dir(&dir_to_read) {
                Ok(entries) => {
                    for entry in entries {
                        if let Ok(entry) = entry {
                            let file_name = entry.file_name();
                            // Build subpath using original symlink path as base
                            let subpath = path.join(&file_name);
                            let sub_metadata = match lfs::symlink_metadata(&subpath) {
                                Ok(m) => m,
                                Err(_) => continue,
                            };
                            if sub_metadata.is_file() {
                                result.push(subpath.to_string_lossy().into_owned());
                            } else if sub_metadata.is_dir() && !sub_metadata.file_type().is_symlink() {
                                result.extend(expand_path(&subpath, false));
                            }
                            // symlinks to directories are skipped
                        }
                    }
                }
                Err(_) => {}
            }
        }
    }
    // else: neither file nor directory (e.g., symlink to non-dir that we don't follow) -> ignore
    result
}

fn print_only_matches(name: &str, line_num: usize, matches: Vec<String>, multiple_files: bool, line_number: bool) {
    for mat in matches {
        if mat.is_empty() {
            continue;
        }
        if multiple_files {
            if line_number {
                println!("{}:{}:{}", name, line_num + 1, mat);
            } else {
                println!("{}:{}", name, mat);
            }
        } else {
            if line_number {
                println!("{}:{}", line_num + 1, mat);
            } else {
                println!("{}", mat);
            }
        }
    }
}

fn print_line_normal(name: &str, line_num: usize, line: &str, multiple_files: bool, line_number: bool) {
    if multiple_files {
        if line_number {
            println!("{}:{}:{}", name, line_num + 1, line);
        } else {
            println!("{}:{}", name, line);
        }
    } else {
        if line_number {
            println!("{}:{}", line_num + 1, line);
        } else {
            println!("{}", line);
        }
    }
}

fn process_lines(
    reader: Box<dyn BufRead>,
    file_path: &str,
    options: &GrepOptions,
    matcher: &Matcher,
    multiple_files: bool,
    name_only: bool,
) -> (bool, bool, usize) {
    let mut found_match = false;
    let mut match_count = 0;
    for (line_num, line_result) in reader.lines().enumerate() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                if !options.no_messages {
                    eprintln!("grep: error reading {}: {}", file_path, e);
                }
                continue;
            }
        };

        let matches = matcher.matches(&line, options);
        let should_print = if options.invert_match { !matches } else { matches };

        if should_print {
            found_match = true;
            match_count += 1;
            if options.quiet {
                return (true, false, match_count);
            }
            if name_only {
                return (true, false, match_count);
            }
            if options.count {
                continue; // Don't print lines when counting
            }

            let name = display_name(file_path);
            if options.only_matching {
                if line.is_empty() {
                    continue;
                }
                if let Some(matches) = matcher.get_matches(&line, options) {
                    if matches.iter().all(|m| m.is_empty()) {
                        continue;
                    }
                    print_only_matches(&name, line_num, matches, multiple_files, options.line_number);
                }
            } else {
                print_line_normal(&name, line_num, &line, multiple_files, options.line_number);
            }
        }
    }
    (found_match, false, match_count)
}

/// Returns (found_match, had_error, match_count).
fn search_file(file_path: &str, options: &GrepOptions, matcher: &Matcher, multiple_files: bool) -> Result<(bool, bool, usize)> {
    let reader: Box<dyn BufRead> = if file_path == "-" {
        Box::new(BufReader::new(io::stdin()))
    } else {
        if std::path::Path::new(file_path).is_dir() {
            if !options.no_messages {
                eprintln!("grep: {}: Is a directory", file_path);
            }
            return Ok((false, true, 0));
        }
        let file = match File::open(file_path) {
            Ok(f) => f,
            Err(e) => {
                if !options.no_messages {
                    eprintln!("grep: {}: {}", file_path, e);
                }
                return Ok((false, true, 0));
            }
        };
        Box::new(BufReader::new(file))
    };
    let name_only = options.files_with_matches || options.files_without_match;
    Ok(process_lines(reader, file_path, options, matcher, multiple_files, name_only))
}

impl Matcher {
    fn new(patterns: &[String], options: &GrepOptions) -> Result<Self> {
        let never_match = Regex::new("[^\\s\\S]").unwrap();
        if patterns.is_empty() || (patterns.len() == 1 && patterns[0].is_empty()) {
            return Ok(Matcher::Regex(never_match));
        }
        match options.match_mode {
            MatchMode::Fixed => {
                Ok(Matcher::Fixed(patterns.to_vec(), options.ignore_case))
            }
            _ => {
                let mut regex_pattern = String::new();

                if options.ignore_case {
                    regex_pattern.push_str("(?i)");
                }

                let escaped: Vec<String> = match options.match_mode {
                    MatchMode::Extended | MatchMode::Perl => {
                        patterns.iter().map(|p| p.to_string()).collect()
                    }
                    MatchMode::Basic => {
                        // Basic regex: . * [ ] ^ $ have special meaning; only escape backslash
                        patterns.iter()
                            .map(|p| p.replace('\\', "\\\\"))
                            .collect()
                    }
                    MatchMode::Fixed => unreachable!(),
                };
                let combined = escaped.join("|");
                if options.word_match {
                    let word_combined: String = patterns.iter()
                        .map(|p| regex::escape(p))
                        .collect::<Vec<_>>()
                        .join("|");
                    regex_pattern.push_str(&format!(r"\b(?:{})\b", word_combined));
                } else {
                    regex_pattern.push_str(&combined);
                }

                if options.line_match {
                    regex_pattern = format!(r"^(?:{})$", regex_pattern);
                }

                let regex = Regex::new(&regex_pattern)
                    .map_err(|e| eyre!("grep: invalid regex: {}", e))?;

                Ok(Matcher::Regex(regex))
            }
        }
    }

    fn matches(&self, line: &str, options: &GrepOptions) -> bool {
        match self {
            Matcher::Regex(regex) => regex.is_match(line),
            Matcher::Fixed(patterns, ignore_case) => {
                let line_to_check = if *ignore_case {
                    line.to_lowercase()
                } else {
                    line.to_string()
                };
                for pattern in patterns {
                    let pattern_to_check = if *ignore_case {
                        pattern.to_lowercase()
                    } else {
                        pattern.clone()
                    };
                    let matched = if options.word_match {
                        let word_regex = format!(r"\b{}\b", regex::escape(&pattern_to_check));
                        Regex::new(&word_regex).unwrap().is_match(&line_to_check)
                    } else if options.line_match {
                        line_to_check == pattern_to_check
                    } else {
                        line_to_check.contains(&pattern_to_check)
                    };
                    if matched {
                        return true;
                    }
                }
                false
            }
        }
    }

    fn get_matches(&self, line: &str, options: &GrepOptions) -> Option<Vec<String>> {
        match self {
            Matcher::Regex(regex) => {
                let mut matches = Vec::new();
                let mut search_start = 0;
                while search_start <= line.len() {
                    let rest = &line[search_start..];
                    let mat = match regex.find(rest) {
                        Some(m) => m,
                        None => break,
                    };
                    let start = search_start + mat.start();
                    let end = search_start + mat.end();
                    if start == end {
                        if rest.is_empty() {
                            break;
                        }
                        search_start += 1; // advance past zero-length match to avoid infinite loop (do not push "")
                    } else {
                        matches.push(line[start..end].to_string());
                        search_start = end;
                    }
                }
                if matches.is_empty() { None } else { Some(matches) }
            }
            Matcher::Fixed(patterns, ignore_case) => {
                let line_to_check = if *ignore_case {
                    line.to_lowercase()
                } else {
                    line.to_string()
                };
                for pattern in patterns {
                    let pattern_to_check = if *ignore_case {
                        pattern.to_lowercase()
                    } else {
                        pattern.clone()
                    };

                    if options.word_match {
                        let word_regex = format!(r"\b{}\b", regex::escape(&pattern_to_check));
                        if let Ok(re) = Regex::new(&word_regex) {
                            let mut matches = Vec::new();
                            for mat in re.find_iter(&line_to_check) {
                                matches.push(line[mat.start()..mat.end()].to_string());
                            }
                            if !matches.is_empty() {
                                return Some(matches);
                            }
                        }
                    } else if options.line_match {
                        if line_to_check == pattern_to_check {
                            return Some(vec![line.to_string()]);
                        }
                    } else if line_to_check.contains(&pattern_to_check) {
                        let mut matches = Vec::new();
                        let mut start = 0;
                        while let Some(pos) = line_to_check[start..].find(&pattern_to_check) {
                            let actual_start = start + pos;
                            let actual_end = actual_start + pattern_to_check.len();
                            matches.push(line[actual_start..actual_end].to_string());
                            start = actual_end;
                        }
                        if !matches.is_empty() {
                            return Some(matches);
                        }
                    }
                }
                None
            }
        }
    }
}


fn build_files_to_search(files: &[String], recursive: bool) -> (Vec<String>, bool) {
    let mut files_to_search = Vec::new();
    let mut has_dir_arg = false;

    if files.is_empty() {
        files_to_search.push("-".to_string());
        return (files_to_search, has_dir_arg);
    }

    for file_path in files {
        if recursive && file_path != "-" {
            let path = std::path::Path::new(file_path);
            let metadata = lfs::symlink_metadata(path).ok();
            let is_symlink = metadata.as_ref().map(|m| m.file_type().is_symlink()).unwrap_or(false);
            let follow_symlinks = is_symlink; // follow symlinks only if the argument itself is a symlink
            let is_dir = if is_symlink && follow_symlinks {
                // If symlink and we will follow it, check target type
                std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
            } else {
                metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false)
            };
            if is_dir {
                has_dir_arg = true;
                files_to_search.extend(expand_path(path, follow_symlinks));
            } else {
                files_to_search.push(file_path.clone());
            }
        } else {
            files_to_search.push(file_path.clone());
        }
    }
    (files_to_search, has_dir_arg)
}

fn handle_files(matcher: &Matcher, options: &GrepOptions) -> Result<bool> {
    let mut found_any_match = false;
    let mut had_error = false;
    let (files_to_search, has_dir_arg) = build_files_to_search(&options.files, options.recursive);
    let multiple_files = files_to_search.len() > 1 || has_dir_arg;
    for file_path in files_to_search {
        let (found, err, count) = search_file(&file_path, options, matcher, multiple_files)?;
        if err {
            had_error = true;
        }
        let name = display_name(&file_path);
        if options.files_with_matches && found {
            println!("{}", name);
            found_any_match = true;
        }
        if options.files_without_match && !found {
            println!("{}", name);
            found_any_match = true;
        }
        if options.count {
            if multiple_files {
                println!("{}:{}", name, count);
            } else {
                println!("{}", count);
            }
            if count > 0 {
                found_any_match = true;
            }
        }
        if found && !options.files_with_matches && !options.files_without_match && !options.count {
            found_any_match = true;
            if options.quiet {
                std::process::exit(0);
            }
        }
    }
    // Exit code: 2 if any error (unless -q and we found a match), 1 if no match, 0 if match
    if had_error && !(options.quiet && found_any_match) {
        std::process::exit(2);
    }
    Ok(found_any_match)
}

pub fn run(options: GrepOptions) -> Result<()> {
    let matcher = Matcher::new(&options.patterns, &options)?;
    let found_any_match = handle_files(&matcher, &options)?;
    if !found_any_match {
        std::process::exit(1);
    }
    Ok(())
}
