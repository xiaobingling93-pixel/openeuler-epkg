use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::path::Path;
use crate::lfs;

pub struct RmOptions {
    pub files: Vec<String>,
    pub recursive: bool,
    pub force: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<RmOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let recursive = matches.get_flag("recursive");
    let force = matches.get_flag("force");

    Ok(RmOptions {
        files,
        recursive,
        force,
    })
}

pub fn command() -> Command {
    Command::new("rm")
        .about("Remove files or directories")
        .arg(Arg::new("recursive")
            .short('r')
            .visible_short_alias('R')
            .long("recursive")
            .help("Remove directories and their contents recursively")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("force")
            .short('f')
            .long("force")
            .help("Ignore nonexistent files and arguments, never prompt")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files")
            .num_args(1..)
            .help("Files or directories to remove")
            .required(true))
}

fn remove_path(path: &Path, recursive: bool, force: bool) -> Result<()> {
    // Check if the path exists as a filesystem entry (including dangling symlinks)
    let symlink_meta = path.symlink_metadata();
    if let Err(_e) = symlink_meta {
        // Path does not exist at all
        if !force {
            return Err(eyre!("rm: cannot remove '{}': No such file or directory", path.display()));
        }
        return Ok(());
    }
    let metadata = symlink_meta.unwrap();

    if metadata.file_type().is_symlink() {
        // Symlinks are removed as files, regardless of target
        lfs::remove_file(path)?;
    } else if metadata.file_type().is_dir() {
        // Directory
        if recursive {
            lfs::remove_dir_all(path)?;
        } else {
            return Err(eyre!("rm: cannot remove '{}': Is a directory", path.display()));
        }
    } else {
        // Regular file or other type
        lfs::remove_file(path)?;
    }

    Ok(())
}

pub fn run(options: RmOptions) -> Result<()> {
    for file_path in &options.files {
        let path = Path::new(file_path);
        remove_path(path, options.recursive, options.force)?;
    }
    Ok(())
}
