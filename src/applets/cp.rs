use clap::{Arg, ArgAction, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use walkdir::WalkDir;
use filetime::{set_file_mtime, FileTime};

pub struct CpOptions {
    pub sources: Vec<String>,
    pub destination: String,
    pub recursive: bool,
    pub force: bool,
    pub archive: bool,
    pub preserve: bool,
    pub dereference: bool,
    #[allow(dead_code)]
    pub selinux: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<CpOptions> {
    let mut args: Vec<String> = matches.get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    if args.len() < 2 {
        return Err(eyre!("cp: missing destination operand"));
    }

    let destination = args.pop().unwrap();
    let sources = args;

    let recursive = matches.get_flag("recursive");
    let force = matches.get_flag("force");
    let archive = matches.get_flag("archive");
    let preserve = matches.get_flag("preserve");
    let dereference = matches.get_flag("dereference");
    let selinux = matches.get_flag("selinux");

    Ok(CpOptions {
        sources,
        destination,
        recursive,
        force,
        archive,
        preserve,
        dereference,
        selinux,
    })
}

pub fn command() -> Command {
    Command::new("cp")
        .about("Copy files and directories")
        .arg(Arg::new("recursive")
            .short('r')
            .long("recursive")
            .help("Copy directories recursively")
            .action(ArgAction::SetTrue))
        .arg(Arg::new("force")
            .short('f')
            .long("force")
            .help("Force overwrite of existing files")
            .action(ArgAction::SetTrue))
        .arg(Arg::new("archive")
            .short('a')
            .long("archive")
            .help("Archive mode: preserve all attributes and don't follow symlinks")
            .action(ArgAction::SetTrue))
        .arg(Arg::new("preserve")
            .short('p')
            .help("Preserve mode, ownership, timestamps")
            .action(ArgAction::SetTrue))
        .arg(Arg::new("dereference")
            .short('d')
            .help("Don't follow symlinks")
            .action(ArgAction::SetTrue))
        .arg(Arg::new("selinux")
            .short('Z')
            .help("Set SELinux security context")
            .action(ArgAction::SetTrue))
        .arg(Arg::new("args")
            .num_args(2..)
            .help("Source files/directories and destination")
            .required(true))
}

pub fn preserve_attributes(src: &Path, dst: &Path, preserve_timestamps: bool) -> Result<()> {
    let metadata = fs::metadata(src)
        .map_err(|e| eyre!("cp: cannot get metadata for '{}': {}", src.display(), e))?;

    // Preserve permissions
    let permissions = metadata.permissions();
    fs::set_permissions(dst, permissions)
        .map_err(|e| eyre!("cp: cannot set permissions for '{}': {}", dst.display(), e))?;

    // Preserve timestamps if requested
    if preserve_timestamps {
        let mtime = metadata.modified()
            .map_err(|e| eyre!("cp: cannot get modification time for '{}': {}", src.display(), e))?;

        // Convert SystemTime to FileTime and set modification time
        let file_time = FileTime::from_system_time(mtime);
        set_file_mtime(dst, file_time)
            .map_err(|e| eyre!("cp: cannot set modification time for '{}': {}", dst.display(), e))?;
    }

    Ok(())
}

#[cfg(not(unix))]
pub fn preserve_attributes(_src: &Path, _dst: &Path, _preserve_timestamps: bool) -> Result<()> {
    // No-op on non-Unix systems
    Ok(())
}

pub fn copy_file(src: &Path, dst: &Path, preserve_attrs: bool, force: bool, dereference: bool) -> Result<()> {
    // If force is enabled and destination exists, try to remove it first
    if force && dst.exists() {
        if dst.is_dir() {
            fs::remove_dir_all(dst)
                .map_err(|e| eyre!("cp: cannot remove directory '{}': {}", dst.display(), e))?;
        } else {
            fs::remove_file(dst)
                .map_err(|e| eyre!("cp: cannot remove file '{}': {}", dst.display(), e))?;
        }
    }

    // Check if source is a symlink
    let src_metadata = fs::symlink_metadata(src)
        .map_err(|e| eyre!("cp: cannot access '{}': {}", src.display(), e))?;

    if src_metadata.file_type().is_symlink() {
        if dereference {
            // Copy the symlink as a symlink - read the link target and create a new symlink
            let target = fs::read_link(src)
                .map_err(|e| eyre!("cp: cannot read link '{}': {}", src.display(), e))?;

            // Create the symlink
            symlink(&target, dst)
                .map_err(|e| eyre!("cp: cannot create symlink '{}' -> '{}': {}", dst.display(), target.display(), e))?;

            if preserve_attrs {
                preserve_attributes(src, dst, true)?;
            }
        } else {
            // Follow the symlink (default behavior) - copy the target file
            fs::copy(src, dst)
                .map_err(|e| eyre!("cp: cannot copy '{}' to '{}': {}", src.display(), dst.display(), e))?;

            if preserve_attrs {
                preserve_attributes(src, dst, true)?;
            }
        }
    } else {
        // Regular file or directory
        fs::copy(src, dst)
            .map_err(|e| eyre!("cp: cannot copy '{}' to '{}': {}", src.display(), dst.display(), e))?;

        if preserve_attrs {
            preserve_attributes(src, dst, true)?;
        }
    }

    Ok(())
}

pub fn copy_directory_recursive(src: &Path, dst: &Path, preserve_attrs: bool, force: bool, dereference: bool) -> Result<()> {
    for entry in WalkDir::new(src) {
        let entry = entry.map_err(|e| eyre!("cp: error walking directory: {}", e))?;
        let src_path = entry.path();
        let relative_path = src_path.strip_prefix(src).unwrap();
        let dst_path = dst.join(relative_path);

        if src_path.is_dir() {
            // Handle force for directories
            if force && dst_path.exists() && !dst_path.is_dir() {
                fs::remove_file(&dst_path)
                    .map_err(|e| eyre!("cp: cannot remove file '{}': {}", dst_path.display(), e))?;
            }

            fs::create_dir_all(&dst_path)
                .map_err(|e| eyre!("cp: cannot create directory '{}': {}", dst_path.display(), e))?;

            if preserve_attrs {
                preserve_attributes(src_path, &dst_path, true)?;
            }
        } else {
            if let Some(parent) = dst_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| eyre!("cp: cannot create directory '{}': {}", parent.display(), e))?;
            }
            copy_file(src_path, &dst_path, preserve_attrs, force, dereference)?;
        }
    }
    Ok(())
}

/// Copy a single source to a destination with specified options.
/// This handles both files and directories (if recursive=true).
/// If destination exists and is a directory, the source will be copied inside it.
pub fn copy_single_item(
    src: &Path,
    dst: &Path,
    preserve_attrs: bool,
    force: bool,
    dereference: bool,
    recursive: bool,
) -> Result<()> {
    if src.is_dir() && !recursive {
        return Err(eyre!("cp: -r not specified; omitting directory '{}'", src.display()));
    }

    if src.is_dir() {
        // Copy directory to destination
        let dst_dir = if dst.exists() && dst.is_dir() {
            dst.join(src.file_name().unwrap())
        } else {
            dst.to_path_buf()
        };
        copy_directory_recursive(src, &dst_dir, preserve_attrs, force, dereference)
    } else {
        // Copy file
        let dst_file = if dst.is_dir() {
            dst.join(src.file_name().unwrap())
        } else {
            dst.to_path_buf()
        };
        copy_file(src, &dst_file, preserve_attrs, force, dereference)
    }
}

pub fn run(options: CpOptions) -> Result<()> {
    let dest_path = Path::new(&options.destination);

    // Determine if we need to preserve attributes
    let preserve_attrs = options.archive || options.preserve;

    // Archive mode implies recursive and preserve attributes
    let recursive = options.recursive || options.archive;

    // Handle force overwrite logic
    if dest_path.exists() && !options.force {
        // Check if destination exists and we're not forcing overwrite
        // For directories, this is usually okay, but for files we might want to warn
        // However, standard cp behavior is to overwrite files unless -i is used
        // Since we don't have -i, we'll follow the force flag
    }

    if options.sources.len() == 1 {
        // Single source
        let src_path = Path::new(&options.sources[0]);
        copy_single_item(
            src_path,
            dest_path,
            preserve_attrs,
            options.force,
            options.dereference,
            recursive,
        )?;
    } else {
        // Multiple sources - destination must be a directory
        if !dest_path.exists() {
            fs::create_dir_all(dest_path)
                .map_err(|e| eyre!("cp: cannot create directory '{}': {}", dest_path.display(), e))?;
        } else if !dest_path.is_dir() {
            return Err(eyre!("cp: target '{}' is not a directory", dest_path.display()));
        }

        for src in &options.sources {
            let src_path = Path::new(src);
            let dst_name = src_path.file_name()
                .ok_or_else(|| eyre!("cp: cannot get filename from '{}'", src))?;
            let dst_path = dest_path.join(dst_name);
            copy_single_item(
                src_path,
                &dst_path,
                preserve_attrs,
                options.force,
                options.dereference,
                recursive,
            )?;
        }
    }

    Ok(())
}
