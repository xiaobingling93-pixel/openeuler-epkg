use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::os::unix::fs::{PermissionsExt, chown};
use std::path::Path;

pub struct InstallOptions {
    pub sources: Vec<String>,
    pub destination: String,
    pub mode: Option<String>,
    pub owner: Option<String>,
    pub group: Option<String>,
    pub directory: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<InstallOptions> {
    let mode = matches.get_one::<String>("mode").cloned();
    let owner = matches.get_one::<String>("owner").cloned();
    let group = matches.get_one::<String>("group").cloned();
    let directory = matches.get_flag("directory");

    let mut args: Vec<String> = matches.get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    if args.len() < 1 {
        return Err(eyre!("install: missing file operand"));
    }

    let destination = args.pop().unwrap();
    let sources = args;

    Ok(InstallOptions {
        sources,
        destination,
        mode,
        owner,
        group,
        directory,
    })
}

pub fn command() -> Command {
    Command::new("install")
        .about("Copy files and set attributes")
        .arg(Arg::new("mode")
            .short('m')
            .long("mode")
            .help("Set permission mode (octal)")
            .value_name("MODE"))
        .arg(Arg::new("owner")
            .short('o')
            .long("owner")
            .help("Set owner (name or uid)")
            .value_name("OWNER"))
        .arg(Arg::new("group")
            .short('g')
            .long("group")
            .help("Set group (name or gid)")
            .value_name("GROUP"))
        .arg(Arg::new("directory")
            .short('d')
            .long("directory")
            .help("Create directories instead of copying files")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("args")
            .num_args(1..)
            .help("Source files/directories and destination")
            .required(true))
}

fn resolve_user(user: &str) -> Result<u32> {
    if let Ok(uid) = user.parse::<u32>() {
        Ok(uid)
    } else {
        // Try to resolve username to UID
        match users::get_user_by_name(user) {
            Some(user) => Ok(user.uid()),
            None => Err(eyre!("install: invalid user '{}'", user)),
        }
    }
}

fn resolve_group(group: &str) -> Result<u32> {
    if let Ok(gid) = group.parse::<u32>() {
        Ok(gid)
    } else {
        // Try to resolve groupname to GID
        match users::get_group_by_name(group) {
            Some(group) => Ok(group.gid()),
            None => Err(eyre!("install: invalid group '{}'", group)),
        }
    }
}

fn set_ownership(path: &Path, owner: Option<&str>, group: Option<&str>) -> Result<()> {
    if owner.is_none() && group.is_none() {
        return Ok(());
    }

    let uid = if let Some(owner_str) = owner {
        Some(resolve_user(owner_str)?)
    } else {
        None
    };

    let gid = if let Some(group_str) = group {
        Some(resolve_group(group_str)?)
    } else {
        None
    };

    chown(path, uid, gid)
        .map_err(|e| eyre!("install: cannot set ownership on '{}': {}", path.display(), e))?;

    Ok(())
}

fn set_permissions(path: &Path, mode_str: &str) -> Result<()> {
    let mode_val = u32::from_str_radix(mode_str, 8)
        .map_err(|_| eyre!("install: invalid mode '{}'", mode_str))?;

    let permissions = std::fs::Permissions::from_mode(mode_val);
    std::fs::set_permissions(path, permissions)
        .map_err(|e| eyre!("install: cannot set permissions on '{}': {}", path.display(), e))?;

    Ok(())
}

fn create_directory_with_attrs(path: &Path, mode: Option<&str>, owner: Option<&str>, group: Option<&str>) -> Result<()> {
    fs::create_dir_all(path)
        .map_err(|e| eyre!("install: cannot create directory '{}': {}", path.display(), e))?;
    if let Some(mode) = mode {
        set_permissions(path, mode)?;
    }
    set_ownership(path, owner, group)?;
    Ok(())
}

fn copy_file_with_attrs(src: &Path, dst: &Path, mode: Option<&str>, owner: Option<&str>, group: Option<&str>) -> Result<()> {
    // Special handling for /dev/null - create an empty file
    if src == Path::new("/dev/null") {
        fs::File::create(dst)
            .map_err(|e| eyre!("install: cannot create '{}': {}", dst.display(), e))?;
    } else {
        fs::copy(src, dst)
            .map_err(|e| eyre!("install: cannot copy '{}' to '{}': {}", src.display(), dst.display(), e))?;
    }
    if let Some(mode) = mode {
        set_permissions(dst, mode)?;
    }
    set_ownership(dst, owner, group)?;
    Ok(())
}

fn copy_single_source(src: &Path, dest: &Path, mode: Option<&str>, owner: Option<&str>, group: Option<&str>) -> Result<()> {
    if dest.is_dir() {
        let file_name = src.file_name()
            .ok_or_else(|| eyre!("install: cannot get filename from '{}'", src.display()))?;
        let dst_path = dest.join(file_name);
        copy_file_with_attrs(src, &dst_path, mode, owner, group)?;
    } else {
        copy_file_with_attrs(src, dest, mode, owner, group)?;
    }
    Ok(())
}

fn copy_multiple_sources(sources: &[String], dest: &Path, mode: Option<&str>, owner: Option<&str>, group: Option<&str>) -> Result<()> {
    if !dest.exists() {
        fs::create_dir_all(dest)
            .map_err(|e| eyre!("install: cannot create directory '{}': {}", dest.display(), e))?;
    } else if !dest.is_dir() {
        return Err(eyre!("install: target '{}' is not a directory", dest.display()));
    }

    for src in sources {
        let src_path = Path::new(src);
        let file_name = src_path.file_name()
            .ok_or_else(|| eyre!("install: cannot get filename from '{}'", src))?;
        let dst_path = dest.join(file_name);
        copy_file_with_attrs(src_path, &dst_path, mode, owner, group)?;
    }
    Ok(())
}

fn run_directory_mode(options: &InstallOptions, dest_path: &Path) -> Result<()> {
    // Create directories from sources
    for dir_path in &options.sources {
        let path = Path::new(dir_path);
        create_directory_with_attrs(path, options.mode.as_deref(), options.owner.as_deref(), options.group.as_deref())?;
    }

    // Also create destination if it's a directory
    if !dest_path.exists() {
        create_directory_with_attrs(dest_path, options.mode.as_deref(), options.owner.as_deref(), options.group.as_deref())?;
    }
    Ok(())
}

fn run_copy_mode(options: &InstallOptions, dest_path: &Path) -> Result<()> {
    if options.sources.len() == 1 {
        let src_path = Path::new(&options.sources[0]);
        copy_single_source(src_path, dest_path, options.mode.as_deref(), options.owner.as_deref(), options.group.as_deref())?;
    } else {
        copy_multiple_sources(&options.sources, dest_path, options.mode.as_deref(), options.owner.as_deref(), options.group.as_deref())?;
    }
    Ok(())
}

pub fn run(options: InstallOptions) -> Result<()> {
    let dest_path = Path::new(&options.destination);

    if options.directory {
        run_directory_mode(&options, dest_path)
    } else {
        run_copy_mode(&options, dest_path)
    }
}