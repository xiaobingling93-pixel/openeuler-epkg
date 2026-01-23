use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::io::{self, Read};

pub struct XargsOptions {
    pub command: Vec<String>,
    pub max_args: Option<usize>,
    pub delimiter: Option<String>,
    pub null_delimiter: bool,
    pub no_run_if_empty: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<XargsOptions> {
    let command: Vec<String> = matches.get_many::<String>("command")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let max_args = matches.get_one::<String>("max-args")
        .and_then(|s| s.parse().ok());

    let delimiter = matches.get_one::<String>("delimiter")
        .map(|s| s.clone());

    let null_delimiter = matches.get_flag("null");

    let no_run_if_empty = matches.get_flag("no-run-if-empty");

    Ok(XargsOptions {
        command,
        max_args,
        delimiter,
        null_delimiter,
        no_run_if_empty,
    })
}

pub fn command() -> Command {
    Command::new("xargs")
        .about("Build and execute command lines from standard input")
        .arg(Arg::new("max-args")
            .short('n')
            .long("max-args")
            .help("Maximum arguments per command line")
            .value_name("MAX"))
        .arg(Arg::new("delimiter")
            .short('d')
            .long("delimiter")
            .help("Input delimiter (default: whitespace)")
            .value_name("DELIM"))
        .arg(Arg::new("null")
            .short('0')
            .long("null")
            .help("Input items are terminated by null character")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("no-run-if-empty")
            .short('r')
            .long("no-run-if-empty")
            .help("Do not run command if input is empty")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("command")
            .num_args(1..)
            .help("Command to execute")
            .required(true))
}

pub fn run(options: XargsOptions) -> Result<()> {
    let mut stdin = io::stdin();
    let mut input = String::new();

    stdin.read_line(&mut input)
        .map_err(|e| eyre!("xargs: error reading input: {}", e))?;

    // Read all remaining input
    let mut buffer = Vec::new();
    stdin.read_to_end(&mut buffer)
        .map_err(|e| eyre!("xargs: error reading input: {}", e))?;

    input.push_str(&String::from_utf8_lossy(&buffer));

    // Remove trailing newline if present
    if input.ends_with('\n') {
        input.pop();
    }

    // Split input based on delimiter
    let args: Vec<String> = if options.null_delimiter {
        // Split on null bytes
        input.split('\0')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    } else if let Some(delimiter) = &options.delimiter {
        // Split on custom delimiter
        input.split(delimiter)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    } else {
        // Split on whitespace (default behavior)
        input.split_whitespace()
            .map(|s| s.to_string())
            .collect()
    };

    // Check if we should run the command
    if options.no_run_if_empty && args.is_empty() {
        return Ok(());
    }

    // Execute commands in batches based on max_args
    if let Some(max_args) = options.max_args {
        for chunk in args.chunks(max_args) {
            execute_command(&options.command, chunk)?;
        }
    } else {
        // If no max_args specified, execute once with all arguments
        if !args.is_empty() {
            execute_command(&options.command, &args)?;
        }
    }

    Ok(())
}

fn execute_command(base_command: &[String], args: &[String]) -> Result<()> {
    if base_command.is_empty() {
        return Ok(());
    }

    let mut cmd = std::process::Command::new(&base_command[0]);
    cmd.args(&base_command[1..]);
    cmd.args(args);

    let status = cmd.status()
        .map_err(|e| eyre!("xargs: failed to execute command: {}", e))?;

    if !status.success() {
        // xargs typically continues even if commands fail, but we can exit on failure
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}