use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::path::Path;

pub struct LinkOptions {
    pub file1: String,
    pub file2: String,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<LinkOptions> {
    let args: Vec<String> = matches.get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    if args.len() != 2 {
        return Err(eyre!("link: missing operand"));
    }

    Ok(LinkOptions {
        file1: args[0].clone(),
        file2: args[1].clone(),
    })
}

pub fn command() -> Command {
    Command::new("link")
        .about("Create a hard link")
        .arg(Arg::new("args")
            .num_args(2)
            .required(true)
            .help("FILE1 and FILE2"))
}

pub fn run(options: LinkOptions) -> Result<()> {
    let file1_path = Path::new(&options.file1);
    let file2_path = Path::new(&options.file2);

    fs::hard_link(file1_path, file2_path)
        .map_err(|e| eyre!("link: cannot create link '{}' to '{}': {}", options.file2, options.file1, e))?;

    Ok(())
}
