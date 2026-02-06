use clap::{Arg, Command};
use color_eyre::Result;
use std::env;
use std::path::PathBuf;
use std::process;
use crate::deb_triggers::{activate_trigger, TRIGGERSDIR, TRIGGERSDEFERREDFILE};

pub struct DpkgTriggerOptions {
    pub trigger_name: Option<String>,
    pub by_package: Option<String>,
    pub await_mode: Option<bool>, // None = default (await), Some(true) = await, Some(false) = no-await
    pub no_act: bool,
    pub check_supported: bool,
}

/// Validate trigger name according to dpkg rules
/// Returns error message if invalid, None if valid
/// Reference: /c/package-managers/dpkg/lib/dpkg/trigname.c
fn validate_trigger_name(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("empty trigger names are not permitted".to_string());
    }

    for c in name.chars() {
        let code = c as u32;
        // Control characters: <= ' ' (0x20) or >= 0x7F (0177 octal)
        if code <= 0x20 || code >= 0x7F {
            return Some("trigger name contains invalid character".to_string());
        }
    }

    None
}

fn handle_check_supported() -> ! {
    // Scriptlets already run inside the environment, so use "/" directly
    let env_root = PathBuf::from("/");
    let triggers_dir = env_root.join(TRIGGERSDIR);
    let unincorp_file = triggers_dir.join(TRIGGERSDEFERREDFILE);

    // Check if triggers system is available
    // dpkg checks if directory exists or Unincorp file exists
    if triggers_dir.exists() || unincorp_file.exists() || std::fs::create_dir_all(&triggers_dir).is_ok() {
        process::exit(0);
    } else {
        eprintln!("dpkg-trigger: triggers data directory not yet created");
        process::exit(1);
    }
}

fn determine_activating_package(options: &DpkgTriggerOptions) -> Result<(Option<String>, bool)> {
    let await_mode = options.await_mode.unwrap_or(true); // Default to await

    if !await_mode {
        // --no-await: no package awaits
        Ok((None, true))
    } else if let Some(ref by_pkg) = options.by_package {
        // --by-package specified
        Ok((Some(by_pkg.clone()), false))
    } else {
        // Try to get from environment variables (set by maintainer scripts)
        // dpkg requires both DPKG_MAINTSCRIPT_PACKAGE and DPKG_MAINTSCRIPT_ARCH
        match (env::var("DPKG_MAINTSCRIPT_PACKAGE"), env::var("DPKG_MAINTSCRIPT_ARCH")) {
            (Ok(pkgname), Ok(arch)) => {
                // Format: pkgname:arch (dpkg uses this format)
                Ok((Some(format!("{}:{}", pkgname, arch)), false))
            }
            (Ok(pkgname), Err(_)) => {
                // Only package name available, use it
                Ok((Some(pkgname), false))
            }
            (Err(_), _) => {
                // Not called from maintainer script and no --by-package
                Err(color_eyre::eyre::eyre!("dpkg-trigger: must be called from a maintainer script (or with a --by-package option"))
            }
        }
    }
}

fn get_trigger_name(options: &DpkgTriggerOptions) -> String {
    match &options.trigger_name {
        Some(name) => name.clone(),
        None => {
            eprintln!("dpkg-trigger: trigger name required");
            process::exit(2);
        }
    }
}

fn validate_trigger_name_or_exit(name: &str) {
    if let Some(error_msg) = validate_trigger_name(name) {
        eprintln!("dpkg-trigger: invalid trigger name '{}': {}", name, error_msg);
        process::exit(2);
    }
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<DpkgTriggerOptions> {
    let check_supported = matches.get_flag("check-supported");
    let trigger_name = if check_supported {
        // --check-supported is a command that takes no arguments
        None
    } else {
        // For trigger activation, trigger name is required
        matches.get_one::<String>("trigger-name")
            .map(|s| s.clone())
    };

    let by_package = matches.get_one::<String>("by-package").cloned();
    let no_await = matches.get_flag("no-await");
    let await_flag = matches.get_flag("await");

    // Determine await mode: --no-await overrides --await, default is await
    let await_mode = if no_await {
        Some(false)
    } else if await_flag {
        Some(true)
    } else {
        None // Default to await
    };

    let no_act = matches.get_flag("no-act");

    Ok(DpkgTriggerOptions {
        trigger_name,
        by_package,
        await_mode,
        no_act,
        check_supported,
    })
}

pub fn command() -> Command {
    Command::new("dpkg-trigger") // Command name with hyphen, module name is dpkg_trigger
        .about("Debian package trigger utility")
        .arg_required_else_help(true)
        .arg(Arg::new("trigger-name")
            .help("Name of the trigger to activate")
            .required_unless_present("check-supported"))
        .arg(Arg::new("by-package")
            .long("by-package")
            .value_name("PACKAGE")
            .help("Override trigger awaiter (normally set by dpkg through DPKG_MAINTSCRIPT_PACKAGE)"))
        .arg(Arg::new("await")
            .long("await")
            .action(clap::ArgAction::SetTrue)
            .help("Package needs to await the processing (default behavior)"))
        .arg(Arg::new("no-await")
            .long("no-await")
            .action(clap::ArgAction::SetTrue)
            .help("No package needs to await the processing"))
        .arg(Arg::new("no-act")
            .long("no-act")
            .action(clap::ArgAction::SetTrue)
            .help("Just test - do not actually change anything"))
        .arg(Arg::new("check-supported")
            .long("check-supported")
            .action(clap::ArgAction::SetTrue)
            .help("Check if the running dpkg supports triggers"))
        .arg(Arg::new("admindir")
            .long("admindir")
            .value_name("DIRECTORY")
            .help("Use DIRECTORY instead of default dpkg database (not fully supported in epkg)"))
        .arg(Arg::new("root")
            .long("root")
            .value_name("DIRECTORY")
            .help("Set root directory (not fully supported in epkg)"))
}

pub fn run(options: DpkgTriggerOptions) -> Result<()> {
    // Handle --check-supported command
    if options.check_supported {
        handle_check_supported();
    }

    // Must have trigger name for activation (unless --check-supported)
    let trigger_name = get_trigger_name(&options);
    validate_trigger_name_or_exit(&trigger_name);

    if options.no_act {
        // Just validate, don't actually activate
        log::info!("dpkg-trigger: --no-act specified, not activating trigger");
        return Ok(());
    }

    // Scriptlets already run inside the environment, so use "/" directly
    let env_root = PathBuf::from("/");

    // Determine activating package (awaiter)
    // Reference: dpkg-trigger main.c parse_awaiter_package()
    // If --no-await, set to "-" (no awaiter)
    // Otherwise, get from --by-package or DPKG_MAINTSCRIPT_PACKAGE + DPKG_MAINTSCRIPT_ARCH
    let (activating_package, no_await) = match determine_activating_package(&options) {
        Ok(tuple) => tuple,
        Err(e) => {
            eprintln!("{}", e);
            process::exit(2);
        }
    };

    // Activate the trigger
    match activate_trigger(
        &env_root,
        &trigger_name,
        activating_package.as_deref(),
        no_await,
    ) {
        Ok(_) => {
            Ok(())
        }
        Err(e) => {
            eprintln!("dpkg-trigger: failed to activate trigger '{}': {}", trigger_name, e);
            process::exit(2);
        }
    }
}

