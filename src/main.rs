mod models;
mod io;
mod download;
mod depends;
mod install;
mod upgrade;
mod remove;
mod list;
mod hash;
mod ipc;
mod store;
mod paths;
mod utils;
mod history;
use std::env;
use crate::models::*;
use crate::ipc::*;
use anyhow::Result;

use clap::{Arg, ArgAction, Command};

fn main() -> Result<()> {
    // Create the CLI app
    let matches = Command::new("epkg")
        .version(env!("CARGO_PKG_VERSION"))
        .author("Wu Fengguang <wfg@mail.ustc.edu.cn>")
        .author("Duan Pengjie <pengjieduan@gmail.com>")
        .author("Yingjiahui <ying_register@163.com>")
        .about("The EPKG package manager")
        .arg(
            Arg::new("env")
                .long("env")
                .value_name("ENV")
                .help("Select the environment")
                .num_args(1)
                .value_parser(clap::value_parser!(String))
        )
        .arg(
            Arg::new("arch")
                .long("arch")
                .value_name("ARCH")
                .help("Select the CPU architecture")
                .num_args(1)
                .value_parser(clap::value_parser!(String))
        )
        .arg(
            Arg::new("simulate")
                .short('s')
                .long("simulate")
                .aliases(&["dry-run"])
                .help("Simulated run without changing the system")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("download_only")
                .long("download-only")
                .help("Download packages without installing")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("quiet")
                .short('q')
                .long("quiet")
                .help("Suppress output")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("verbose")
                .short('v')
                .long("verbose")
                .help("Verbose operation, show debug messages")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("assume_yes")
                .short('y')
                .long("assume-yes")
                .help("Automatically answer yes to all prompts")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("ignore_missing")
                .short('m')
                .long("ignore-missing")
                .help("Ignore missing packages")
                .action(ArgAction::SetTrue)
        )
        .subcommand(
            Command::new("install")
                .about("Install packages")
                .arg(
                    Arg::new("install_suggests")
                    .long("install-suggests")
                    .help("Consider suggested packages as a dependency for installing")
                    .action(ArgAction::SetTrue)
                )
                .arg(
                    Arg::new("no_install_recommends")
                    .long("no-install-recommends")
                    .help("Do not consider recommended packages as a dependency for installing")
                    .action(ArgAction::SetTrue)
                )
                .arg(
                    Arg::new("package-spec")
                        .num_args(1..)
                        .required(true)
                        .help("Package specifications to install")
                )
        )
        .subcommand(
            Command::new("upgrade")
                .about("upgrade packages")
                .arg(
                    Arg::new("package-spec")
                        .num_args(1..)
                        .required(false)
                        .help("Package specifications to upgrade")
                )
        )
        .subcommand(
            Command::new("remove")
                .about("Remove packages")
                .arg(
                    Arg::new("package-spec")
                        .num_args(1..)
                        .required(true)
                        .help("Package specifications to remove")
                )
        )
        .subcommand(
            Command::new("list")
                .about("List packages")
                .arg(
                    Arg::new("list_all")
                        .long("all")
                        .help("List all packages")
                        .action(ArgAction::SetTrue)
                )
                .arg(
                    Arg::new("list_installed")
                        .long("installed")
                        .help("List installed packages")
                        .action(ArgAction::SetTrue)
                )
                .arg(
                    Arg::new("list_available")
                        .long("available")
                        .help("List available packages")
                        .action(ArgAction::SetTrue)
                )
                .arg(
                    Arg::new("glob-pattern")
                        .num_args(1..)
                        .required(false)
                        .help("Package glob pattern to list")
                )
        )
        .subcommand(
            Command::new("history")
                .about("Show environment history")
        )
        .subcommand(
            Command::new("rollback")
                .about("Rollback environment to a specific history")
                .arg(
                    Arg::new("history-id")
                        .num_args(1)
                        .required(true)
                        .help("History ID to rollback")
                        .value_parser(clap::value_parser!(u64))
                )
        )
        .subcommand(
            Command::new("hash")
                .about("Compute binary package hash")
                .arg(
                    Arg::new("package-store-dir")
                        .num_args(1..)
                        .required(true)
                        .help("Package store dir to compute hash")
                )
        )
        .get_matches();

    if matches.contains_id("version") {
        println!("epkg version {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // Create EPKGOptions and PackageManager instance
    let mut options: EPKGOptions = Default::default();

    options.env = if let Some(env) = matches.get_one::<String>("env") {
        // Use the command-line argument if provided
        env.to_string()
    } else if let Ok(active_env) = env::var("EPKG_ACTIVE_ENV") {
        // Use the environment variable if set
        active_env
    } else {
        // Use the default value
        "main".to_string()
    };

    options.arch = if let Some(arch) = matches.get_one::<String>("arch") {
        arch.to_string()
    } else {
        std::env::consts::ARCH.to_string()
    };

    options.simulate            = matches.get_flag("simulate");
    options.download_only       = matches.get_flag("download_only");
    options.quiet               = matches.get_flag("quiet");
    options.verbose             = matches.get_flag("verbose");
    options.assume_yes          = matches.get_flag("assume_yes");
    options.ignore_missing      = matches.get_flag("ignore_missing");

    let mut package_manager: PackageManager = Default::default();
    package_manager.options = options;

    // record raw command
    let command_line = std::env::args().collect::<Vec<String>>().join(" ");

    // Handle subcommands
    if let Some(matches) = matches.subcommand_matches("install") {
        if let Some(package_specs) = matches.get_many::<String>("package-spec") {
            package_manager.options.install_suggests = matches.get_flag("install_suggests");
            package_manager.options.no_install_recommends = matches.get_flag("no_install_recommends");
            package_manager.fork_on_suid()?;
            let packages_vec: Vec<String> = package_specs.clone().map(|s| s.clone()).collect();
            package_manager.install_packages(packages_vec.clone(), false)?;
            package_manager.record_history("install", packages_vec.clone(), &command_line).unwrap();
        }
    }

    if let Some(matches) = matches.subcommand_matches("upgrade") {
        if let Some(package_specs) = matches.get_many::<String>("package-spec") {
            package_manager.fork_on_suid()?;
            package_manager.upgrade_packages(package_specs)?;
        }
    }

    if let Some(matches) = matches.subcommand_matches("remove") {
        if let Some(package_specs) = matches.get_many::<String>("package-spec") {
            package_manager.fork_on_suid()?;
            let packages_vec: Vec<String> = package_specs.clone().map(|s| s.clone()).collect();
            package_manager.remove_packages(packages_vec.clone(), false)?;
            package_manager.record_history("remove", packages_vec.clone(), &command_line)?;
        }
    }

    if let Some(matches) = matches.subcommand_matches("list") {
        if let Some(package_specs) = matches.get_one::<String>("glob-pattern") {
            package_manager.options.list_all = matches.get_flag("list_all");
            package_manager.options.list_installed = matches.get_flag("list_installed");
            package_manager.options.list_available = matches.get_flag("list_available");
            privdrop_on_suid();
            package_manager.list_packages(package_specs)?;
        }
    }

    if let Some(_matches) = matches.subcommand_matches("history") {
        package_manager.print_history()?;
    }

    if let Some(matches) = matches.subcommand_matches("rollback") {
        if let Some(rollback_id) = matches.get_one::<u64>("history-id") {
            package_manager.rollback_history(*rollback_id)?;
        }
    }

    if let Some(matches) = matches.subcommand_matches("hash") {
        if let Some(package_store_dir) = matches.get_many::<String>("package-store-dir") {
            privdrop_on_suid();
            for dir in package_store_dir {
                let hash = crate::hash::epkg_store_hash(&dir)?;
                println!("{}", hash);
            }
        }
    }

    Ok(())
}
