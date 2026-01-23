use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::path::Path;
use crate::posix::posix_chmod;

pub struct ChmodOptions {
    pub mode: String,
    pub files: Vec<String>,
    pub recursive: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<ChmodOptions> {
    let mode = matches.get_one::<String>("mode")
        .ok_or_else(|| eyre!("chmod: missing operand"))?
        .clone();

    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let recursive = matches.get_flag("recursive");

    Ok(ChmodOptions { mode, files, recursive })
}

pub fn command() -> Command {
    Command::new("chmod")
        .about("Change file permissions")
        .arg(Arg::new("recursive")
            .short('R')
            .long("recursive")
            .help("Change permissions recursively")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("mode")
            .help("Permission mode (octal or symbolic)")
            .required(true))
        .arg(Arg::new("files")
            .num_args(1..)
            .help("Files to change permissions for")
            .required(true))
}

fn apply_mode_to_path(path: &Path, mode_str: &str) -> Result<()> {
    let path_str = path.to_string_lossy();
    posix_chmod(&path_str, mode_str)
        .map_err(|e| eyre!("chmod: cannot change permissions of '{}': {:?}", path.display(), e))?;
    Ok(())
}

fn process_path_recursive(path: &Path, mode_str: &str, recursive: bool) -> Result<()> {
    if recursive && path.is_dir() {
        // Process directory recursively
        for entry in fs::read_dir(path)
            .map_err(|e| eyre!("chmod: cannot read directory '{}': {}", path.display(), e))?
        {
            let entry = entry
                .map_err(|e| eyre!("chmod: cannot read directory entry in '{}': {}", path.display(), e))?;
            let entry_path = entry.path();
            process_path_recursive(&entry_path, mode_str, recursive)?;
        }
    }

    // Apply mode to current path
    apply_mode_to_path(path, mode_str)?;

    Ok(())
}

pub fn run(options: ChmodOptions) -> Result<()> {
    for file_path in &options.files {
        let path = Path::new(file_path);
        process_path_recursive(path, &options.mode, options.recursive)?;
    }

    Ok(())
}