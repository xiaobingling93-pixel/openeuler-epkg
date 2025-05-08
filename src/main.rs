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
use std::path::PathBuf;
use crate::models::*;
use crate::ipc::*;
use anyhow::Result;
use pretty_env_logger::env_logger;

use clap::{arg, ArgAction, Command};

fn main() -> Result<()> {
    env_logger::init();

    // Create the CLI app, prefer the clap Builder API for more controls
    let matches = Command::new("epkg")
        .version(env!("CARGO_PKG_VERSION"))
        .author("Wu Fengguang <wfg@mail.ustc.edu.cn>")
        .author("Duan Pengjie <pengjieduan@gmail.com>")
        .author("Yingjiahui <ying_register@163.com>")
        .about("The EPKG package manager")
        .arg(arg!(-e --env <ENV> "Select the environment").default_value("main"))
        .arg(arg!(--arch <ARCH> "Select the CPU architecture").default_value(std::env::consts::ARCH))
        .arg(arg!(-s --simulate "Simulated run without changing the system").aliases(["dry-run"]))
        .arg(arg!(--download-only "Download packages without installing"))
        .arg(arg!(-q --quiet "Suppress output"))
        .arg(arg!(-v --verbose "Verbose operation, show debug messages"))
        .arg(arg!(-y --assume-yes "Automatically answer yes to all prompts"))
        .arg(arg!(-m --ignore-missing "Ignore missing packages"))
        .subcommand(
            Command::new("init")
                .about("Initialize personal epkg dir layout")
                .arg(arg!(--version <VERSION> "Version of epkg to install").default_value("master"))
                .arg(
                    arg!(--store <STORE> "Store mode: 'shared' (reused by all users), 'private' (current user only), or 'auto' (shared if installed by root)")
                        .default_value("auto")
                        .value_parser(["shared", "private", "auto"]),
                )
        )
        .subcommand(Command::new("update").about("Update package metadata"))
        .subcommand(
            Command::new("install")
                .about("Install packages")
                .arg(arg!(--install-suggests "Consider suggested packages as a dependency for installing"))
                .arg(arg!(--no-install-recommends "Do not consider recommended packages as a dependency for installing"))
                .arg(arg!([PACKAGE_SPEC] ... "Package specifications to install").required_unless_present("local"))
                .arg(arg!(--local "Install packages from local filesystem"))
                .arg(arg!(--fs <DIR> "Local filesystem directory to install packages"))
                .arg(arg!(--symlink <DIR> "Local symlink directory to install packages"))
                .arg(arg!(--ebin "Install package binaries to ebin/"))
        )
        .subcommand(
            Command::new("upgrade")
                .about("upgrade packages")
                .arg(arg!([PACKAGE_SPEC] ... "Package specifications to upgrade"))
        )
        .subcommand(
            Command::new("remove")
                .about("Remove packages")
                .arg(arg!(<PACKAGE_SPEC> ... "Package specifications to remove"))
        )
        .subcommand(
            Command::new("list")
                .about("List packages")
                .arg(arg!(--all "List all packages"))
                .arg(arg!(--installed "List installed packages"))
                .arg(arg!(--available "List available packages"))
                .arg(arg!(<GLOB_PATTERN> "Package glob pattern to list"))
        )
        .subcommand(
            Command::new("env")
                .about("Environment management")
                .subcommand(Command::new("list").about("List all environments"))
                .subcommand(
                    Command::new("create")
                        .about("Create a new environment")
                        .arg(arg!(<ENV_NAME> "Name of the new environment"))
                        .arg(arg!(--channel <CHANNEL> "Set the channel for the environment"))
                        .arg(arg!(--public "Usable by all users in the machine"))
                        .arg(arg!(--path <PATH> "Specify custom path for the environment"))
                        .arg(arg!(--config <FILE> "Configuration file to use"))
                )
                .subcommand(
                    Command::new("remove")
                        .about("Remove an environment")
                        .arg(arg!(<ENV_NAME> "Name of the environment to remove"))
                )
                .subcommand(
                    Command::new("register")
                        .about("Register an environment")
                        .arg(arg!(<ENV_NAME> "Name of the environment to register"))
                        .arg(arg!(--priority <PRIORITY> "Set the priority for the environment"))
                )
                .subcommand(
                    Command::new("unregister")
                        .about("Unregister an environment")
                        .arg(arg!(<ENV_NAME> "Name of the environment to unregister"))
                )
                .subcommand(
                    Command::new("activate")
                        .about("Activate an environment")
                        .arg(arg!(<ENV_NAME> "Name of the environment to activate"))
                        .arg(arg!(--pure "Create a pure environment"))
                        .arg(arg!(--stack "Stack this environment on top of the current one"))
                )
                .subcommand(Command::new("deactivate").about("Deactivate the current environment"))
                .subcommand(
                    Command::new("export")
                        .about("Export environment configuration")
                        .arg(arg!(<ENV_NAME> "Name of the environment to export"))
                        .arg(arg!(-o --output <FILE> "Output file path"))
                )
                .subcommand(Command::new("path").about("Update PATH environment variable"))
                .subcommand(
                    Command::new("config")
                        .about("Configure environment settings")
                        .subcommand(
                            Command::new("edit")
                                .about("Edit environment configuration file")
                        )
                        .subcommand(
                            Command::new("get")
                                .about("Get environment configuration value")
                                .arg(arg!(<NAME> "Configuration name to get"))
                        )
                        .subcommand(
                            Command::new("set")
                                .about("Set environment configuration value")
                                .arg(arg!(<NAME> "Configuration name to set"))
                                .arg(arg!(<VALUE> "Value to set"))
                        )
                )
        )
        .subcommand(
            Command::new("history")
                .about("Show environment history")
                .arg(arg!([MAX_GENERATIONS] "Maximum number of generations to show").value_parser(clap::value_parser!(u64)))
        )
        .subcommand(
            Command::new("restore")
                .about("Restore environment to a specific generation")
                .arg(arg!(<GEN_ID> "Generation ID to restore to (negative number for relative rollback)").value_parser(clap::value_parser!(i64)))
        )
        .subcommand(
            Command::new("repo")
                .about("Repository management")
                .subcommand(Command::new("list").about("List all available repositories"))
        )
        .subcommand(
            Command::new("hash")
                .about("Compute binary package hash")
                .arg(arg!(<PACKAGE_STORE_DIR> ... "Package store dir to compute hash"))
        )
        .subcommand(
            Command::new("build")
                .about("Build package from source")
                .arg(arg!(<PACKAGE_YAML> "Package YAML file to build"))
        )
        .get_matches();

    if matches.contains_id("version") {
        println!("epkg version {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // Create EPKGOptions and PackageManager instance
    let mut options: EPKGOptions = Default::default();
    options.env = matches.get_one::<String>("env").map_or_else(
        || env::var("EPKG_ACTIVE_ENV").map(|s| s.trim_end_matches(':').to_string()).unwrap_or_else(|_| "main".to_string()),
        |s| s.to_string()
    );
    options.arch = matches.get_one::<String>("arch").map_or_else(
        || std::env::consts::ARCH.to_string(),
        |s| s.to_string()
    );

    if !SUPPORT_ARCH_LIST.contains(&options.arch) {
        return Err(anyhow!("Unsupported system architecture: {}", arch));
    }

    options.simulate       = matches.get_flag("simulate");
    options.download_only  = matches.get_flag("download-only");
    options.quiet          = matches.get_flag("quiet");
    options.verbose        = matches.get_flag("verbose");
    options.assume_yes     = matches.get_flag("assume-yes");
    options.ignore_missing = matches.get_flag("ignore-missing");
    options.command_line   = std::env::args().collect::<Vec<String>>().join(" ");

    let mut package_manager: PackageManager = Default::default();
    package_manager.options = options;

    match matches.subcommand() {
        Some(("init",    sub_matches)) => package_manager.command_init(sub_matches)?,
        Some(("update",  _))           => package_manager.command_update()?,
        Some(("install", sub_matches)) => package_manager.command_install(sub_matches)?,
        Some(("upgrade", sub_matches)) => package_manager.command_upgrade(sub_matches)?,
        Some(("remove",  sub_matches)) => package_manager.command_remove(sub_matches)?,
        Some(("list",    sub_matches)) => package_manager.command_list(sub_matches)?,
        Some(("history", _))           => package_manager.command_history()?,
        Some(("restore", sub_matches)) => package_manager.command_restore(sub_matches)?,
        Some(("repo",    sub_matches)) => package_manager.command_repo(sub_matches)?,
        Some(("hash",    sub_matches)) => package_manager.command_hash(sub_matches)?,
        Some(("build",   sub_matches)) => package_manager.command_build(sub_matches)?,
        Some(("env",     sub_matches)) => package_manager.command_env(sub_matches)?,
        _ => {} // No subcommand or unknown subcommand
    }

    Ok(())
}

// Command handlers
impl PackageManager {

    fn command_init(&mut self, sub_matches: &clap::ArgMatches) -> Result<()> {
        self.options.shared_store = sub_matches.get_one::<String>("store")
            .map(|s| match s.as_str() {
                "shared" => true,
                "private" => false,
                "auto" => nix::unistd::geteuid().is_root(),
                _ => false
            })
            .unwrap_or_else(|| nix::unistd::geteuid().is_root());

        self.options.version = sub_matches.get_one::<String>("version")
            .map(|s| s.to_string())
            .unwrap_or_else(|| "master".to_string());

        self.dirs = EPKGDirs::builder()
            .with_options(self.options.clone())
            .build()?;
        self.init()
    }

    fn command_update(&mut self) -> Result<()> {
        self.fork_on_suid()?;
        self.cache_repo()
    }

    fn command_install(&mut self, sub_matches: &clap::ArgMatches) -> Result<()> {
        if sub_matches.get_flag("local") {
            if let (Some(fs_dir), Some(symlink_dir)) = (sub_matches.get_one::<String>("fs"), sub_matches.get_one::<String>("symlink")) {
                let ebin = sub_matches.get_flag("ebin");
                let fs_dir = PathBuf::from(fs_dir);
                let symlink_dir = PathBuf::from(symlink_dir);
                self.new_package(&fs_dir, &symlink_dir, ebin)?;
            }
        } else if let Some(package_specs) = sub_matches.get_many::<String>("PACKAGE_SPEC") {
            self.options.install_suggests = sub_matches.get_flag("install-suggests");
            self.options.no_install_recommends = sub_matches.get_flag("no-install-recommends");
            self.fork_on_suid()?;
            self.cache_repo()?;
            let packages_vec: Vec<String> = package_specs.cloned().collect();
            self.install_packages(packages_vec)?;
        }
        Ok(())
    }

    fn command_upgrade(&mut self, sub_matches: &clap::ArgMatches) -> Result<()> {
        if let Some(package_specs) = sub_matches.get_many::<String>("PACKAGE_SPEC") {
            self.fork_on_suid()?;
            self.upgrade_packages(package_specs)?;
        }
        Ok(())
    }

    fn command_remove(&mut self, sub_matches: &clap::ArgMatches) -> Result<()> {
        if let Some(package_specs) = sub_matches.get_many::<String>("PACKAGE_SPEC") {
            self.fork_on_suid()?;
            let packages_vec: Vec<String> = package_specs.cloned().collect();
            self.remove_packages(packages_vec)?;
        }
        Ok(())
    }

    fn command_list(&mut self, sub_matches: &clap::ArgMatches) -> Result<()> {
        if let Some(glob_pattern) = sub_matches.get_one::<String>("GLOB_PATTERN") {
            self.options.list_all = sub_matches.get_flag("all");
            self.options.list_installed = sub_matches.get_flag("installed");
            self.options.list_available = sub_matches.get_flag("available");
            privdrop_on_suid();
            self.list_packages(glob_pattern)?;
        }
        Ok(())
    }

    fn command_history(&mut self) -> Result<()> {
        if let Some(max_generations) = sub_matches.get_one::<u64>("MAX_GENERATIONS") {
            self.options.max_generations = Some(*max_generations);
        }
        self.print_history()
    }

    fn command_restore(&mut self, sub_matches: &clap::ArgMatches) -> Result<()> {
        if let Some(rollback_id) = sub_matches.get_one::<i64>("GEN_ID") {
            self.rollback_history(*rollback_id)?;
        }
        Ok(())
    }

    fn command_repo(&mut self, sub_matches: &clap::ArgMatches) -> Result<()> {
        if let Some(_) = sub_matches.subcommand_matches("list") {
            self.fork_on_suid()?;
            crate::repo::list_repos()?;
        }
        Ok(())
    }

    fn command_hash(&self, sub_matches: &clap::ArgMatches) -> Result<()> {
        if let Some(package_store_dirs) = sub_matches.get_many::<String>("PACKAGE_STORE_DIR") {
            privdrop_on_suid();
            for dir in package_store_dirs {
                let hash = crate::hash::epkg_store_hash(dir)?;
                println!("{}", hash);
            }
        }
        Ok(())
    }

    fn command_build(&mut self, sub_matches: &clap::ArgMatches) -> Result<()> {
        if let Some(package_yaml) = sub_matches.get_one::<String>("PACKAGE_YAML") {
            privdrop_on_suid();

            let build_script = self.dirs.epkg_manager_cache.join("build/scripts/generic-build.sh");
            if !build_script.exists() {
                return Err(anyhow::anyhow!("Build script not found"));
            }

            let mut command = std::process::Command::new("bash");
            command.arg(build_script);
            command.arg(package_yaml);
            command.status()?;
        }
        Ok(())
    }

    fn command_env(&mut self, sub_matches: &clap::ArgMatches) -> Result<()> {
        match sub_matches.subcommand() {
            Some(("list", _)) => self.list_environments(),
            Some(("create", sub_matches)) => {
                if let Some(name) = sub_matches.get_one::<String>("ENV_NAME") {
                    self.options.channel = sub_matches.get_one::<String>("channel").cloned();
                    self.options.env_path = sub_matches.get_one::<String>("path").cloned();
                    self.options.config_file = sub_matches.get_one::<String>("config").cloned();
                    self.options.public = sub_matches.get_flag("public");
                    self.create_environment(name)
                } else {
                    Ok(())
                }
            }
            Some(("remove", sub_matches)) => {
                if let Some(name) = sub_matches.get_one::<String>("ENV_NAME") {
                    self.remove_environment(name)
                } else {
                    Ok(())
                }
            }
            Some(("register", sub_matches)) => {
                if let Some(name) = sub_matches.get_one::<String>("ENV_NAME") {
                    self.options.priority = sub_matches.get_one::<i32>("priority").cloned();
                    self.register_environment(name)
                } else {
                    Ok(())
                }
            }
            Some(("unregister", sub_matches)) => {
                if let Some(name) = sub_matches.get_one::<String>("ENV_NAME") {
                    self.unregister_environment(name)
                } else {
                    Ok(())
                }
            }
            Some(("activate", sub_matches)) => {
                if let Some(name) = sub_matches.get_one::<String>("ENV_NAME") {
                    self.options.pure = sub_matches.get_flag("pure");
                    self.options.stack = sub_matches.get_flag("stack");
                    self.activate_environment(name)
                } else {
                    Ok(())
                }
            }
            Some(("deactivate", _)) => self.deactivate_environment(),
            Some(("export", sub_matches)) => {
                if let Some(name) = sub_matches.get_one::<String>("ENV_NAME") {
                    let output = sub_matches.get_one::<String>("output").cloned();
                    self.export_environment(name, output)
                } else {
                    Ok(())
                }
            }
            Some(("path", _)) => self.update_path(),
            Some(("config", sub_matches)) => {
                match sub_matches.subcommand() {
                    Some(("edit", _)) => self.edit_environment_config(),
                    Some(("get", sub_matches)) => {
                        if let Some(name) = sub_matches.get_one::<String>("NAME") {
                            self.get_environment_config(name)
                        } else {
                            Ok(())
                        }
                    }
                    Some(("set", sub_matches)) => {
                        if let (Some(name), Some(value)) = (sub_matches.get_one::<String>("NAME"), sub_matches.get_one::<String>("VALUE")) {
                            self.set_environment_config(name, value)
                        } else {
                            Ok(())
                        }
                    }
                    _ => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }

}
