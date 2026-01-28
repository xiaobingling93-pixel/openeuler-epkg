use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use crate::posix::{posix_stat, PosixError};
use users::{get_user_by_uid, get_group_by_gid};
use libc;

#[derive(Clone)]
struct FileEntry {
    name: String,
    path: PathBuf,
    metadata: fs::Metadata,
    mtime: u64,
}

pub struct LsOptions {
    pub paths: Vec<PathBuf>,
    pub all: bool,
    pub almost_all: bool,
    pub long: bool,
    pub human_readable: bool,
    pub directory: bool,
    pub recursive: bool,
    pub time_sort: bool,
    pub reverse: bool,
    pub size_sort: bool,
    pub classify: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<LsOptions> {
    let paths: Vec<PathBuf> = matches.get_many::<String>("paths")
        .map(|vals| vals.map(|s| PathBuf::from(s)).collect())
        .unwrap_or_else(|| vec![PathBuf::from(".")]);

    let all = matches.get_flag("all");
    let almost_all = matches.get_flag("almost_all");
    let long = matches.get_flag("long");
    let human_readable = matches.get_flag("human_readable");
    let directory = matches.get_flag("directory");
    let recursive = matches.get_flag("recursive");
    let time_sort = matches.get_flag("time_sort");
    let reverse   = matches.get_flag("reverse");
    let size_sort = matches.get_flag("size_sort");
    let classify  = matches.get_flag("classify");

    Ok(LsOptions {
        paths,
        all,
        almost_all,
        long,
        human_readable,
        directory,
        recursive,
        time_sort,
        reverse,
        size_sort,
        classify,
    })
}

pub fn command() -> Command {
    Command::new("ls")
        .about("List directory contents")
        .disable_help_flag(true)
        .arg(Arg::new("paths")
            .num_args(0..)
            .help("File(s) to list (default: current directory)"))
        .arg(Arg::new("all")
            .short('a')
            .long("all")
            .action(clap::ArgAction::SetTrue)
            .help("Do not ignore entries starting with ."))
        .arg(Arg::new("almost_all")
            .short('A')
            .long("almost-all")
            .action(clap::ArgAction::SetTrue)
            .help("Do not list implied . and .."))
        .arg(Arg::new("long")
            .short('l')
            .long("long")
            .action(clap::ArgAction::SetTrue)
            .help("Use a long listing format"))
        .arg(Arg::new("human_readable")
            .short('h')
            .long("human-readable")
            .action(clap::ArgAction::SetTrue)
            .help("With -l and -s, print sizes like 1K 234M 2G etc."))
        .arg(Arg::new("directory")
            .short('d')
            .long("directory")
            .action(clap::ArgAction::SetTrue)
            .help("List directories themselves, not their contents"))
        .arg(Arg::new("recursive")
            .short('R')
            .long("recursive")
            .action(clap::ArgAction::SetTrue)
            .help("List subdirectories recursively"))
        .arg(Arg::new("time_sort")
            .short('t')
            .action(clap::ArgAction::SetTrue)
            .help("Sort by time, newest first; see --time"))
        .arg(Arg::new("reverse")
            .short('r')
            .long("reverse")
            .action(clap::ArgAction::SetTrue)
            .help("Reverse order while sorting"))
        .arg(Arg::new("size_sort")
            .short('S')
            .action(clap::ArgAction::SetTrue)
            .help("Sort by file size, largest first"))
        .arg(Arg::new("classify")
            .short('F')
            .long("classify")
            .action(clap::ArgAction::SetTrue)
            .help("Append indicator (one of */=>@|) to entries"))
}

fn format_size(size: u64, human_readable: bool) -> String {
    if human_readable {
        const UNITS: &[&str] = &["B", "K", "M", "G", "T", "P", "E"];
        let mut size = size as f64;
        let mut unit_idx = 0;
        while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
            size /= 1024.0;
            unit_idx += 1;
        }
        if unit_idx == 0 {
            format!("{}", size as u64)
        } else {
            format!("{:.1}{}", size, UNITS[unit_idx])
        }
    } else {
        size.to_string()
    }
}

fn format_long_entry(entry: &FileEntry, human_readable: bool) -> Result<String> {
    let stat = posix_stat(entry.path.to_str().unwrap())
        .map_err(|e| match e {
            PosixError::Io(io_err) => eyre!("{}", io_err),
            PosixError::InvalidArgument(msg) => eyre!("{}", msg),
            PosixError::NotFound => eyre!("File not found"),
        })?;

    // File type and permissions
    let file_type_char = if stat.mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32 {
        'd'
    } else if stat.mode & libc::S_IFMT as u32 == libc::S_IFLNK as u32 {
        'l'
    } else if stat.mode & libc::S_IFMT as u32 == libc::S_IFCHR as u32 {
        'c'
    } else if stat.mode & libc::S_IFMT as u32 == libc::S_IFBLK as u32 {
        'b'
    } else if stat.mode & libc::S_IFMT as u32 == libc::S_IFIFO as u32 {
        'p'
    } else if stat.mode & libc::S_IFMT as u32 == libc::S_IFSOCK as u32 {
        's'
    } else {
        '-'
    };

    let mode_str = stat.mode_str;
    let permissions = format!("{}{}", file_type_char, mode_str);

    // Links
    let nlink = stat.nlink;

    // Owner and group
    let owner = get_user_by_uid(stat.uid)
        .map(|u| u.name().to_string_lossy().to_string())
        .unwrap_or_else(|| stat.uid.to_string());
    let group = get_group_by_gid(stat.gid)
        .map(|g| g.name().to_string_lossy().to_string())
        .unwrap_or_else(|| stat.gid.to_string());

    // Size
    let size_str = format_size(stat.size, human_readable);

    // Date formatting
    let date_str = if let Ok(datetime) = time::OffsetDateTime::from_unix_timestamp(stat.mtime as i64) {
        let now = time::OffsetDateTime::now_utc();
        let six_months_ago = now - time::Duration::days(180);
        if datetime > six_months_ago {
            // Recent: show month day hour:minute (e.g., "Jan 15 14:30")
            format!("{:>3} {:>2} {:02}:{:02}",
                format!("{:?}", datetime.month()).chars().take(3).collect::<String>(),
                datetime.day(),
                datetime.hour(),
                datetime.minute())
        } else {
            // Old: show month day year (e.g., "Jan 15 2023")
            format!("{:>3} {:>2} {:>4}",
                format!("{:?}", datetime.month()).chars().take(3).collect::<String>(),
                datetime.day(),
                datetime.year())
        }
    } else {
        "".to_string()
    };

    // Name (with symlink target if applicable)
    let name = if entry.metadata.is_symlink() {
        if let Ok(target) = fs::read_link(&entry.path) {
            format!("{} -> {}", entry.name, target.display())
        } else {
            entry.name.clone()
        }
    } else {
        entry.name.clone()
    };

    Ok(format!("{} {:>2} {:<8} {:<8} {:>8} {} {}", permissions, nlink, owner, group, size_str, date_str, name))
}

fn get_classify_indicator(entry: &FileEntry) -> char {
    if entry.metadata.is_dir() {
        '/'
    } else if entry.metadata.is_symlink() {
        '@'
    } else {
        // Check file type using mode
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = entry.metadata.permissions().mode();
            let file_type = mode & libc::S_IFMT as u32;
            if file_type == libc::S_IFCHR as u32 || file_type == libc::S_IFBLK as u32 {
                return '=';
            } else if file_type == libc::S_IFIFO as u32 {
                return '|';
            } else if file_type == libc::S_IFSOCK as u32 {
                return '=';
            } else if mode & 0o111 != 0 {
                return '*';
            }
        }
        '\0'
    }
}

fn list_directory(dir: &Path, options: &LsOptions, prefix: &str) -> Result<()> {
    let entries = fs::read_dir(dir)
        .map_err(|e| eyre!("ls: {}: {}", dir.display(), e))?;

    let mut file_entries: Vec<FileEntry> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().to_string();

            // Filter hidden files unless -a or -A
            if !options.all && !options.almost_all && name.starts_with('.') {
                return None;
            }

            // Filter . and .. unless -a (but allow with -A)
            if !options.all && (name == "." || name == "..") {
                return None;
            }

            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            let mtime = metadata.modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_secs();

            Some(FileEntry {
                name,
                path,
                metadata,
                mtime,
            })
        })
        .collect();

    // Sort entries
    if options.time_sort {
        file_entries.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    } else if options.size_sort {
        file_entries.sort_by(|a, b| {
            let size_a = a.metadata.len();
            let size_b = b.metadata.len();
            size_b.cmp(&size_a)
        });
    } else {
        file_entries.sort_by(|a, b| a.name.cmp(&b.name));
    }

    if options.reverse {
        file_entries.reverse();
    }

    // Print entries
    for entry in &file_entries {
        if options.long {
            let line = format_long_entry(entry, options.human_readable)?;
            println!("{}", line);
        } else {
            let mut name = entry.name.clone();
            if options.classify {
                let indicator = get_classify_indicator(entry);
                if indicator != '\0' {
                    name.push(indicator);
                }
            }
            println!("{}", name);
        }
    }

    // Handle recursive listing
    if options.recursive {
        for entry in &file_entries {
            if entry.metadata.is_dir() && entry.name != "." && entry.name != ".." {
                println!("\n{}{}:", prefix, entry.path.display());
                list_directory(&entry.path, options, &format!("{}  ", prefix))?;
            }
        }
    }

    Ok(())
}

fn print_path_header_if_needed(path: &Path, options: &LsOptions) {
    if options.paths.len() > 1 {
        println!("{}:", path.display());
    }
}

fn file_entry_for_path(path: &Path, metadata: fs::Metadata) -> Result<FileEntry> {
    let mtime = metadata.modified()
        .map_err(|e| eyre!("ls: {}: {}", path.display(), e))?
        .duration_since(UNIX_EPOCH)
        .map_err(|e| eyre!("ls: {}: {}", path.display(), e))?
        .as_secs();

    Ok(FileEntry {
        name: path.file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| path.display().to_string()),
        path: path.to_path_buf(),
        metadata,
        mtime,
    })
}

fn print_single_entry(entry: &FileEntry, options: &LsOptions) -> Result<()> {
    if options.long {
        let line = format_long_entry(entry, options.human_readable)?;
        println!("{}", line);
    } else {
        let mut name = entry.name.clone();
        if options.classify {
            let indicator = get_classify_indicator(entry);
            if indicator != '\0' {
                name.push(indicator);
            }
        }
        println!("{}", name);
    }
    Ok(())
}

fn list_path(path: &Path, options: &LsOptions) -> Result<()> {
    let metadata = fs::metadata(path)
        .map_err(|e| eyre!("ls: {}: {}", path.display(), e))?;

    if options.directory || metadata.is_file() {
        // List the file/directory itself (with -d flag or if it's a file)
        let entry = file_entry_for_path(path, metadata)?;
        print_single_entry(&entry, options)?;
        Ok(())
    } else if metadata.is_dir() {
        // List directory contents
        list_directory(path, options, "")
    } else {
        Err(eyre!("ls: {}: Not a directory", path.display()))
    }
}

fn print_trailing_blank_line_between_paths(path: &Path, options: &LsOptions) {
    if options.paths.len() > 1 && path != options.paths.last().unwrap() {
        println!();
    }
}

pub fn run(options: LsOptions) -> Result<()> {
    for path in &options.paths {
        print_path_header_if_needed(path, &options);
        list_path(path, &options)?;
        print_trailing_blank_line_between_paths(path, &options);
    }

    Ok(())
}
