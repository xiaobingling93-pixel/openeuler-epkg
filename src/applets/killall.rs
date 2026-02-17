use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use nix::sys::signal::Signal;

pub struct KillallOptions {
    pub program: String,
    pub signal: Signal,
    pub quiet: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<KillallOptions> {
    let program = matches.get_one::<String>("program")
        .ok_or_else(|| eyre!("killall: missing program name"))?
        .clone();

    let signal_str = matches.get_one::<String>("signal")
        .map(|s| s.as_str())
        .unwrap_or("TERM");

    let signal = crate::utils::parse_signal(signal_str)?;

    let quiet = matches.get_flag("quiet");

    Ok(KillallOptions { program, signal, quiet })
}

pub fn command() -> Command {
    Command::new("killall")
        .about("Kill processes by name")
        .arg(Arg::new("signal")
            .short('s')
            .help("Signal to send (name or number)")
            .value_name("SIGNAL"))
        .arg(Arg::new("quiet")
            .short('q')
            .help("Don't complain if no processes were killed")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("program")
            .help("Program name to kill")
            .required(true))
}

fn find_and_kill_processes(program: &str, signal: Signal, quiet: bool) -> Result<()> {
    let mut found_any = false;

    for pid_result in crate::utils::iterate_processes()? {
        let pid = pid_result?;
        if let Some(proc_name) = crate::utils::get_process_name(pid) {
            if proc_name == program {
                crate::utils::kill_process(pid as i32, signal, "killall")?;
                found_any = true;
            }
        }
    }

    if !found_any && !quiet {
        // killall exits with non-zero status when no processes found (unless quiet)
        std::process::exit(1);
    }

    Ok(())
}

pub fn run(options: KillallOptions) -> Result<()> {
    find_and_kill_processes(&options.program, options.signal, options.quiet)
}
