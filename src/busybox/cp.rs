use clap::{Arg, ArgAction, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::fs::File;
use crate::lfs;
use std::io;
use std::path::Path;
use walkdir::WalkDir;
#[cfg(unix)]
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
    pub parents: bool,             // --parents
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

fn add_basic_options(cmd: Command) -> Command {
    cmd
        .arg(Arg::new("recursive")
            .short('r')
            .visible_short_alias('R')
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
        .arg(Arg::new("selinux")
            .short('Z')
            .help("Set SELinux security context")
            .action(ArgAction::SetTrue))
}

fn add_symlink_options(cmd: Command) -> Command {
    cmd
        .arg(Arg::new("dereference")
            .short('d')
            .help("Same as --no-dereference --preserve=links (don't follow symlinks, preserve hard links)")
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
}

fn add_target_options(cmd: Command) -> Command {
    cmd
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
}

fn add_other_options(cmd: Command) -> Command {
    cmd
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
        .arg(Arg::new("parents")
            .long("parents")
            .help("Form the name of each destination file by appending to the target directory a slash and the full name of the source file")
            .action(ArgAction::SetTrue))
}

fn parse_sources_and_destination(matches: &clap::ArgMatches) -> Result<(Vec<String>, String, Option<String>)> {
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
    Ok((sources, destination, target_directory))
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<CpOptions> {
    let (sources, destination, target_directory) = parse_sources_and_destination(matches)?;

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
    let parents = matches.get_flag("parents");
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
        parents,
        selinux,
        // Derived fields - will be computed below
        preserve_symlink_cmdline: false,
        preserve_symlink_recursive: false,
    };
    options.compute_derived();
    Ok(options)
}

pub fn command() -> Command {
    let cmd = Command::new("cp")
        .about("Copy files and directories");
    let cmd = add_basic_options(cmd);
    let cmd = add_symlink_options(cmd);
    let cmd = add_target_options(cmd);
    let cmd = add_other_options(cmd);
    cmd.arg(Arg::new("args")
        .num_args(0..)
        .help("Source files/directories and destination")
        .required(false))
}

/// Identity for hard-link deduplication when copying with `-d` / `-a`.
fn hardlink_identity(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(windows)]
    {
        // `file_index` / `volume_serial_number` require unstable `windows_by_handle`;
        // without them we cannot dedupe hard links — copy each link target separately.
        let _ = meta;
        None
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        None
    }
}

/// Determine if we should preserve symlinks (not follow them) based on options
fn should_preserve_symlink(
    options: &CpOptions,
    is_cmdline_arg: bool,
) -> bool {
    // Priority: -L (always follow) overrides everything
    if options.dereference_always {
        return false; // -L: always follow
    }
    // -H (follow command-line symlinks): override -P for command-line args
    if options.follow_cmdline_symlinks && is_cmdline_arg {
        return false; // -H: follow command-line symlinks
    }
    // -P (never follow)
    if options.no_dereference {
        return true; // -P: never follow
    }
    // -H for recursive (preserve symlinks during recursion)
    if options.follow_cmdline_symlinks && !is_cmdline_arg {
        return true; // -H: preserve during recursive traversal
    }
    // -d or -a: preserve symlinks
    if options.no_dereference_d || options.archive {
        return true; // -d or -a: preserve symlinks
    }
    // Default: preserve symlinks in recursive mode, follow in non-recursive mode
    options.recursive
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

#[cfg(unix)]
pub fn preserve_attributes(src: &Path, dst: &Path, preserve_timestamps: bool) -> Result<()> {
    let metadata = fs::metadata(src)
        .map_err(|e| eyre!("cp: cannot get metadata for '{}': {}", src.display(), e))?;

    // Preserve permissions
    let permissions = metadata.permissions();
    lfs::set_permissions(dst, permissions)?;

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

/// Remove destination if it exists and force flag is set.
/// Returns Ok(()) if removal succeeded or not needed.
fn force_remove_if_exists(dst: &Path, options: &CpOptions) -> Result<()> {
    if options.force && dst.exists() {
        if dst.is_dir() {
            lfs::remove_dir_all(dst)?;
        } else {
            lfs::remove_file(dst)?;
        }
    }
    Ok(())
}

/// Check if copy should be skipped due to -n or -u flags.
/// Returns Some(()) if copy should be skipped (and appropriate message printed), None otherwise.
fn check_skip_copy(src: &Path, dst: &Path, options: &CpOptions) -> Result<Option<()>> {
    // Check -n (no-clobber): skip if destination exists
    if options.no_clobber && dst.exists() {
        if options.verbose {
            eprintln!("cp: skipping existing file '{}'", dst.display());
        }
        return Ok(Some(()));
    }

    // Check -u (update): only copy if source is newer
    if options.update && dst.exists() {
        if !is_source_newer(src, dst)? {
            if options.verbose {
                eprintln!("cp: skipping '{}' (destination is newer or same age)", dst.display());
            }
            return Ok(Some(()));
        }
    }

    Ok(None)
}

/// Create symbolic link from dst to src if -s flag is set.
/// Returns Ok(true) if symbolic link was created, Ok(false) otherwise.
fn create_symbolic_link_if_requested(src: &Path, dst: &Path, options: &CpOptions) -> Result<bool> {
    if !options.symbolic_link {
        return Ok(false);
    }
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

    lfs::symlink(&link_target, dst)?;

    if options.verbose {
        eprintln!("'{}' -> '{}'", src.display(), dst.display());
    }
    Ok(true)
}

/// Create hard link from dst to src if -l flag is set.
/// Returns Ok(true) if hard link was created, Ok(false) otherwise.
fn create_hard_link_if_requested(src: &Path, dst: &Path, options: &CpOptions) -> Result<bool> {
    if !options.link {
        return Ok(false);
    }
    lfs::hard_link(src, dst)?;

    if options.verbose {
        eprintln!("'{}' -> '{}'", src.display(), dst.display());
    }
    Ok(true)
}

/// Copy file based on its type (symlink, regular file, special device).
/// The preserve_symlink flag determines whether symlinks are copied as symlinks or followed.
fn copy_with_attributes(
    copy_op: impl FnOnce() -> Result<()>,
    src: &Path,
    dst: &Path,
    options: &CpOptions,
) -> Result<()> {
    copy_op()?;
    if options.preserve {
        preserve_attributes(src, dst, true)?;
    }
    if options.verbose {
        eprintln!("'{}' -> '{}'", src.display(), dst.display());
    }
    Ok(())
}

fn copy_based_on_file_type(
    src: &Path,
    dst: &Path,
    options: &CpOptions,
    preserve_symlink: bool,
) -> Result<()> {
    // Check if source is a symlink
    let src_metadata = lfs::symlink_metadata(src)?;

    if src_metadata.file_type().is_symlink() {
        if preserve_symlink {
            copy_with_attributes(
                || {
                    let target = fs::read_link(src)
                        .map_err(|e| eyre!("cp: cannot read link '{}': {}", src.display(), e))?;
                    lfs::symlink(&target, dst)
                },
                src,
                dst,
                options,
            )?;
        } else {
            copy_with_attributes(
                || lfs::copy(src, dst).map(|_| ()),
                src,
                dst,
                options,
            )?;
        }
    } else if src_metadata.file_type().is_file() {
        copy_with_attributes(
            || lfs::copy(src, dst).map(|_| ()),
            src,
            dst,
            options,
        )?;
    } else if src_metadata.file_type().is_dir() {
        return Err(eyre!("cp: cannot copy directory '{}' without -R", src.display()));
    } else {
        // Device, fifo, etc. — copy by read/write to create a regular file
        let mut src_file = File::open(src)
            .map_err(|e| eyre!("cp: cannot open '{}' for reading: {}", src.display(), e))?;
        let mut dst_file = lfs::file_create(dst)?;
        io::copy(&mut src_file, &mut dst_file)
            .map_err(|e| eyre!("cp: cannot copy '{}' to '{}': {}", src.display(), dst.display(), e))?;

        if options.verbose {
            eprintln!("'{}' -> '{}'", src.display(), dst.display());
        }
    }
    Ok(())
}

pub fn copy_file(
    src: &Path,
    dst: &Path,
    options: &CpOptions,
    is_recursive: bool,
) -> Result<()> {
    // Early skip checks and force removal
    if let Some(()) = check_skip_copy(src, dst, options)? {
        return Ok(());
    }
    force_remove_if_exists(dst, options)?;

    // Handle -s (symbolic-link): create symlink instead of copying
    if create_symbolic_link_if_requested(src, dst, options)? {
        return Ok(());
    }

    // Handle -l (link): create hard link instead of copying
    if create_hard_link_if_requested(src, dst, options)? {
        return Ok(());
    }

    // Determine which symlink handling to use based on context
    let preserve_symlink = if is_recursive {
        options.preserve_symlink_recursive
    } else {
        options.preserve_symlink_cmdline
    };

    copy_based_on_file_type(src, dst, options, preserve_symlink)
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
                lfs::remove_file(&dst_path)?;
            }

            lfs::create_dir_all(&dst_path)?;

            if options.preserve {
                preserve_attributes(src_path, &dst_path, true)?;
            }
        } else {
            if let Some(parent) = dst_path.parent() {
                lfs::create_dir_all(parent)?;
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
    // With -d/-P, a symlink to a directory is copied as a symlink, not treated as a directory
    let is_symlink = lfs::symlink_metadata(src).map(|m| m.file_type().is_symlink()).unwrap_or(false);
    let treat_as_dir = src.is_dir() && !(is_symlink && options.preserve_symlink_cmdline);

    if treat_as_dir && !options.recursive {
        return Err(eyre!("cp: -r not specified; omitting directory '{}'", src.display()));
    }

    if treat_as_dir {
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

/// Handle --parents flag: copy source to destination preserving full path.
/// Returns Ok(true) if parents flag was set and copying performed, Ok(false) otherwise.
fn handle_parents_option(dest_path: &Path, options: &CpOptions) -> Result<bool> {
    if !options.parents {
        return Ok(false);
    }
    if options.sources.len() != 1 {
        return Err(eyre!("cp: with --parents, exactly one source is required"));
    }
    let src_path = Path::new(&options.sources[0]);
    if src_path.is_dir() {
        return Err(eyre!("cp: with --parents, source must not be a directory"));
    }
    let dst_file = dest_path.join(src_path);
    if let Some(parent) = dst_file.parent() {
        lfs::create_dir_all(parent)?;
    }
    copy_file(src_path, &dst_file, options, false)?;
    Ok(true)
}

/// Handle single source copy.
fn handle_single_source(dest_path: &Path, options: &CpOptions) -> Result<()> {
    let src_path = Path::new(&options.sources[0]);
    copy_single_item(src_path, dest_path, options)
}

/// Handle multiple sources copy.
fn process_source(
    src: &str,
    dest_path: &Path,
    options: &CpOptions,
    hardlink_map: &mut std::collections::HashMap<(u64, u64), std::path::PathBuf>,
    omitted_directory: &mut bool,
) -> Result<()> {
    let src_path = Path::new(src);
    let dst_name = src_path.file_name()
        .ok_or_else(|| eyre!("cp: cannot get filename from '{}'", src))?;
    let dst_path = dest_path.join(dst_name);

    // Without -r, skip directories with a message (GNU/BusyBox behavior). With -d/-P, symlinks are copied as symlinks so don't omit them even if they point to a directory.
    let is_symlink = lfs::symlink_metadata(src_path).map(|m| m.file_type().is_symlink()).unwrap_or(false);
    let preserve = options.preserve_symlink_cmdline;
    if src_path.is_dir() && !options.recursive && !(is_symlink && preserve) {
        eprintln!("cp: omitting directory '{}'", src_path.display());
        *omitted_directory = true;
        return Ok(());
    }

    if options.no_dereference_d || options.archive {
        if let Ok(meta) = lfs::symlink_metadata(src_path) {
            if let Some(key) = hardlink_identity(&meta) {
                if meta.is_file() && !meta.file_type().is_symlink() {
                    if let Some(existing) = hardlink_map.get(&key) {
                        lfs::hard_link(existing, &dst_path)?;
                        return Ok(());
                    }
                    hardlink_map.insert(key, dst_path.clone());
                }
            }
        }
    }

    copy_single_item(src_path, &dst_path, options)
}

fn handle_multiple_sources(dest_path: &Path, options: &CpOptions) -> Result<()> {
    // Multiple sources - destination must be a directory (unless -T is specified)
    if options.no_target_directory {
        return Err(eyre!("cp: target '{}' is not a directory", dest_path.display()));
    }

    if !dest_path.exists() {
        lfs::create_dir_all(dest_path)?;
    } else if !dest_path.is_dir() {
        return Err(eyre!("cp: target '{}' is not a directory", dest_path.display()));
    }

    // When -d (preserve links): track (dev, inode) -> destination path so we hard-link instead of copying
    let mut hardlink_map: std::collections::HashMap<(u64, u64), std::path::PathBuf> =
        std::collections::HashMap::new();
    let mut omitted_directory = false;

    for src in &options.sources {
        process_source(src, dest_path, options, &mut hardlink_map, &mut omitted_directory)?;
    }

    if omitted_directory {
        std::process::exit(1);
    }

    Ok(())
}
pub fn run(options: CpOptions) -> Result<()> {
    let dest_path = Path::new(&options.destination);

    // --parents: destination is dir, form path as dir/source_path
    if handle_parents_option(dest_path, &options)? {
        return Ok(());
    }

    // Handle force overwrite logic
    if dest_path.exists() && !options.force {
        // Check if destination exists and we're not forcing overwrite
        // For directories, this is usually okay, but for files we might want to warn
        // However, standard cp behavior is to overwrite files unless -i is used
        // Since we don't have -i, we'll follow the force flag
    }

    if options.sources.len() == 1 {
        handle_single_source(dest_path, &options)
    } else {
        handle_multiple_sources(dest_path, &options)
    }
}
