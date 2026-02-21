use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::path::Path;
use crate::applets::extract_reference_metadata;
use crate::posix::posix_chmod;

pub struct ChmodOptions {
    pub mode: String,
    pub files: Vec<String>,
    pub recursive: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<ChmodOptions> {
    let recursive = matches.get_flag("recursive");
    let reference = matches.get_one::<String>("reference").cloned();

    let args: Vec<String> = matches.get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    if args.is_empty() && reference.is_none() {
        return Err(eyre!("chmod: missing operand"));
    }

    let (mode, files) = if let Some(ref ref_file) = reference {
        // --reference mode: get mode from reference file
        if args.is_empty() {
            return Err(eyre!("chmod: missing operand"));
        }
        let ref_path = std::path::Path::new(ref_file);
        let (_, _, mode) = extract_reference_metadata(ref_path)
            .map_err(|e| eyre!("chmod: {}", e))?;
        (format!("{:o}", mode), args)
    } else {
        let mode = args[0].clone();
        let files = args[1..].to_vec();
        if files.is_empty() {
            return Err(eyre!("chmod: missing operand"));
        }
        (mode, files)
    };

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
        .arg(Arg::new("reference")
            .long("reference")
            .help("Use RFILE's mode rather than specifying a MODE value")
            .value_name("RFILE"))
        .arg(Arg::new("args")
            .num_args(1..)
            .help("MODE and files (or just files with --reference)"))
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