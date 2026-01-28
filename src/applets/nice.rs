use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::process;

pub struct NiceOptions {
    pub adjustment: Option<i32>,
    pub command: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<NiceOptions> {
    let adjustment = matches.get_one::<String>("adjustment")
        .and_then(|s| s.parse::<i32>().ok());

    let command: Vec<String> = matches.get_many::<String>("command")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    if command.is_empty() {
        return Err(eyre!("nice: missing operand"));
    }

    Ok(NiceOptions { adjustment, command })
}

pub fn command() -> Command {
    Command::new("nice")
        .about("Run a program with modified scheduling priority")
        .arg(Arg::new("adjustment")
            .short('n')
            .long("adjustment")
            .help("Add ADJUSTMENT to the priority (default: 10)")
            .value_name("ADJUSTMENT"))
        .arg(Arg::new("command")
            .num_args(1..)
            .required(true)
            .help("Command to run"))
}

pub fn run(options: NiceOptions) -> Result<()> {
    let cmd = &options.command[0];
    let args = &options.command[1..];

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let adjustment = options.adjustment.unwrap_or(10);

        let mut process_cmd = process::Command::new(cmd);
        process_cmd.args(args);

        // Set nice value for the child process using pre_exec
        unsafe {
            process_cmd.pre_exec(move || {
                let result = libc::nice(adjustment);
                if result == -1 {
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() != Some(0) {
                        return Err(err);
                    }
                }
                Ok(())
            });
        }

        let status = process_cmd.status()
            .map_err(|e| eyre!("nice: cannot execute '{}': {}", cmd, e))?;

        std::process::exit(status.code().unwrap_or(1));
    }
    #[cfg(not(unix))]
    {
        // On non-Unix, just run the command without nice adjustment
        let mut process_cmd = process::Command::new(cmd);
        process_cmd.args(args);
        let status = process_cmd.status()
            .map_err(|e| eyre!("nice: cannot execute '{}': {}", cmd, e))?;

        std::process::exit(status.code().unwrap_or(1));
    }
}
