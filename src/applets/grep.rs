use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use regex::Regex;
use std::fs::File;
use std::io::{self, BufRead, BufReader};

/// Build shared command arguments used by all grep variants
pub fn build_shared_args(cmd: Command) -> Command {
    cmd.arg(Arg::new("ignore-case")
            .short('i')
            .long("ignore-case")
            .help("Ignore case distinctions")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("line-number")
            .short('n')
            .long("line-number")
            .help("Print line numbers")
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
        .arg(Arg::new("quiet")
            .short('q')
            .long("quiet")
            .help("Suppress all output")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("silent")
            .long("silent")
            .help("Suppress all output")
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
        .arg(Arg::new("pattern")
            .help("Pattern to search for")
            .required(true))
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files to search (if none, read from stdin)"))
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
    Fixed(String, bool), // pattern, ignore_case
}

pub struct GrepOptions {
    pub pattern: String,
    pub files: Vec<String>,
    pub ignore_case: bool,
    pub line_number: bool,
    pub invert_match: bool,
    pub word_match: bool,
    pub line_match: bool,
    pub quiet: bool,
    pub no_messages: bool,
    pub only_matching: bool,
    #[allow(dead_code)]
    pub text: bool,
    pub match_mode: MatchMode,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<GrepOptions> {
    parse_shared_options(matches, MatchMode::Basic)
}

pub fn parse_shared_options(matches: &clap::ArgMatches, default_match_mode: MatchMode) -> Result<GrepOptions> {
    let pattern = matches.get_one::<String>("pattern")
        .ok_or_else(|| eyre!("grep: missing pattern"))?
        .clone();

    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let ignore_case = matches.get_flag("ignore-case");
    let line_number = matches.get_flag("line-number");
    let invert_match = matches.get_flag("invert-match");
    let word_match = matches.get_flag("word-regexp");
    let line_match = matches.get_flag("line-regexp");
    let quiet = matches.get_flag("quiet") || matches.get_flag("silent");
    let no_messages = matches.get_flag("no-messages");
    let only_matching = matches.get_flag("only-matching");
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

    Ok(GrepOptions {
        pattern,
        files,
        ignore_case,
        line_number,
        invert_match,
        word_match,
        line_match,
        quiet,
        no_messages,
        only_matching,
        text,
        match_mode,
    })
}

pub fn command() -> Command {
    build_shared_args(Command::new("grep")
        .about("Search for patterns in files"))
}

fn search_file(file_path: &str, options: &GrepOptions, matcher: &Matcher) -> Result<bool> {
    let file_result = File::open(file_path);
    let file = match file_result {
        Ok(f) => f,
        Err(e) => {
            if !options.no_messages {
                eprintln!("grep: {}: {}", file_path, e);
            }
            return Ok(false);
        }
    };
    let reader = BufReader::new(file);
    let mut found_match = false;

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
            if options.quiet {
                return Ok(true);
            }

            if options.only_matching {
                if let Some(matches) = matcher.get_matches(&line, options) {
                    for mat in matches {
                        if options.files.len() > 1 {
                            if options.line_number {
                                println!("{}:{}:{}", file_path, line_num + 1, mat);
                            } else {
                                println!("{}:{}", file_path, mat);
                            }
                        } else {
                            if options.line_number {
                                println!("{}:{}", line_num + 1, mat);
                            } else {
                                println!("{}", mat);
                            }
                        }
                    }
                }
            } else {
                if options.files.len() > 1 {
                    // Multiple files - print filename
                    if options.line_number {
                        println!("{}:{}:{}", file_path, line_num + 1, line);
                    } else {
                        println!("{}:{}", file_path, line);
                    }
                } else {
                    // Single file or stdin
                    if options.line_number {
                        println!("{}:{}", line_num + 1, line);
                    } else {
                        println!("{}", line);
                    }
                }
            }
        }
    }

    Ok(found_match)
}

impl Matcher {
    fn new(pattern: &str, options: &GrepOptions) -> Result<Self> {
        match options.match_mode {
            MatchMode::Fixed => {
                Ok(Matcher::Fixed(pattern.to_string(), options.ignore_case))
            }
            _ => {
                let mut regex_pattern = String::new();

                if options.ignore_case {
                    regex_pattern.push_str("(?i)");
                }

                match options.match_mode {
                    MatchMode::Extended | MatchMode::Perl => {
                        // For extended and perl regex, we can use the pattern as-is
                        // since regex crate supports most extended regex features
                        regex_pattern.push_str(pattern);
                    }
                    MatchMode::Basic => {
                        // Basic regex - escape special characters that are special in extended regex
                        // but not in basic regex. This is a simplification - full basic regex
                        // would require a different parser
                        regex_pattern.push_str(&regex::escape(pattern));
                    }
                    MatchMode::Fixed => unreachable!(),
                }

                if options.word_match {
                    regex_pattern = format!(r"\b{}\b", regex_pattern);
                }

                if options.line_match {
                    regex_pattern = format!(r"^{}$", regex_pattern);
                }

                let regex = Regex::new(&regex_pattern)
                    .map_err(|e| eyre!("grep: invalid regex '{}': {}", pattern, e))?;

                Ok(Matcher::Regex(regex))
            }
        }
    }

    fn matches(&self, line: &str, options: &GrepOptions) -> bool {
        match self {
            Matcher::Regex(regex) => regex.is_match(line),
            Matcher::Fixed(pattern, ignore_case) => {
                let (line_to_check, pattern_to_check) = if *ignore_case {
                    (line.to_lowercase(), pattern.to_lowercase())
                } else {
                    (line.to_string(), pattern.clone())
                };

                if options.word_match {
                    // Word match: pattern must be surrounded by word boundaries
                    let word_regex = format!(r"\b{}\b", regex::escape(&pattern_to_check));
                    Regex::new(&word_regex).unwrap().is_match(&line_to_check)
                } else if options.line_match {
                    // Line match: entire line must match
                    line_to_check == pattern_to_check
                } else {
                    // Simple substring match
                    line_to_check.contains(&pattern_to_check)
                }
            }
        }
    }

    fn get_matches(&self, line: &str, options: &GrepOptions) -> Option<Vec<String>> {
        match self {
            Matcher::Regex(regex) => {
                let mut matches = Vec::new();
                for mat in regex.find_iter(line) {
                    matches.push(mat.as_str().to_string());
                }
                if matches.is_empty() { None } else { Some(matches) }
            }
            Matcher::Fixed(pattern, ignore_case) => {
                let (line_to_check, pattern_to_check) = if *ignore_case {
                    (line.to_lowercase(), pattern.to_lowercase())
                } else {
                    (line.to_string(), pattern.clone())
                };

                if options.word_match {
                    // For word matching, we need to find whole word matches
                    let word_regex = format!(r"\b{}\b", regex::escape(&pattern_to_check));
                    let regex = Regex::new(&word_regex).unwrap();
                    let mut matches = Vec::new();
                    for mat in regex.find_iter(&line_to_check) {
                        matches.push(line[mat.start()..mat.end()].to_string());
                    }
                    if matches.is_empty() { None } else { Some(matches) }
                } else if options.line_match {
                    // For line matching, if it matches, return the whole line
                    if self.matches(line, options) {
                        Some(vec![line.to_string()])
                    } else {
                        None
                    }
                } else {
                    // For substring matching, find all occurrences in the original line
                    let mut matches = Vec::new();
                    let mut start = 0;
                    while let Some(pos) = line_to_check[start..].find(&pattern_to_check) {
                        let actual_start = start + pos;
                        let actual_end = actual_start + pattern_to_check.len();
                        matches.push(line[actual_start..actual_end].to_string());
                        start = actual_end;
                    }
                    if matches.is_empty() { None } else { Some(matches) }
                }
            }
        }
    }
}

pub fn run(options: GrepOptions) -> Result<()> {
    let matcher = Matcher::new(&options.pattern, &options)?;

    let mut found_any_match = false;

    if options.files.is_empty() {
        // Read from stdin
        let stdin = io::stdin();
        let reader = BufReader::new(stdin);

        for (line_num, line_result) in reader.lines().enumerate() {
            let line = match line_result {
                Ok(l) => l,
                Err(e) => {
                    if !options.no_messages {
                        eprintln!("grep: error reading stdin: {}", e);
                    }
                    continue;
                }
            };

            let matches = matcher.matches(&line, &options);
            let should_print = if options.invert_match { !matches } else { matches };

            if should_print {
                found_any_match = true;
                if options.quiet {
                    return Ok(());
                }

                if options.only_matching {
                    if let Some(matches) = matcher.get_matches(&line, &options) {
                        for mat in matches {
                            if options.line_number {
                                println!("{}:{}", line_num + 1, mat);
                            } else {
                                println!("{}", mat);
                            }
                        }
                    }
                } else {
                    if options.line_number {
                        println!("{}:{}", line_num + 1, line);
                    } else {
                        println!("{}", line);
                    }
                }
            }
        }
    } else {
        // Search in files
        for file_path in &options.files {
            let found = search_file(file_path, &options, &matcher)?;
            if found {
                found_any_match = true;
                if options.quiet {
                    return Ok(());
                }
            }
        }
    }

    // Set exit code based on whether matches were found (grep convention)
    // Exit code 0 if matches found, 1 if no matches found, 2 if errors
    if !found_any_match {
        std::process::exit(1);
    }

    Ok(())
}