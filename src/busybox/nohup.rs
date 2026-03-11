use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::env;
use std::fs::OpenOptions;
use std::process;

pub struct NohupOptions {
    pub command: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<NohupOptions> {
    let command: Vec<String> = matches.get_many::<String>("command")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    if command.is_empty() {
        return Err(eyre!("nohup: missing operand"));
    }

    Ok(NohupOptions { command })
}

pub fn command() -> Command {
    Command::new("nohup")
        .about("Run a command immune to hangups")
        .arg(Arg::new("command")
            .num_args(1..)
            .required(true)
            .help("Command to run"))
}

pub fn run(options: NohupOptions) -> Result<()> {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, SigHandler};

        // Ignore SIGHUP
        unsafe {
            nix::sys::signal::signal(Signal::SIGHUP, SigHandler::SigIgn)
                .map_err(|e| eyre!("nohup: cannot ignore SIGHUP: {}", e))?;
        }
    }

    // Determine nohup.out location (try current dir, then $HOME)
    let nohup_file = if std::path::Path::new("nohup.out").parent().is_some() {
        "nohup.out".to_string()
    } else {
        env::var("HOME")
            .map(|home| format!("{}/nohup.out", home))
            .unwrap_or_else(|_| "nohup.out".to_string())
    };

    // Check if stdout/stderr are terminals
    let stdout_is_tty = unsafe { libc::isatty(libc::STDOUT_FILENO) != 0 };
    let stderr_is_tty = unsafe { libc::isatty(libc::STDERR_FILENO) != 0 };

    let cmd = &options.command[0];
    let args = &options.command[1..];

    let mut process_cmd = process::Command::new(cmd);
    process_cmd.args(args);

    if stdout_is_tty || stderr_is_tty {
        // Redirect stdout and stderr to nohup.out
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(true)
            .open(&nohup_file)
            .map_err(|e| eyre!("nohup: cannot open '{}': {}", nohup_file, e))?;

        if stdout_is_tty {
            process_cmd.stdout(file.try_clone().unwrap());
        }
        if stderr_is_tty {
            process_cmd.stderr(file);
        }

        // Print message to original stderr if it's a terminal
        if stderr_is_tty {
            eprintln!("nohup: appending output to '{}'", nohup_file);
        }
    }

    let status = process_cmd.status()
        .map_err(|e| eyre!("nohup: cannot execute '{}': {}", cmd, e))?;

    std::process::exit(status.code().unwrap_or(1));
}
