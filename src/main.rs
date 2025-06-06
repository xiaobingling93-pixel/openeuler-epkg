mod dirs;
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
mod utils;
mod history;
mod environment;
mod init;
mod path;
mod repo;
mod mmio;
mod packages_stream;
mod deb_repo;
mod rpm_repo;
mod deb_pkg;
mod rpm_pkg;
mod apk_repo;
mod apk_pkg;
mod epkg;
mod version;

#[cfg(debug_assertions)]
mod rpm_verify;

use std::env;
use std::path::Path;
use std::path::PathBuf;
use std::process::exit;
use std::io::Write;
use std::backtrace::Backtrace;
use time::OffsetDateTime;
use time::macros::format_description;
use crate::models::*;
use crate::ipc::*;
use crate::ipc::privdrop_on_suid;
use crate::dirs::get_epkg_manager_path;
use color_eyre::Result;
use color_eyre::eyre;
use ctrlc;
use clap::{arg, Command};
use env_logger;
use log;
use list::ListScope;

fn main() -> Result<()> {
    color_eyre::install()?;
    setup_logging();
    setup_ctrlc();

    // 第一次访问会触发命令行解析和配置初始化
    log::trace!("Application starting with config: {:#?}", config());

    let mut package_manager: PackageManager = Default::default();
    let matches = &CLAP_MATCHES;
    match matches.subcommand() {
        Some(("init",       sub_matches))  =>  package_manager.command_init(sub_matches)?,
        Some(("env",        sub_matches))  =>  package_manager.command_env(sub_matches)?,
        Some(("list",       sub_matches))  =>  package_manager.command_list(sub_matches)?,
        Some(("install",    sub_matches))  =>  package_manager.command_install(sub_matches)?,
        Some(("upgrade",    sub_matches))  =>  package_manager.command_upgrade(sub_matches)?,
        Some(("remove",     sub_matches))  =>  package_manager.command_remove(sub_matches)?,
        Some(("history",    sub_matches))  =>  package_manager.command_history(sub_matches)?,
        Some(("restore",    sub_matches))  =>  package_manager.command_restore(sub_matches)?,
        Some(("update",     _))            =>  package_manager.command_update()?,
        Some(("repo",       sub_matches))  =>  package_manager.command_repo(sub_matches)?,
        Some(("hash",       sub_matches))  =>  package_manager.command_hash(sub_matches)?,
        Some(("build",      sub_matches))  =>  package_manager.command_build(sub_matches)?,
        Some(("unpack",     sub_matches))  =>  package_manager.command_unpack(&sub_matches)?,
        _ => {} // No subcommand or unknown subcommand
    }

    Ok(())
}

fn setup_logging() {
    env_logger::Builder::from_default_env()
        .format(|buf, record| {
            writeln!(
                buf,
                "[{} {} {}:{}] {}",
                match OffsetDateTime::now_local() {
                    Ok(dt) => dt.format(&format_description!("[year]-[month]-[day] [hour repr:24]:[minute]:[second] [offset_hour sign:mandatory][offset_minute]")).unwrap_or_else(|_| "<time_fmt_err>".to_string()),
                    Err(_) => "<local_time_err>".to_string(),
                },
                record.level(),
                record.file().unwrap_or("unknown"),
                record.line().unwrap_or(0),
                record.args()
            )
        })
        .init();
}

fn setup_ctrlc() {
    if !std::env::var("RUST_BACKTRACE").is_ok() {
        return;
    }

    // Set up Ctrl-C handler
    ctrlc::set_handler(move || {
        println!("\nReceived Ctrl-C! Printing backtrace...");

        // Capture and print the backtrace
        let backtrace = Backtrace::capture();
        println!("{:#?}", backtrace);

        // Exit gracefully
        std::process::exit(1);
    }).expect("Failed to set Ctrl-C handler");
}

pub fn parse_cmdline() -> clap::ArgMatches {
    Command::new("epkg")
        .version(env!("CARGO_PKG_VERSION"))
        .author("Wu Fengguang <wfg@mail.ustc.edu.cn>")
        .author("Duan Pengjie <pengjieduan@gmail.com>")
        .author("Yingjiahui <ying_register@163.com>")
        .about("The EPKG package manager")
        .arg_required_else_help(true) // This will show help if no args are provided
        .arg(arg!(-c --config <FILE> "Configuration file to use"))
        .arg(arg!(-e --env <ENV> "Select the environment"))
        .arg(arg!(--arch <ARCH> "Select the CPU architecture").default_value(std::env::consts::ARCH))
        .arg(arg!(-s --simulate "Simulated run without changing the system").aliases(["dry-run"]))
        .arg(arg!(--"download-only" "Download packages without installing"))
        .arg(arg!(-q --quiet "Suppress output"))
        .arg(arg!(-v --verbose "Verbose operation, show debug messages"))
        .arg(arg!(-y --"assume-yes" "Automatically answer yes to all prompts"))
        .arg(arg!(-m --"ignore-missing" "Ignore missing packages"))
        .arg(arg!(--"metadata-expire" <SECONDS> "Metadata expiration time in seconds (0=never, -1=always)").value_parser(clap::value_parser!(i32)))
        .arg(arg!(--proxy <URL> "HTTP proxy URL (e.g., http://proxy.example.com:8080)"))
        .arg(arg!(--"nr-parallel" <NUMBER> "Number of parallel download threads").value_parser(clap::value_parser!(usize)))
        .arg(arg!(--"parallel-processing" <BOOL> "Enable parallel processing for metadata updates (true/false)").value_parser(clap::value_parser!(bool)))
        .subcommand(
            Command::new("init")
                .about("Initialize personal epkg dir layout")
                .arg(arg!(--version <VERSION>).help(format!("Version of epkg to install [default: {}]", DEFAULT_VERSION)))
                .arg(
                    arg!(--store <STORE> "Store mode: 'shared' (reused by all users), 'private' (current user only), or 'auto' (shared if installed by root)")
                        .default_value("auto")
                        .value_parser(["shared", "private", "auto"]),
                )
        )
        .subcommand(
            Command::new("env")
                .about("Environment management")
                .arg_required_else_help(true)
                .subcommand(
                    Command::new("list")
                        .about("List all environments")
                )
                .subcommand(
                    Command::new("create")
                        .about("Create a new environment")
                        .arg(arg!(<ENV_NAME> "Name of the new environment"))
                        .arg(arg!(-c --channel <CHANNEL> "Set the channel for the environment"))
                        .arg(arg!(-P --public "Usable by all users in the machine"))
                        .arg(arg!(-p --path <PATH> "Specify custom path for the environment"))
                        .arg(arg!(-i --import <FILE> "Import from config file"))
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
                        .arg(arg!(-p --pure "Create a pure environment"))
                        .arg(arg!(-s --stack "Stack this environment on top of the current one"))
                )
                .subcommand(
                    Command::new("deactivate")
                        .about("Deactivate the current environment")
                )
                .subcommand(
                    Command::new("export")
                        .about("Export environment configuration")
                        .arg(arg!(<ENV_NAME> "Name of the environment to export"))
                        .arg(arg!(-o --output <FILE> "Output file path"))
                )
                .subcommand(
                    Command::new("path")
                        .about("Update PATH environment variable")
                )
                .subcommand(
                    Command::new("config")
                        .about("Configure environment settings")
                        .arg_required_else_help(true)
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
            Command::new("list")
                .about("List packages")
                .arg(arg!(--all "List all packages"))
                .arg(arg!(--installed "List installed packages"))
                .arg(arg!(--available "List available packages"))
                .arg(arg!(--upgradable "List upgradable packages"))
                .arg(arg!([GLOB_PATTERN] "Package name filtering"))
        )
        .subcommand(
            Command::new("install")
                .about("Install packages")
                .arg(arg!(--"install-suggests" "Consider suggested packages as a dependency for installing"))
                .arg(arg!(--"no-install-recommends" "Do not consider recommended packages as a dependency for installing"))
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
            Command::new("history")
                .about("Show environment history")
                .arg(arg!([MAX_GENERATIONS] "Maximum number of generations to show").value_parser(clap::value_parser!(u32)))
        )
        .subcommand(
            Command::new("restore")
                .about("Restore environment to a specific generation")
                .arg(arg!(<GEN_ID> "Generation ID to restore to (negative number for relative rollback)").value_parser(clap::value_parser!(i32)))
        )
        .subcommand(
            Command::new("update")
                .about("Update package metadata")
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
        .subcommand(
            Command::new("unpack")
                .about("Unpack package file(s) into a temporary directory")
                .arg(arg!(<PACKAGE_FILE> ... "Package files to unpack").required(true))
        )
        .get_matches()
}

fn parse_yaml_config<T>(path: &str) -> Result<T>
where
    T: for<'de> serde::Deserialize<'de>,
{
    std::fs::read_to_string(path)
        .map_err(|e| eyre::eyre!("Failed to read config file {}: {}", path, e))
        .and_then(|s| {
            serde_yaml::from_str(&s)
                .map_err(|e| eyre::eyre!("Failed to parse YAML from config file {}: {}", path, e))
        })
}

pub fn parse_options_common(matches: &clap::ArgMatches) -> Result<EPKGConfig> {
    if matches.contains_id("version") {
        println!("epkg version {}", env!("CARGO_PKG_VERSION"));
        exit(0);
    }
    let mut config: EPKGConfig = matches.get_one::<String>("config").map_or_else(
        || {
            // Try default config file location
            let default_config_path = dirs::get_home_epkg_path()?.join("config").join("options.yaml");
            if default_config_path.exists() {
                parse_yaml_config(default_config_path.to_str().expect("Default config path is not valid UTF-8"))
            } else {
                // Using "{}" ensures that serde processes an empty map, allowing field-level
                // #[serde(default = "...")] attributes to be applied.
                // An empty string "" typically parses to Yaml::Null, which doesn't trigger these defaults for struct fields.
                Ok(serde_yaml::from_str("{}")
                    .unwrap_or_else(|e| panic!("Failed to load default config from empty map: {:?}", e)))
            }
        },
        |s| parse_yaml_config(s)
    )?;

    config.common.env = matches.get_one::<String>("env").map_or_else(
        || env::var("EPKG_ACTIVE_ENV").map(|s| s.trim_end_matches(':').to_string()).unwrap_or_else(|_| "main".to_string()),
        |s| s.to_string()
    );
    if let Some(arch) = matches.get_one::<String>("arch") {
        config.common.arch = arch.to_string();
    }
    if config.common.arch.is_empty() {
        config.common.arch = models::default_arch();
        eprintln!("arch was configured to empty, using default architecture: {}", config.common.arch);
    }

    if !SUPPORT_ARCH_LIST.contains(&config.common.arch.as_str()) {
        return Err(eyre::eyre!("Unsupported system architecture: {}", config.common.arch));
    }

    config.common.simulate          = matches.get_flag("simulate");
    config.common.download_only     = matches.get_flag("download-only");

    if matches.contains_id("quiet") {
        config.common.quiet             = matches.get_flag("quiet");
    }
    if matches.contains_id("verbose") {
        config.common.verbose           = matches.get_flag("verbose");
    }
    if matches.contains_id("assume-yes") {
        config.common.assume_yes        = matches.get_flag("assume-yes");
    }
    if matches.contains_id("ignore-missing") {
        config.common.ignore_missing    = matches.get_flag("ignore-missing");
    }

    // Parse new common options
    if let Some(metadata_expire) = matches.get_one::<i32>("metadata-expire") {
        config.common.metadata_expire = *metadata_expire;
    }
    if let Some(proxy) = matches.get_one::<String>("proxy") {
        config.common.proxy = proxy.to_string();
    }
    // Setup parallel processing parameters
    setup_parallel_params(&mut config, matches);

    config.command_line             = std::env::args().collect::<Vec<String>>().join(" ");

    Ok(config)
}

/// Setup parallel processing parameters based on command line arguments and system capabilities
fn setup_parallel_params(config: &mut EPKGConfig, matches: &clap::ArgMatches) {
    // Handle nr_parallel parameter
    if let Some(nr_parallel) = matches.get_one::<usize>("nr-parallel") {
        // Ensure nr_parallel is at least 1
        config.common.nr_parallel = if *nr_parallel == 0 { 1 } else { *nr_parallel };
    }

    // Handle parallel_processing parameter
    if let Some(parallel_processing) = matches.get_one::<bool>("parallel-processing") {
        // User explicitly set parallel_processing
        config.common.parallel_processing = *parallel_processing;
    } else if config.common.nr_parallel <= 1 {
        // Auto-disable if nr_parallel <= 1, overriding the default
        config.common.parallel_processing = false;
    }
    // Otherwise, use the default value set by default_parallel_processing()
}

pub fn parse_options_subcommand(matches: &clap::ArgMatches, mut config: EPKGConfig) -> Result<EPKGConfig> {
    config.subcommand = matches.subcommand().map(|(name, _)| name.to_string()).unwrap_or_default();
    if config.subcommand != "init" {
        config.init.shared_store = utils::is_running_as_root() && Path::new("/opt/epkg/store").exists();
    }

    match matches.subcommand() {
        Some(("init",       sub_matches))  =>  parse_options_init(&mut config, sub_matches).expect("Failed to parse init options"),
        Some(("env",        sub_matches))  =>  parse_options_env(&mut config, sub_matches).expect("Failed to parse env options"),
        Some(("list",       sub_matches))  =>  parse_options_list(&mut config, sub_matches).expect("Failed to parse list options"),
        Some(("install",    sub_matches))  =>  parse_options_install(&mut config, sub_matches).expect("Failed to parse install options"),
        Some(("upgrade",    sub_matches))  =>  parse_options_upgrade(&mut config, sub_matches).expect("Failed to parse upgrade options"),
        Some(("remove",     sub_matches))  =>  parse_options_remove(&mut config, sub_matches).expect("Failed to parse remove options"),
        Some(("history",    sub_matches))  =>  parse_options_history(&mut config, sub_matches).expect("Failed to parse history options"),
        Some(("restore",    sub_matches))  =>  parse_options_restore(&mut config, sub_matches).expect("Failed to parse restore options"),
        Some(("update",     sub_matches))  =>  parse_options_update(&mut config, sub_matches).expect("Failed to parse update options"),
        Some(("repo",       sub_matches))  =>  parse_options_repo(&mut config, sub_matches).expect("Failed to parse repo options"),
        Some(("hash",       sub_matches))  =>  parse_options_hash(&mut config, sub_matches).expect("Failed to parse hash options"),
        Some(("build",      sub_matches))  =>  parse_options_build(&mut config, sub_matches).expect("Failed to parse build options"),
        Some(("unpack",     sub_matches))  =>  parse_options_unpack(&mut config, sub_matches).expect("Failed to parse unpack options"),
        _ => {} // No subcommand or unknown subcommand
    }
    log::debug!("Configuration: {:#?}", config);
    Ok(config)
}

fn parse_options_init(config: &mut EPKGConfig, sub_matches: &clap::ArgMatches) -> Result<()> {
    config.init.shared_store = sub_matches.get_one::<String>("store")
        .map(|s| match s.as_str() {
            "shared" => true,
            "private" => false,
            "auto" => utils::is_running_as_root(),
            _ => false
        })
        .unwrap_or_else(|| utils::is_running_as_root());

    if let Some(version) = sub_matches.get_one::<String>("version") {
        config.init.version = version.to_string();
    } else if config.init.version.is_empty() {
        config.init.version = models::default_version();
        eprintln!("version was configured to empty, using default version: {}", config.init.version);
    }

    // compose options for creating "common" env, must be done here since
    // config() content will freeze after first call, and some routines rely on it.
    config.common.env = "common".to_string();
    config.env.public = config.init.shared_store;
    config.env.channel = Some(DEFAULT_CHANNEL.to_string());

    Ok(())
}

fn parse_options_env(config: &mut EPKGConfig, matches: &clap::ArgMatches) -> Result<()> {
    match matches.subcommand() {
        Some(("create", sub_matches)) => {
            if let Some(env_name) = sub_matches.get_one::<String>("ENV_NAME") {
                config.common.env = env_name.to_string();
            }
            if let Some(channel) = sub_matches.get_one::<String>("channel") {
                config.env.channel = Some(channel.to_string());
            }
            if sub_matches.contains_id("public") {
                config.env.public = sub_matches.get_flag("public");
            }
            config.env.env_path = sub_matches.get_one::<String>("path").cloned();
            config.env.import_file = sub_matches.get_one::<String>("import").cloned();
        }
        Some(("remove", sub_matches)) => {
            if let Some(env_name) = sub_matches.get_one::<String>("ENV_NAME") {
                config.common.env = env_name.to_string();
            }
        }
        Some(("register", sub_matches)) => {
            if let Some(env_name) = sub_matches.get_one::<String>("ENV_NAME") {
                config.common.env = env_name.to_string();
            }
            config.env.priority = sub_matches.get_one::<i32>("priority").cloned();
        }
        Some(("unregister", sub_matches)) => {
            if let Some(env_name) = sub_matches.get_one::<String>("ENV_NAME") {
                config.common.env = env_name.to_string();
            }
        }
        Some(("activate", sub_matches)) => {
            if let Some(env_name) = sub_matches.get_one::<String>("ENV_NAME") {
                config.common.env = env_name.to_string();
            }
            if sub_matches.contains_id("pure") {
                config.env.pure = sub_matches.get_flag("pure");
            }
            if sub_matches.contains_id("stack") {
                config.env.stack = sub_matches.get_flag("stack");
            }
        }
        Some(("export", sub_matches)) => {
            if let Some(env_name) = sub_matches.get_one::<String>("ENV_NAME") {
                config.common.env = env_name.to_string();
            }
        }
        _ => {}
    }
    Ok(())
}

fn parse_options_list(config: &mut EPKGConfig, sub_matches: &clap::ArgMatches) -> Result<()> {
    if sub_matches.get_flag("all") {
        config.list.list_all = true;
    }
    if sub_matches.get_flag("installed") {
        config.list.list_installed = true;
    }
    if sub_matches.get_flag("available") {
        config.list.list_available = true;
    }
    // Note: upgradable option will be handled directly in the list command
    Ok(())
}

fn parse_options_install(config: &mut EPKGConfig, sub_matches: &clap::ArgMatches) -> Result<()> {
    if let Some(_package_specs) = sub_matches.get_many::<String>("PACKAGE_SPEC") {
        if sub_matches.contains_id("install-suggests") {
            config.install.install_suggests = sub_matches.get_flag("install-suggests");
        }
        if sub_matches.contains_id("no-install-recommends") {
            config.install.no_install_recommends = sub_matches.get_flag("no-install-recommends");
        }
    }
    Ok(())
}

fn parse_options_upgrade(_options: &mut EPKGConfig, _sub_matches: &clap::ArgMatches) -> Result<()> {
    Ok(())
}

fn parse_options_remove(_options: &mut EPKGConfig, _sub_matches: &clap::ArgMatches) -> Result<()> {
    Ok(())
}

fn parse_options_history(config: &mut EPKGConfig, sub_matches: &clap::ArgMatches) -> Result<()> {
    if let Some(max_generations) = sub_matches.get_one::<u32>("MAX_GENERATIONS") {
        config.history.max_generations = Some(*max_generations);
    }
    Ok(())
}

fn parse_options_restore(_options: &mut EPKGConfig, _sub_matches: &clap::ArgMatches) -> Result<()> {
    Ok(())
}

fn parse_options_update(_options: &mut EPKGConfig, _sub_matches: &clap::ArgMatches) -> Result<()> {
    Ok(())
}

fn parse_options_repo(_options: &mut EPKGConfig, _sub_matches: &clap::ArgMatches) -> Result<()> {
    Ok(())
}

fn parse_options_hash(_options: &mut EPKGConfig, _sub_matches: &clap::ArgMatches) -> Result<()> {
    Ok(())
}

fn parse_options_build(_options: &mut EPKGConfig, _sub_matches: &clap::ArgMatches) -> Result<()> {
    Ok(())
}

fn parse_options_unpack(_config: &mut EPKGConfig, _sub_matches: &clap::ArgMatches) -> Result<()> {
    // Placeholder for unpack specific options
    Ok(())
}

// Command handlers
impl PackageManager {

    fn command_init(&mut self, _sub_matches: &clap::ArgMatches) -> Result<()> {
        self.init()
    }

    fn command_env(&mut self, sub_matches: &clap::ArgMatches) -> Result<()> {
        match sub_matches.subcommand() {
            Some(("list", _)) => self.list_environments(),
            Some(("create", sub_matches)) => {
                if let Some(name) = sub_matches.get_one::<String>("ENV_NAME") {
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

    fn command_list(&mut self, sub_matches: &clap::ArgMatches) -> Result<()> {
        privdrop_on_suid();

        // Determine scope - only one should be true, with installed as default
        let scope = if sub_matches.get_flag("all") {
            ListScope::All
        } else if sub_matches.get_flag("available") {
            ListScope::Available
        } else if sub_matches.get_flag("upgradable") {
            ListScope::Upgradable
        } else {
            ListScope::Installed // default
        };

        let pattern = sub_matches.get_one::<String>("GLOB_PATTERN")
            .map(|s| s.as_str())
            .unwrap_or("");

        self.revise_channel_metadata()?;
        self.list_packages_with_scope(scope, pattern)?;
        Ok(())
    }

    fn command_install(&mut self, sub_matches: &clap::ArgMatches) -> Result<()> {
        if sub_matches.get_flag("local") {
            if let (Some(fs_dir), Some(symlink_dir)) = (sub_matches.get_one::<String>("fs"), sub_matches.get_one::<String>("symlink")) {
                let ebin = sub_matches.get_flag("ebin");
                let fs_dir = PathBuf::from(fs_dir);
                let symlink_dir = PathBuf::from(symlink_dir);
                // First phase: Link the package
                self.link_package(&fs_dir, &symlink_dir)?;
                // Second phase: Expose the package if ebin flag is set
                if ebin {
                    self.expose_package(&fs_dir, &symlink_dir)?;
                }
            }
        } else if let Some(package_specs) = sub_matches.get_many::<String>("PACKAGE_SPEC") {
            self.fork_on_suid()?;
            self.revise_channel_metadata()?;
            let packages_vec: Vec<String> = package_specs.cloned().collect();
            self.install_packages(packages_vec)?;
        }
        Ok(())
    }

    fn command_upgrade(&mut self, sub_matches: &clap::ArgMatches) -> Result<()> {
        if let Some(package_specs) = sub_matches.get_many::<String>("PACKAGE_SPEC") {
            self.fork_on_suid()?;
            self.revise_channel_metadata()?;
            self.upgrade_packages(package_specs)?;
        }
        Ok(())
    }

    fn command_remove(&mut self, sub_matches: &clap::ArgMatches) -> Result<()> {
        if let Some(package_specs) = sub_matches.get_many::<String>("PACKAGE_SPEC") {
            self.fork_on_suid()?;
            self.revise_channel_metadata()?;
            let packages_vec: Vec<String> = package_specs.cloned().collect();
            self.remove_packages(packages_vec)?;
        }
        Ok(())
    }

    fn command_history(&mut self, _sub_matches: &clap::ArgMatches) -> Result<()> {
        self.print_history()
    }

    fn command_restore(&mut self, sub_matches: &clap::ArgMatches) -> Result<()> {
        if let Some(rollback_id) = sub_matches.get_one::<i32>("GEN_ID") {
            self.rollback_history(*rollback_id)?;
        }
        Ok(())
    }

    fn command_update(&mut self) -> Result<()> {
        self.fork_on_suid()?;
        self.revise_channel_metadata()
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

            let epkg_manager_path = get_epkg_manager_path()
                .map_err(|_| eyre::eyre!("epkg-manager not installed"))?;
            let build_script = epkg_manager_path.join("build/scripts/generic-build.sh");
            if !build_script.exists() {
                return Err(eyre::eyre!("Build script not found"));
            }

            let mut command = std::process::Command::new("bash");
            command.arg(build_script);
            command.arg(package_yaml);
            command.status()?;
        }
        Ok(())
    }

    fn command_unpack(&self, sub_matches: &clap::ArgMatches) -> Result<()> {
        if let Some(package_files_iter) = sub_matches.get_many::<String>("PACKAGE_FILE") {
            let files: Vec<String> = package_files_iter.cloned().collect();

            // PACKAGE_FILE is required by clap, so files should not be empty if this block is reached.
            privdrop_on_suid(); // Drop privileges if running as SUID

            match crate::store::unpack_packages(files) {
                Ok(pkgidlines) => {
                    if pkgidlines.is_empty() {
                        println!("No packages were unpacked by the store. This might indicate issues with the provided files or empty input.");
                    } else {
                        for pkgidline in pkgidlines {
                            // pkgidline is "pkgid__ca_hash__pkgname__version"
                            // The actual store directory name is "ca_hash__pkgname__version"
                            let parts: Vec<&str> = pkgidline.splitn(2, "__").collect();
                            if parts.len() == 2 {
                                let store_dir_name = parts[1];
                                let final_path = dirs().epkg_store.join(store_dir_name);
                                println!("{}", final_path.display());
                            } else {
                                eprintln!("Warning: Could not parse pkgidline '{}' to determine store path.", pkgidline);
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Error during store unpacking process: {}", e);
                    // Consider returning the error to propagate it, e.g.:
                    // return Err(e).wrap_err("Failed in unpack command");
                }
            }
        }
        // If execution reaches here, it implies sub_matches.get_many was None,
        // but clap should have handled the 'required' argument before calling this command.
        // If not, an explicit error or log for "No package files specified" could be added.
        Ok(())
    }

}
