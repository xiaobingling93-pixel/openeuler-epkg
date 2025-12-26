use clap::{Arg, Command};
use color_eyre::Result;
use std::fs;
use std::path::PathBuf;

pub struct LsOptions {
    pub directory: PathBuf,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<LsOptions> {
    let directory = matches.get_one::<String>("directory")
        .map(|s| PathBuf::from(s))
        .unwrap_or_else(|| PathBuf::from("."));

    Ok(LsOptions { directory })
}

pub fn command() -> Command {
    Command::new("ls")
        .about("List directory contents")
        .arg(Arg::new("directory")
            .help("Directory to list (default: current directory)"))
}

pub fn run(options: LsOptions) -> Result<()> {
    let entries = fs::read_dir(&options.directory)
        .map_err(|e| color_eyre::eyre::eyre!("ls: {}: {}", options.directory.display(), e))?;

    let mut names: Vec<String> = entries
        .filter_map(|entry| {
            entry.ok().map(|e| {
                e.file_name().to_string_lossy().to_string()
            })
        })
        .collect();

    names.sort();
    let output = names.join("\n");
    println!("{}", output);
    Ok(())
}

