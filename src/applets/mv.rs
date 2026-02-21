use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::path::Path;
use crate::lfs;

pub struct MvOptions {
    pub sources: Vec<String>,
    pub destination: String,
    pub force: bool,
    pub no_clobber: bool,
    pub selinux_context: bool,
    #[allow(dead_code)] pub target_directory: Option<String>, // -t (used during parsing, converted to destination)
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<MvOptions> {
    let target_directory = matches.get_one::<String>("target_directory").cloned();
    let mut args: Vec<String> = matches.get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let (sources, destination) = if let Some(tdir) = &target_directory {
        // -t flag: format is mv -t DIRECTORY SOURCE...
        if args.is_empty() {
            return Err(eyre!("mv: missing file operand"));
        }
        (args, tdir.clone())
    } else {
        // Normal format: mv SOURCE... DEST
        if args.len() < 2 {
            return Err(eyre!("mv: missing destination operand"));
        }
        let dest = args.pop().unwrap();
        (args, dest)
    };

    let force = matches.get_flag("force");
    let no_clobber = matches.get_flag("no_clobber");
    let selinux_context = matches.get_flag("selinux_context");

    Ok(MvOptions {
        sources,
        destination,
        force,
        no_clobber,
        selinux_context,
        target_directory,
    })
}

pub fn command() -> Command {
    Command::new("mv")
        .about("Move or rename files and directories")
        .arg(Arg::new("force")
            .short('f')
            .long("force")
            .help("Force overwrite of existing files")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("no_clobber")
            .short('n')
            .long("no-clobber")
            .help("Do not overwrite existing files")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("selinux_context")
            .short('Z')
            .help("Set SELinux security context (not implemented)")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("target_directory")
            .short('t')
            .long("target-directory")
            .value_name("DIRECTORY")
            .help("Move all SOURCE arguments into DIRECTORY")
            .action(clap::ArgAction::Set))
        .arg(Arg::new("args")
            .num_args(0..)
            .help("Source files/directories and destination")
            .required(false))
}

fn move_file(src: &Path, dst: &Path, force: bool, no_clobber: bool) -> Result<()> {
    // Check if destination exists
    if dst.exists() {
        if no_clobber {
            // Skip if no-clobber is enabled
            return Ok(());
        }
        if !force {
            // Fail if not forcing and destination exists
            return Err(eyre!("mv: '{}' already exists", dst.display()));
        }
        // Force overwrite - remove destination first
        if dst.is_dir() {
            lfs::remove_dir_all(dst)?;
        } else {
            lfs::remove_file(dst)?;
        }
    }

    // Perform the move
    lfs::rename(src, dst)?;

    Ok(())
}

pub fn run(options: MvOptions) -> Result<()> {
    let dest_path = Path::new(&options.destination);

    if options.sources.len() == 1 {
        // Single source - simple rename or move
        let src_path = Path::new(&options.sources[0]);
        move_file(src_path, dest_path, options.force, options.no_clobber)?;
    } else {
        // Multiple sources - destination must be a directory
        if !dest_path.exists() {
            return Err(eyre!("mv: target '{}' is not a directory", dest_path.display()));
        }
        if !dest_path.is_dir() {
            return Err(eyre!("mv: target '{}' is not a directory", dest_path.display()));
        }

        for src in &options.sources {
            let src_path = Path::new(src);
            let file_name = src_path.file_name()
                .ok_or_else(|| eyre!("mv: cannot get filename from '{}'", src))?;
            let dst_path = dest_path.join(file_name);

            move_file(src_path, &dst_path, options.force, options.no_clobber)?;
        }
    }

    // SELinux context flag is acknowledged but not implemented
    if options.selinux_context {
        // TODO: Implement SELinux context setting when needed
    }

    Ok(())
}