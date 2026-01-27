use clap::{Arg, Command};
use color_eyre::Result;
use std::path::Path;

use crate::applets::systemd_sysusers::{group_exists, user_exists, validate_user_group_name};
use crate::userdb;

#[derive(Debug, Clone, Default)]
pub struct GroupAddOptions {
    pub system: bool,
    pub gid: Option<String>,
    pub name: String,
    pub force: bool,
    pub non_unique: bool,
    pub root: Option<String>,
    pub prefix: Option<String>,
    pub users: Option<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<GroupAddOptions> {
    let system = matches.get_flag("system");
    let gid = matches.get_one::<String>("gid").cloned();
    let name = matches
        .get_one::<String>("group")
        .expect("group name is required")
        .clone();
    let force = matches.get_flag("force");
    let non_unique = matches.get_flag("non_unique");
    let root = matches.get_one::<String>("root").cloned();
    let prefix = matches.get_one::<String>("prefix").cloned();
    let users = matches.get_one::<String>("users").cloned();

    Ok(GroupAddOptions {
        system,
        gid,
        name,
        force,
        non_unique,
        root,
        prefix,
        users,
    })
}

pub fn command() -> Command {
    Command::new("groupadd")
        .about("Create a new group (minimal subset)")
        .arg(
            Arg::new("system")
                .short('r')
                .long("system")
                .help("Create a system group")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("gid")
                .short('g')
                .long("gid")
                .value_name("GID")
                .help("The numerical value of the group's ID"),
        )
        .arg(
            Arg::new("force")
                .short('f')
                .long("force")
                .help("Exit successfully if the group already exists. When used with -g, and the specified GID already exists, another (unique) GID is chosen")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("non_unique")
                .short('o')
                .long("non-unique")
                .help("Permit the creation of a group with an already used numerical ID")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("root")
                .short('R')
                .long("root")
                .value_name("CHROOT_DIR")
                .help("Apply changes in the CHROOT_DIR directory and use the configuration files from the CHROOT_DIR directory"),
        )
        .arg(
            Arg::new("prefix")
                .short('P')
                .long("prefix")
                .value_name("PREFIX_DIR")
                .help("Apply changes to configuration files under the root filesystem found under the directory PREFIX_DIR"),
        )
        .arg(
            Arg::new("users")
                .short('U')
                .long("users")
                .value_name("USERS")
                .help("A comma-separated list of usernames to add as members of the group"),
        )
        .arg(
            Arg::new("group")
                .required(true)
                .value_name("GROUP")
                .help("Group name"),
        )
}

pub fn run(options: GroupAddOptions) -> Result<()> {
    // Check that -o (non_unique) is not used without -g (gid)
    if options.non_unique && options.gid.is_none() {
        return Err(color_eyre::eyre::eyre!(
            "option --non-unique (-o) requires --gid (-g)"
        ));
    }

    // Validate group name format
    validate_user_group_name(&options.name)?;

    // Determine root path: -R takes precedence over -P, then default to /
    let root_path = if let Some(ref root) = options.root {
        Some(Path::new(root))
    } else if let Some(ref prefix) = options.prefix {
        Some(Path::new(prefix))
    } else {
        Some(Path::new("/"))
    };

    // Check if group already exists
    let group_exists_flag = group_exists(&options.name, root_path)?;
    if group_exists_flag {
        if options.force {
            // With -f, exit successfully if group already exists (like the C code)
            return Ok(());
        } else {
            return Err(color_eyre::eyre::eyre!("group '{}' already exists", options.name));
        }
    }

    // Read existing groups to check for GID conflicts
    let groups = userdb::read_group(root_path)?;
    let used_gids: std::collections::HashSet<u32> = groups.iter().map(|g| g.gid).collect();

    // Handle GID specification
    let gid_str = if let Some(ref gid) = options.gid {
        let requested_gid: u32 = gid.parse()
            .map_err(|e| color_eyre::eyre::eyre!("Invalid gid '{}': {}", gid, e))?;

        // Check if GID is already used
        if used_gids.contains(&requested_gid) {
            if options.force {
                // With -f, cancel -g and use auto-assigned GID
                None
            } else if !options.non_unique {
                // Without -o, GID conflict is an error
                return Err(color_eyre::eyre::eyre!("GID '{}' already exists", requested_gid));
            } else {
                // With -o, allow duplicate GID
                Some(gid.as_str())
            }
        } else {
            Some(gid.as_str())
        }
    } else {
        None
    };

    // Create the group
    let _ = userdb::ensure_group(&options.name, gid_str, options.system, root_path)?;

    // Add users to the group if specified
    if let Some(ref users_str) = options.users {
        for user in users_str.split(',') {
            let user = user.trim();
            if !user.is_empty() {
                // Validate that the user exists (like the C code does)
                if !user_exists(user, root_path)? {
                    return Err(color_eyre::eyre::eyre!(
                        "Invalid member username {}",
                        user
                    ));
                }
                userdb::add_user_to_group(user, &options.name, root_path)?;
            }
        }
    }

    Ok(())
}

