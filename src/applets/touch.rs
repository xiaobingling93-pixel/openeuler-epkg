use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs::File;
use std::path::Path;
use crate::posix::posix_utime;

pub struct TouchOptions {
    pub files: Vec<String>,
    pub no_create: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<TouchOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let no_create = matches.get_flag("no-create");

    Ok(TouchOptions {
        files,
        no_create,
    })
}

pub fn command() -> Command {
    Command::new("touch")
        .about("Update file timestamps or create files")
        .arg(Arg::new("no-create")
            .short('c')
            .long("no-create")
            .help("Do not create files that do not exist")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files")
            .num_args(1..)
            .help("Files to touch")
            .required(true))
}

pub fn run(options: TouchOptions) -> Result<()> {
    for file_path in &options.files {
        let path = Path::new(file_path);

        if path.exists() {
            // Update both access and modification times to current time
            posix_utime(file_path, None, None)
                .map_err(|e| eyre!("touch: cannot touch '{}': {:?}", file_path, e))?;
        } else if !options.no_create {
            // Create the file
            File::create(path)
                .map_err(|e| eyre!("touch: cannot touch '{}': {}", file_path, e))?;
            // File was just created, so times are already current
        }
        // If file doesn't exist and no_create is true, do nothing
    }
    Ok(())
}