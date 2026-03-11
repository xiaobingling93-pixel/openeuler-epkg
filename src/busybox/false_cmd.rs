use clap::Command;
use color_eyre::Result;

pub struct FalseOptions;

pub fn parse_options(_matches: &clap::ArgMatches) -> Result<FalseOptions> {
    Ok(FalseOptions)
}

pub fn command() -> Command {
    Command::new("false")
        .about("Return an unsuccessful exit status")
}

pub fn run(_options: FalseOptions) -> Result<()> {
    std::process::exit(1);
}

