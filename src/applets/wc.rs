use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs::File;
use std::io::{self, Read};

pub struct WcOptions {
    pub files: Vec<String>,
    pub lines: bool,
    pub words: bool,
    pub bytes: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<WcOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let lines = matches.get_flag("lines");
    let words = matches.get_flag("words");
    let bytes = matches.get_flag("bytes");

    Ok(WcOptions {
        files,
        lines,
        words,
        bytes,
    })
}

pub fn command() -> Command {
    Command::new("wc")
        .about("Print newline, word, and byte counts for each file")
        .arg(Arg::new("lines")
            .short('l')
            .long("lines")
            .help("Print only line counts")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("words")
            .short('w')
            .long("words")
            .help("Print only word counts")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("bytes")
            .short('c')
            .long("bytes")
            .help("Print only byte counts")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files to count (if none, read from stdin)"))
}

#[derive(Default)]
struct Counts {
    lines: usize,
    words: usize,
    bytes: usize,
}

fn count_content(content: &str) -> Counts {
    let lines = content.lines().count();
    let words = content.split_whitespace().count();
    let bytes = content.len();

    Counts { lines, words, bytes }
}

fn count_file(file_path: &str) -> Result<Counts> {
    let mut file = File::open(file_path)
        .map_err(|e| eyre!("wc: {}: {}", file_path, e))?;

    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|e| eyre!("wc: error reading {}: {}", file_path, e))?;

    Ok(count_content(&content))
}

fn count_stdin() -> Result<Counts> {
    let mut content = String::new();
    io::stdin()
        .read_to_string(&mut content)
        .map_err(|e| eyre!("wc: error reading stdin: {}", e))?;

    Ok(count_content(&content))
}

fn print_counts(counts: &Counts, options: &WcOptions, filename: Option<&str>) {
    let show_all = !options.lines && !options.words && !options.bytes;

    if options.lines || show_all {
        print!("{:>8}", counts.lines);
    }
    if options.words || show_all {
        print!("{:>8}", counts.words);
    }
    if options.bytes || show_all {
        print!("{:>8}", counts.bytes);
    }

    if let Some(filename) = filename {
        print!(" {}", filename);
    }
    println!();
}

pub fn run(options: WcOptions) -> Result<()> {
    if options.files.is_empty() {
        // Count from stdin
        let counts = count_stdin()?;
        print_counts(&counts, &options, None);
    } else {
        let mut total_counts = Counts::default();

        // Count each file
        for file_path in &options.files {
            let counts = count_file(file_path)?;
            print_counts(&counts, &options, Some(file_path));

            total_counts.lines += counts.lines;
            total_counts.words += counts.words;
            total_counts.bytes += counts.bytes;
        }

        // Print totals if more than one file
        if options.files.len() > 1 {
            print_counts(&total_counts, &options, Some("total"));
        }
    }

    Ok(())
}