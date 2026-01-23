use clap::{Arg, Command};
use color_eyre::Result;
use std::fs;
use std::path::Path;
use std::os::unix::fs::PermissionsExt;

pub struct TestOptions {
    pub expression: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<TestOptions> {
    let expression: Vec<String> = matches.get_many::<String>("expression")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(TestOptions { expression })
}

pub fn command() -> Command {
    Command::new("test")
        .about("Evaluate expressions")
        .arg(Arg::new("expression")
            .num_args(0..)
            .help("Expression to evaluate"))
}

fn evaluate_expression(args: &[String]) -> bool {
    if args.is_empty() {
        return false;
    }

    match args[0].as_str() {
        "!" => {
            if args.len() < 2 {
                return false;
            }
            !evaluate_expression(&args[1..])
        }
        "-a" => {
            if args.len() < 3 {
                return false;
            }
            evaluate_expression(&args[1..2]) && evaluate_expression(&args[2..])
        }
        "-o" => {
            if args.len() < 3 {
                return false;
            }
            evaluate_expression(&args[1..2]) || evaluate_expression(&args[2..])
        }
        "-f" => {
            if args.len() < 2 {
                return false;
            }
            Path::new(&args[1]).is_file()
        }
        "-d" => {
            if args.len() < 2 {
                return false;
            }
            Path::new(&args[1]).is_dir()
        }
        "-e" => {
            if args.len() < 2 {
                return false;
            }
            Path::new(&args[1]).exists()
        }
        "-s" => {
            if args.len() < 2 {
                return false;
            }
            match fs::metadata(&args[1]) {
                Ok(metadata) => metadata.len() > 0,
                Err(_) => false,
            }
        }
        "-r" => {
            if args.len() < 2 {
                return false;
            }
            match fs::metadata(&args[1]) {
                Ok(metadata) => {
                    #[cfg(unix)]
                    {
                        let permissions = metadata.permissions();
                        permissions.mode() & 0o400 != 0
                    }
                    #[cfg(not(unix))]
                    {
                        true // Assume readable on non-Unix
                    }
                }
                Err(_) => false,
            }
        }
        "-w" => {
            if args.len() < 2 {
                return false;
            }
            match fs::metadata(&args[1]) {
                Ok(metadata) => {
                    #[cfg(unix)]
                    {
                        let permissions = metadata.permissions();
                        permissions.mode() & 0o200 != 0
                    }
                    #[cfg(not(unix))]
                    {
                        true // Assume writable on non-Unix
                    }
                }
                Err(_) => false,
            }
        }
        "-x" => {
            if args.len() < 2 {
                return false;
            }
            match fs::metadata(&args[1]) {
                Ok(metadata) => {
                    #[cfg(unix)]
                    {
                        let permissions = metadata.permissions();
                        permissions.mode() & 0o100 != 0
                    }
                    #[cfg(not(unix))]
                    {
                        true // Assume executable on non-Unix
                    }
                }
                Err(_) => false,
            }
        }
        "-z" => {
            if args.len() < 2 {
                return false;
            }
            args[1].is_empty()
        }
        "-n" => {
            if args.len() < 2 {
                return false;
            }
            !args[1].is_empty()
        }
        "=" => {
            if args.len() < 3 {
                return false;
            }
            args[1] == args[2]
        }
        "!=" => {
            if args.len() < 3 {
                return false;
            }
            args[1] != args[2]
        }
        "-eq" => {
            if args.len() < 3 {
                return false;
            }
            match (args[1].parse::<i64>(), args[2].parse::<i64>()) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            }
        }
        "-ne" => {
            if args.len() < 3 {
                return false;
            }
            match (args[1].parse::<i64>(), args[2].parse::<i64>()) {
                (Ok(a), Ok(b)) => a != b,
                _ => false,
            }
        }
        "-lt" => {
            if args.len() < 3 {
                return false;
            }
            match (args[1].parse::<i64>(), args[2].parse::<i64>()) {
                (Ok(a), Ok(b)) => a < b,
                _ => false,
            }
        }
        "-le" => {
            if args.len() < 3 {
                return false;
            }
            match (args[1].parse::<i64>(), args[2].parse::<i64>()) {
                (Ok(a), Ok(b)) => a <= b,
                _ => false,
            }
        }
        "-gt" => {
            if args.len() < 3 {
                return false;
            }
            match (args[1].parse::<i64>(), args[2].parse::<i64>()) {
                (Ok(a), Ok(b)) => a > b,
                _ => false,
            }
        }
        "-ge" => {
            if args.len() < 3 {
                return false;
            }
            match (args[1].parse::<i64>(), args[2].parse::<i64>()) {
                (Ok(a), Ok(b)) => a >= b,
                _ => false,
            }
        }
        _ => {
            // Single argument - test if non-empty
            if args.len() == 1 {
                !args[0].is_empty()
            } else {
                // Default to false for unrecognized expressions
                false
            }
        }
    }
}

pub fn run(options: TestOptions) -> Result<()> {
    let result = evaluate_expression(&options.expression);

    if result {
        std::process::exit(0);
    } else {
        std::process::exit(1);
    }
}
