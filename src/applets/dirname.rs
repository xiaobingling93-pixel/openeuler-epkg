use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::path::Path;

pub struct DirnameOptions {
    pub name: String,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<DirnameOptions> {
    let name = matches.get_one::<String>("name")
        .ok_or_else(|| eyre!("dirname: missing operand"))?
        .clone();

    Ok(DirnameOptions { name })
}

pub fn command() -> Command {
    Command::new("dirname")
        .about("Strip last component from file name")
        .arg(Arg::new("name")
            .required(true)
            .help("File path"))
}

pub fn run(options: DirnameOptions) -> Result<()> {
    let path = Path::new(&options.name);
    let dirname = path.parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                "."
            } else {
                p.as_os_str().to_str().unwrap_or(".")
            }
        })
        .unwrap_or(".");

    println!("{}", dirname);
    Ok(())
}
