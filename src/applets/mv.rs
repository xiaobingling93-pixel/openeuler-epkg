use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::path::Path;

pub struct MvOptions {
    pub sources: Vec<String>,
    pub destination: String,
    pub force: bool,
    pub no_clobber: bool,
    pub selinux_context: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<MvOptions> {
    let mut args: Vec<String> = matches.get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    if args.len() < 2 {
        return Err(eyre!("mv: missing destination operand"));
    }

    let destination = args.pop().unwrap();
    let sources = args;

    let force = matches.get_flag("force");
    let no_clobber = matches.get_flag("no_clobber");
    let selinux_context = matches.get_flag("selinux_context");

    Ok(MvOptions {
        sources,
        destination,
        force,
        no_clobber,
        selinux_context,
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
        .arg(Arg::new("args")
            .num_args(2..)
            .help("Source files/directories and destination")
            .required(true))
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
            fs::remove_dir_all(dst)
                .map_err(|e| eyre!("mv: cannot remove directory '{}': {}", dst.display(), e))?;
        } else {
            fs::remove_file(dst)
                .map_err(|e| eyre!("mv: cannot remove file '{}': {}", dst.display(), e))?;
        }
    }

    // Perform the move
    fs::rename(src, dst)
        .map_err(|e| eyre!("mv: cannot move '{}' to '{}': {}", src.display(), dst.display(), e))?;

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