use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::time::Duration;

pub struct UsleepOptions {
    pub microseconds: u64,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<UsleepOptions> {
    let microseconds = matches.get_one::<String>("microseconds")
        .ok_or_else(|| eyre!("usleep: missing operand"))?
        .parse::<u64>()
        .map_err(|e| eyre!("usleep: invalid number '{}': {}", matches.get_one::<String>("microseconds").unwrap(), e))?;

    Ok(UsleepOptions { microseconds })
}

pub fn command() -> Command {
    Command::new("usleep")
        .about("Sleep for MICROSECONDS microseconds")
        .arg(Arg::new("microseconds")
            .required(true)
            .help("Number of microseconds to sleep"))
}

pub fn run(options: UsleepOptions) -> Result<()> {
    let duration = Duration::from_micros(options.microseconds);
    std::thread::sleep(duration);
    Ok(())
}
