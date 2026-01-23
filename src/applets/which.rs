use clap::{Arg, Command};
use color_eyre::Result;
use std::env;

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
    if let Some(paths) = env::var_os("PATH") {
        for path_dir in env::split_paths(&paths) {
            let full_path = path_dir.join(command);
            if full_path.exists() && full_path.is_file() {
                // Check if executable (on Unix systems)
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(metadata) = full_path.metadata() {
                        let permissions = metadata.permissions();
                        if permissions.mode() & 0o111 != 0 {
                            return Some(full_path.to_string_lossy().to_string());
                        }
                    }
                }
                #[cfg(not(unix))]
                {
                    return Some(full_path.to_string_lossy().to_string());
                }
            }
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