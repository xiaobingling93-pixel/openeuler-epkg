mod models;
mod io;
mod download;
mod depends;
mod parse_requires;
mod install;
mod upgrade;
mod remove;
mod list;
mod hash;
mod ipc;
mod store;
mod dirs;
mod utils;
mod history;
mod environment;
mod init;
mod path;
mod repo;
use std::env;
use crate::models::*;
use crate::ipc::*;
use anyhow::Result;
use pretty_env_logger::env_logger;

use clap::{Arg, ArgAction, Command};

fn main() -> Result<()> {
    env_logger::init();

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
            Command::new("init")
                .about("Initialize personal epkg dir layout")
                .arg(
                    Arg::new("version")
                        .long("version")
                        .help("Version of epkg to install")
                        .value_name("VERSION")
                        .default_value("master")
                        .num_args(1)
                        .value_parser(clap::value_parser!(String))
                )
                .arg(
                    Arg::new("store")
                        .long("store")
                        .value_name("STORE")
                        .help("Store mode: 'shared' (reused by all users), 'private' (current user only), or 'auto' (shared if installed by root)")
                        .default_value("auto")
                        .num_args(1)
                        .value_parser(["shared", "private", "auto"])
                )
        )
        .subcommand(
            Command::new("update")
                .about("Update package metadata")
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
                        .required_unless_present("local")
                        .help("Package specifications to install")
                )
                .arg(
                    Arg::new("local")
                    .long("local")
                    .help("Install packages from local filesystem")
                    .action(ArgAction::SetTrue)
                )
                .arg(
                    Arg::new("fs")
                    .long("fs")
                    .help("Local filesystem directory to install packages")
                    .num_args(1)
                    .required(false)
                )
                .arg(
                    Arg::new("symlink")
                    .long("symlink")
                    .help("Local symlink directory to install packages")
                    .num_args(1)
                    .required(false)
                )
                .arg(
                    Arg::new("appbin")
                    .long("appbin")
                    .help("Install appbin packages")
                    .action(ArgAction::SetTrue)
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
                    Arg::new("assume_yes")
                    .short('y')
                    .long("assume-yes")
                    .help("Automatically answer yes to all prompts")
                    .action(ArgAction::SetTrue)
                )
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
                        .required(true)
                        .help("Package glob pattern to list")
                )
        )
        .subcommand(
            Command::new("history")
                .about("Show environment history")
        )
        .subcommand(
            Command::new("env")
                .about("Environment management")
                .subcommand(
                    Command::new("list")
                        .about("List all environments")
                )
                .subcommand(
                    Command::new("create")
                        .about("Create a new environment")
                        .arg(
                            Arg::new("channel")
                                .long("channel")
                                .value_name("CHANNEL")
                                .required(false)
                                .help("Set the channel for the environment")
                        )
                        .arg(
                            Arg::new("public")
                                .long("public")
                                .required(false)
                                .action(ArgAction::SetTrue)
                                .help("Usable by all users in the machine")
                        )
                        .arg(
                            Arg::new("name")
                                .num_args(1)
                                .required(true)
                                .help("Name of the new environment")
                        )
                )
                .subcommand(
                    Command::new("remove")
                        .about("Remove an environment")
                        .arg(
                            Arg::new("name")
                                .num_args(1)
                                .required(true)
                                .help("Name of the environment to remove")
                        )
                )
                .subcommand(
                    Command::new("register")
                        .about("Register an environment")
                        .arg(
                            Arg::new("name")
                                .num_args(1)
                                .required(true)
                                .help("Name of the environment to register")
                        )
                        .arg(
                            Arg::new("priority")
                                .long("priority")
                                .value_name("PRIORITY")
                                .required(false)
                                .help("Set the priority for the environment")
                        )
                )
                .subcommand(
                    Command::new("unregister")
                        .about("Unregister an environment")
                        .arg(
                            Arg::new("name")
                                .num_args(1)
                                .required(true)
                                .help("Name of the environment to unregister")
                        )
                )
                // The below activate/deactivate won't be called by shell env()
                // since they will only modify ENV vars.
                .subcommand(
                    Command::new("activate")
                        .about("Activate an environment")
                        .arg(
                            Arg::new("pure")
                                .long("pure")
                                .help("Create a pure environment")
                                .required(false)
                                .action(ArgAction::SetTrue)
                        )
                        .arg(
                            Arg::new("name")
                                .num_args(1)
                                .required(true)
                                .help("Name of the environment to activate")
                        )
                )
                .subcommand(
                    Command::new("deactivate")
                        .about("Deactivate the current environment")
                )
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
            Command::new("repo")
            .about("Repository management")
            .subcommand(
                Command::new("list")
                .about("List all available repositories")
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
        .subcommand(
            Command::new("build")
                .about("Build package from source")
                .arg(
                    Arg::new("package-yaml")
                        .num_args(1)
                        .required(true)
                        .help("Package YAML file to build")
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
    if let Some(sub_matches) = matches.subcommand_matches("init") {
        // Set options from command line
        package_manager.options.shared_store = sub_matches.get_one::<String>("store")
            .map(|s| match s.as_str() {
                "shared" => true,
                "private" => false,
                "auto" => {
                    // Use shared if root, private otherwise
                    let uid = nix::unistd::geteuid();
                    uid.is_root()
                }
                _ => false // Default to private if unknown value
            })
            .unwrap_or_else(|| {
                // Default to auto behavior if no store specified
                let uid = nix::unistd::geteuid();
                uid.is_root()
            });

        package_manager.options.version = sub_matches.get_one::<String>("version")
            .map(|s| s.to_string())
            .unwrap_or_else(|| "master".to_string());
        package_manager.dirs = EPKGDirs::builder()
            .with_options(package_manager.options)
            .build()?;
        package_manager.init()?;
    } else {
        package_manager.dirs = EPKGDirs::builder()
            .with_options(package_manager.options)
            .build()?;
    }

    if let Some(_sub_matches) = matches.subcommand_matches("update") {
        package_manager.fork_on_suid()?;
        package_manager.cache_repo()?;
    }

    if let Some(sub_matches) = matches.subcommand_matches("install") {
        if sub_matches.get_flag("local") {
            if let (Some(fs_dir), Some(symlink_dir)) = (sub_matches.get_one::<String>("fs"), sub_matches.get_one::<String>("symlink")) {
                let appbin = sub_matches.get_flag("appbin");
                package_manager.new_package(&fs_dir.clone(), &symlink_dir.clone(), appbin)?;
            }
        } else {
            if let Some(package_specs) = sub_matches.get_many::<String>("package-spec") {
                package_manager.options.install_suggests = sub_matches.get_flag("install_suggests");
                package_manager.options.no_install_recommends = sub_matches.get_flag("no_install_recommends");
                package_manager.fork_on_suid()?;
                package_manager.cache_repo()?;
                let packages_vec: Vec<String> = package_specs.clone().map(|s| s.clone()).collect();
                package_manager.install_packages(packages_vec.clone(), &command_line)?;
            }
        }
    }

    if let Some(sub_matches) = matches.subcommand_matches("upgrade") {
        if let Some(package_specs) = sub_matches.get_many::<String>("package-spec") {
            package_manager.fork_on_suid()?;
            package_manager.upgrade_packages(package_specs)?;
        }
    }

    if let Some(sub_matches) = matches.subcommand_matches("remove") {
        if let Some(package_specs) = sub_matches.get_many::<String>("package-spec") {
            let assume_yes = sub_matches.get_flag("assume_yes");
            package_manager.fork_on_suid()?;
            let packages_vec: Vec<String> = package_specs.clone().map(|s| s.clone()).collect();
            package_manager.remove_packages(packages_vec.clone(), assume_yes, &command_line)?;
        }
    }

    if let Some(sub_matches) = matches.subcommand_matches("list") {
        if let Some(package_specs) = sub_matches.get_one::<String>("glob-pattern") {
            package_manager.options.list_all = sub_matches.get_flag("list_all");
            package_manager.options.list_installed = sub_matches.get_flag("list_installed");
            package_manager.options.list_available = sub_matches.get_flag("list_available");
            privdrop_on_suid();
            package_manager.list_packages(package_specs)?;
        }
    }

    if let Some(_sub_matches) = matches.subcommand_matches("history") {
        package_manager.print_history()?;
    }

    if let Some(sub_matches) = matches.subcommand_matches("rollback") {
        if let Some(rollback_id) = sub_matches.get_one::<u64>("history-id") {
            package_manager.rollback_history(*rollback_id, &command_line)?;
        }
    }

    if let Some(sub_matches) = matches.subcommand_matches("repo") {
        if let Some(_sub_matches) = sub_matches.subcommand_matches("list") {
            package_manager.fork_on_suid()?;
            crate::repo::list_repos()?;
        }
    }

    if let Some(sub_matches) = matches.subcommand_matches("hash") {
        if let Some(package_store_dir) = sub_matches.get_many::<String>("package-store-dir") {
            privdrop_on_suid();
            for dir in package_store_dir {
                let hash = crate::hash::epkg_store_hash(&dir)?;
                println!("{}", hash);
            }
        }
    }

    if let Some(sub_matches) = matches.subcommand_matches("build") {
        if let Some(package_yaml) = sub_matches.get_one::<String>("package-yaml") {
            privdrop_on_suid();

            let build_script = package_manager.dirs.epkg_manager_cache.join("build/scripts/generic-build.sh");
            if !build_script.exists() {
                return Err(anyhow::anyhow!("Build script not found"));
            }

            let mut command = std::process::Command::new("bash");
            command.arg(build_script);
            command.arg(package_yaml.as_str());
            command.status()?;
        }
    }

    if let Some(sub_matches) = matches.subcommand_matches("env") {
        if let Some(_sub_matches) = sub_matches.subcommand_matches("list") {
            package_manager.list_environments()?;
        } else if let Some(sub_matches) = sub_matches.subcommand_matches("create") {
            if let Some(name) = sub_matches.get_one::<String>("name") {
                package_manager.options.channel = sub_matches.get_one::<String>("channel").map(|s| s.to_string());
                package_manager.options.public = sub_matches.get_flag("public");
                package_manager.create_environment(name)?;
            }
        } else if let Some(sub_matches) = sub_matches.subcommand_matches("remove") {
            if let Some(name) = sub_matches.get_one::<String>("name") {
                package_manager.remove_environment(name)?;
            }
        } else if let Some(sub_matches) = sub_matches.subcommand_matches("register") {
            if let Some(name) = sub_matches.get_one::<String>("name") {
                package_manager.options.priority = sub_matches.get_one::<i32>("priority").cloned();
                package_manager.register_environment(name)?;
            }
        } else if let Some(sub_matches) = sub_matches.subcommand_matches("unregister") {
            if let Some(name) = sub_matches.get_one::<String>("name") {
                package_manager.unregister_environment(name)?;
            }
        } else if let Some(sub_matches) = sub_matches.subcommand_matches("activate") {
            if let Some(name) = sub_matches.get_one::<String>("name") {
                package_manager.options.pure = sub_matches.get_flag("pure");
                package_manager.activate_environment(name)?;
            }
        } else if let Some(_sub_matches) = sub_matches.subcommand_matches("deactivate") {
            package_manager.deactivate_environment()?;
        }
    }

    Ok(())
}
