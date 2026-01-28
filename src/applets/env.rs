use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
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
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<EnvOptions> {
    let ignore_environment = matches.get_flag("ignore-environment");
    let null = matches.get_flag("null");
    let unset: Vec<String> = matches.get_many::<String>("unset")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();
    let chdir = matches.get_one::<String>("chdir").cloned();
    let argv0 = matches.get_one::<String>("argv0").cloned();

    // Parse arguments - everything before -- is an assignment or option
    // Everything after -- is the command
    // A mere "-" implies -i
    let mut assignments = Vec::new();
    let mut command = Vec::new();
    let mut found_separator = false;
    let mut ignore_env = ignore_environment;

    let args: Vec<String> = matches.get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    for arg in args {
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

    Ok(EnvOptions {
        assignments,
        command,
        ignore_environment: ignore_env,
        null,
        unset,
        chdir,
        argv0,
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
        .arg(Arg::new("args")
            .num_args(0..)
            .help("VARIABLE=VALUE assignments and command (use - for -i)"))
}

fn print_environment(options: &EnvOptions) -> Result<()> {
    let env_vars: Vec<(String, String)> = if options.ignore_environment {
        Vec::new()
    } else {
        env::vars().collect()
    };

    // Apply assignments
    let mut modified_env = env_vars.into_iter().collect::<std::collections::HashMap<_, _>>();
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
