use clap::Command;
use color_eyre::Result;

use super::grep::{GrepOptions, MatchMode};

pub fn command() -> Command {
    super::grep::build_shared_args(Command::new("egrep")
        .about("Search for extended regular expressions in files"))
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<GrepOptions> {
    super::grep::parse_shared_options(matches, MatchMode::Extended)
}

pub fn run(options: GrepOptions) -> Result<()> {
    super::grep::run(options)
}
