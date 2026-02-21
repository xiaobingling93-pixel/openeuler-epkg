use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use crate::lfs;

pub struct MkdirOptions {
    pub directories: Vec<String>,
    pub parents: bool,
    pub mode: Option<String>,
    pub selinux_context: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<MkdirOptions> {
    let directories: Vec<String> = matches.get_many::<String>("directories")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let parents = matches.get_flag("parents");
    let mode = matches.get_one::<String>("mode").cloned();
    let selinux_context = matches.get_flag("selinux_context");

    Ok(MkdirOptions {
        directories,
        parents,
        mode,
        selinux_context,
    })
}

pub fn command() -> Command {
    Command::new("mkdir")
        .about("Create directories")
        .arg(Arg::new("parents")
            .short('p')
            .long("parents")
            .help("Create parent directories as needed")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("mode")
            .short('m')
            .long("mode")
            .help("Set permission mode (octal)")
            .value_name("MODE"))
        .arg(Arg::new("selinux_context")
            .short('Z')
            .help("Set SELinux security context (not implemented)")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("directories")
            .num_args(1..)
            .help("Directories to create")
            .required(true))
}

fn set_directory_permissions(path: &Path, mode_str: &str) -> Result<()> {
    let octal_mode = u32::from_str_radix(mode_str, 8)
        .map_err(|_| eyre!("mkdir: invalid mode '{}'", mode_str))?;

    let permissions = fs::Permissions::from_mode(octal_mode);
    lfs::set_permissions(path, permissions)?;

    Ok(())
}

pub fn run(options: MkdirOptions) -> Result<()> {
    for dir_path in &options.directories {
        let path = Path::new(dir_path);

        if options.parents {
            lfs::create_dir_all(path)?;
        } else {
            lfs::create_dir(path)?;
        }

        // Set permissions if specified
        if let Some(ref mode) = options.mode {
            set_directory_permissions(path, mode)?;
        }

        // SELinux context flag is acknowledged but not implemented
        if options.selinux_context {
            // TODO: Implement SELinux context setting when needed
        }
    }
    Ok(())
}