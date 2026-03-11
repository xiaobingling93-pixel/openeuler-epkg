use clap::{Arg, Command};
use color_eyre::Result;
use std::env;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use crate::run::is_executable;

#[cfg(unix)]
const DEFAULT_PATH: &str = "/sbin:/usr/sbin:/bin:/usr/bin";
#[cfg(windows)]
const DEFAULT_PATH: &str = "C:\\Windows\\System32;C:\\Windows;C:\\Windows\\System32\\Wbem";

pub struct WhichOptions {
    pub commands: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<WhichOptions> {
    let commands: Vec<String> = matches.get_many::<String>("commands")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(WhichOptions { commands })
}

pub fn command() -> Command {
    Command::new("which")
        .about("Locate commands in PATH")
        .arg(Arg::new("commands")
            .num_args(1..)
            .help("Commands to locate")
            .required(true))
}

fn find_command_in_path(command: &str) -> Option<String> {
    // If command contains a slash, treat as direct path
    #[cfg(unix)]
    let has_path_separator = command.contains('/');
    #[cfg(windows)]
    let has_path_separator = command.contains('/') || command.contains('\\');
    #[cfg(not(any(unix, windows)))]
    let has_path_separator = command.contains('/');

    if has_path_separator {
        let path = Path::new(command);
        if path.exists() && path.is_file() && {
            #[cfg(unix)]
            { is_executable(path).ok()? }
            #[cfg(not(unix))]
            { true } // On non-Unix, assume any file is executable
        } {
            return Some(command.to_string());
        }
        return None;
    }

    // Determine search directories
    let path_dirs: Vec<PathBuf> = if let Some(paths) = env::var_os("PATH") {
        env::split_paths(&paths).collect()
    } else {
        #[cfg(unix)]
        {
            DEFAULT_PATH.split(':').map(PathBuf::from).collect()
        }
        #[cfg(windows)]
        {
            DEFAULT_PATH.split(';').map(PathBuf::from).collect()
        }
        #[cfg(not(any(unix, windows)))]
        {
            Vec::new()
        }
    };

    for path_dir in path_dirs {
        let full_path = path_dir.join(command);
        if full_path.exists() && full_path.is_file() && {
            #[cfg(unix)]
            { is_executable(&full_path).ok()? }
            #[cfg(not(unix))]
            { true } // On non-Unix, assume any file is executable
        } {
            return Some(full_path.to_string_lossy().to_string());
        }
    }
    None
}

pub fn run(options: WhichOptions) -> Result<()> {
    let mut found_any = false;

    for command in &options.commands {
        if let Some(path) = find_command_in_path(command) {
            println!("{}", path);
            found_any = true;
        }
    }

    if !found_any {
        // which typically exits with non-zero status if no commands found
        std::process::exit(1);
    }

    Ok(())
}
