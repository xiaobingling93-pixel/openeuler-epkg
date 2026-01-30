use clap::{Arg, Command};
use color_eyre::Result;

use crate::applets::groupadd::{GroupAddOptions, run as run_groupadd};
use crate::userdb::add_user_to_group;
use crate::userdb::{group_exists, user_exists};
use crate::userdb;

#[derive(Debug, Clone, PartialEq, Eq)]
enum AddGroupMode {
    CreateGroup,
    AddUserToGroup,
}

#[derive(Debug, Clone)]
pub struct AddGroupCmd {
    pub options: GroupAddOptions,
    pub user: Option<String>,
    mode: AddGroupMode,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<AddGroupCmd> {
    let system = matches.get_flag("system") || matches.get_flag("system_short");
    let quiet = matches.get_flag("quiet");
    let gid = matches
        .get_one::<String>("gid")
        .or_else(|| matches.get_one::<String>("gid_short"))
        .cloned();

    let mut positionals: Vec<String> = matches
        .get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    if positionals.is_empty() {
        // Should not happen due to clap validation, but keep a safe default
        return Ok(AddGroupCmd {
            options: GroupAddOptions::default(),
            user: None,
            mode: AddGroupMode::CreateGroup,
        });
    }

    let name = positionals.remove(0);
    let user = if !positionals.is_empty() {
        Some(positionals.remove(0))
    } else {
        None
    };

    let mode = if user.is_some() {
        AddGroupMode::AddUserToGroup
    } else {
        AddGroupMode::CreateGroup
    };

    let options = GroupAddOptions {
        system,
        gid,
        name,
        ..Default::default()
    };

    let _ = quiet; // ignored, we are quiet by default

    Ok(AddGroupCmd { options, user, mode })
}

pub fn command() -> Command {
    Command::new("addgroup")
        .about("Debian compatible addgroup (subset)")
        .arg(
            Arg::new("system")
                .long("system")
                .help("Create a system group")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("quiet")
                .long("quiet")
                .help("Reduce output (ignored)")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("gid")
                .long("gid")
                .value_name("GID")
                .help("Numeric GID"),
        )
        .arg(
            Arg::new("force_badname")
                .long("force-badname")
                .help("Allow bad group names (ignored)")
                .action(clap::ArgAction::SetTrue),
        )
        // Short options used in Alpine scripts
        .arg(
            Arg::new("system_short")
                .short('S')
                .help("Create a system group")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("gid_short")
                .short('g')
                .value_name("GID")
                .help("Numeric GID"),
        )
        // Positional: group [user]
        .arg(
            Arg::new("args")
                .value_name("ARGS")
                .num_args(1..=2)
                .help("group [user]"),
        )
}

pub fn run(cmd: AddGroupCmd) -> Result<()> {
    match cmd.mode {
        AddGroupMode::CreateGroup => {
            run_groupadd(cmd.options)?;
        }
        AddGroupMode::AddUserToGroup => {
            let group = &cmd.options.name;
            let user = cmd.user.as_ref().expect("user must be present");

            // Both group and user must exist (Debian addgroup behavior)
            if !group_exists(group, None)? {
                return Err(color_eyre::eyre::eyre!("The group `{}' does not exist.", group));
            }
            if !user_exists(user, None)? {
                return Err(color_eyre::eyre::eyre!("The user `{}' does not exist.", user));
            }

            // Check if user is already a member (warn but succeed)
            let groups = userdb::read_group(None)?;
            if let Some(g) = groups.iter().find(|g| g.name == *group) {
                if g.members.iter().any(|m| m == user) {
                    // User already a member, exit successfully (Debian behavior)
                    return Ok(());
                }
            }

            add_user_to_group(user, group, None)?;
        }
    }

    Ok(())
}

