//! uname - print system information
//!
//! Compatible with coreutils/busybox uname: -s (kernel name, default), -n (nodename),
//! -r (release), -v (version), -m (machine), -a (all). On Unix uses libc uname(2) via
//! `posix_uname`; on Windows uses environment and `std::env::consts`.

use clap::{Arg, Command};
use color_eyre::Result;

#[cfg(unix)]
use crate::posix::posix_uname;

#[derive(Debug)]
pub struct UnameOptions {
    pub all: bool,
    pub kernel_name: bool,
    pub nodename: bool,
    pub kernel_release: bool,
    pub kernel_version: bool,
    pub machine: bool,
}

#[derive(Debug)]
struct UnameFields {
    sysname: String,
    nodename: String,
    release: String,
    version: String,
    machine: String,
}

fn fetch_uname() -> Result<UnameFields> {
    #[cfg(unix)]
    {
        let u = posix_uname().map_err(|e| color_eyre::eyre::eyre!("uname: {:?}", e))?;
        Ok(UnameFields {
            sysname: u.sysname,
            nodename: u.nodename,
            release: u.release,
            version: u.version,
            machine: u.machine,
        })
    }
    #[cfg(windows)]
    {
        use std::env;

        let nodename = env::var("COMPUTERNAME").unwrap_or_else(|_| "localhost".to_string());
        let release = env::var("OS").unwrap_or_else(|_| "Windows_NT".to_string());
        Ok(UnameFields {
            sysname: "Windows_NT".to_string(),
            nodename,
            release,
            version: format!("{} {}", env::consts::OS, env::consts::ARCH),
            machine: env::consts::ARCH.to_string(),
        })
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        use std::env;

        Ok(UnameFields {
            sysname: env::consts::OS.to_string(),
            nodename: "localhost".to_string(),
            release: "unknown".to_string(),
            version: format!("{} {}", env::consts::OS, env::consts::ARCH),
            machine: env::consts::ARCH.to_string(),
        })
    }
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<UnameOptions> {
    let all = matches.get_flag("all");
    let kernel_name = matches.get_flag("kernel_name");
    let nodename = matches.get_flag("nodename");
    let kernel_release = matches.get_flag("kernel_release");
    let kernel_version = matches.get_flag("kernel_version");
    let machine = matches.get_flag("machine");

    Ok(UnameOptions {
        all,
        kernel_name,
        nodename,
        kernel_release,
        kernel_version,
        machine,
    })
}

pub fn command() -> Command {
    Command::new("uname")
        .about("Print system information")
        .arg(
            Arg::new("all")
                .short('a')
                .long("all")
                .action(clap::ArgAction::SetTrue)
                .help("Print all information"),
        )
        .arg(
            Arg::new("kernel_name")
                .short('s')
                .long("kernel-name")
                .action(clap::ArgAction::SetTrue)
                .help("Print kernel name"),
        )
        .arg(
            Arg::new("nodename")
                .short('n')
                .long("nodename")
                .action(clap::ArgAction::SetTrue)
                .help("Print network node hostname"),
        )
        .arg(
            Arg::new("kernel_release")
                .short('r')
                .long("kernel-release")
                .action(clap::ArgAction::SetTrue)
                .help("Print kernel release"),
        )
        .arg(
            Arg::new("kernel_version")
                .short('v')
                .long("kernel-version")
                .action(clap::ArgAction::SetTrue)
                .help("Print kernel version"),
        )
        .arg(
            Arg::new("machine")
                .short('m')
                .long("machine")
                .action(clap::ArgAction::SetTrue)
                .help("Print machine hardware name"),
        )
}

pub fn run(options: UnameOptions) -> Result<()> {
    let u = fetch_uname()?;

    let any_flag = options.kernel_name
        || options.nodename
        || options.kernel_release
        || options.kernel_version
        || options.machine;

    if options.all {
        let suffix = if u.sysname == "Linux" {
            " GNU/Linux"
        } else {
            ""
        };
        println!(
            "{} {} {} {} {}{}",
            u.sysname, u.nodename, u.release, u.version, u.machine, suffix
        );
        return Ok(());
    }

    if !any_flag {
        // Default: kernel name only
        println!("{}", u.sysname);
        return Ok(());
    }

    let mut out = String::new();
    if options.kernel_name {
        out.push_str(&u.sysname);
    }
    if options.nodename {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&u.nodename);
    }
    if options.kernel_release {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&u.release);
    }
    if options.kernel_version {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&u.version);
    }
    if options.machine {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&u.machine);
    }
    println!("{}", out);
    Ok(())
}
