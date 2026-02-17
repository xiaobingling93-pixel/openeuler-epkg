use clap::{Arg, Command};
use color_eyre::Result;
use std::env;

pub struct PrintenvOptions {
    pub vars: Vec<String>,
    pub null: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<PrintenvOptions> {
    let vars: Vec<String> = matches.get_many::<String>("vars")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let null = matches.get_flag("null");

    Ok(PrintenvOptions { vars, null })
}

pub fn command() -> Command {
    Command::new("printenv")
        .about("Print environment variables")
        .arg(Arg::new("null")
            .short('0')
            .long("null")
            .help("End each output line with NUL, not newline")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("vars")
            .num_args(0..)
            .help("Environment variables to print (if none, print all)"))
}

pub fn run(options: PrintenvOptions) -> Result<()> {
    if options.vars.is_empty() {
        // Print all environment variables
        for (key, value) in env::vars() {
            if options.null {
                print!("{}={}\0", key, value);
            } else {
                println!("{}={}", key, value);
            }
        }
    } else {
        // Print specific variables
        for var in &options.vars {
            match env::var(var) {
                Ok(value) => {
                    if options.null {
                        print!("{}\0", value);
                    } else {
                        println!("{}", value);
                    }
                }
                Err(_) => {
                    // Variable not found, exit with status 1
                    std::process::exit(1);
                }
            }
        }
    }
    Ok(())
}
