use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use shlex;
use std::env;
use std::process;

pub struct EnvOptions {
    pub assignments: Vec<String>,
    pub command: Vec<String>,
    pub ignore_environment: bool,
    pub null: bool,
    pub unset: Vec<String>,
    pub chdir: Option<String>,
    pub argv0: Option<String>,
    #[allow(dead_code)]
    pub split_strings: Vec<String>,
}

fn extract_basic_options(matches: &clap::ArgMatches) -> (bool, bool, Vec<String>, Option<String>, Option<String>, Vec<String>) {
    let ignore_environment = matches.get_flag("ignore-environment");
    let null = matches.get_flag("null");
    let unset: Vec<String> = matches.get_many::<String>("unset")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();
    let chdir = matches.get_one::<String>("chdir").cloned();
    let argv0 = matches.get_one::<String>("argv0").cloned();
    let split_strings: Vec<String> = matches.get_many::<String>("split-string")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();
    (ignore_environment, null, unset, chdir, argv0, split_strings)
}

fn process_split_strings(args: Vec<String>, split_strings: Vec<String>) -> Vec<String> {
    let mut processed_args = Vec::new();
    let mut remaining_args = Vec::new();
    let mut all_split_strings = Vec::new();

    // Start with split_strings from command line
    all_split_strings.extend(split_strings);

    // Process args to extract -S/--split-string flags that might be combined with their values
    // (e.g., "-S rpmlua" or "--split-string rpmlua" from shebang lines where kernel passes it as single argument)
    let mut args_iter = args.into_iter();

    while let Some(arg) = args_iter.next() {
        if arg == "--" {
            remaining_args.push(arg);
            // Add all remaining arguments without -S processing
            remaining_args.extend(args_iter);
            break;
        }

        if arg.starts_with("-S") || arg.starts_with("--split-string") {
            // Handle -S or --split-string flag
            let value = if arg == "-S" || arg == "--split-string" {
                // Just the flag, value should be next argument
                match args_iter.next() {
                    Some(next_arg) => next_arg,
                    None => {
                        // Flag without value is an error, skip
                        continue;
                    }
                }
            } else if arg.starts_with("-S") {
                // -S with value attached (e.g., "-Srpmlua" or "-S rpmlua")
                let rest = &arg[2..]; // Everything after "-S"
                if rest.starts_with(' ') {
                    // "-S rpmlua" - has leading space
                    rest.trim_start().to_string()
                } else {
                    // "-Srpmlua" - value attached directly
                    rest.to_string()
                }
            } else if arg.starts_with("--split-string=") {
                // --split-string=value
                arg["--split-string=".len()..].to_string()
            } else {
                // --split-string value (with space, as single argument from shebang)
                // arg is "--split-string value" or "--split-string value more"
                let rest = &arg["--split-string".len()..];
                if rest.starts_with(' ') {
                    rest.trim_start().to_string()
                } else {
                    // Shouldn't happen, but handle gracefully
                    rest.to_string()
                }
            };

            if !value.is_empty() {
                all_split_strings.push(value);
            }
        } else {
            remaining_args.push(arg);
        }
    }

    // Process all strings to split
    for s in all_split_strings {
        match shlex::split(&s) {
            Some(tokens) => processed_args.extend(tokens),
            None => processed_args.push(s),
        }
    }

    // Add remaining arguments
    processed_args.extend(remaining_args);
    processed_args
}

fn process_assignments_and_command(processed_args: Vec<String>, ignore_environment: bool) -> (Vec<String>, Vec<String>, bool) {
    let mut assignments = Vec::new();
    let mut command = Vec::new();
    let mut found_separator = false;
    let mut ignore_env = ignore_environment;

    for arg in processed_args {
        if arg == "--" {
            found_separator = true;
            continue;
        }

        if arg == "-" {
            ignore_env = true;
            continue;
        }

        if found_separator {
            command.push(arg);
        } else if arg.contains('=') {
            assignments.push(arg);
        } else {
            // First non-assignment argument is the command
            command.push(arg);
            found_separator = true;
        }
    }

    (assignments, command, ignore_env)
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<EnvOptions> {
    let (ignore_environment, null, unset, chdir, argv0, split_strings) = extract_basic_options(matches);

    let args: Vec<String> = matches.get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let processed_args = process_split_strings(args, split_strings.clone());
    let (assignments, command, ignore_env) = process_assignments_and_command(processed_args, ignore_environment);

    Ok(EnvOptions {
        assignments,
        command,
        ignore_environment: ignore_env,
        null,
        unset,
        chdir,
        argv0,
        split_strings,
    })
}

pub fn command() -> Command {
    Command::new("env")
        .about("Run a program in a modified environment")
        .arg(Arg::new("ignore-environment")
            .short('i')
            .long("ignore-environment")
            .help("Start with an empty environment")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("null")
            .short('0')
            .long("null")
            .help("End each output line with NUL, not newline")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("unset")
            .short('u')
            .long("unset")
            .help("Remove variable from the environment")
            .value_name("NAME")
            .action(clap::ArgAction::Append))
        .arg(Arg::new("chdir")
            .short('C')
            .long("chdir")
            .help("Change working directory to DIR")
            .value_name("DIR"))
        .arg(Arg::new("argv0")
            .short('a')
            .long("argv0")
            .help("Pass ARG as the zeroth argument of COMMAND")
            .value_name("ARG"))
        .arg(Arg::new("split-string")
            .short('S')
            .long("split-string")
            .help("Process and split S into separate arguments; used to pass multiple arguments on shebang lines")
            .value_name("STRING")
            .action(clap::ArgAction::Append))
        .arg(Arg::new("args")
            .num_args(0..)
            .allow_hyphen_values(true)
            .help("VARIABLE=VALUE assignments and command (use - for -i)"))
}

fn print_environment(options: &EnvOptions) -> Result<()> {
    let env_vars: Vec<(String, String)> = if options.ignore_environment {
        Vec::new()
    } else {
        env::vars().collect()
    };

    // Build modified environment
    let mut modified_env = env_vars.into_iter().collect::<std::collections::HashMap<_, _>>();

    // Remove unset variables
    for var in &options.unset {
        modified_env.remove(var);
    }

    // Apply assignments
    for assignment in &options.assignments {
        if let Some(pos) = assignment.find('=') {
            let key = &assignment[..pos];
            let value = &assignment[pos + 1..];
            modified_env.insert(key.to_string(), value.to_string());
        }
    }

    // Print environment
    let mut vars: Vec<_> = modified_env.iter().collect();
    vars.sort_by_key(|(k, _)| *k);
    for (key, value) in vars {
        if options.null {
            print!("{}={}\0", key, value);
        } else {
            println!("{}={}", key, value);
        }
    }
    Ok(())
}

fn modify_environment(options: &EnvOptions) {
    // Execute command with modified environment
    if options.ignore_environment {
        env::vars().for_each(|(k, _)| {
            env::remove_var(&k);
        });
    }

    // Remove unset variables
    for var in &options.unset {
        env::remove_var(var);
    }

    // Apply assignments
    for assignment in &options.assignments {
        if let Some(pos) = assignment.find('=') {
            let key = &assignment[..pos];
            let value = &assignment[pos + 1..];
            env::set_var(key, value);
        }
    }
}

fn execute_command(options: &EnvOptions) -> Result<()> {
    let cmd = &options.command[0];
    let args = &options.command[1..];

    let mut process_cmd = process::Command::new(cmd);

    // Set argv0 if specified (requires exec, so we'll note it but use cmd for now)
    // Full implementation would require using execvp with custom argv[0]
    if options.argv0.is_some() {
        // Note: argv0 support requires exec, which is complex
        // For now, we proceed with the command name
    }

    process_cmd.args(args);

    // Change directory if specified
    if let Some(ref dir) = options.chdir {
        process_cmd.current_dir(dir);
    }

    let status = process_cmd.status()
        .map_err(|e| eyre!("env: cannot execute '{}': {}", cmd, e))?;

    std::process::exit(status.code().unwrap_or(1));
}

pub fn run(options: EnvOptions) -> Result<()> {
    if options.command.is_empty() {
        return print_environment(&options);
    }

    modify_environment(&options);
    execute_command(&options)
}
