use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::path::Path;
use crate::lfs;

pub struct UnlinkOptions {
    pub file: String,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<UnlinkOptions> {
    let file = matches.get_one::<String>("file")
        .ok_or_else(|| eyre!("unlink: missing operand"))?
        .clone();

    Ok(UnlinkOptions { file })
}

pub fn command() -> Command {
    Command::new("unlink")
        .about("Remove a file")
        .arg(Arg::new("file")
            .required(true)
            .help("File to remove"))
}

pub fn run(options: UnlinkOptions) -> Result<()> {
    let path = Path::new(&options.file);
    lfs::remove_file(path)?;
    Ok(())
}
