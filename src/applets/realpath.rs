use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::path::Path;

pub struct RealpathOptions {
    pub files: Vec<String>,
    pub canonicalize: bool,
    pub quiet: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<RealpathOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let canonicalize = matches.get_flag("canonicalize");
    let quiet = matches.get_flag("quiet");

    if files.is_empty() {
        return Err(eyre!("realpath: missing operand"));
    }

    Ok(RealpathOptions {
        files,
        canonicalize,
        quiet,
    })
}

pub fn command() -> Command {
    Command::new("realpath")
        .about("Print the resolved absolute file name")
        .arg(Arg::new("canonicalize")
            .short('e')
            .long("canonicalize-existing")
            .help("All components of the path must exist")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("quiet")
            .short('q')
            .long("quiet")
            .help("Suppress most error messages")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files")
            .num_args(1..)
            .help("Files to resolve"))
}

pub fn run(options: RealpathOptions) -> Result<()> {
    for file in &options.files {
        let path = Path::new(file);
        // Default behavior: canonicalize (resolve symlinks)
        // -e requires all components to exist
        match if options.canonicalize {
            // -e: all components must exist
            path.canonicalize()
        } else {
            // Default: canonicalize, but handle missing components gracefully
            if path.is_absolute() {
                path.canonicalize()
                    .or_else(|_| Ok(path.to_path_buf()))
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(path))
                    .and_then(|p| p.canonicalize())
                    .or_else(|_| {
                        std::env::current_dir()
                            .map(|cwd| cwd.join(path))
                    })
            }
        } {
            Ok(real_path) => {
                println!("{}", real_path.display());
            }
            Err(e) => {
                if !options.quiet {
                    eprintln!("realpath: {}: {}", file, e);
                }
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
