use clap::{Arg, Command};
use color_eyre::Result;

use crate::userdb::{add_user_to_group, ensure_group};
use crate::userdb::{group_exists, user_exists};
use crate::userdb;
use crate::busybox::useradd::UserAddOptions;

#[derive(Debug, Clone, PartialEq, Eq)]
enum AddUserMode {
    CreateUser,
    AddToGroup,
}

#[derive(Debug, Clone)]
pub struct AddUserCmd {
    pub options: UserAddOptions,
    pub target_group: Option<String>,
    mode: AddUserMode,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<AddUserCmd> {
    let system = matches.get_flag("system");
    let disabled_password = matches.get_flag("disabled_password");
    let _no_create_home = matches.get_flag("no_create_home");

    let home = matches.get_one::<String>("home").cloned();
    let shell = matches.get_one::<String>("shell").cloned();
    let gecos = matches.get_one::<String>("gecos").cloned();
    // Primary group from --ingroup or -G (busybox semantics)
    let primary_group = matches.get_one::<String>("ingroup").cloned();

    let group_flag = matches.get_flag("group");

    let positionals: Vec<String> = matches
        .get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    // Debian semantics:
    // - adduser [options] user
    // - adduser [options] user group   -> add existing user to group
    if positionals.is_empty() {
        // clap should guarantee at least 1, but be safe
        return Ok(AddUserCmd {
            options: UserAddOptions::default(),
            target_group: None,
            mode: AddUserMode::CreateUser,
        });
    }

    let (username, target_group, mode) = parse_positionals(positionals, system, group_flag);

    let options = UserAddOptions {
        system,
        home,
        shell,
        gecos,
        primary_group,
        lock_password: disabled_password,
        username,
        ..Default::default()
    };

    Ok(AddUserCmd {
        options,
        target_group,
        mode,
    })
}

// Helper functions for building CLI arguments
// Debian adduser --help:
// adduser [--uid id] [--firstuid id] [--lastuid id]
//         [--gid id] [--firstgid id] [--lastgid id] [--ingroup group]
//         [--add-extra-groups] [--shell shell]
//         [--comment comment] [--home dir] [--no-create-home]
//         [--allow-all-names] [--allow-bad-names]
//         [--disabled-password] [--disabled-login]
//         [--conf file] [--quiet] [--verbose] [--debug]
//         user
//     Add a regular user
//
// adduser --system
//         [--uid id] [--group] [--ingroup group] [--gid id]
//         [--shell shell] [--comment comment] [--home dir] [--no-create-home]
//         [--conf file] [--quiet] [--verbose] [--debug]
//         user
//    Add a system user
//
// adduser --group
//         [--gid ID] [--firstgid id] [--lastgid id]
//         [--conf file] [--quiet] [--verbose] [--debug]
//         group
//
// adduser USER GROUP
//    Add an existing user to an existing group

// Short options used in Alpine scripts, refer to busybox adduser:
//
// BusyBox v1.37.0 (2025-01-17 18:12:01 UTC) multi-call binary.
// Usage: adduser [OPTIONS] USER [GROUP]
// Create new user, or add USER to GROUP
//         -h DIR          Home directory
//         -g GECOS        GECOS field
//         -s SHELL        Login shell
//         -G GRP          Group
//         -S              Create a system user
//         -D              Don't assign a password
//         -H              Don't create home directory
//         -u UID          User id
//         -k SKEL         Skeleton directory (/etc/skel)
fn parse_positionals(
    mut positionals: Vec<String>,
    system: bool,
    group_flag: bool,
) -> (String, Option<String>, AddUserMode) {
    let username = positionals.remove(0);
    let target_group = if !positionals.is_empty() {
        Some(positionals.remove(0))
    } else {
        None
    };
    let mode = if target_group.is_some() && !system && !group_flag {
        AddUserMode::AddToGroup
    } else {
        AddUserMode::CreateUser
    };
    (username, target_group, mode)
}

pub fn command() -> Command {
    let mut cmd = Command::new("adduser")
        .about("Debian compatible adduser (subset)")
        .disable_help_flag(true);
    for arg in vec![
        Arg::new("system")
            .short('S')
            .long("system")
            .help("Create a system account")
            .action(clap::ArgAction::SetTrue),
        Arg::new("disabled_password")
            .short('D')
            .long("disabled-password")
            .help("Lock the account (no password)")
            .action(clap::ArgAction::SetTrue),
        Arg::new("quiet")
            .long("quiet")
            .help("Reduce output (ignored)")
            .action(clap::ArgAction::SetTrue),
        Arg::new("home")
            .short('h')
            .long("home")
            .value_name("DIR")
            .help("Home directory"),
        Arg::new("no_create_home")
            .short('H')
            .long("no-create-home")
            .help("Do not create home directory")
            .action(clap::ArgAction::SetTrue),
        Arg::new("shell")
            .short('s')
            .long("shell")
            .value_name("SHELL")
            .help("Login shell"),
        Arg::new("gecos")
            .short('g')
            .long("gecos")
            .visible_alias("comment")
            .value_name("GECOS")
            .help("GECOS/comment"),
        Arg::new("group")
            .long("group")
            .help("Create group with same name as user")
            .action(clap::ArgAction::SetTrue),
        Arg::new("ingroup")
            .short('G')
            .long("ingroup")
            .value_name("GROUP")
            .help("Primary group"),
        Arg::new("allow_bad_names")
            .long("allow-bad-names")
            .visible_alias("force-badname")
            .visible_alias("allow-badname")
            .help("Relax name checks (ignored, we are permissive)")
            .action(clap::ArgAction::SetTrue),
        Arg::new("stdout_msg_level")
            .long("stdoutmsglevel")
            .value_name("LEVEL")
            .help("Message level (ignored)")
            .num_args(0..=1),
    ] {
        cmd = cmd.arg(arg);
    }
    cmd.arg(Arg::new("args").value_name("ARGS").num_args(1..=2).help("user [group]"))
        .arg(Arg::new("help").long("help").action(clap::ArgAction::Help))
}

pub fn run(cmd: AddUserCmd) -> Result<()> {
    match cmd.mode {
        AddUserMode::AddToGroup => {
            let user = &cmd.options.username;
            let group = cmd
                .target_group
                .as_ref()
                .expect("group must be present in AddToGroup mode");

            // Both user and group must exist (Debian adduser behavior)
            if !user_exists(user, None)? {
                return Err(color_eyre::eyre::eyre!("The user `{}' does not exist.", user));
            }
            if !group_exists(group, None)? {
                return Err(color_eyre::eyre::eyre!("The group `{}' does not exist.", group));
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
        AddUserMode::CreateUser => {
            let mut opts = cmd.options;

            // If requested, ensure primary group exists (system group)
            if let Some(ref g) = opts.primary_group {
                if !group_exists(g, None)? {
                    ensure_group(g, None, opts.system, None)?;
                }
            }

            // Call through to useradd-like implementation
            // Map disabled_password -> lock_password
            // For system daemon users, default home if none given
            if opts.home.is_none() && opts.system {
                opts.home = Some("/nonexistent".to_string());
            }
            // Use useradd helper
            crate::busybox::useradd::run(opts)?;
        }
    }

    Ok(())
}

