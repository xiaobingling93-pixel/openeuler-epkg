use clap::{Arg, ArgAction, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use nix::mount::{mount, MsFlags};
use std::fs::File;
use std::io::{BufRead, BufReader};
use libc;

// Missing mount flags from libc (Linux kernel ABI)
// Refer to /usr/include/linux/mount.h

#[derive(Debug)]
enum MountMode {
    List,
    MountAll,
    SingleMount,
}

#[allow(dead_code)]
pub struct MountOptions {
    mode: MountMode,
    // For single mount
    source: Option<String>,
    target: Option<String>,
    fstype: Option<String>,
    flags: MsFlags,
    data: Option<String>,
    // Options
    read_only: bool,
    fake: bool,
    verbose: bool,
    no_mtab: bool,
    bind: bool,
    rbind: bool,
    move_mount: bool,
    remount: bool,
    // For -a
    opt_match: Option<String>,
    fstab: Option<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<MountOptions> {
    let source = matches.get_one::<String>("source").map(|s| s.to_string());
    let target = matches.get_one::<String>("target").map(|s| s.to_string());
    let fstype = matches.get_one::<String>("fstype").map(|s| s.to_string());
    let options = matches.get_one::<String>("options").map(|s| s.to_string());
    let read_only = matches.get_flag("readonly");
    let fake = matches.get_flag("fake");
    let verbose = matches.get_flag("verbose");
    let no_mtab = matches.get_flag("no-mtab");
    let bind = matches.get_flag("bind");
    let rbind = matches.get_flag("rbind");
    let move_mount = matches.get_flag("move");
    let remount = matches.get_flag("remount");
    let mount_all = matches.get_flag("all");
    let opt_match = matches.get_one::<String>("match").map(|s| s.to_string());
    let fstab = matches.get_one::<String>("fstab").map(|s| s.to_string());

    // Determine mode
    let mode = if mount_all {
        MountMode::MountAll
    } else if source.is_none() && target.is_none() {
        MountMode::List
    } else {
        MountMode::SingleMount
    };

    // Parse -o options
    let (flags, data) = parse_mount_options(options.as_deref())?;

    // Apply boolean flags
    let mut final_flags = flags;
    if read_only {
        final_flags |= MsFlags::MS_RDONLY;
    }
    if bind {
        final_flags |= MsFlags::MS_BIND;
    }
    if rbind {
        final_flags |= MsFlags::MS_BIND | MsFlags::MS_REC;
    }
    if move_mount {
        final_flags |= MsFlags::MS_MOVE;
    }
    if remount {
        final_flags |= MsFlags::MS_REMOUNT;
    }

    Ok(MountOptions {
        mode,
        source,
        target,
        fstype,
        flags: final_flags,
        data,
        read_only,
        fake,
        verbose,
        no_mtab,
        bind,
        rbind,
        move_mount,
        remount,
        opt_match,
        fstab,
    })
}

/// Parse -o option string into MsFlags and data string
pub fn parse_mount_options(opts: Option<&str>) -> Result<(MsFlags, Option<String>)> {
    let mut flags = MsFlags::empty();
    let mut data_parts = Vec::new();

    if let Some(opts_str) = opts {
        for opt in opts_str.split(',') {
            match opt {
                // Basic flags
                "ro"            => flags |= MsFlags::MS_RDONLY,
                "rw"            => flags &= !MsFlags::MS_RDONLY,
                "nosuid"        => flags |= MsFlags::MS_NOSUID,
                "suid"          => flags &= !MsFlags::MS_NOSUID,
                "nodev"         => flags |= MsFlags::MS_NODEV,
                "dev"           => flags &= !MsFlags::MS_NODEV,
                "noexec"        => flags |= MsFlags::MS_NOEXEC,
                "exec"          => flags &= !MsFlags::MS_NOEXEC,
                "sync"          => flags |= MsFlags::MS_SYNCHRONOUS,
                "async"         => flags &= !MsFlags::MS_SYNCHRONOUS,
                "remount"       => flags |= MsFlags::MS_REMOUNT,
                "bind"          => flags |= MsFlags::MS_BIND,
                "rbind"         => flags |= MsFlags::MS_BIND | MsFlags::MS_REC,
                "recursive"     => flags |= MsFlags::MS_REC,
                "move"          => flags |= MsFlags::MS_MOVE,
                // Propagation flags
                "silent"        => flags |= MsFlags::from_bits_truncate(libc::MS_SILENT),
                "loud"          => flags &= !MsFlags::from_bits_truncate(libc::MS_SILENT),
                "shared"        => flags |= MsFlags::from_bits_truncate(libc::MS_SHARED),
                "slave"         => flags |= MsFlags::from_bits_truncate(libc::MS_SLAVE),
                "private"       => flags |= MsFlags::MS_PRIVATE,
                "unbindable"    => flags |= MsFlags::from_bits_truncate(libc::MS_UNBINDABLE),
                "rshared"       => flags |= MsFlags::from_bits_truncate(libc::MS_REC | libc::MS_SHARED),
                "rslave"        => flags |= MsFlags::from_bits_truncate(libc::MS_REC | libc::MS_SLAVE),
                "rprivate"      => flags |= MsFlags::MS_REC | MsFlags::MS_PRIVATE,
                "runbindable"   => flags |= MsFlags::from_bits_truncate(libc::MS_REC | libc::MS_UNBINDABLE),
                // Time-related flags
                "relatime"      => flags |= MsFlags::from_bits_truncate(libc::MS_RELATIME),
                "norelatime"    => flags &= !MsFlags::from_bits_truncate(libc::MS_RELATIME),
                "strictatime"   => flags |= MsFlags::from_bits_truncate(libc::MS_STRICTATIME),
                "nostrictatime" => flags &= !MsFlags::from_bits_truncate(libc::MS_STRICTATIME),
                "nodiratime"    => flags |= MsFlags::from_bits_truncate(libc::MS_NODIRATIME),
                "diratime"      => flags &= !MsFlags::from_bits_truncate(libc::MS_NODIRATIME),
                "noatime"       => flags |= MsFlags::from_bits_truncate(libc::MS_NOATIME),
                "atime"         => flags &= !MsFlags::from_bits_truncate(libc::MS_NOATIME),
                "lazytime"      => flags |= MsFlags::from_bits_truncate(libc::MS_LAZYTIME),
                "nolazytime"    => flags &= !MsFlags::from_bits_truncate(libc::MS_LAZYTIME),
                "dirsync"       => flags |= MsFlags::from_bits_truncate(libc::MS_DIRSYNC),
                // New flags from /usr/include/linux/mount.h
                "mand"          => flags |= MsFlags::from_bits_truncate(libc::MS_MANDLOCK),
                "mandlock"      => flags |= MsFlags::from_bits_truncate(libc::MS_MANDLOCK),
                "nomand"        => flags &= !MsFlags::from_bits_truncate(libc::MS_MANDLOCK),
                "nosymfollow"   => flags |= MsFlags::from_bits_truncate(libc::MS_NOSYMFOLLOW),
                "symfollow"     => flags &= !MsFlags::from_bits_truncate(libc::MS_NOSYMFOLLOW),
                "posixacl"      => flags |= MsFlags::from_bits_truncate(libc::MS_POSIXACL),
                "iversion"      => flags |= MsFlags::from_bits_truncate(libc::MS_I_VERSION),
                "noiversion"    => flags &= !MsFlags::from_bits_truncate(libc::MS_I_VERSION),
                // Fstab helper options (no-op)
                "defaults" | "auto" | "noauto" | "nouser" | "user" | "users" | "owner" | "group" | "nofail" | "_netdev" | "sw" | "swap" | "loop" => (),
                // Userspace-only options (not passed to kernel)
                opt if opt.starts_with("X-") || opt.starts_with("x-") => (),
                opt if opt.starts_with("comment=") => (),
                opt if opt.starts_with("loop=") => (),
                opt if opt.starts_with("offset=") => (),
                opt if opt.starts_with("sizelimit=") => (),
                opt if opt.starts_with("encryption=") => (),
                opt if opt.starts_with("loinit=") => (),
                // Options that go into data string
                _ => data_parts.push(opt.to_string()),
            }
        }
    }

    let data = if data_parts.is_empty() {
        None
    } else {
        Some(data_parts.join(","))
    };

    Ok((flags, data))
}pub fn command() -> Command {
    Command::new("mount")
        .about("Mount a filesystem")
        .arg(
            Arg::new("source")
                .help("Device, directory, or file to mount")
                .index(1)
        )
        .arg(
            Arg::new("target")
                .help("Directory to mount onto")
                .index(2)
        )
        .arg(
            Arg::new("fstype")
                .short('t')
                .long("type")
                .help("Filesystem type")
        )
        .arg(
            Arg::new("options")
                .short('o')
                .help("Mount options (comma-separated)")
        )
        .arg(
            Arg::new("readonly")
                .short('r')
                .help("Mount read-only")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("fake")
                .short('f')
                .help("Fake mount (dry run)")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("verbose")
                .short('v')
                .help("Verbose output")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("no-mtab")
                .short('n')
                .help("Do not update /etc/mtab")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("bind")
                .long("bind")
                .help("Bind mount (make a subtree visible elsewhere)")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("rbind")
                .long("rbind")
                .help("Recursive bind mount")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("move")
                .long("move")
                .help("Move a mounted tree to another location")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("remount")
                .long("remount")
                .help("Remount an already mounted filesystem")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("all")
                .short('a')
                .help("Mount all filesystems in /etc/fstab")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("match")
                .short('O')
                .help("Mount only filesystems with option OPT (for -a)")
        )
        .arg(
            Arg::new("fstab")
                .short('T')
                .help("Use alternate fstab file")
        )
}

pub fn run(options: MountOptions) -> Result<()> {
    match options.mode {
        MountMode::List => list_mounts(options.fstype.as_deref()),
        MountMode::MountAll => mount_all(options),
        MountMode::SingleMount => single_mount(options),
    }
}

fn list_mounts(fstype: Option<&str>) -> Result<()> {
    let file = File::open("/proc/mounts").or_else(|_| File::open("/etc/mtab"))?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            let curr_fstype = parts[2];
            if fstype.is_none() || fstype == Some(curr_fstype) {
                println!("{} on {} type {} ({})", parts[0], parts[1], curr_fstype, parts.get(3).unwrap_or(&""));
            }
        }
    }
    Ok(())
}

fn mount_all(_options: MountOptions) -> Result<()> {
    Err(eyre!("mount -a not yet implemented"))
}

fn single_mount(options: MountOptions) -> Result<()> {
    if options.fake {
        println!("Would mount {:?} to {:?} with type {:?} flags {:?} data {:?}",
            options.source, options.target, options.fstype, options.flags, options.data);
        return Ok(());
    }

    let target = options.target.ok_or_else(|| eyre!("Target directory required"))?;
    let source = options.source;
    let fstype = options.fstype;
    let data = options.data;

    mount(
        source.as_deref(),
        target.as_str(),
        fstype.as_deref(),
        options.flags,
        data.as_deref(),
    ).map_err(|e| eyre!("mount failed: {}", e))?;

    if options.verbose {
        println!("Mounted {:?} to {}", source, target);
    }

    // TODO: update /etc/mtab unless options.no_mtab is set
    Ok(())
}
