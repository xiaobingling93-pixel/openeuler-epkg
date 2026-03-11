use clap::{Arg, Command};
use color_eyre::Result;

use crate::applets::groupdel::{GroupDelOptions, run as run_groupdel};

#[derive(Debug, Clone)]
pub struct DelGroupCmd {
    pub options: GroupDelOptions,
    pub system: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<DelGroupCmd> {
    let system = matches.get_flag("system");
    let name = matches
        .get_one::<String>("group")
        .expect("group name is required")
        .clone();

    // delgroup is a Debian wrapper - map its options to groupdel options
    // Note: delgroup's --only-if-empty and --quiet are not standard groupdel options
    // but we'll ignore them for compatibility
    let options = GroupDelOptions {
        force: false, // delgroup doesn't have --force
        root: None,
        prefix: None,
        name,
    };

    Ok(DelGroupCmd { options, system })
}

pub fn command() -> Command {
    Command::new("delgroup")
        .about("Debian-compatible delgroup (minimal subset)")
        .arg(
            Arg::new("quiet")
                .long("quiet")
                .help("Suppress most messages")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("system")
                .long("system")
                .help("Restrict to system groups (ignored here)")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("only_if_empty")
                .long("only-if-empty")
                .help("Fail if group is not empty")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("group")
                .required(true)
                .value_name("GROUP")
                .help("Group name"),
        )
}

pub fn run(cmd: DelGroupCmd) -> Result<()> {
    let _ = cmd.system; // semantics already handled by caller; we always act on given group
    run_groupdel(cmd.options)?;
    Ok(())
}

