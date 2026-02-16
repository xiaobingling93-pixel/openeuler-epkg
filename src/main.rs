mod dirs;
mod models;
mod io;
mod lfs;
mod download;
mod depends;
mod resolve;
mod solver_tests;
mod parse_requires;
mod rpm_requires;
mod conda_requires;
mod parse_provides;
mod provides;
mod install;
mod upgrade;
mod remove;
mod hash;
mod ipc;
mod store;
mod package_cache;
mod link;
mod expose;
mod xdesktop;
mod transaction;
mod world;
mod utils;
mod mtree;
mod posix;
mod history;
mod environment;
mod deinit;
mod init;
mod path;
mod repo;
mod mmio;
mod mirror;
mod location;
mod package;
mod packages_stream;
mod index_html;
mod deb_repo;
mod deb_pkg;
mod deb_sources;
mod rpm_repo;
mod rpm_pkg;
mod rpm_sources;
mod apk_repo;
mod apk_pkg;
mod arch_repo;
mod arch_pkg;
mod aur;
mod conda_repo;
mod conda_pkg;
mod conda_link;
mod shebang;
mod version_constraint;
mod epkg;
mod parse_version;
mod plan;
mod version_compare;
mod scriptlets;
mod hooks;
mod userdb;
mod deb_triggers;
mod rpm_triggers;
mod lua;
mod risks;
mod run;
mod applets;
mod info;
mod list;
mod search;
mod gc;
mod service;

#[cfg(debug_assertions)]
mod rpm_verify;

use std::env;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::io::Write;
use std::panic;

use time::OffsetDateTime;
use time::macros::format_description;
use crate::models::*;
use crate::dirs::*;
use crate::environment::*;
use crate::io::edit_environment_config;
use crate::io::load_installed_packages;
use crate::io::read_yaml_file;
use crate::path::update_path;
use crate::repo::sync_channel_metadata;
use crate::list::list_packages_with_scope;
use crate::install::install_packages;
use crate::upgrade::upgrade_packages;
use crate::remove::remove_packages;
use crate::history::{print_history, rollback_history};
use crate::init::{install_epkg, try_light_init, light_init, upgrade_epkg};
use crate::run::{command_run, command_busybox, RunOptions};
use color_eyre::Result;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use clap::{arg, Command};
use ctrlc;
use env_logger;
use log;
use list::ListScope;

#[cfg(not(test))]
fn main() -> Result<()> {
    color_eyre::config::HookBuilder::default()
        .display_env_section(false)                 // Don't show environment variables by default
        .display_location_section(true)             // Show file:line:column
        .theme(color_eyre::config::Theme::dark())   // Use dark theme for better contrast
        .install()?;
    setup_logging();
    setup_ctrlc();

    let argv: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    log::debug!("argv[{}]: {:?}", argv.len(), argv);

    // Init CONFIG (and CLAP_MATCHES) for either applet or epkg main invocation
    let invoked_as_applet = crate::applets::is_invoked_as_applet();
    crate::models::init_config(invoked_as_applet)?;

    // Gracefully exit instead of panic on half piping
    // - epkg list --all | head
    // - epkg busybox cat long-file | head
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // If invoked as an applet (via symlink/hardlink), handle it and return early
    if invoked_as_applet {
        match crate::applets::handle_applet_invocation()? {
            Some(_) => return Ok(()), // Handled as applet, exit
            None => {} // Should not happen after is_invoked_as_applet()
        }
    }

    log::trace!("Application starting with config: {:#?}", &*config());

    try_light_init()?;

    let matches = clap_matches();
    match matches.subcommand() {
        Some(("self",       sub_matches))  =>  command_self(sub_matches)?,
        Some(("env",        sub_matches))  =>  command_env(sub_matches)?,
        Some(("list",       sub_matches))  =>  command_list(sub_matches)?,
        Some(("info",       sub_matches))  =>  command_info(sub_matches)?,
        Some(("install",    sub_matches))  =>  command_install(sub_matches)?,
        Some(("upgrade",    sub_matches))  =>  command_upgrade(sub_matches)?,
        Some(("remove",     sub_matches))  =>  command_remove(sub_matches)?,
        Some(("history",    sub_matches))  =>  command_history(sub_matches)?,
        Some(("restore",    sub_matches))  =>  command_restore(sub_matches)?,
        Some(("update",     sub_matches))  =>  command_update(sub_matches)?,
        Some(("repo",       sub_matches))  =>  command_repo(sub_matches)?,
        Some(("hash",       sub_matches))  =>  command_hash(sub_matches)?,
        Some(("build",      sub_matches))  =>  command_build(sub_matches)?,
        Some(("unpack",     sub_matches))  =>  command_unpack(&sub_matches)?,
        Some(("convert",    sub_matches))  =>  command_convert(&sub_matches)?,
        Some(("run",        sub_matches))  =>  command_run(sub_matches)?,
        Some(("busybox",    sub_matches))  =>  command_busybox(sub_matches)?,
        Some(("search",     sub_matches))  =>  command_search(sub_matches)?,
        Some(("gc",         sub_matches))  =>  command_gc(sub_matches)?,
        Some(("service",    sub_matches))  =>  command_service(sub_matches)?,
        _ => {} // No subcommand or unknown subcommand
    }

    Ok(())
}

#[cfg(not(test))]
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

#[cfg(not(test))]
fn setup_ctrlc() {
    // Enable backtrace collection if RUST_BACKTRACE is set
    if !std::env::var("RUST_BACKTRACE").is_ok() {
        return;
    }

    // Set up Ctrl-C handler with better debugging info
    ctrlc::set_handler(move || {
        println!("\nReceived Ctrl-C! Cancelling downloads and collecting thread backtraces...");

        // Cancel all pending downloads first
        crate::download::cancel_downloads();

        // Print current command and process info
        let args: Vec<String> = std::env::args().collect();
        println!("Command: {}", args.join(" "));
        println!("Process ID: {}", std::process::id());
        println!("Current directory: {:?}", std::env::current_dir().unwrap_or_default());
        println!("Elapsed time: {:?}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default());

        // Dump mirror performance stats if available
        if let Ok(mirrors_guard) = crate::mirror::MIRRORS.try_lock() {
            println!("\nMirror performance statistics:");
            crate::mirror::dump_mirror_performance_stats(&mirrors_guard, true);
        } else {
            println!("\nCould not access mirror statistics (lock contention)");
        }

        crate::download::DOWNLOAD_MANAGER.dump_all_tasks();

        // Get information about all threads
        print_all_thread_backtraces();

        // Show some system info that might be helpful
        println!("\nEnvironment variables of interest:");
        for (key, value) in std::env::vars() {
            if key.starts_with("RUST_") || key.starts_with("EPKG_") || key.starts_with("CARGO_") {
                println!("  {}={}", key, value);
            }
        }

        // Exit gracefully
        println!("\nExiting due to Ctrl-C...");
        std::process::exit(130); // Standard exit code for SIGINT
    }).expect("Failed to set Ctrl-C handler");
}

fn parse_link_type(link_str: &str) -> Result<LinkType> {
    match link_str {
        "hardlink"  => Ok(LinkType::Hardlink),
        "symlink"   => Ok(LinkType::Symlink),
        "reflink"   => Ok(LinkType::Reflink),
        "move"      => Ok(LinkType::Move),
        "runpath"   => Ok(LinkType::Runpath),
        _ => Err(eyre::eyre!("Invalid link type: '{}'. Valid options are: hardlink, symlink, reflink, move, runpath", link_str)),
    }
}

fn print_all_thread_backtraces() {
    // DON'T capture the useless signal handler backtrace - it shows nothing useful
    // Instead, get the actual kernel stack traces of all threads

    // Try to get detailed runtime information
    println!("\n=== Runtime Information ===");

    // Show memory usage and thread info
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        println!("Process status:");
        for line in status.lines() {
            if line.starts_with("VmRSS:") || line.starts_with("VmSize:") ||
               line.starts_with("Threads:") || line.starts_with("State:") ||
               line.starts_with("PPid:") || line.starts_with("TracerPid:") {
                println!("  {}", line);
            }
        }
    }

    // Show currently open files
    if let Ok(entries) = std::fs::read_dir("/proc/self/fd") {
        let mut fds = Vec::new();
        for entry in entries {
            if let Ok(entry) = entry {
                let fd_num = entry.file_name();
                if let Ok(link) = std::fs::read_link(entry.path()) {
                    fds.push(format!("{}: {}", fd_num.to_string_lossy(), link.display()));
                }
            }
        }
        println!("Open file descriptors ({}):", fds.len());
        for fd in fds.iter().take(20) { // Show first 20 FDs
            println!("  {}", fd);
        }
        if fds.len() > 20 {
            println!("  ... and {} more", fds.len() - 20);
        }
    }

    // Show network connections if available
    if let Ok(tcp) = std::fs::read_to_string("/proc/self/net/tcp") {
        let mut lines = tcp.lines();
        if let Some(header) = lines.next() {
            let connections: Vec<_> = lines.take(10).collect();
            if !connections.is_empty() {
                println!("Active TCP connections:");
                println!("  {}", header); // Show the header explaining columns
                for conn in connections {
                    println!("  {}", conn);
                }
            }
        }
    }

    try_print_backtrace();

    println!("\n=== Debugging Tips ===");
    println!("To get more detailed debugging information:");
    println!("1. Attach gdb: gdb -p $(pidof epkg)");
    println!("2. Use strace: strace -p $(pidof epkg)");
    println!("3. Run with: RUST_LOG=debug RUST_BACKTRACE=full {}", std::env::args().collect::<Vec<_>>().join(" "));
    println!("4. Check system logs: journalctl --since '1 minute ago' --grep epkg");
}

fn try_print_backtrace() {
    use std::process::Command;

    // Only run in debug builds
    if !cfg!(debug_assertions) {
        return;
    }

    let pid = std::process::id();
    println!("=== Attempting to get userspace backtraces ===");

    // Try kernel stack trace first - this is always safe and never hangs
    if let Ok(stack) = std::fs::read_to_string(format!("/proc/{}/stack", pid)) {
        if !stack.trim().is_empty() {
            println!("Kernel stack trace:");
            println!("{}", stack);
        }
    }

    // Try lightweight tools that don't hang (eu-stack, pstack)
    // eu-stack requires -p flag for PID
    if let Ok(output) = Command::new("eu-stack")
        .args(["-p", &pid.to_string()])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if !stdout.trim().is_empty() {
                println!("eu-stack output:");
                println!("{}", stdout);
                return; // Successfully got stack trace
            }
        }
    }

    // pstack takes PID directly
    if let Ok(output) = Command::new("pstack")
        .arg(pid.to_string())
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if !stdout.trim().is_empty() {
                println!("pstack output:");
                println!("{}", stdout);
                return; // Successfully got stack trace
            }
        }
    }
}

fn add_global_args_and_help(cmd: Command) -> Command {
    cmd.author("Wu Fengguang <wfg@mail.ustc.edu.cn>")
        .author("Duan Pengjie <pengjieduan@gmail.com>")
        .author("Yingjiahui <ying_register@163.com>")
        .about("The EPKG package manager")
        .version(env!("EPKG_VERSION_INFO"))
        .arg_required_else_help(true) // This will show help if no args are provided
        .arg(arg!(--config <FILE> "Configuration file to use").hide(true).global(true))
        .arg(arg!(-e --env <ENV_NAME> "Select the environment by name or owner/name").hide(true).global(true))
        .arg(arg!(-r --root <DIR> "Select the environment by root dir").hide(true).global(true))
        .arg(arg!(--arch <ARCH> "Select the CPU architecture").default_value(std::env::consts::ARCH).hide(true).global(true))
        .arg(arg!(--"dry-run" "Simulated run without changing the system").hide(true).global(true))
        .arg(arg!(--"download-only" "Download packages without installing").hide(true).global(true))
        .arg(arg!(-q --quiet "Suppress output").hide(true).global(true))
        .arg(arg!(-v --verbose "Verbose operation, show debug messages").hide(true).global(true))
        .arg(arg!(-y --"assume-yes" "Automatically answer yes to all prompts").hide(true).global(true))
        .arg(arg!(--"assume-no" "Automatically answer no to all prompts").hide(true).global(true))
        .arg(arg!(--"ignore-missing" "Ignore missing packages").hide(true).global(true))
        .arg(arg!(--"metadata-expire" <SECONDS> "Metadata expiration time in seconds (0=never, -1=always)").value_parser(clap::value_parser!(i32)).hide(true).global(true))
        .arg(arg!(--proxy <URL> "HTTP proxy URL (e.g., http://proxy.example.com:8080)").hide(true).global(true))
        .arg(arg!(--"retry" <NUMBER> "Number of retries for download tasks").value_parser(clap::value_parser!(usize)).hide(true).global(true))
        .arg(arg!(--"parallel-download" <NUMBER> "Number of parallel download threads").value_parser(clap::value_parser!(usize)).hide(true).global(true))
        .arg(arg!(--"parallel-processing" <BOOL> "Enable parallel processing for metadata updates (true/false)").value_parser(clap::value_parser!(bool)).hide(true).global(true))
        .override_usage("epkg [OPTIONS] <COMMAND>")
        .help_template(
            "{about}\n\n\
USAGE: {usage}\n\n\
COMMANDS:\n{subcommands}\n\n\
OPTIONS:
      --config <FILE>               Configuration file to use
  -e, --env <ENV_NAME>              Select the environment by name or owner/name
  -r, --root <DIR>                  Select the environment by root dir
      --arch <ARCH>                 Select the CPU architecture
      --dry-run                     Simulated run without changing the system
      --download-only               Download packages without installing
  -q, --quiet                       Suppress output
  -v, --verbose                     Verbose operation, show debug messages
  -y, --assume-yes                  Automatically answer yes to all prompts
      --assume-no                   Automatically answer no to all prompts
  -m, --ignore-missing              Ignore missing packages
      --metadata-expire <SECONDS>   Metadata expiration time in seconds (0=never, -1=always)
      --proxy <URL>                 HTTP proxy URL (e.g., http://proxy.example.com:8080)
      --retry <NUMBER>              Number of retries for download tasks
      --parallel-download <NUMBER>  Number of parallel download threads
      --parallel-processing <BOOL>  Enable parallel processing for metadata updates (true/false) [possible values: true, false]
  -h, --help                        Print help
  -V, --version                     Print version")
}

fn add_self_subcommand(cmd: Command) -> Command {
    cmd.subcommand(
        Command::new("self")
            .about("Manage epkg installation")
            .arg_required_else_help(true)
            .subcommand(
                Command::new("install")
                    .about("Install epkg")
                    .arg(arg!(--commit <COMMIT>).help(format!("Source commit of epkg to install [default: {}]", DEFAULT_COMMIT)))
                    .arg(arg!(-c --channel <CHANNEL> "Set the channel for the environment, e.g. debian or debian:12"))
                    .arg(arg!(   --repo <REPO> "Add one or more repos separated by space, e.g. ceph postgresql").num_args(1..))
                    .arg(
                        arg!(--store <STORE> "Store mode: 'shared' (reused by all users), 'private' (current user only), or 'auto' (shared if installed by root)")
                            .default_value("auto")
                            .value_parser(["shared", "private", "auto"]),
                    )
            )
            .subcommand(
                Command::new("upgrade")
                    .about("Upgrade epkg installation")
            )
            .subcommand(
                Command::new("remove")
                    .about("Remove epkg installation")
                    .arg(
                        arg!(--scope <SCOPE> "Scope of removal: 'personal' (current user only) or 'global' (all users)")
                            .default_value("personal")
                            .value_parser(["personal", "global"]),
                    )
            )
    )
}

fn add_env_subcommand(cmd: Command) -> Command {
    cmd.subcommand(
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
                    .arg(arg!([ENV_NAME] "Environment name or owner/name"))
                    .arg(arg!(-c --channel <CHANNEL> "Set the channel for the environment, e.g. debian or debian:12"))
                    .arg(arg!(   --repo <REPO> "Add one or more repos separated by space, e.g. ceph postgresql").num_args(1..))
                    .arg(arg!(-P --public "Usable by all users in the machine"))
                    .arg(arg!(-i --import <FILE> "Import from config file"))
                    .arg(arg!(--link <LINK> "Link type: hardlink, symlink, move, or runpath").value_parser(["hardlink", "symlink", "move", "runpath"]))
            )
            .subcommand(
                Command::new("remove")
                    .about("Remove an environment")
                    .arg(arg!([ENV_NAME] "Environment name or owner/name"))
            )
            .subcommand(
                Command::new("register")
                    .about("Register an environment")
                    .arg(arg!([ENV_NAME] "Environment name or owner/name"))
                    .arg(arg!(--priority <PRIORITY> "Set the priority for the environment").value_parser(clap::value_parser!(i32)))
            )
            .subcommand(
                Command::new("unregister")
                    .about("Unregister an environment")
                    .arg(arg!([ENV_NAME] "Environment name or owner/name"))
            )
            .subcommand(
                Command::new("activate")
                    .about("Activate an environment")
                    .arg(arg!([ENV_NAME] "Environment name or owner/name"))
                    .arg(arg!(   --pure "Create a pure environment"))
                    .arg(arg!(-s --stack "Stack this environment on top of the current one"))
            )
            .subcommand(
                Command::new("deactivate")
                    .about("Deactivate the current environment")
            )
            .subcommand(
                Command::new("export")
                    .about("Export environment configuration")
                    .arg(arg!([ENV_NAME] "Environment name or owner/name"))
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
}

fn add_package_operation_subcommands(cmd: Command) -> Command {
    cmd.subcommand(
        Command::new("list")
            .about("List packages")
            .arg(arg!(--all "List all packages"))
            .arg(arg!(--installed "List installed packages"))
            .arg(arg!(--available "List available packages"))
            .arg(arg!(--upgradable "List upgradable packages"))
            .arg(arg!([GLOB_PATTERN] "Package name filtering"))
    )
    .subcommand(
        Command::new("info")
            .about("Show package information")
            .arg(arg!(--files "Show filelist for installed packages"))
            .arg(arg!(--scripts "Show install scriptlets for installed packages"))
            .arg(arg!(--"store-path" "Show store path for installed packages"))
            .arg(arg!(<PACKAGE_SPEC> ... "Package specifications to show info for").required(true))
            .arg_required_else_help(true) // This will show help if no args are provided
    )
    .subcommand(
        Command::new("install")
            .about("Install packages")
            .arg(arg!(--"install-suggests" "Consider suggested packages as a dependency for installing"))
            .arg(arg!(--"no-install-recommends" "Do not consider recommended packages as a dependency for installing"))
            .arg(arg!(--"no-install-essentials" "Do not automatically install essential packages"))
            .arg(arg!(--"no-install" <PACKAGES> "Packages to exclude from installation (comma-separated list, use -pkgname to remove from list)").value_delimiter(','))
            .arg(arg!(--"prefer-low-version" "Prefer lower/older versions when multiple candidates are available"))
            .arg(arg!([PACKAGE_SPEC] ... "Package specifications to install (can be package names, local .rpm/.deb files, or URLs to package files)"))
    )
    .subcommand(
        Command::new("upgrade")
            .about("Upgrade packages")
            .arg(arg!(--full "Full upgrade: upgrade all packages, not just those in world.json"))
            .arg(arg!([PACKAGE_SPEC] ... "Package specifications to upgrade"))
    )
    .subcommand(
        Command::new("remove")
            .about("Remove packages")
            .arg(arg!(<PACKAGE_SPEC> ... "Package specifications to remove"))
    )
}

fn add_history_and_utility_subcommands(cmd: Command) -> Command {
    cmd.subcommand(
            Command::new("history")
                .about("Show environment history")
                .arg(arg!([MAX_GENERATIONS] "Maximum number of generations to show").value_parser(clap::value_parser!(u32)))
        )
        .subcommand(
            Command::new("restore")
                .about("Restore environment to a specific generation")
                .arg(arg!(<GEN_ID> "Generation ID to restore to (negative number for relative rollback)").value_parser(clap::value_parser!(i32)).allow_negative_numbers(true))
        )
        .subcommand(
            Command::new("update")
                .about("Update package metadata")
                .arg(arg!(--"need-files" "Download filelists (needed for file/path search)"))
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
                .about("Unpack package file(s) into a store directory")
                .arg(arg!(<PACKAGE_FILE> ... "Package files to unpack").required(true))
                .arg_required_else_help(true) // This will show help if no args are provided
        )
        .subcommand(
            Command::new("convert")
                .about("Convert rpm/deb/apk/... packages to epkg format")
                .arg(arg!(--"out-dir" <OUTPUT_DIR> "Output directory").default_value("."))
                .arg(arg!(--"origin-url" <ORIGIN_URL> "Where the package originated from").required(true))
                .arg(arg!(<PACKAGE_FILE>... "Package files to convert (RPM, DEB, APK, etc.)").required(true))
                .arg_required_else_help(true) // This will show help if no args are provided
        )
}

fn add_search_and_gc_subcommands(cmd: Command) -> Command {
    cmd.subcommand(
            Command::new("search")
                .about("Search for packages and files")
                .arg(arg!(-f --files "Search in file names"))
                .arg(arg!(-p --paths "Search in full paths"))
                .arg(arg!(-x --regexp "Pattern is regular expression, refer to https://docs.rs/regex/latest/regex/#syntax"))
                .arg(arg!(-i --"ignore-case" "Case-insensitive search"))
                .arg(arg!(<PATTERN> "Pattern to search for"))
        )
        .subcommand(
            Command::new("gc")
                .about("Garbage collection - clean up unused cache and store files")
                .arg(arg!(--"old-downloads" <DAYS> "Remove download files older than DAYS (0 = all files)")
                    .value_parser(clap::value_parser!(u64)))
        )
}



fn add_run_subcommand(cmd: Command) -> Command {
    cmd.subcommand(
            Command::new("run")
                .about("Run command in environment namespace")
                .long_about(r#"Run a command in an isolated environment namespace.

ENVIRONMENT SELECTION (in order of precedence):
1. Explicit selection via command line flags:
   • -e, --env <ENV_NAME>   Select environment by name (e.g., "myenv" or "owner/myenv")
   • -r, --root <DIR>       Select the environment by root dir

2. If no command line flags are provided:
   • EPKG_ACTIVE_ENV environment variable (if set)
   • /etc/epkg/env.yaml configuration file (if exists)

3. Auto-detection (only for 'epkg run' when no environment selected above):
   • Path detection: Command is treated as a path if it contains '/' or exists as a file
   • If command is a path: Search upward for .eenv directory starting from command's parent
     - .eenv with valid config → Use resolved environment name
     - .eenv without config → Use .eenv directory path as environment
     - No .eenv found → Use MAIN_ENV (default environment)
   • If command is not a path and no .eenv found: Search registered environments for command
     - Command found → Use environment containing the command
     - Command not found → Use MAIN_ENV (default environment)

Use '--' to separate epkg options from command arguments when needed.

EXAMPLES:
  # Run command with auto-detected environment
  epkg run ./script.sh              # Searches for .eenv in script's directory
  epkg run python                   # Searches registered environments for 'python'
  epkg run /usr/local/bin/myapp     # Searches for .eenv in /usr/local/bin

  # Run command with explicit environment
  epkg run -e myenv python
  epkg run -r /path/to/env bash

  # Run with additional mounts and user
  epkg run -M /data,/config -u appuser node server.js

  # Separate epkg options from command arguments
  epkg run -- jq --jq-option        # Use '--' when command arguments start with '-'
"#)
                .arg(arg!(-M --mount <DIRS> "Comma-separated list of additional directories to mount"))
                .arg(arg!(-u --user <USER> "Run as specified user (username or UID)"))
                .arg(arg!(--timeout <SECONDS> "Timeout in seconds (0 = no timeout)").value_parser(clap::value_parser!(String)))
                .arg(arg!(<command> "Command to execute"))
                .arg(arg!([args] ... "Arguments to pass to the command (use '--' to separate from epkg options)"))
                .allow_hyphen_values(true)
                .trailing_var_arg(true)
        )
}

fn add_busybox_subcommand(cmd: Command) -> Command {
    cmd.subcommand(
            Command::new("busybox")
                .about("Run built-in command implementations")
                .arg_required_else_help(true)
                .allow_external_subcommands(true)
        )
}

fn add_service_subcommand(cmd: Command) -> Command {
    cmd.subcommand(
            Command::new("service")
                .about("Service management - start/stop/restart/status/reload services")
                .arg_required_else_help(true)
                .subcommand(
                    Command::new("start")
                        .about("Start a service")
                        .arg(arg!(<SERVICE_NAME> "Name of the service to start (without .service extension)"))
                )
                .subcommand(
                    Command::new("stop")
                        .about("Stop a service")
                        .arg(arg!(<SERVICE_NAME> "Name of the service to stop (without .service extension)"))
                )
                .subcommand(
                    Command::new("status")
                        .about("Show service status")
                        .arg(arg!(--all "Show status for all services across all environments"))
                        .arg(arg!([SERVICE_NAME] "Name of the service to check (without .service extension)"))
                )
                .subcommand(
                    Command::new("reload")
                        .about("Reload a service")
                        .arg(arg!(<SERVICE_NAME> "Name of the service to reload (without .service extension)"))
                )
                .subcommand(
                    Command::new("restart")
                        .about("Restart a service")
                        .arg(arg!(<SERVICE_NAME> "Name of the service to restart (without .service extension)"))
                )
        )
}


fn build_epkg_command() -> Command {
    let cmd = Command::new("epkg");
    let cmd = add_global_args_and_help(cmd);
    let cmd = add_self_subcommand(cmd);
    let cmd = add_env_subcommand(cmd);
    let cmd = add_package_operation_subcommands(cmd);
    let cmd = add_history_and_utility_subcommands(cmd);
    let cmd = add_run_subcommand(cmd);
    let cmd = add_busybox_subcommand(cmd);
    let cmd = add_search_and_gc_subcommands(cmd);
    let cmd = add_service_subcommand(cmd);
    cmd
}

/// Parse command line from environment args (normal epkg invocation).
pub fn parse_cmdline() -> clap::ArgMatches {
    let args: Vec<String> = env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    build_epkg_command().get_matches_from(args)
}

/// Parse command line from given args (used when running as applet so main parser is not run on applet argv).
pub fn parse_cmdline_from(args: Vec<String>) -> clap::ArgMatches {
    build_epkg_command().get_matches_from(args)
}

fn load_config_from_matches(matches: &clap::ArgMatches) -> Result<EPKGConfig> {
    let config = matches.get_one::<String>("config").map_or_else(
        || {
            // Try default config file location
            let default_config_path = PathBuf::from(dirs::get_home()?).join(".epkg/config/options.yaml");
            if default_config_path.exists() {
                read_yaml_file(&default_config_path)
            } else {
                // Using "{}" ensures that serde processes an empty map, allowing field-level
                // #[serde(default = "...")] attributes to be applied.
                // An empty string "" typically parses to Yaml::Null, which doesn't trigger these defaults for struct fields.
                Ok(serde_yaml::from_str("{}")
                    .unwrap_or_else(|e| panic!("Failed to load default config from empty map: {:?}", e)))
            }
        },
        |s| read_yaml_file(Path::new(s)),
    )?;
    Ok(config)
}

fn set_arch_and_validate(matches: &clap::ArgMatches, config: &mut EPKGConfig) -> Result<()> {
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
    Ok(())
}

fn set_common_flags(matches: &clap::ArgMatches, config: &mut EPKGConfig) {
    config.common.dry_run          = matches.get_flag("dry-run");
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
    if matches.contains_id("assume-no") {
        config.common.assume_no         = matches.get_flag("assume-no");
    }
    if matches.contains_id("ignore-missing") {
        config.common.ignore_missing    = matches.get_flag("ignore-missing");
    }
}

fn set_command_line_and_subcommand(matches: &clap::ArgMatches, config: &mut EPKGConfig) -> Result<()> {
    let args: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let command_line = if args.len() > 1 {
        // Remove program path and join the rest
        "epkg ".to_owned() + &args[1..].join(" ")
    } else {
        String::new()
    };
    config.command_line = command_line;
    config.subcommand = EpkgCommand::from(matches.subcommand_name().unwrap_or(""));
    if config.subcommand != EpkgCommand::SelfInstall {
        config.init.shared_store = utils::determine_shared_store()
            .wrap_err("Failed to determine shared_store mode")?;
    }
    Ok(())
}

fn set_metadata_expire_and_proxy(matches: &clap::ArgMatches, config: &mut EPKGConfig) {
    // Parse new common options
    if config.subcommand == EpkgCommand::Remove ||
        config.subcommand == EpkgCommand::Restore {
        config.common.metadata_expire = 0;  // no auto update
    } else if let Some(metadata_expire) = matches.get_one::<i32>("metadata-expire") {
        config.common.metadata_expire = *metadata_expire;
    }

    if let Some(proxy) = matches.get_one::<String>("proxy") {
        config.common.proxy = proxy.to_string();
    }
}

/// Setup parallel processing parameters based on command line arguments and system capabilities
fn setup_parallel_params(config: &mut EPKGConfig, matches: &clap::ArgMatches) {
    // Handle nr_retry parameter
    if let Some(nr_retry) = matches.get_one::<usize>("retry") {
        config.common.nr_retry = *nr_retry;
    }

    // Handle nr_parallel_download parameter
    if let Some(nr) = matches.get_one::<usize>("parallel-download") {
        // Ensure nr is at least 1
        config.common.nr_parallel_download = if *nr == 0 { 1 } else { *nr };
    }

    // Handle parallel_processing parameter
    if let Some(parallel_processing) = matches.get_one::<bool>("parallel-processing") {
        // User explicitly set parallel_processing
        config.common.parallel_processing = *parallel_processing;
    } else if config.common.nr_parallel_download <= 1 {
        // Auto-disable if nr_parallel_download <= 1, overriding the default
        config.common.parallel_processing = false;
    }
    // Otherwise, use the default value set by default_parallel_processing()
}

pub fn parse_options_common(matches: &clap::ArgMatches) -> Result<EPKGConfig> {
    let mut config = load_config_from_matches(matches)?;
    determine_environment_explicit(matches, &mut config);
    set_arch_and_validate(matches, &mut config)?;
    set_common_flags(matches, &mut config);
    set_command_line_and_subcommand(matches, &mut config)?;
    set_metadata_expire_and_proxy(matches, &mut config);
    setup_parallel_params(&mut config, matches);
    Ok(config)
}

/// Classification of ENV_NAME input
#[derive(Debug, PartialEq)]
enum EnvNameType {
    /// Simple environment name (e.g., "myenv")
    Name,
    /// Owner/name format for public environments (e.g., "owner/env")
    OwnerName,
    /// Path-like environment root (e.g., "/path/to/env", "./env", "path/with/slashes/", ".")
    Path,
}

/// Classify an ENV_NAME string into one of the categories.
///
/// ENV_NAME can be one of three forms:
/// 1. Simple name (e.g., "myenv") - resolved to user's environment directory
/// 2. Owner/name (e.g., "owner/env") - resolved to public environment directory
/// 3. Path-like input (e.g., "/path/to/env", "./env", "../env", "path/with/slashes/", ".")
///    - Leading "/", "./", "../", or trailing "/" indicate a path
///    - Multiple slashes also indicate a path
///    - Otherwise treated as owner/name if exactly one slash, else simple name
fn classify_env_name(env_name: &str) -> EnvNameType {
    // Check for path-like indicators
    if env_name.starts_with('/') || env_name.starts_with("./") || env_name.starts_with("../") || env_name.ends_with('/') {
        return EnvNameType::Path;
    }

    // Check for slash
    if let Some(slash_pos) = env_name.find('/') {
        // Check if there's exactly one slash and no leading/trailing slash
        // (already handled by starts_with/ends_with above)
        // If multiple slashes, treat as path
        if env_name[slash_pos + 1..].contains('/') {
            return EnvNameType::Path;
        }
        // Single slash, not at edges -> owner/name format
        return EnvNameType::OwnerName;
    }

    // No special indicators
    EnvNameType::Name
}

/// Validate an environment name.
///
/// Ensures the name is a known form (Name or OwnerName) and doesn't contain
/// invalid characters when used as a file name.
/// Returns Ok(()) if valid, Err otherwise.
fn validate_env_name(env_name: &str) -> Result<()> {
    let classification = classify_env_name(env_name);
    match classification {
        EnvNameType::Path => {
            return Err(eyre::eyre!(
                "Environment name '{}' looks like a path. Use '-r {}' for root dir selection.",
                env_name, env_name
            ));
        }
        EnvNameType::Name | EnvNameType::OwnerName => {}
    }

    // Check for invalid characters in filename
    // Allow alphanumeric, underscore, hyphen, dot, and slash (only one slash for owner/name)
    // Disallow characters that are problematic in filenames: whitespace, "..", \0, / (except one), :, *, ?, ", <, >, |
    // Also disallow leading/trailing spaces
    if env_name.is_empty() {
        return Err(eyre::eyre!("Environment name cannot be empty"));
    }
    if env_name.contains('\0') {
        return Err(eyre::eyre!("Environment name cannot contain null character"));
    }
    if env_name.contains("..") {
        return Err(eyre::eyre!("Environment name cannot contain .."));
    }

    // Check for invalid characters (excluding slash which is allowed for owner/name)
    // We'll allow slash only as a separator between owner and name
    let invalid_chars = [' ', '\t', '\\', ':', '*', '?', '"', '<', '>', '|'];
    if let Some(ch) = env_name.chars().find(|c| invalid_chars.contains(c)) {
        return Err(eyre::eyre!("Environment name contains invalid character '{}'", ch));
    }

    // Additional validation for owner/name format
    if classification == EnvNameType::OwnerName {
        let parts: Vec<&str> = env_name.split('/').collect();
        if parts.len() != 2 {
            return Err(eyre::eyre!("Owner/name format must be exactly 'owner/name'"));
        }
        let owner = parts[0];
        let name = parts[1];
        if owner.is_empty() || name.is_empty() {
            return Err(eyre::eyre!("Owner and name parts cannot be empty"));
        }
        // Validate each part doesn't contain additional slashes (already ensured)
    }

    Ok(())
}

/// Generate an environment name from a dir by replacing '/' with '__'.
/// Auto-generated names start with '__' to distinguish them from user-provided names.
fn env_name_from_path(dir: &str) -> String {
    let trimmed = dir.trim_matches('/');
    if trimmed.is_empty() {
        return "root".to_string();
    }
    let with_underscores = trimmed.replace('/', "__");
    // Ensure name starts with '__' to mark as auto-generated
    if with_underscores.starts_with("__") {
        with_underscores
    } else {
        format!("__{}", with_underscores)
    }
}

/// Resolve a filesystem dir to an environment's canonical name.
///
/// This function expects a filesystem dir to an environment root directory
/// (containing etc/epkg/env.yaml). It loads the environment configuration file
/// and extracts the env_config.name field.
///
/// Returns error if the dir does not exist or does not contain a valid environment configuration.
pub fn resolve_env_root(env_root: &str) -> Result<String> {
    let env_root = std::fs::canonicalize(env_root).map_err(|e| {
        eyre::eyre!(
            "Environment root '{}' does not exist or cannot be accessed: {}",
            env_root,
            e
        )
    })?;

    if !env_root.is_dir() {
        return Err(eyre::eyre!(
            "Environment root '{}' is not a directory",
            env_root.display()
        ));
    }

    let config_path = env_root.join("etc/epkg/env.yaml");
    if !config_path.exists() {
        return Err(eyre::eyre!(
            "Environment not found at path: {}\n  (missing configuration file: {})",
            env_root.display(),
            config_path.display()
        ));
    }

    let env_config = io::read_yaml_file::<EnvConfig>(&config_path)?;
    Ok(env_config.name)
}

/// Process explicit environment selection flags (`-e` and `-r`).
///
/// This function checks for the presence of command-line flags that explicitly
/// select an environment:
/// - `-e, --env <ENV_NAME>`: sets `config.common.env_name` to the provided name
///   and marks the environment as explicitly selected (`env_explicit = true`)
/// - `-r, --root <PATH>`: sets `config.common.env_root` to the provided dir
///   and marks the environment as explicitly selected (`env_explicit = true`)
///
/// The `-e` flag is checked first; if both flags are present, `-e` takes precedence.
///
/// Returns `true` if either flag was present (environment explicitly selected),
/// `false` otherwise.
///
/// Note: This function does not consider environment variables, configuration files,
/// or auto-detection. Those are handled by `determine_environment_final()`.
fn determine_environment_explicit(matches: &clap::ArgMatches, config: &mut EPKGConfig) -> bool {
    if let Some(env_arg) = matches.get_one::<String>("env") {
        config.common.env_name = env_arg.to_string();
        config.common.env_explicit = true;
        return true;
    }

    if let Some(dir) = matches.get_one::<String>("root") {
        config.common.env_root = dir.to_string();
        config.common.env_explicit = true;
        return true;
    }

    false
}

/// Determine final environment after parsing all command-line options.
///
/// This function is called after all subcommand options have been parsed and determines
/// the final environment to use based on the following precedence order:
/// 1. Already set `env_name` (from `-e` flag or previous steps)
/// 2. Already set `env_root` (from `-r` flag) → resolved to environment name
/// 3. `EPKG_ACTIVE_ENV` environment variable
/// 4. `/etc/epkg/env.yaml` configuration file
/// 5. Auto-detection (only for `epkg run` subcommand when no environment selected above)
/// 6. `MAIN_ENV` as final fallback
///
/// # Situations (when auto-detection applies)
/// - User runs `epkg run <command>` without `-e` or `-r` flags
/// - No `EPKG_ACTIVE_ENV` environment variable is set
/// - No `/etc/epkg/env.yaml` configuration exists
/// - Environment hasn't been explicitly selected via other means
///
/// # Strategies for auto-detection
/// 1. **Path detection**: Determine if command is a filesystem path
///    - Command contains '/' → treat as path
///    - Command exists in current directory → treat as path
///    - Otherwise → treat as command name to search for
/// 2. **.eenv discovery**: If command is a path, search for `.eenv` directory starting from
///    command's parent directory up to filesystem root
///    - If `.eenv` found and contains valid config → use that environment
///    - If `.eenv` found but no valid config → use `.eenv` directory as environment path
/// 3. **Registered environment search**: If command is not a path and no `.eenv` found,
///    search all registered environments for the command
///    - Command found in registered environment → use that environment
///    - Command not found → fall back to MAIN_ENV
/// 4. **Fallback**: If command is a path but no `.eenv` found → use MAIN_ENV
///
/// # Examples
/// - `epkg run ./script.sh` → searches for `.eenv` in script's directory
/// - `epkg run python` → searches registered environments for 'python' command
/// - `epkg run /usr/local/bin/myapp` → searches for `.eenv` in `/usr/local/bin`
/// - `epkg run -e myenv python` → explicit environment, this function not invoked
fn determine_environment_final(config: &mut EPKGConfig) -> Result<()> {
    if !config.common.env_name.is_empty() {
        return Ok(());
    }
    if !config.common.env_root.is_empty() {
        config.common.env_name = resolve_env_root(&config.common.env_root)?;
        return Ok(());
    }

    if let Ok(active_env) = env::var("EPKG_ACTIVE_ENV") {
        config.common.env_name = active_env.trim_end_matches(':').to_string();
        config.common.env_explicit = true;
        return Ok(());
    } 

    // epkg may be run inside an env, try /etc/epkg/env.yaml
    let env_yaml_path = Path::new("/etc/epkg/env.yaml");
    if env_yaml_path.exists() {
        let env_config_data = read_yaml_file::<EnvConfig>(env_yaml_path)?;

        // ENV_CONFIG will be loaded later via LazyLock when first accessed
        // It will use config.common.env_name which we're setting here
        config.common.env_name = env_config_data.name;
        config.common.env_root = "/".to_string();
        config.common.env_explicit = true;
        return Ok(());
    }

    // Environment detection based on run command (for 'epkg run' subcommand)
    if !config.run.command.is_empty() && config.subcommand == EpkgCommand::Run {
        let command = config.run.command.clone();
        let (is_path, search_dir) = determine_command_path_info(&command);

        // Search for .eenv directory
        if let Some(dot_eenv) = find_nearest_dot_eenv(&search_dir) {
            set_env_name_by_path(&dot_eenv, config)?;
        } else if !is_path {
            // Command not a path, no .eenv found: search registered environments
            search_registered_envs(&command, config);
        }
        // If command is a path but no .eenv found, keep MAIN_ENV (already set)
    }

    // If environment still not determined, proceed with standard fallbacks
    if config.common.env_name.is_empty() {
        config.common.env_name = MAIN_ENV.to_string();
        config.common.env_explicit = false;
    }

    Ok(())
}

fn determine_command_path_info(command: &str) -> (bool, PathBuf) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if command.contains('/') {
        // Command contains slash: treat as path
        let cmd_path = Path::new(command);
        let parent = cmd_path.parent().unwrap_or(&cwd);
        (true, parent.to_path_buf())
    } else if Path::new(command).exists() {
        // Command exists as a file in current directory: treat as path
        (true, cwd.clone())
    } else {
        // Command not a path and not a file: we're clueless
        (false, cwd.clone())
    }
}

fn set_env_name_by_path(dot_eenv: &Path, options: &mut EPKGConfig) -> Result<()> {
    // Found a .eenv directory, try to resolve it to environment name
    let resolved_name = resolve_env_root(dot_eenv.to_string_lossy().as_ref())?;
    options.common.env_name = resolved_name;
    options.common.env_root = dot_eenv.to_string_lossy().to_string();
    options.common.env_explicit = true;
    Ok(())
}

fn search_registered_envs(command: &str, options: &mut EPKGConfig) {
    if let Ok(Some(env_name)) = find_command_in_registered_envs(command) {
        // Command found in a registered environment
        options.common.env_name = env_name;
        options.common.env_explicit = true;
    }
}

pub fn parse_options_subcommand(matches: &clap::ArgMatches, mut config: EPKGConfig) -> Result<EPKGConfig> {
    match matches.subcommand() {
        Some(("self",       sub_matches))  =>  parse_options_self(&mut config, sub_matches).expect("Failed to parse self options"),
        Some(("env",        sub_matches))  =>  parse_options_env(&mut config, sub_matches).expect("Failed to parse env options"),
        Some(("list",       sub_matches))  =>  parse_options_list(&mut config, sub_matches).expect("Failed to parse list options"),
        Some(("info",       sub_matches))  =>  parse_options_info(&mut config, sub_matches).expect("Failed to parse info options"),
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
        Some(("convert",    sub_matches))  =>  parse_options_convert(&mut config, sub_matches).expect("Failed to parse convert options"),
        Some(("run",        sub_matches))  =>  parse_options_run(&mut config, sub_matches).expect("Failed to parse run options"),
        Some(("search",     sub_matches))  =>  parse_options_search(&mut config, sub_matches).expect("Failed to parse search options"),
        Some(("service",    sub_matches))  =>  parse_options_service(&mut config, sub_matches).expect("Failed to parse service options"),
        _ => {} // No subcommand or unknown subcommand
    }
    determine_environment_final(&mut config)?;
    validate_env_name(&config.common.env_name)?;
    log::trace!("Configuration: {:#?}", config);
    Ok(config)
}

fn parse_options_self(config: &mut EPKGConfig, sub_matches: &clap::ArgMatches) -> Result<()> {
    use crate::utils;
    config.common.env_name = SELF_ENV.to_string();
    config.common.env_explicit = true;
    match sub_matches.subcommand() {
        Some(("install", sub_matches)) => {
            config.subcommand = EpkgCommand::SelfInstall;
            config.init.shared_store = sub_matches.get_one::<String>("store")
                .map(|s| match s.as_str() {
                    "shared" => true,
                    "private" => false,
                    "auto" => utils::is_running_as_root(),
                    _ => false
                })
                .unwrap_or_else(|| utils::is_running_as_root());

            if let Some(commit) = sub_matches.get_one::<String>("commit") {
                config.init.commit = commit.to_string();
            } else if config.init.commit.is_empty() {
                config.init.commit = models::default_commit();
                eprintln!("commit was configured to empty, using default commit: {}", config.init.commit);
            }

            // compose options for creating SELF_ENV
            if let Some(channel) = sub_matches.get_one::<String>("channel") {
                config.env.channel = Some(channel.to_string());
            }
            if let Some(repos) = sub_matches.get_many::<String>("repo") {
                config.env.repos = repos.map(|s| s.to_string()).collect();
            }
        }
        Some(("upgrade", _sub_matches)) => {
            config.subcommand = EpkgCommand::SelfUpgrade;
        }
        Some(("remove", _sub_matches)) => {
            config.subcommand = EpkgCommand::SelfRemove;
        }
        _ => {}
    }
    Ok(())
}


fn parse_options_env(config: &mut EPKGConfig, matches: &clap::ArgMatches) -> Result<()> {
    if let Some((subcommand_name, sub_matches)) = matches.subcommand() {
        // Common logic for env subcommands that have ENV_NAME argument
        if matches!(subcommand_name, "create" | "remove" | "register" | "unregister" | "activate" | "export") {
            if let Some(env_name) = sub_matches.get_one::<String>("ENV_NAME") {
                // ENV_NAME must be a name or owner/name, not a path (same semantic as -e)
                config.common.env_name = env_name.to_string();
                config.common.env_explicit = true;
            } else if !config.common.env_root.is_empty() &&
                config.common.env_name.is_empty() /* no '-e ENV_NAME' option either */ {
                config.common.env_name = env_name_from_path(&config.common.env_root);
                config.common.env_explicit = true;
            }
        }

        // Subcommand-specific logic
        match subcommand_name {
            "create" => {
                if let Some(channel) = sub_matches.get_one::<String>("channel") {
                    config.env.channel = Some(channel.to_string());
                }
                if let Some(repos) = sub_matches.get_many::<String>("repo") {
                    config.env.repos = repos.map(|s| s.to_string()).collect();
                }
                if sub_matches.contains_id("public") {
                    config.env.public = sub_matches.get_flag("public");
                }
                config.env.import_file = sub_matches.get_one::<String>("import").cloned();
                if let Some(link_str) = sub_matches.get_one::<String>("link") {
                    config.env.link = Some(parse_link_type(link_str.as_str())?);
                }
            }
            "remove" => {
            }
            "register" => {
                config.env.priority = sub_matches.get_one::<i32>("priority").cloned();
            }
            "unregister" => {
            }
            "activate" => {
                if sub_matches.contains_id("pure") {
                    config.env.pure = sub_matches.get_flag("pure");
                }
                if sub_matches.contains_id("stack") {
                    config.env.stack = sub_matches.get_flag("stack");
                }
            }
            "export" => {
            }
            _ => {}
        }
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

fn parse_options_info(_config: &mut EPKGConfig, _sub_matches: &clap::ArgMatches) -> Result<()> {
    // Info command options are handled directly in command_info
    Ok(())
}

fn parse_options_install(config: &mut EPKGConfig, sub_matches: &clap::ArgMatches) -> Result<()> {
    if sub_matches.contains_id("install-suggests") {
        config.install.install_suggests = sub_matches.get_flag("install-suggests");
    }
    if sub_matches.contains_id("no-install-recommends") {
        config.install.no_install_recommends = sub_matches.get_flag("no-install-recommends");
    }
    if sub_matches.contains_id("no-install-essentials") {
        config.install.no_install_essentials = sub_matches.get_flag("no-install-essentials");
    }
    if let Some(no_install) = sub_matches.get_many::<String>("no-install") {
        // Store original cmdline string (comma-separated, e.g., "pkg1,pkg2,-pkg3")
        config.install.no_install = no_install.map(|s| s.to_string()).collect::<Vec<_>>().join(",");
    }
    if sub_matches.contains_id("prefer-low-version") {
        config.install.prefer_low_version = sub_matches.get_flag("prefer-low-version");
    }
    Ok(())
}

fn parse_options_upgrade(config: &mut EPKGConfig, sub_matches: &clap::ArgMatches) -> Result<()> {
    config.upgrade.full_upgrade = sub_matches.get_flag("full");
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

fn parse_options_update(config: &mut EPKGConfig, sub_matches: &clap::ArgMatches) -> Result<()> {
    config.update.need_files = sub_matches.get_flag("need-files");
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

fn parse_options_unpack(_options: &mut EPKGConfig, _sub_matches: &clap::ArgMatches) -> Result<()> {
    Ok(())
}

fn parse_options_convert(_config: &mut EPKGConfig, _sub_matches: &clap::ArgMatches) -> Result<()> {
    // Placeholder for convert specific options
    Ok(())
}

fn parse_options_run(options: &mut EPKGConfig, sub_matches: &clap::ArgMatches) -> Result<()> {
    // Parse mount directories
    let mount_dirs = if let Some(mount_str) = sub_matches.get_one::<String>("mount") {
        mount_str.split(',').map(|s| s.trim().to_string()).collect()
    } else {
        Vec::new()
    };

    let user = sub_matches.get_one::<String>("user").cloned();

    let command = sub_matches.get_one::<String>("command")
        .ok_or_else(|| eyre::eyre!("Command is required"))?
        .clone();

    let args: Vec<String> = if let Some(args_iter) = sub_matches.get_many::<String>("args") {
        args_iter.cloned().collect()
    } else {
        Vec::new()
    };

    let timeout = if let Some(timeout_str) = sub_matches.get_one::<String>("timeout") {
        timeout_str.parse::<u64>()
            .map_err(|e| eyre::eyre!("Invalid timeout value '{}': {}", timeout_str, e))?
    } else {
        0 // Default: no timeout
    };

    options.run = RunOptions {
        mount_dirs,
        user,
        command,
        args,
        timeout,
        ..Default::default()
    };

    Ok(())
}

fn parse_options_search(config: &mut EPKGConfig, sub_matches: &clap::ArgMatches) -> Result<()> {
    let options = search::SearchOptions {
        files: sub_matches.get_flag("files"),
        paths: sub_matches.get_flag("paths"),
        regexp: sub_matches.get_flag("regexp"),
        ignore_case: sub_matches.get_flag("ignore-case"),
        origin_pattern: sub_matches.get_one::<String>("PATTERN").unwrap().to_string(),
        ..Default::default()
    };

    // Warn if using -f (files) flag with a pattern containing path separators
    if options.files && options.origin_pattern.contains('/') {
        eprintln!("Warning: Using -f|--files flag with pattern '{}' that contains '/'.\nConsider using -p|--paths flag instead for path-based searches.", options.origin_pattern);
        exit(0);
    }

    config.search = options;
    Ok(())
}

fn parse_options_service(config: &mut EPKGConfig, sub_matches: &clap::ArgMatches) -> Result<()> {
    if let Some(("status", status_matches)) = sub_matches.subcommand() {
        config.service.all = status_matches.get_flag("all");
    }
    Ok(())
}


fn command_env(sub_matches: &clap::ArgMatches) -> Result<()> {
    let name = &config().common.env_name;
    match sub_matches.subcommand() {
        Some(("list", _))           => list_environments(),
        Some(("create", _))         => create_environment(name),
        Some(("remove", _))         => remove_environment(name),
        Some(("register", _))       => register_environment(name),
        Some(("unregister", _))     => unregister_environment(name),
        Some(("activate", _))       => activate_environment(name),
        Some(("deactivate", _))     => deactivate_environment(),
        Some(("path", _))           => update_path(),
        Some(("export", sub_matches)) => {
            let output = sub_matches.get_one::<String>("output").cloned();
            export_environment(output)
        }
        Some(("config", sub_matches)) => {
            match sub_matches.subcommand() {
                Some(("edit", _)) => edit_environment_config(),
                Some(("get", sub_matches)) => {
                    if let Some(name) = sub_matches.get_one::<String>("NAME") {
                        get_environment_config(name)
                    } else {
                        Ok(())
                    }
                }
                Some(("set", sub_matches)) => {
                    if let (Some(name), Some(value)) = (sub_matches.get_one::<String>("NAME"), sub_matches.get_one::<String>("VALUE")) {
                        set_environment_config(name, value)
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

fn command_list(sub_matches: &clap::ArgMatches) -> Result<()> {
    // Determine scope - only one should be true, with installed as default
    let scope = if sub_matches.get_flag("all")   {  ListScope::All
    } else if sub_matches.get_flag("available")  {  ListScope::Available
    } else if sub_matches.get_flag("upgradable") {  ListScope::Upgradable
    } else                                       {  ListScope::Installed // default
    };

    let pattern = sub_matches.get_one::<String>("GLOB_PATTERN")
        .map(|s| s.as_str())
        .unwrap_or("");

    sync_channel_metadata()?;
    list_packages_with_scope(scope, pattern)?;
    Ok(())
}

fn command_info(sub_matches: &clap::ArgMatches) -> Result<()> {
    // First call sync_channel_metadata to prepare data
    sync_channel_metadata()?;

    // Load installed packages info
    load_installed_packages()?;

    // Get all arguments (package specs and key=val filters combined)
    let mut all_args: Vec<String> = Vec::new();

    // Add PACKAGE_SPEC arguments
    if let Some(package_specs) = sub_matches.get_many::<String>("PACKAGE_SPEC") {
        all_args.extend(package_specs.cloned());
    }

    // Get command options
    let show_files = sub_matches.get_flag("files");
    let show_scripts = sub_matches.get_flag("scripts");
    let show_store_path = sub_matches.get_flag("store-path");

    // Use the info module function
    crate::info::show_package_info(
        &all_args,
        show_files,
        show_scripts,
        show_store_path,
    )?;

    Ok(())
}

fn command_install(sub_matches: &clap::ArgMatches) -> Result<()> {
    if let Some(package_specs) = sub_matches.get_many::<String>("PACKAGE_SPEC") {
        let packages_vec: Vec<String> = package_specs.cloned().collect();
        install_packages(packages_vec).map(|_| ())?;
    }
    Ok(())
}

fn command_upgrade(sub_matches: &clap::ArgMatches) -> Result<()> {
    let package_names: Vec<String> = sub_matches
        .get_many::<String>("PACKAGE_SPEC")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_else(Vec::new);

    upgrade_packages(package_names).map(|_| ())
}

fn command_remove(sub_matches: &clap::ArgMatches) -> Result<()> {
    if let Some(package_specs) = sub_matches.get_many::<String>("PACKAGE_SPEC") {
        let packages_vec: Vec<String> = package_specs.cloned().collect();
        remove_packages(packages_vec).map(|_| ())?;
    }
    Ok(())
}

fn command_history(_sub_matches: &clap::ArgMatches) -> Result<()> {
    print_history()
}

fn command_restore(sub_matches: &clap::ArgMatches) -> Result<()> {
    if let Some(rollback_id) = sub_matches.get_one::<i32>("GEN_ID") {
        rollback_history(*rollback_id)?;
    }
    Ok(())
}

fn command_update(_sub_matches: &clap::ArgMatches) -> Result<()> {
    sync_channel_metadata()?;
    Ok(())
}

fn command_repo(sub_matches: &clap::ArgMatches) -> Result<()> {
    if let Some(_) = sub_matches.subcommand_matches("list") {
        crate::repo::list_repos()?;
    }
    Ok(())
}

fn command_hash(sub_matches: &clap::ArgMatches) -> Result<()> {
    if let Some(package_store_dirs) = sub_matches.get_many::<String>("PACKAGE_STORE_DIR") {
        for dir in package_store_dirs {
            let hash = crate::hash::epkg_store_hash(dir)?;
            println!("{}", hash);
        }
    }
    Ok(())
}

fn command_build(sub_matches: &clap::ArgMatches) -> Result<()> {
    if let Some(package_yaml) = sub_matches.get_one::<String>("PACKAGE_YAML") {
        let epkg_src_path = get_epkg_src_path();
        let build_script = epkg_src_path.join("build/scripts/generic-build.sh");
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

fn command_unpack(sub_matches: &clap::ArgMatches) -> Result<()> {
    if let Some(package_files_iter) = sub_matches.get_many::<String>("PACKAGE_FILE") {
        let files: Vec<String> = package_files_iter.cloned().collect();

        match crate::store::unpack_packages(files) {
            Ok(final_dirs) => {
                if final_dirs.is_empty() {
                    println!("No packages were unpacked by the store. This might indicate issues with the provided files or empty input.");
                } else {
                    for final_dir in &final_dirs {
                        // Print both the final directory path and the pkgline (directory name)
                        println!("{}", final_dir.display());
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

fn command_convert(sub_matches: &clap::ArgMatches) -> Result<()> {
    if let Some(package_files_iter) = sub_matches.get_many::<String>("PACKAGE_FILE") {
        let files: Vec<String> = package_files_iter.cloned().collect();
        let mut out_dir = sub_matches.get_one::<String>("out-dir").map(|s| s.as_str()).unwrap_or("");
        if out_dir == "" {
            out_dir = ".";
        }
        let origin_url = sub_matches.get_one::<String>("origin-url")
            .map(|s| s.as_str())
            .unwrap_or("default_url");

        match crate::store::unpack_packages(files) {
            Ok(final_dirs) => {
                if final_dirs.is_empty() {
                    println!("No packages were unpacked by the store. This might indicate issues with the provided files or empty input.");
                } else {
                    for final_dir in &final_dirs {
                        // Compress the package using the final directory path
                        epkg::compress_packages(final_dir, &out_dir, &origin_url)?;
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

fn command_search(_sub_matches: &clap::ArgMatches) -> Result<()> {
    // channel_config() cannot be referenced at parse_options_search() time,
    // so setup the derived options.u8_literal here
    let mut options = config().search.clone();

    search::search_repo_cache(&mut options)?;
    Ok(())
}

fn command_gc(sub_matches: &clap::ArgMatches) -> Result<()> {
    let old_downloads_days = sub_matches.get_one::<u64>("old-downloads").copied();
    gc::gc_epkg(old_downloads_days)?;
    Ok(())
}

fn command_service(sub_matches: &clap::ArgMatches) -> Result<()> {
    service::command_service(sub_matches)
}

fn command_self(sub_matches: &clap::ArgMatches) -> Result<()> {
    match sub_matches.subcommand() {
        Some(("install", _sub_matches)) => {
            if find_env_base(SELF_ENV).is_none() {
                install_epkg()?;
            }

            if find_env_base(MAIN_ENV).is_none() {
                light_init()?;
            } else {
                eprintln!("epkg was already initialized for current user");
            }
        }
        Some(("upgrade", _sub_matches)) => {
            upgrade_epkg()?;
        }
        Some(("remove", sub_matches)) => {
            if let Some(scope) = sub_matches.get_one::<String>("scope") {
                deinit::deinit_epkg(scope)?;
            }
        }
        _ => {}
    }
    Ok(())
}
