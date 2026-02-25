use clap::{Arg, ArgAction, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use nix::mount::{umount2, MntFlags};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

pub struct UmountOptions {
    // Target directory or device
    target: String,
    // Flags
    force: bool,
    lazy: bool,
    no_mtab: bool,
    remount_ro: bool,
    all: bool,
    fstype: Option<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<UmountOptions> {
    let target = matches.get_one::<String>("target").map(|s| s.to_string());
    let force = matches.get_flag("force");
    let lazy = matches.get_flag("lazy");
    let no_mtab = matches.get_flag("no-mtab");
    let remount_ro = matches.get_flag("remount");
    let all = matches.get_flag("all");
    let fstype = matches.get_one::<String>("fstype").map(|s| s.to_string());

    // Determine target
    let target = if all {
        "".to_string()
    } else {
        target.ok_or_else(|| eyre!("Target directory required"))?
    };

    Ok(UmountOptions {
        target,
        force,
        lazy,
        no_mtab,
        remount_ro,
        all,
        fstype,
    })
}

pub fn command() -> Command {
    Command::new("umount")
        .about("Unmount filesystems")
        .arg(
            Arg::new("target")
                .help("Filesystem or directory to unmount")
                .index(1)
        )
        .arg(
            Arg::new("force")
                .short('f')
                .help("Force unmount (i.e., unreachable NFS server)")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("lazy")
                .short('l')
                .help("Lazy unmount (detach filesystem)")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("no-mtab")
                .short('n')
                .help("Don't erase /etc/mtab entries")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("remount")
                .short('r')
                .help("Remount devices read-only if mount is busy")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("all")
                .short('a')
                .help("Unmount all filesystems")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("fstype")
                .short('t')
                .help("Unmount only these filesystem type(s)")
        )
}

pub fn run(options: UmountOptions) -> Result<()> {
    if options.all {
        return unmount_all(options);
    }
    single_unmount(options)
}

fn single_unmount(options: UmountOptions) -> Result<()> {
    let target = Path::new(&options.target);
    let mut flags = MntFlags::empty();
    if options.force {
        flags |= MntFlags::MNT_FORCE;
    }
    if options.lazy {
        flags |= MntFlags::MNT_DETACH;
    }

    // If remount ro requested, try to remount read-only first
    if options.remount_ro {
        // TODO: implement remount read-only
        eprintln!("Warning: -r (remount read-only) not yet implemented");
    }

    umount2(target, flags).map_err(|e| eyre!("umount failed: {}", e))?;

    // TODO: update /etc/mtab unless options.no_mtab is set
    Ok(())
}

fn unmount_all(options: UmountOptions) -> Result<()> {
    let file = File::open("/proc/mounts").or_else(|_| File::open("/etc/mtab"))?;
    let reader = BufReader::new(file);
    let mut targets = Vec::new();

    for line in reader.lines() {
        let line = line?;
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let curr_fstype = parts[2];
            if options.fstype.as_ref().map_or(true, |ft| fstype_matches(curr_fstype, ft)) {
                targets.push(parts[1].to_string()); // mount point
            }
        }
    }

    // Unmount in reverse order (most recent first)
    for target in targets.iter().rev() {
        let opts = UmountOptions {
            target: target.clone(),
            force: options.force,
            lazy: options.lazy,
            no_mtab: options.no_mtab,
            remount_ro: options.remount_ro,
            all: false,
            fstype: options.fstype.clone(),
        };
        if let Err(e) = single_unmount(opts) {
            eprintln!("Failed to unmount {}: {}", target, e);
        }
    }
    Ok(())
}

/// Simple fstype matching: supports comma-separated list
fn fstype_matches(mount_fstype: &str, fstype_list: &str) -> bool {
    fstype_list.split(',').any(|ft| ft == mount_fstype)
}