use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use nix::sys::signal::Signal;

pub struct KillOptions {
    pub signal: Signal,
    pub pids: Vec<i32>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<KillOptions> {
    let signal_str = matches.get_one::<String>("signal")
        .map(|s| s.as_str())
        .unwrap_or("TERM");

    let signal = crate::utils::parse_signal(signal_str)?;

    let pids: Vec<i32> = matches.get_many::<String>("pids")
        .map(|vals| vals.map(|s| s.parse().map_err(|_| eyre!("kill: invalid process id: {}", s))).collect())
        .unwrap_or_else(|| Ok(Vec::new()))
        .map_err(|_| eyre!("kill: invalid process id"))?;

    Ok(KillOptions { signal, pids })
}

pub fn command() -> Command {
    Command::new("kill")
        .about("Send signals to processes")
        .arg(Arg::new("signal")
            .short('s')
            .help("Signal to send (name or number)")
            .value_name("SIGNAL"))
        .arg(Arg::new("pids")
            .help("Process IDs to signal")
            .required(true)
            .num_args(1..))
}

pub fn run(options: KillOptions) -> Result<()> {
    for &pid in &options.pids {
        crate::utils::kill_process(pid, options.signal, "kill")?;
    }

    Ok(())
}
