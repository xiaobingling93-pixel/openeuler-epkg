use clap::{Arg, Command};
use color_eyre::Result;
use std::path::Path;

use crate::applets::systemd_sysusers::{add_user_to_group, group_exists, user_exists};
use crate::userdb;

#[derive(Debug, Clone, Default)]
pub struct UserAddOptions {
    pub system: bool,
    pub home: Option<String>,
    pub shell: Option<String>,
    pub gecos: Option<String>,
    pub primary_group: Option<String>,
    pub lock_password: bool,
    #[allow(dead_code)]
    pub no_create_home: bool,
    #[allow(dead_code)]
    pub create_home: bool,
    pub uid: Option<String>,
    pub supplementary_groups: Option<String>,
    pub no_user_group: bool,
    pub user_group: bool,
    pub non_unique: bool,
    pub username: String,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<UserAddOptions> {
    // useradd-compatible interface
    let system = matches.get_flag("system");
    let home = matches.get_one::<String>("home").cloned();
    let shell = matches.get_one::<String>("shell").cloned();
    let gecos = matches.get_one::<String>("gecos").cloned();
    let primary_group = matches.get_one::<String>("gid").cloned();
    // By default, new accounts are locked (no password) per useradd man page
    let lock_password = true;
    let no_create_home = matches.get_flag("no_create_home");
    let create_home = matches.get_flag("create_home");
    let uid = matches.get_one::<String>("uid").cloned();
    let supplementary_groups = matches.get_one::<String>("groups").cloned();
    let no_user_group = matches.get_flag("no_user_group");
    let user_group = matches.get_flag("user_group");
    let non_unique = matches.get_flag("non_unique");

    let username = matches
        .get_one::<String>("username")
        .expect("username is required")
        .clone();

    Ok(UserAddOptions {
        system,
        home,
        shell,
        gecos,
        primary_group,
        lock_password,
        no_create_home,
        create_home,
        uid,
        supplementary_groups,
        no_user_group,
        user_group,
        non_unique,
        username,
    })
}

pub fn command() -> Command {
    Command::new("useradd")
        .about("Create a new user (minimal subset)")
        .arg(
            Arg::new("system")
                .short('r')
                .long("system")
                .help("create a system account")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("home")
                .short('d')
                .long("home-dir")
                .value_name("HOME_DIR")
                .help("home directory of the new account"),
        )
        .arg(
            Arg::new("shell")
                .short('s')
                .long("shell")
                .value_name("SHELL")
                .help("login shell of the new account"),
        )
        .arg(
            Arg::new("gecos")
                .short('c')
                .long("comment")
                .value_name("COMMENT")
                .help("GECOS field of the new account"),
        )
        .arg(
            Arg::new("gid")
                .short('g')
                .long("gid")
                .value_name("GROUP")
                .help("name or ID of the primary group of the new account"),
        )
        .arg(
            Arg::new("create_home")
                .short('m')
                .long("create-home")
                .help("create the user's home directory")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("no_create_home")
                .short('M')
                .long("no-create-home")
                .help("do not create the user's home directory")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("uid")
                .short('u')
                .long("uid")
                .value_name("UID")
                .help("user ID of the new account"),
        )
        .arg(
            Arg::new("groups")
                .short('G')
                .long("groups")
                .value_name("GROUPS")
                .help("list of supplementary groups of the new account"),
        )
        .arg(
            Arg::new("no_user_group")
                .short('N')
                .long("no-user-group")
                .help("do not create a group with the same name as the user")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("user_group")
                .short('U')
                .long("user-group")
                .help("create a group with the same name as the user")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("non_unique")
                .short('o')
                .long("non-unique")
                .help("allow to create users with duplicate (non-unique) UID")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("username")
                .required(true)
                .value_name("LOGIN")
                .help("login name of the new account"),
        )
}

fn validate_options(options: &UserAddOptions) -> Result<()> {
    // Validate flag conflicts (matching C implementation)
    if options.non_unique && options.uid.is_none() {
        return Err(color_eyre::eyre::eyre!(
            "-o flag is only allowed with the -u flag"
        ));
    }
    if options.user_group && options.primary_group.is_some() {
        return Err(color_eyre::eyre::eyre!(
            "options -U and -g conflict"
        ));
    }
    if options.user_group && options.no_user_group {
        return Err(color_eyre::eyre::eyre!(
            "options -U and -N conflict"
        ));
    }
    if options.create_home && options.no_create_home {
        return Err(color_eyre::eyre::eyre!(
            "options -m and -M conflict"
        ));
    }
    Ok(())
}

fn check_user_exists(username: &str) -> Result<()> {
    // Check if user already exists (C behavior: exit with error)
    if user_exists(username, None)? {
        return Err(color_eyre::eyre::eyre!(
            "user '{}' already exists",
            username
        ));
    }
    Ok(())
}

fn check_group_conflict(username: &str, user_group: bool) -> Result<()> {
    // Check if group with same name exists when -U is used
    if user_group {
        if group_exists(username, None)? {
            return Err(color_eyre::eyre::eyre!(
                "group {} exists - if you want to add this user to that group, use -g.",
                username
            ));
        }
    }
    Ok(())
}

fn compute_defaults(options: &UserAddOptions) -> (String, String, String) {
    let home = options
        .home
        .clone()
        .unwrap_or_else(|| {
            if options.system {
                "/nonexistent".to_string()
            } else {
                format!("/home/{}", options.username)
            }
        });
    let shell = options
        .shell
        .clone()
        .unwrap_or_else(|| "/bin/false".to_string());
    let gecos = options.gecos.clone().unwrap_or_default();

    (home, shell, gecos)
}

fn determine_primary_group(options: &UserAddOptions) -> Option<&str> {
    // Determine primary group handling (matching C implementation logic)
    // - If -N (no_user_group): use default GID
    // - If -g (primary_group specified): use that group
    // - If -U (user_group) or no flag: create user group (default behavior)
    if options.no_user_group {
        // Use default GID (100 for system, 1000 for regular)
        Some(if options.system { "100" } else { "1000" })
    } else if options.primary_group.is_some() {
        // Use specified primary group
        options.primary_group.as_deref()
    } else {
        // Create user group (default behavior when no -g or -N)
        None
    }
}

fn validate_uid(uid: Option<&String>, non_unique: bool) -> Result<()> {
    // Check for duplicate UID if -u is specified and -o is not
    if let Some(uid_str) = uid {
        if !non_unique {
            let users = userdb::read_passwd(Some(Path::new("/")))?;
            let uid_val: u32 = uid_str.parse().map_err(|e| {
                color_eyre::eyre::eyre!("Invalid UID {}: {}", uid_str, e)
            })?;
            if users.iter().any(|u| u.uid == uid_val) {
                return Err(color_eyre::eyre::eyre!(
                    "UID {} already exists (use -o to allow duplicate UID)",
                    uid_str
                ));
            }
        }
    }
    Ok(())
}

fn add_supplementary_groups(username: &str, groups_str: Option<&String>) -> Result<()> {
    // Add user to supplementary groups if specified
    if let Some(groups_str) = groups_str {
        for group in groups_str.split(',') {
            let group = group.trim();
            if !group.is_empty() {
                add_user_to_group(username, group, None)?;
            }
        }
    }
    Ok(())
}

pub fn run(options: UserAddOptions) -> Result<()> {
    validate_options(&options)?;
    check_user_exists(&options.username)?;
    check_group_conflict(&options.username, options.user_group)?;

    let (home, shell, gecos) = compute_defaults(&options);
    let gid_str = determine_primary_group(&options);

    validate_uid(options.uid.as_ref(), options.non_unique)?;

    userdb::create_user(
        &options.username,
        options.uid.as_deref(),
        gid_str,
        &gecos,
        &home,
        &shell,
        options.system,
        options.lock_password,
        Some(Path::new("/")),
    )?;

    add_supplementary_groups(&options.username, options.supplementary_groups.as_ref())?;

    Ok(())
}

