use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::path::Path;

pub struct BasenameOptions {
    pub name: String,
    pub suffix: Option<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<BasenameOptions> {
    let name = matches.get_one::<String>("name")
        .ok_or_else(|| eyre!("basename: missing operand"))?
        .clone();

    let suffix = matches.get_one::<String>("suffix").cloned();

    Ok(BasenameOptions { name, suffix })
}

pub fn command() -> Command {
    Command::new("basename")
        .about("Strip directory and suffix from filenames")
        .arg(Arg::new("name")
            .required(true)
            .help("File path"))
        .arg(Arg::new("suffix")
            .help("Suffix to remove"))
}

pub fn run(options: BasenameOptions) -> Result<()> {
    let path = Path::new(&options.name);
    let mut basename = path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    // Remove suffix if provided (do not remove when result would be empty — POSIX/busybox behavior)
    if let Some(ref suffix) = options.suffix {
        if basename.ends_with(suffix) && basename.len() > suffix.len() {
            basename.truncate(basename.len() - suffix.len());
        }
    }

    println!("{}", basename);
    Ok(())
}
