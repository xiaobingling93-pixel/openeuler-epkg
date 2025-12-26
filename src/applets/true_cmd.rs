use clap::Command;
use color_eyre::Result;

pub struct TrueOptions;

pub fn parse_options(_matches: &clap::ArgMatches) -> Result<TrueOptions> {
    Ok(TrueOptions)
}

pub fn command() -> Command {
    Command::new("true")
        .about("Return a successful exit status")
}

pub fn run(_options: TrueOptions) -> Result<()> {
    Ok(())
}

