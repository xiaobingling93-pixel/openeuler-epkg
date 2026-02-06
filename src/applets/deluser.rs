use clap::{Arg, Command};
use color_eyre::Result;
use std::path::Path;

use crate::userdb::{group_exists, user_exists};
use crate::applets::groupdel::{GroupDelOptions, run as run_groupdel};
use crate::userdb;
use crate::applets::userdel::UserDelOptions;

#[derive(Debug, Clone, PartialEq, Eq)]
enum DelUserMode {
    DeleteAccount,
    RemoveFromGroup,
    DeleteGroup,
}

#[derive(Debug, Clone)]
pub struct DelUserCmd {
    pub options: UserDelOptions,
    pub group: Option<String>,
    mode: DelUserMode,
    pub group_mode: bool,
    pub system: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<DelUserCmd> {
    let _quiet = matches.get_flag("quiet"); // deluser-specific, not passed to userdel
    let system = matches.get_flag("system");
    let remove_home = matches.get_flag("remove_home");
    let group_mode = matches.get_flag("group");

    let positionals: Vec<String> = matches
        .get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let username = positionals
        .get(0)
        .cloned()
        .unwrap_or_else(|| "".to_string());

    let group = if positionals.len() > 1 {
        Some(positionals[1].clone())
    } else {
        None
    };

    // Determine mode: --group means delete group, not remove from group
    let mode = if group_mode {
        // --group flag: delete the group (group name is in username position)
        DelUserMode::DeleteGroup
    } else if group.is_some() {
        // Two arguments: remove user from group
        DelUserMode::RemoveFromGroup
    } else {
        // One argument: delete user account
        DelUserMode::DeleteAccount
    };

    let options = UserDelOptions {
        force: false,
        remove_home,
        root: None,
        prefix: None,
        selinux_user: false,
        username,
    };

    Ok(DelUserCmd {
        options,
        group,
        mode,
        group_mode,
        system,
    })
}

pub fn command() -> Command {
    Command::new("deluser")
        .about("Debian-compatible deluser (minimal subset)")
        .arg(
            Arg::new("quiet")
                .short('q')
                .long("quiet")
                .help("Suppress most messages")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("system")
                .long("system")
                .help("Remove system user (no special handling here)")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("remove_home")
                .long("remove-home")
                .help("Remove home directory")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("group")
                .long("group")
                .help("Remove user from group instead of deleting user")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("args")
                .value_name("ARGS")
                .num_args(1..=2)
                .help("user [group]"),
        )
}

fn delete_account(cmd: DelUserCmd) -> Result<()> {
    // Delegate to userdel core
    crate::applets::userdel::run(cmd.options)
}

fn delete_group(cmd: DelUserCmd) -> Result<()> {
    // --group flag: delete the group (group name is in username position)
    let group_name = &cmd.options.username;
    if !group_exists(group_name, None)? {
        // Group doesn't exist - exit successfully (Debian behavior when --system is set)
        if cmd.system {
            return Ok(());
        } else {
            return Err(color_eyre::eyre::eyre!("The group `{}' does not exist.", group_name));
        }
    }
    // Use groupdel to delete the group
    let groupdel_opts = GroupDelOptions {
        force: false,
        root: None,
        prefix: None,
        name: group_name.clone(),
    };
    run_groupdel(groupdel_opts)
}

fn remove_from_group(cmd: DelUserCmd) -> Result<()> {
    let user = &cmd.options.username;
    let group = cmd
        .group
        .as_ref()
        .expect("group must be provided for group removal");

    // Both user and group must exist (Debian deluser behavior)
    if !user_exists(user, None)? {
        return Err(color_eyre::eyre::eyre!("The user `{}' does not exist.", user));
    }
    if !group_exists(group, None)? {
        return Err(color_eyre::eyre::eyre!("The group `{}' does not exist.", group));
    }

    // Check if trying to remove from primary group (not allowed)
    let users = userdb::read_passwd(Some(Path::new("/")))?;
    let groups = userdb::read_group(Some(Path::new("/")))?;
    if let Some(user_entry) = users.iter().find(|u| u.name == *user) {
        if let Some(group_entry) = groups.iter().find(|g| g.name == *group) {
            if user_entry.gid == group_entry.gid {
                return Err(color_eyre::eyre::eyre!("You may not remove the user from their primary group."));
            }
        }
    }

    // Check if user is a member of the group
    let groups = userdb::read_group(Some(Path::new("/")))?;
    if let Some(g) = groups.iter().find(|g| g.name == *group) {
        if !g.members.iter().any(|m| m == user) {
            return Err(color_eyre::eyre::eyre!("The user `{}' is not a member of group `{}'.", user, group));
        }
    }

    // Remove user from group
    // If --group was specified (group_mode), also remove empty groups
    // This matches Debian deluser behavior for system groups
    let remove_empty = cmd.group_mode || cmd.system;
    userdb::remove_user_from_group(user, group, remove_empty, Some(Path::new("/")))
}

pub fn run(cmd: DelUserCmd) -> Result<()> {
    match cmd.mode {
        DelUserMode::DeleteAccount => delete_account(cmd),
        DelUserMode::DeleteGroup => delete_group(cmd),
        DelUserMode::RemoveFromGroup => remove_from_group(cmd),
    }
}

