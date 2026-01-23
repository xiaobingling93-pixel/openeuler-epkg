use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use users::{get_user_by_uid, get_group_by_gid};
use crate::posix::{posix_stat, PosixStat};

pub struct StatOptions {
    pub format: Option<String>,
    pub dereference: bool,
    pub files: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<StatOptions> {
    let format = matches.get_one::<String>("format").cloned();
    let dereference = matches.get_flag("dereference");
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(StatOptions { format, dereference, files })
}

pub fn command() -> Command {
    Command::new("stat")
        .about("Display file or file system status")
        .arg(Arg::new("format")
            .short('c')
            .long("format")
            .help("Use the specified FORMAT instead of the default")
            .value_name("FORMAT"))
        .arg(Arg::new("dereference")
            .short('L')
            .long("dereference")
            .help("Follow symlinks")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files")
            .num_args(1..)
            .help("Files to display status for"))
}

fn format_output(stat: &PosixStat, format: &str) -> Result<String> {
    let mut result = String::new();
    let mut chars = format.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '%' {
            if let Some(spec) = chars.next() {
                match spec {
                    'u' => result.push_str(&stat.uid.to_string()),
                    'g' => result.push_str(&stat.gid.to_string()),
                    'a' => result.push_str(&format!("{:o}", stat.mode & 0o777)),
                    'h' => result.push_str(&stat.nlink.to_string()),
                    'U' => {
                        let uid = stat.uid;
                        if let Some(user) = get_user_by_uid(uid) {
                            result.push_str(user.name().to_string_lossy().as_ref());
                        } else {
                            result.push_str(&uid.to_string());
                        }
                    }
                    'd' => result.push_str(&stat.dev.to_string()),
                    'i' => result.push_str(&stat.ino.to_string()),
                    '%' => result.push('%'),
                    _ => return Err(eyre!("stat: invalid format specifier '%{}'", spec)),
                }
            } else {
                result.push('%');
            }
        } else {
            result.push(ch);
        }
    }

    Ok(result)
}

fn stat_file(path: &Path, format: Option<&str>, dereference: bool) -> Result<()> {
    let path_str = if dereference {
        // For dereference, we need to resolve symlinks first
        fs::canonicalize(path)
            .map_err(|e| eyre!("stat: cannot stat '{}': {}", path.display(), e))?
            .to_string_lossy()
            .to_string()
    } else {
        path.to_string_lossy().to_string()
    };

    let stat = posix_stat(&path_str)
        .map_err(|e| eyre!("stat: cannot stat '{}': {:?}", path.display(), e))?;

    let output = if let Some(fmt) = format {
        format_output(&stat, fmt)?
    } else {
        // Default format similar to GNU stat
        // Note: posix_stat doesn't provide blocks/blksize, so we get them separately
        let metadata = if dereference {
            fs::metadata(path)
        } else {
            fs::symlink_metadata(path)
        }.map_err(|e| eyre!("stat: cannot stat '{}': {}", path.display(), e))?;

        format!("  File: {}\n  Size: {}\n  Blocks: {}\n  IO Block: {}\nDevice: {}\n  Inode: {}\n  Links: {}\nAccess: ({:o}/{})  Uid: ( {} / {} )  Gid: ( {} / {} )",
            path.display(),
            stat.size,
            metadata.blocks(),
            metadata.blksize(),
            stat.dev,
            stat.ino,
            stat.nlink,
            stat.mode & 0o777,
            stat.mode_str,
            stat.uid,
            get_user_by_uid(stat.uid).map(|u| u.name().to_string_lossy().to_string()).unwrap_or_else(|| stat.uid.to_string()),
            stat.gid,
            get_group_by_gid(stat.gid).map(|g| g.name().to_string_lossy().to_string()).unwrap_or_else(|| stat.gid.to_string()),
        )
    };

    println!("{}", output);
    Ok(())
}


pub fn run(options: StatOptions) -> Result<()> {
    if options.files.is_empty() {
        return Err(eyre!("stat: missing operand"));
    }

    for file in &options.files {
        let path = Path::new(file);
        stat_file(path, options.format.as_deref(), options.dereference)?;
    }

    Ok(())
}