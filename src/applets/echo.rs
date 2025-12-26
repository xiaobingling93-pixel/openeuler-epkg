use clap::{Arg, Command};
use color_eyre::Result;

pub struct EchoOptions {
    pub text: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<EchoOptions> {
    let text: Vec<String> = matches.get_many::<String>("text")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(EchoOptions { text })
}

pub fn command() -> Command {
    Command::new("echo")
        .about("Display a line of text")
        .arg(Arg::new("text")
            .num_args(0..)
            .help("Text to display"))
}

pub fn run(options: EchoOptions) -> Result<()> {
    println!("{}", options.text.join(" "));
    Ok(())
}

