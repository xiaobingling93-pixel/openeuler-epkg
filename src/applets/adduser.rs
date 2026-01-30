use clap::{Arg, Command};
use color_eyre::Result;

use crate::userdb::{add_user_to_group, ensure_group};
use crate::userdb::{group_exists, user_exists};
use crate::userdb;
use crate::applets::useradd::UserAddOptions;

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
    let system = matches.get_flag("system") || matches.get_flag("system_short");
    let disabled_password = matches.get_flag("disabled_password") || matches.get_flag("disabled_password_short");
    let _no_create_home = matches.get_flag("no_create_home") || matches.get_flag("no_create_home_short");

    let home = matches
        .get_one::<String>("home")
        .or_else(|| matches.get_one::<String>("home_short"))
        .cloned();

    let shell = matches
        .get_one::<String>("shell")
        .or_else(|| matches.get_one::<String>("shell_short"))
        .cloned();

    let gecos = matches
        .get_one::<String>("gecos")
        .or_else(|| matches.get_one::<String>("gecos_short"))
        .cloned();

    // Primary group from --ingroup or -G (busybox semantics)
    let primary_group = matches
        .get_one::<String>("ingroup")
        .or_else(|| matches.get_one::<String>("ingroup_short"))
        .cloned();

    let group_flag = matches.get_flag("group");

    let mut positionals: Vec<String> = matches
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

    let username = positionals.remove(0);
    let target_group = if !positionals.is_empty() {
        Some(positionals.remove(0))
    } else {
        None
    };

    // Determine mode
    let mode = if target_group.is_some() && !system && !group_flag {
        AddUserMode::AddToGroup
    } else {
        AddUserMode::CreateUser
    };

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

pub fn command() -> Command {
    Command::new("adduser")
        .about("Debian compatible adduser (subset)")
        // Common long options
        .arg(
            Arg::new("system")
                .long("system")
                .help("Create a system account")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("disabled_password")
                .long("disabled-password")
                .help("Lock the account (no password)")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("quiet")
                .long("quiet")
                .help("Reduce output (ignored)")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("home")
                .long("home")
                .value_name("DIR")
                .help("Home directory"),
        )
        .arg(
            Arg::new("no_create_home")
                .long("no-create-home")
                .help("Do not create home directory")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("shell")
                .long("shell")
                .value_name("SHELL")
                .help("Login shell"),
        )
        .arg(
            Arg::new("gecos")
                .long("gecos")
                .value_name("GECOS")
                .help("GECOS/comment"),
        )
        .arg(
            Arg::new("group")
                .long("group")
                .help("Create group with same name as user")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("ingroup")
                .long("ingroup")
                .value_name("GROUP")
                .help("Primary group"),
        )
        .arg(
            Arg::new("allow_bad_names")
                .long("allow-bad-names")
                .long("force-badname")
                .help("Relax name checks (ignored, we are permissive)")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("stdout_msg_level")
                .long("stdoutmsglevel")
                .value_name("LEVEL")
                .help("Message level (ignored)")
                .num_args(0..=1),
        )
        // Short options used in Alpine scripts
        .arg(
            Arg::new("system_short")
                .short('S')
                .help("Create a system account")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("disabled_password_short")
                .short('D')
                .help("Disable password")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("no_create_home_short")
                .short('H')
                .help("Do not create home directory")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("home_short")
                .short('h')
                .value_name("DIR")
                .help("Home directory"),
        )
        .arg(
            Arg::new("shell_short")
                .short('s')
                .value_name("SHELL")
                .help("Login shell"),
        )
        .arg(
            Arg::new("gecos_short")
                .short('g')
                .value_name("GECOS")
                .help("GECOS/comment"),
        )
        .arg(
            Arg::new("ingroup_short")
                .short('G')
                .value_name("GROUP")
                .help("Primary group"),
        )
        // Positional arguments: user [group]
        .arg(
            Arg::new("args")
                .value_name("ARGS")
                .num_args(1..=2)
                .help("user [group]"),
        )
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
            crate::applets::useradd::run(opts)?;
        }
    }

    Ok(())
}

