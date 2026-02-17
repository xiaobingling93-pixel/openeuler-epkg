use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::path::Path;
use std::path::PathBuf;

pub struct ReadlinkOptions {
    pub files: Vec<String>,
    pub canonicalize: bool,
    pub canonicalize_existing: bool,
    pub canonicalize_missing: bool,
    pub no_newline: bool,
    pub quiet: bool,
    pub verbose: bool,
    pub zero: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<ReadlinkOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let canonicalize = matches.get_flag("canonicalize");
    let canonicalize_existing = matches.get_flag("canonicalize-existing");
    let canonicalize_missing = matches.get_flag("canonicalize-missing");
    let no_newline = matches.get_flag("no-newline");
    let quiet = matches.get_flag("quiet") || matches.get_flag("silent");
    let verbose = matches.get_flag("verbose");
    let zero = matches.get_flag("zero");

    if files.is_empty() {
        return Err(eyre!("readlink: missing operand"));
    }

    Ok(ReadlinkOptions {
        files,
        canonicalize,
        canonicalize_existing,
        canonicalize_missing,
        no_newline,
        quiet,
        verbose,
        zero,
    })
}

pub fn command() -> Command {
    Command::new("readlink")
        .about("Print value of a symbolic link")
        .arg(Arg::new("canonicalize")
            .short('f')
            .long("canonicalize")
            .help("Canonicalize by following every symlink in every component of the given name recursively")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("canonicalize-existing")
            .short('e')
            .long("canonicalize-existing")
            .help("Canonicalize, all components must exist")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("canonicalize-missing")
            .short('m')
            .long("canonicalize-missing")
            .help("Canonicalize, no requirements on components existence")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("no-newline")
            .short('n')
            .long("no-newline")
            .help("Do not output the trailing delimiter")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("quiet")
            .short('q')
            .help("Suppress error messages")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("silent")
            .short('s')
            .long("silent")
            .help("Suppress most error messages")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("verbose")
            .short('v')
            .long("verbose")
            .help("Report error messages")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("zero")
            .short('z')
            .long("zero")
            .help("End each output line with NUL, not newline")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files")
            .num_args(1..)
            .required(true)
            .help("Symbolic links to read"))
}

fn output_delimiter(options: &ReadlinkOptions) -> &'static str {
    if options.zero { "\0" } else { "\n" }
}

fn resolve_target_path(path: &Path, options: &ReadlinkOptions) -> std::io::Result<PathBuf> {
    if options.canonicalize || options.canonicalize_existing || options.canonicalize_missing {
        // Canonicalize the entire path
        if options.canonicalize_existing {
            path.canonicalize()
        } else if options.canonicalize_missing {
            // For -m, we try to canonicalize but don't require existence
            path.canonicalize().or_else(|_| {
                // If canonicalize fails, try to resolve what we can
                if path.is_absolute() {
                    Ok(path.to_path_buf())
                } else {
                    std::env::current_dir()
                        .map(|cwd| cwd.join(path))
                        .and_then(|p| p.canonicalize())
                }
            })
        } else {
            path.canonicalize()
        }
    } else {
        // Just read the symlink target
        std::fs::read_link(path).map(|p| p.to_path_buf())
    }
}

fn print_target_path(target_path: &Path, options: &ReadlinkOptions, delimiter: &str) {
    if options.no_newline {
        print!("{}", target_path.display());
    } else {
        print!("{}{}", target_path.display(), delimiter);
    }
}

fn handle_error(file: &str, e: std::io::Error, options: &ReadlinkOptions) -> ! {
    if !options.quiet {
        if options.verbose {
            eprintln!("readlink: {}: {}", file, e);
        }
    }
    std::process::exit(1);
}

pub fn run(options: ReadlinkOptions) -> Result<()> {
    let delimiter = output_delimiter(&options);

    for file in &options.files {
        let path = Path::new(file);

        match resolve_target_path(path, &options) {
            Ok(target_path) => {
                print_target_path(&target_path, &options, delimiter);
            }
            Err(e) => {
                handle_error(file, e, &options);
            }
        }
    }
    Ok(())
}
