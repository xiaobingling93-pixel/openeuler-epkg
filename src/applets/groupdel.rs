use clap::{Arg, Command};
use color_eyre::Result;
use std::path::Path;

use crate::userdb::group_exists;
use crate::userdb;

#[derive(Debug, Clone, Default)]
pub struct GroupDelOptions {
    pub force: bool,
    pub root: Option<String>,
    pub prefix: Option<String>,
    pub name: String,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<GroupDelOptions> {
    let force = matches.get_flag("force");
    let root = matches.get_one::<String>("root").cloned();
    let prefix = matches.get_one::<String>("prefix").cloned();
    let name = matches
        .get_one::<String>("group")
        .expect("group name is required")
        .clone();

    Ok(GroupDelOptions {
        force,
        root,
        prefix,
        name,
    })
}

pub fn command() -> Command {
    Command::new("groupdel")
        .about("Delete a group")
        .arg(
            Arg::new("root")
                .short('R')
                .long("root")
                .value_name("CHROOT_DIR")
                .help("directory to chroot into"),
        )
        .arg(
            Arg::new("prefix")
                .short('P')
                .long("prefix")
                .value_name("PREFIX_DIR")
                .help("prefix directory where are located the /etc/* files"),
        )
        .arg(
            Arg::new("force")
                .short('f')
                .long("force")
                .help("delete group even if it is the primary group of a user")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("group")
                .required(true)
                .value_name("GROUP")
                .help("Group name"),
        )
}

fn find_primary_group_user(group_name: &str, root: Option<&Path>) -> Result<Option<String>> {
    let groups = userdb::read_group(root)?;
    let target_gid = groups
        .iter()
        .find(|g| g.name == group_name)
        .map(|g| g.gid);

    if let Some(gid) = target_gid {
        let users = userdb::read_passwd(root)?;
        if let Some(user) = users.iter().find(|u| u.gid == gid) {
            return Ok(Some(user.name.clone()));
        }
    }
    Ok(None)
}

pub fn run(options: GroupDelOptions) -> Result<()> {
    // Determine root path: -R takes precedence over -P, then default to /
    let root_path = if let Some(ref root) = options.root {
        Some(Path::new(root))
    } else if let Some(ref prefix) = options.prefix {
        Some(Path::new(prefix))
    } else {
        Some(Path::new("/"))
    };

    // Check if group exists (exit code 6)
    if !group_exists(&options.name, root_path)? {
        eprintln!("groupdel: group '{}' does not exist", options.name);
        std::process::exit(6);
    }

    // Check if group is a primary group (exit code 8 unless -f is used)
    if !options.force {
        if let Some(username) = find_primary_group_user(&options.name, root_path)? {
            eprintln!("groupdel: cannot remove the primary group of user '{}'", username);
            std::process::exit(8);
        }
    }

    // Delete the group (exit code 10 on file update error)
    userdb::delete_group(&options.name, false, root_path)
        .map_err(|e| {
            eprintln!("groupdel: {}", e);
            std::process::exit(10);
        })
}

