use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use time::{OffsetDateTime, macros::format_description};
use users::{get_user_by_uid, get_group_by_gid};
use crate::posix::{posix_stat, posix_statfs, PosixStat, PosixStatFs};

pub struct StatOptions {
    pub format: Option<String>,
    pub dereference: bool,
    pub file_system: bool,
    pub files: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<StatOptions> {
    let format = matches.get_one::<String>("format").cloned();
    let dereference = matches.get_flag("dereference");
    let file_system = matches.get_flag("file_system");
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(StatOptions { format, dereference, file_system, files })
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
        .arg(Arg::new("file_system")
            .short('f')
            .long("file-system")
            .help("Display file system status instead of file status")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files")
            .num_args(1..)
            .help("Files to display status for"))
}

fn format_output(path: &Path, stat: &PosixStat, format: &str) -> Result<String> {
    let mut result = String::new();
    let mut chars = format.chars().peekable();

    // File type character (for %A, matches coreutils stat)
    let file_type_char = {
        use libc::{S_IFMT, S_IFLNK, S_IFDIR, S_IFCHR, S_IFBLK, S_IFIFO, S_IFSOCK};
        let ft = stat.mode & S_IFMT as u32;
        if ft == S_IFDIR as u32 {
            'd'
        } else if ft == S_IFLNK as u32 {
            'l'
        } else if ft == S_IFCHR as u32 {
            'c'
        } else if ft == S_IFBLK as u32 {
            'b'
        } else if ft == S_IFIFO as u32 {
            'p'
        } else if ft == S_IFSOCK as u32 {
            's'
        } else {
            '-'
        }
    };

    while let Some(ch) = chars.next() {
        if ch == '%' {
            if let Some(spec) = chars.next() {
                match spec {
                    'u' => result.push_str(&stat.uid.to_string()),
                    'g' => result.push_str(&stat.gid.to_string()),
                    'U' => {
                        let uid = stat.uid;
                        if let Some(user) = get_user_by_uid(uid) {
                            result.push_str(user.name().to_string_lossy().as_ref());
                        } else {
                            result.push_str(&uid.to_string());
                        }
                    }
                    'G' => {
                        let gid = stat.gid;
                        if let Some(group) = get_group_by_gid(gid) {
                            result.push_str(group.name().to_string_lossy().as_ref());
                        } else {
                            result.push_str(&gid.to_string());
                        }
                    }
                    'a' => result.push_str(&format!("{:o}", stat.mode & 0o777)),
                    'h' => result.push_str(&stat.nlink.to_string()),
                    'd' => result.push_str(&stat.dev.to_string()),
                    'i' => result.push_str(&stat.ino.to_string()),
                    'f' => result.push_str(&format!("{:x}", stat.mode)),
                    's' => result.push_str(&stat.size.to_string()),
                    'X' => result.push_str(&stat.atime.to_string()),
                    'Y' => result.push_str(&stat.mtime.to_string()),
                    'Z' => result.push_str(&stat.ctime.to_string()),
                    'A' => result.push_str(&format!("{}{}", file_type_char, stat.mode_str)),
                    'n' => result.push_str(&path.display().to_string()),
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

fn format_timestamp(secs: i64, nsec: i64) -> Option<String> {
    let dt = OffsetDateTime::from_unix_timestamp(secs).ok()?;
    let dt = dt.replace_nanosecond(nsec as u32).ok()?;

    // Convert to local offset similar to coreutils `stat`
    let local = OffsetDateTime::now_local()
        .ok()
        .map(|now| dt.to_offset(now.offset()))
        .unwrap_or(dt);

    let fmt = format_description!("[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:9] [offset_hour sign:mandatory][offset_minute]");
    local.format(&fmt).ok()
}

fn resolve_stat_path(path: &Path, dereference: bool) -> Result<String> {
    if dereference {
        // For dereference, we need to resolve symlinks first
        Ok(fs::canonicalize(path)
            .map_err(|e| eyre!("stat: cannot stat '{}': {}", path.display(), e))?
            .to_string_lossy()
            .to_string())
    } else {
        Ok(path.to_string_lossy().to_string())
    }
}

fn format_file_status(
    path: &Path,
    stat: &PosixStat,
    metadata: &fs::Metadata,
    dereference: bool,
) -> Result<String> {
    let blocks = metadata.blocks();
    let blksize = metadata.blksize();

    // Device: major,minor (still use raw dev for %d via format_output)
    let dev = metadata.dev();
    let major = libc::major(dev);
    let minor = libc::minor(dev);

    let uid_name = get_user_by_uid(stat.uid)
        .map(|u| u.name().to_string_lossy().to_string())
        .unwrap_or_else(|| stat.uid.to_string());
    let gid_name = get_group_by_gid(stat.gid)
        .map(|g| g.name().to_string_lossy().to_string())
        .unwrap_or_else(|| stat.gid.to_string());

    // Timestamps from metadata with nanosecond precision
    let atime_secs = metadata.atime();
    let atime_nsec = metadata.atime_nsec();
    let mtime_secs = metadata.mtime();
    let mtime_nsec = metadata.mtime_nsec();
    let ctime_secs = metadata.ctime();
    let ctime_nsec = metadata.ctime_nsec();

    let access_ts = format_timestamp(atime_secs, atime_nsec).unwrap_or_default();
    let modify_ts = format_timestamp(mtime_secs, mtime_nsec).unwrap_or_default();
    let change_ts = format_timestamp(ctime_secs, ctime_nsec).unwrap_or_default();

    // Birth time (may not be available on all filesystems)
    let birth_ts = metadata.created().ok().and_then(|t| {
        let dur = t.duration_since(std::time::UNIX_EPOCH).ok()?;
        format_timestamp(dur.as_secs() as i64, dur.subsec_nanos() as i64)
    });

    // Permissions string with file type character, matching coreutils' Access line
    let permissions = {
        use libc::{S_IFMT, S_IFLNK, S_IFDIR, S_IFCHR, S_IFBLK, S_IFIFO, S_IFSOCK};
        let ft = stat.mode & S_IFMT as u32;
        let ch = if ft == S_IFDIR as u32 {
            'd'
        } else if ft == S_IFLNK as u32 {
            'l'
        } else if ft == S_IFCHR as u32 {
            'c'
        } else if ft == S_IFBLK as u32 {
            'b'
        } else if ft == S_IFIFO as u32 {
            'p'
        } else if ft == S_IFSOCK as u32 {
            's'
        } else {
            '-'
        };
        format!("{}{}", ch, stat.mode_str)
    };

    // Human-readable file type for IO Block line
    let file_type_human = match stat.file_type.as_str() {
        "regular" => "regular file",
        "link" => "symbolic link",
        other => other,
    };

    // For symlinks, coreutils `stat` shows "path -> target" on the File: line
    let file_display = if metadata.file_type().is_symlink() && !dereference {
        if let Ok(target) = fs::read_link(path) {
            format!("{} -> {}", path.display(), target.display())
        } else {
            path.display().to_string()
        }
    } else {
        path.display().to_string()
    };

    Ok(format!(
        "  File: {}\n  Size: {}\tBlocks: {}\tIO Block: {}   {}\nDevice: {},{}\tInode: {}\tLinks: {}\nAccess: ({:04o}/{})  Uid: ({:>5}/ {:>8})   Gid: ({:>5}/ {:>8})\nAccess: {}\nModify: {}\nChange: {}\n Birth: {}",
        file_display,
        stat.size,
        blocks,
        blksize,
        file_type_human,
        major,
        minor,
        stat.ino,
        stat.nlink,
        stat.mode & 0o777,
        permissions,
        stat.uid,
        uid_name,
        stat.gid,
        gid_name,
        access_ts,
        modify_ts,
        change_ts,
        birth_ts.unwrap_or_else(|| "-".to_string()),
    ))
}

fn format_fs_type(f_type: i64) -> String {
    // Map a few common Linux magic numbers to names, fall back to hex.
    match f_type as u64 {
        0x9123683E => "btrfs".to_string(),
        0xEF53 => "ext2/ext3".to_string(),
        0x68737173 => "squashfs".to_string(),
        0x58465342 => "xfs".to_string(),
        0x6969 => "nfs".to_string(),
        other => format!("{:x}", other),
    }
}

fn format_fs_output(path: &Path, fs: &PosixStatFs, format: Option<&str>) -> Result<String> {
    if let Some(fmt) = format {
        // Implement a subset of GNU stat -f format specifiers
        let mut result = String::new();
        let mut chars = fmt.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '%' {
                if let Some(spec) = chars.next() {
                    match spec {
                        'n' => result.push_str(&path.display().to_string()),
                        'i' => result.push_str(&format!("{:x}", fs.f_fsid)),
                        'l' => result.push_str(&fs.f_namelen.to_string()),
                        't' => result.push_str(&format!("{:x}", fs.f_type)),
                        's' => result.push_str(&fs.f_bsize.to_string()),
                        'S' => result.push_str(&fs.f_bsize.to_string()),
                        'b' => result.push_str(&fs.f_blocks.to_string()),
                        'f' => result.push_str(&fs.f_bfree.to_string()),
                        'a' => result.push_str(&fs.f_bavail.to_string()),
                        'c' => result.push_str(&fs.f_files.to_string()),
                        'd' => result.push_str(&fs.f_ffree.to_string()),
                        'T' => result.push_str(&format_fs_type(fs.f_type)),
                        '%' => result.push('%'),
                        _ => return Err(eyre!("stat: invalid file system format specifier '%{}'", spec)),
                    }
                } else {
                    result.push('%');
                }
            } else {
                result.push(ch);
            }
        }

        Ok(result)
    } else {
        // Default filesystem format similar to GNU `stat -f`
        let fs_type_name = format_fs_type(fs.f_type);
        Ok(format!(
            "  File: \"{}\"\n    ID: {:x} Namelen: {}     Type: {}\nBlock size: {}       Fundamental block size: {}\nBlocks: Total: {}   Free: {}   Available: {}\nInodes: Total: {}          Free: {}",
            path.display(),
            fs.f_fsid,
            fs.f_namelen,
            fs_type_name,
            fs.f_bsize,
            fs.f_bsize,
            fs.f_blocks,
            fs.f_bfree,
            fs.f_bavail,
            fs.f_files,
            fs.f_ffree,
        ))
    }
}

fn stat_file(path: &Path, format: Option<&str>, dereference: bool) -> Result<()> {
    let path_str = resolve_stat_path(path, dereference)?;

    let stat = posix_stat(&path_str)
        .map_err(|e| eyre!("stat: cannot stat '{}': {:?}", path.display(), e))?;

    let output = if let Some(fmt) = format {
        format_output(path, &stat, fmt)?
    } else {
        // Default format similar to GNU stat
        // Note: posix_stat doesn't provide blocks/blksize or nanosecond timestamps,
        // so we get them separately from std metadata.
        let metadata = if dereference {
            fs::metadata(path)
        } else {
            fs::symlink_metadata(path)
        }
        .map_err(|e| eyre!("stat: cannot stat '{}': {}", path.display(), e))?;

        format_file_status(path, &stat, &metadata, dereference)?
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
        if options.file_system {
            let fs = posix_statfs(file)
                .map_err(|e| eyre!("stat: cannot read file system information for '{}': {:?}", file, e))?;
            let out = format_fs_output(path, &fs, options.format.as_deref())?;
            println!("{}", out);
        } else {
            stat_file(path, options.format.as_deref(), options.dereference)?;
        }
    }

    Ok(())
}
