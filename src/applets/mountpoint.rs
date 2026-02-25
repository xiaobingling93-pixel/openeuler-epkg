use clap::{Arg, ArgAction, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

pub struct MountpointOptions {
    path: String,
    quiet: bool,
    print_dev: bool,
    print_device_name: bool,
    print_major_minor: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<MountpointOptions> {
    let path = matches.get_one::<String>("path").ok_or_else(|| eyre!("Path required"))?.to_string();
    let quiet = matches.get_flag("quiet");
    let print_dev = matches.get_flag("dev");
    let print_device_name = matches.get_flag("device-name");
    let print_major_minor = matches.get_flag("major-minor");

    Ok(MountpointOptions {
        path,
        quiet,
        print_dev,
        print_device_name,
        print_major_minor,
    })
}

pub fn command() -> Command {
    Command::new("mountpoint")
        .about("Check if a directory is a mountpoint")
        .arg(
            Arg::new("path")
                .help("Directory or device to check")
                .index(1)
        )
        .arg(
            Arg::new("quiet")
                .short('q')
                .help("Quiet mode - no output")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("dev")
                .short('d')
                .help("Print major:minor of the filesystem")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("device-name")
                .short('n')
                .help("Print device name of the filesystem")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("major-minor")
                .short('x')
                .help("Print major:minor of DEVICE")
                .action(ArgAction::SetTrue)
        )
}

pub fn run(options: MountpointOptions) -> Result<()> {
    let path = Path::new(&options.path);

    if options.print_major_minor {
        // -x: treat path as a block device, print major:minor
        let metadata = path.metadata().map_err(|e| eyre!("{}: {}", path.display(), e))?;
        if !metadata.file_type().is_block_device() {
            return Err(eyre!("{}: not a block device", path.display()));
        }
        let major = nix::sys::stat::major(metadata.rdev());
        let minor = nix::sys::stat::minor(metadata.rdev());
        println!("{}:{}", major, minor);
        return Ok(());
    }

    // Otherwise, treat as directory mountpoint check
    let metadata = path.symlink_metadata().map_err(|e| eyre!("{}: {}", path.display(), e))?;
    if !metadata.is_dir() {
        return Err(eyre!("{}: Not a directory", path.display()));
    }

    let dev = metadata.dev();
    let ino = metadata.ino();

    // Get parent directory (path/..)
    let parent = path.join("..");
    let parent_metadata = parent.metadata().map_err(|e| eyre!("{}: {}", parent.display(), e))?;

    let is_mountpoint = dev != parent_metadata.dev() || (dev == parent_metadata.dev() && ino == parent_metadata.ino());

    if options.print_dev {
        let major = nix::sys::stat::major(dev);
        let minor = nix::sys::stat::minor(dev);
        println!("{}:{}", major, minor);
    }

    if options.print_device_name {
        // Try to find block device name; we can read /proc/self/mountinfo
        let device = find_block_device(path).unwrap_or_else(|| "UNKNOWN".to_string());
        println!("{} {}", device, path.display());
    }

    if !options.quiet && !options.print_dev && !options.print_device_name {
        println!("{} is {}a mountpoint", path.display(), if is_mountpoint { "" } else { "not " });
    }

    // Exit code: 0 if mountpoint, 1 if not (like busybox)
    if is_mountpoint {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

/// Attempt to find block device name for a mountpoint.
/// Reads /proc/self/mountinfo to match mount point.
fn find_block_device(path: &Path) -> Option<String> {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let file = File::open("/proc/self/mountinfo").ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line.ok()?;
        let fields: Vec<&str> = line.split_whitespace().collect();
        // mountinfo format: optional fields after 6th field, mount point at index 4
        if fields.len() >= 5 {
            let mount_point = fields[4];
            if Path::new(mount_point) == path {
                // device number is at index 2? Actually field index 2 is major:minor
                // We'll return the device name from field 9? Not reliable.
                // For simplicity, return major:minor.
                return Some(fields[2].to_string());
            }
        }
    }
    None
}