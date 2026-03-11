use clap::{Arg, Command};
use color_eyre::Result;
use std::path::Path;

use crate::userdb::user_exists;
use crate::userdb;

#[derive(Debug, Clone, Default)]
pub struct UserDelOptions {
    pub force: bool,
    pub remove_home: bool,
    pub root: Option<String>,
    pub prefix: Option<String>,
    pub selinux_user: bool,
    pub username: String,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<UserDelOptions> {
    let force = matches.get_flag("force");
    let remove_home = matches.get_flag("remove_home");
    let root = matches.get_one::<String>("root").cloned();
    let prefix = matches.get_one::<String>("prefix").cloned();
    let selinux_user = matches.get_flag("selinux_user");
    let username = matches
        .get_one::<String>("username")
        .expect("username is required")
        .clone();

    Ok(UserDelOptions {
        force,
        remove_home,
        root,
        prefix,
        selinux_user,
        username,
    })
}

pub fn command() -> Command {
    Command::new("userdel")
        .about("Delete a user account and related files")
        .arg(
            Arg::new("force")
                .short('f')
                .long("force")
                .help("force some actions that would fail otherwise\ne.g. removal of user still logged in\nor files, even if not owned by the user")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("remove_home")
                .short('r')
                .long("remove")
                .help("remove home directory and mail spool")
                .action(clap::ArgAction::SetTrue),
        )
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
            Arg::new("selinux_user")
                .short('Z')
                .long("selinux-user")
                .help("Remove any SELinux user mapping for the user's login")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("username")
                .required(true)
                .value_name("LOGIN")
                .help("User name"),
        )
}

pub fn run(options: UserDelOptions) -> Result<()> {
    // Determine root path: -R takes precedence over -P, then default to /
    let root_path = if let Some(ref root) = options.root {
        Some(Path::new(root))
    } else if let Some(ref prefix) = options.prefix {
        Some(Path::new(prefix))
    } else {
        Some(Path::new("/"))
    };

    // Check if user exists (exit code 6)
    if !user_exists(&options.username, root_path)? {
        eprintln!("userdel: user '{}' does not exist", options.username);
        std::process::exit(6);
    }

    // Check if user is currently logged in (exit code 8 unless -f is used)
    // Note: Full implementation would require checking active sessions/processes
    // For now, we respect the force flag but don't perform the actual check
    if !options.force {
        // TODO: Implement actual check for logged-in users
        // If user is logged in and -f is not set, exit with code 8
    }

    // TODO: Handle SELinux user mapping removal if -Z is specified
    if options.selinux_user {
        // SELinux user mapping removal not yet implemented
    }

    // Delete the user (exit code 1 on password file error, exit code 10 on group file error, exit code 12 on home directory removal error)
    userdb::delete_user(&options.username, options.remove_home, root_path)
        .map_err(|e| {
            eprintln!("userdel: {}", e);
            // Note: We can't distinguish between different error types from delete_user,
            // so we use exit code 1 as a general error
            std::process::exit(1);
        })
}

