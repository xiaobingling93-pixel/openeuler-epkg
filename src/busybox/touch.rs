use clap::{value_parser, Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::ffi::OsString;
use std::fs::File;
use std::path::Path;
use crate::posix::posix_utime;

pub struct TouchOptions {
    pub files: Vec<OsString>,
    pub no_create: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<TouchOptions> {
    let files: Vec<OsString> = matches.get_many::<OsString>("files")
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
            .value_parser(value_parser!(OsString))
            .help("Files to touch")
            .required(true))
}

pub fn run(options: TouchOptions) -> Result<()> {
    for file_path in &options.files {
        let path = Path::new(file_path);

        if path.exists() {
            posix_utime(path, None, None)
                .map_err(|e| eyre!("touch: cannot touch '{}': {:?}", path.display(), e))?;
        } else if !options.no_create {
            File::create(path)
                .map_err(|e| eyre!("touch: cannot touch '{}': {}", path.display(), e))?;
        }
    }
    Ok(())
}