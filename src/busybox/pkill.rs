use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use nix::sys::signal::Signal;
use regex::Regex;

pub struct PkillOptions {
    pub pattern: String,
    pub signal: Signal,
    pub full_command: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<PkillOptions> {
    let args = matches.get_many::<String>("args")
        .map(|vals| vals.map(|s| s.as_str()).collect::<Vec<_>>())
        .unwrap_or_default();

    let mut signal_str = "TERM";
    let mut pattern = None;

    // First check if -s signal was provided
    if let Some(s_signal) = matches.get_one::<String>("signal") {
        signal_str = s_signal;
    }

    // Parse positional arguments for signals and pattern
    for arg in args {
        if arg.starts_with('-') && arg.len() > 1 {
            // This looks like a signal argument (like -SIGUSR2, -9, etc.)
            let signal_part = &arg[1..]; // Remove the leading dash
            if crate::utils::is_signal_name(signal_part) || signal_part.chars().all(|c| c.is_ascii_digit()) {
                signal_str = signal_part;
            } else {
                return Err(eyre!("pkill: invalid signal: {}", arg));
            }
        } else {
            // This should be the pattern
            if pattern.is_some() {
                return Err(eyre!("pkill: multiple patterns specified"));
            }
            pattern = Some(arg.to_string());
        }
    }

    let pattern = pattern.ok_or_else(|| eyre!("pkill: missing pattern"))?;

    let signal = crate::utils::parse_signal(signal_str)?;

    let full_command = matches.get_flag("full");

    Ok(PkillOptions { pattern, signal, full_command })
}

pub fn command() -> Command {
    Command::new("pkill")
        .about("Look up or signal processes based on name")
        .arg(Arg::new("signal")
            .short('s')
            .help("Signal to send (name or number)")
            .value_name("SIGNAL"))
        .arg(Arg::new("full")
            .short('f')
            .help("Match against full command line instead of just process name")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("args")
            .help("Signal specifications and pattern (signals start with -)")
            .num_args(1..))
}

fn find_and_kill_processes(pattern: &str, signal: Signal, full_command: bool) -> Result<()> {
    let regex = Regex::new(pattern)
        .map_err(|e| eyre!("pkill: invalid pattern '{}': {}", pattern, e))?;

    let mut found_any = false;

    for pid_result in crate::utils::iterate_processes()? {
        let pid = pid_result?;
        let match_string = if full_command {
            crate::utils::get_process_cmdline(pid)
        } else {
            crate::utils::get_process_name(pid)
        };

        if let Some(match_string) = match_string {
            if regex.is_match(&match_string) {
                crate::utils::kill_process(pid as i32, signal, "pkill")?;
                found_any = true;
            }
        }
    }

    if !found_any {
        // pkill exits with non-zero status when no processes found
        std::process::exit(1);
    }

    Ok(())
}

pub fn run(options: PkillOptions) -> Result<()> {
    find_and_kill_processes(&options.pattern, options.signal, options.full_command)
}
