use clap::{Arg, ArgAction, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use walkdir::WalkDir;
use filetime::{set_file_mtime, FileTime};

#[derive(Default)]
pub struct CpOptions {
    pub sources: Vec<String>,
    pub destination: String,
    pub recursive: bool,
    pub force: bool,
    pub archive: bool,
    pub preserve: bool,
    pub no_dereference_d: bool,    // -d (same as --no-dereference --preserve=links)
    pub dereference_always: bool,  // -L
    pub no_dereference: bool,      // -P
    pub follow_cmdline_symlinks: bool, // -H
    #[allow(dead_code)]
    pub target_directory: Option<String>, // -t (used during parsing, converted to destination)
    pub no_target_directory: bool, // -T
    pub update: bool,              // -u
    pub verbose: bool,             // -v
    pub no_clobber: bool,          // -n
    pub symbolic_link: bool,       // -s
    pub link: bool,                // -l
    #[allow(dead_code)]
    pub selinux: bool,
    // Derived/computed fields (for symlink handling which depends on context)
    pub preserve_symlink_cmdline: bool, // computed from symlink handling options
    pub preserve_symlink_recursive: bool, // computed from symlink handling options
}

impl CpOptions {
    /// Compute derived parameters based on the option flags.
    /// This updates base fields in place and computes symlink handling.
    /// This should be called after setting the base options.
    pub fn compute_derived(&mut self) {
        // Archive mode implies preserve attributes
        if self.archive {
            self.preserve = true;
        }

        // Archive mode implies recursive
        if self.archive {
            self.recursive = true;
        }

        // -n (no-clobber) overrides -f (force)
        if self.no_clobber {
            self.force = false;
        }

        // Determine symlink handling: -L (always follow), -P (never follow), -H (follow only cmdline), -d/-a (never follow), default (follow)
        self.preserve_symlink_cmdline = should_preserve_symlink(
            self,
            true, // is_cmdline_arg
        );
        self.preserve_symlink_recursive = should_preserve_symlink(
            self,
            false, // is_cmdline_arg
        );
    }
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<CpOptions> {
    let target_directory = matches.get_one::<String>("target_directory").cloned();
    let mut args: Vec<String> = matches.get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let (sources, destination) = if let Some(tdir) = &target_directory {
        // -t flag: format is cp -t DIRECTORY SOURCE...
        if args.is_empty() {
            return Err(eyre!("cp: missing file operand"));
        }
        (args, tdir.clone())
    } else {
        // Normal format: cp SOURCE... DEST
        if args.len() < 2 {
            return Err(eyre!("cp: missing destination operand"));
        }
        let dest = args.pop().unwrap();
        (args, dest)
    };

    let recursive = matches.get_flag("recursive");
    let force = matches.get_flag("force");
    let archive = matches.get_flag("archive");
    let preserve = matches.get_flag("preserve");
    let no_dereference_d = matches.get_flag("dereference");
    let dereference_always = matches.get_flag("dereference_always");
    let no_dereference = matches.get_flag("no_dereference");
    let follow_cmdline_symlinks = matches.get_flag("follow_cmdline_symlinks");
    let no_target_directory = matches.get_flag("no_target_directory");
    let update = matches.get_flag("update");
    let verbose = matches.get_flag("verbose");
    let no_clobber = matches.get_flag("no_clobber");
    let symbolic_link = matches.get_flag("symbolic_link");
    let link = matches.get_flag("link");
    let selinux = matches.get_flag("selinux");

    let mut options = CpOptions {
        sources,
        destination,
        recursive,
        force,
        archive,
        preserve,
        no_dereference_d,
        dereference_always,
        no_dereference,
        follow_cmdline_symlinks,
        target_directory,
        no_target_directory,
        update,
        verbose,
        no_clobber,
        symbolic_link,
        link,
        selinux,
        // Derived fields - will be computed below
        preserve_symlink_cmdline: false,
        preserve_symlink_recursive: false,
    };
    options.compute_derived();
    Ok(options)
}

pub fn command() -> Command {
    Command::new("cp")
        .about("Copy files and directories")
        .arg(Arg::new("recursive")
            .short('r')
            .short('R')
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
            .help("Same as --no-dereference --preserve=links (don't follow symlinks, preserve hard links)")
            .action(ArgAction::SetTrue))
        .arg(Arg::new("selinux")
            .short('Z')
            .help("Set SELinux security context")
            .action(ArgAction::SetTrue))
        .arg(Arg::new("dereference_always")
            .short('L')
            .long("dereference")
            .help("Always follow symbolic links in SOURCE")
            .action(ArgAction::SetTrue))
        .arg(Arg::new("no_dereference")
            .short('P')
            .long("no-dereference")
            .help("Never follow symbolic links in SOURCE")
            .action(ArgAction::SetTrue))
        .arg(Arg::new("follow_cmdline_symlinks")
            .short('H')
            .help("Follow command-line symbolic links in SOURCE")
            .action(ArgAction::SetTrue))
        .arg(Arg::new("target_directory")
            .short('t')
            .long("target-directory")
            .value_name("DIRECTORY")
            .help("Copy all SOURCE arguments into DIRECTORY")
            .action(ArgAction::Set))
        .arg(Arg::new("no_target_directory")
            .short('T')
            .long("no-target-directory")
            .help("Treat DEST as a normal file")
            .action(ArgAction::SetTrue))
        .arg(Arg::new("update")
            .short('u')
            .long("update")
            .help("Copy only when the SOURCE file is newer than the destination file or when the destination file is missing")
            .action(ArgAction::SetTrue))
        .arg(Arg::new("verbose")
            .short('v')
            .long("verbose")
            .help("Explain what is being done")
            .action(ArgAction::SetTrue))
        .arg(Arg::new("no_clobber")
            .short('n')
            .long("no-clobber")
            .help("Do not overwrite an existing file")
            .action(ArgAction::SetTrue))
        .arg(Arg::new("symbolic_link")
            .short('s')
            .long("symbolic-link")
            .help("Make symbolic links instead of copying")
            .action(ArgAction::SetTrue))
        .arg(Arg::new("link")
            .short('l')
            .long("link")
            .help("Hard link files instead of copying")
            .action(ArgAction::SetTrue))
        .arg(Arg::new("args")
            .num_args(0..)
            .help("Source files/directories and destination")
            .required(false))
}

/// Determine if we should preserve symlinks (not follow them) based on options
fn should_preserve_symlink(
    options: &CpOptions,
    is_cmdline_arg: bool,
) -> bool {
    // Priority: -L (never preserve), -P (always preserve), -H (preserve only for recursive), -d/-a (preserve), default (don't preserve)
    if options.dereference_always {
        false // -L: always follow
    } else if options.no_dereference {
        true // -P: never follow
    } else if options.follow_cmdline_symlinks && !is_cmdline_arg {
        true // -H: only follow command-line args, preserve during recursive
    } else if options.no_dereference_d || options.archive {
        true // -d or -a: preserve symlinks
    } else {
        false // default: follow symlinks
    }
}

/// Check if source is newer than destination (for -u flag)
fn is_source_newer(src: &Path, dst: &Path) -> Result<bool> {
    if !dst.exists() {
        return Ok(true); // Destination doesn't exist, so source is "newer"
    }

    let src_metadata = fs::metadata(src)
        .map_err(|e| eyre!("cp: cannot get metadata for '{}': {}", src.display(), e))?;
    let dst_metadata = fs::metadata(dst)
        .map_err(|e| eyre!("cp: cannot get metadata for '{}': {}", dst.display(), e))?;

    let src_mtime = src_metadata.modified()
        .map_err(|e| eyre!("cp: cannot get modification time for '{}': {}", src.display(), e))?;
    let dst_mtime = dst_metadata.modified()
        .map_err(|e| eyre!("cp: cannot get modification time for '{}': {}", dst.display(), e))?;

    Ok(src_mtime > dst_mtime)
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

pub fn copy_file(
    src: &Path,
    dst: &Path,
    options: &CpOptions,
    is_recursive: bool,
) -> Result<()> {
    // Check -n (no-clobber): skip if destination exists
    if options.no_clobber && dst.exists() {
        if options.verbose {
            eprintln!("cp: skipping existing file '{}'", dst.display());
        }
        return Ok(());
    }

    // Check -u (update): only copy if source is newer
    if options.update && dst.exists() {
        if !is_source_newer(src, dst)? {
            if options.verbose {
                eprintln!("cp: skipping '{}' (destination is newer or same age)", dst.display());
            }
            return Ok(());
        }
    }

    // If force is enabled and destination exists, try to remove it first
    if options.force && dst.exists() {
        if dst.is_dir() {
            fs::remove_dir_all(dst)
                .map_err(|e| eyre!("cp: cannot remove directory '{}': {}", dst.display(), e))?;
        } else {
            fs::remove_file(dst)
                .map_err(|e| eyre!("cp: cannot remove file '{}': {}", dst.display(), e))?;
        }
    }

    // Handle -s (symbolic-link): create symlink instead of copying
    if options.symbolic_link {
        // Try to create relative path if both are absolute
        let link_target = if let (Some(dst_parent), Some(src_abs)) = (dst.parent(), src.canonicalize().ok()) {
            if let Ok(rel) = src_abs.strip_prefix(dst_parent) {
                rel.to_path_buf()
            } else {
                src.to_path_buf()
            }
        } else {
            src.to_path_buf()
        };

        symlink(&link_target, dst)
            .map_err(|e| eyre!("cp: cannot create symlink '{}' -> '{}': {}", dst.display(), link_target.display(), e))?;

        if options.verbose {
            eprintln!("'{}' -> '{}'", src.display(), dst.display());
        }
        return Ok(());
    }

    // Handle -l (link): create hard link instead of copying
    if options.link {
        fs::hard_link(src, dst)
            .map_err(|e| eyre!("cp: cannot create hard link '{}' to '{}': {}", dst.display(), src.display(), e))?;

        if options.verbose {
            eprintln!("'{}' -> '{}'", src.display(), dst.display());
        }
        return Ok(());
    }

    // Determine which symlink handling to use based on context
    let preserve_symlink = if is_recursive {
        options.preserve_symlink_recursive
    } else {
        options.preserve_symlink_cmdline
    };

    // Check if source is a symlink
    let src_metadata = fs::symlink_metadata(src)
        .map_err(|e| eyre!("cp: cannot access '{}': {}", src.display(), e))?;

    if src_metadata.file_type().is_symlink() {
        if preserve_symlink {
            // Copy the symlink as a symlink - read the link target and create a new symlink
            let target = fs::read_link(src)
                .map_err(|e| eyre!("cp: cannot read link '{}': {}", src.display(), e))?;

            // Create the symlink
            symlink(&target, dst)
                .map_err(|e| eyre!("cp: cannot create symlink '{}' -> '{}': {}", dst.display(), target.display(), e))?;

            if options.preserve {
                preserve_attributes(src, dst, true)?;
            }

            if options.verbose {
                eprintln!("'{}' -> '{}'", src.display(), dst.display());
            }
        } else {
            // Follow the symlink (default behavior) - copy the target file
            fs::copy(src, dst)
                .map_err(|e| eyre!("cp: cannot copy '{}' to '{}': {}", src.display(), dst.display(), e))?;

            if options.preserve {
                preserve_attributes(src, dst, true)?;
            }

            if options.verbose {
                eprintln!("'{}' -> '{}'", src.display(), dst.display());
            }
        }
    } else {
        // Regular file or directory
        fs::copy(src, dst)
            .map_err(|e| eyre!("cp: cannot copy '{}' to '{}': {}", src.display(), dst.display(), e))?;

        if options.preserve {
            preserve_attributes(src, dst, true)?;
        }

        if options.verbose {
            eprintln!("'{}' -> '{}'", src.display(), dst.display());
        }
    }

    Ok(())
}

pub fn copy_directory_recursive(
    src: &Path,
    dst: &Path,
    options: &CpOptions,
) -> Result<()> {
    for entry in WalkDir::new(src) {
        let entry = entry.map_err(|e| eyre!("cp: error walking directory: {}", e))?;
        let src_path = entry.path();
        let relative_path = src_path.strip_prefix(src).unwrap();
        let dst_path = dst.join(relative_path);

        if src_path.is_dir() {
            // Handle force for directories
            if options.force && dst_path.exists() && !dst_path.is_dir() {
                fs::remove_file(&dst_path)
                    .map_err(|e| eyre!("cp: cannot remove file '{}': {}", dst_path.display(), e))?;
            }

            fs::create_dir_all(&dst_path)
                .map_err(|e| eyre!("cp: cannot create directory '{}': {}", dst_path.display(), e))?;

            if options.preserve {
                preserve_attributes(src_path, &dst_path, true)?;
            }
        } else {
            if let Some(parent) = dst_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| eyre!("cp: cannot create directory '{}': {}", parent.display(), e))?;
            }
            copy_file(
                src_path,
                &dst_path,
                options,
                true, // is_recursive
            )?;
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
    options: &CpOptions,
) -> Result<()> {
    if src.is_dir() && !options.recursive {
        return Err(eyre!("cp: -r not specified; omitting directory '{}'", src.display()));
    }

    if src.is_dir() {
        // Copy directory to destination
        let dst_dir = if !options.no_target_directory && dst.exists() && dst.is_dir() {
            dst.join(src.file_name().unwrap())
        } else {
            dst.to_path_buf()
        };
        copy_directory_recursive(
            src,
            &dst_dir,
            options,
        )
    } else {
        // Copy file - use cmdline setting for command-line arguments
        let dst_file = if !options.no_target_directory && dst.is_dir() {
            dst.join(src.file_name().unwrap())
        } else {
            dst.to_path_buf()
        };
        copy_file(
            src,
            &dst_file,
            options,
            false, // is_recursive - this is a command-line argument
        )
    }
}

pub fn run(options: CpOptions) -> Result<()> {
    let dest_path = Path::new(&options.destination);

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
            &options,
        )?;
    } else {
        // Multiple sources - destination must be a directory (unless -T is specified)
        if options.no_target_directory {
            return Err(eyre!("cp: target '{}' is not a directory", dest_path.display()));
        }

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
                &options,
            )?;
        }
    }

    Ok(())
}
