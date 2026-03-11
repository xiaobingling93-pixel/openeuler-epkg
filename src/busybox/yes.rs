use clap::{Arg, Command};
use color_eyre::Result;

pub struct YesOptions {
    pub string: String,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<YesOptions> {
    let string = matches.get_many::<String>("string")
        .map(|vals| vals.cloned().collect::<Vec<String>>().join(" "))
        .unwrap_or_else(|| "y".to_string());

    Ok(YesOptions { string })
}

pub fn command() -> Command {
    Command::new("yes")
        .about("Repeatedly output a string")
        .arg(Arg::new("string")
            .num_args(0..)
            .help("String to output (default: 'y')"))
}

pub fn run(options: YesOptions) -> Result<()> {
    loop {
        println!("{}", options.string);
    }
}
