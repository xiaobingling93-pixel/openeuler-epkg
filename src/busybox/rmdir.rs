use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::path::Path;
#[cfg(unix)]
use libc;

#[cfg(unix)]
fn is_directory_not_empty_error(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(libc::ENOTEMPTY)
}

#[cfg(not(unix))]
fn is_directory_not_empty_error(_e: &std::io::Error) -> bool {
    false
}

pub struct RmdirOptions {
    pub directories: Vec<String>,
    pub parents: bool,
    pub ignore_fail_on_non_empty: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<RmdirOptions> {
    let directories: Vec<String> = matches.get_many::<String>("directories")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let parents = matches.get_flag("parents");
    let ignore_fail_on_non_empty = matches.get_flag("ignore-fail-on-non-empty");

    Ok(RmdirOptions {
        directories,
        parents,
        ignore_fail_on_non_empty,
    })
}

pub fn command() -> Command {
    Command::new("rmdir")
        .about("Remove empty directories")
        .arg(Arg::new("parents")
            .short('p')
            .long("parents")
            .help("Remove parent directories if empty")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("ignore-fail-on-non-empty")
            .long("ignore-fail-on-non-empty")
            .help("Ignore failures when directory is not empty")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("directories")
            .num_args(1..)
            .help("Directories to remove")
            .required(true))
}

fn remove_directory(path: &Path, parents: bool, ignore_fail_on_non_empty: bool) -> Result<()> {
    if parents {
        // Remove parent directories recursively
        let mut current_path = path.to_path_buf();
        while current_path != Path::new("/") && current_path != Path::new("") {
            match fs::remove_dir(&current_path) {
                Ok(()) => {},
                Err(e) => {
                    if ignore_fail_on_non_empty && is_directory_not_empty_error(&e) {
                        // Ignore the error for non-empty directories
                    } else {
                        return Err(eyre!("rmdir: failed to remove '{}': {}", current_path.display(), e));
                    }
                }
            }

            // Move to parent directory
            if let Some(parent) = current_path.parent() {
                current_path = parent.to_path_buf();
            } else {
                break;
            }
        }
    } else {
        match fs::remove_dir(path) {
            Ok(()) => {},
            Err(e) => {
                if ignore_fail_on_non_empty && is_directory_not_empty_error(&e) {
                    // Ignore the error for non-empty directories
                } else {
                    return Err(eyre!("rmdir: failed to remove '{}': {}", path.display(), e));
                }
            }
        }
    }

    Ok(())
}

pub fn run(options: RmdirOptions) -> Result<()> {
    for dir_path in &options.directories {
        let path = Path::new(dir_path);
        remove_directory(path, options.parents, options.ignore_fail_on_non_empty)?;
    }
    Ok(())
}