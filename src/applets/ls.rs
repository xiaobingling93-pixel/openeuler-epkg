use clap::{Arg, Command, ArgAction};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use crate::posix::{posix_stat, PosixError};
use users::{get_user_by_uid, get_group_by_gid};
use libc;
use std::io::IsTerminal;
use std::env;
use std::os::unix::fs::FileTypeExt;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ColorOption {
    Never,
    Always,
    Auto,
}

#[derive(Clone)]
pub enum TimeStyle {
    FullIso,
    LongIso,
    Iso,
    Locale,
    Custom(String), // +FORMAT
    PosixFullIso,
    PosixLongIso,
    PosixIso,
    PosixLocale,
    PosixCustom(String),
}

#[derive(Clone, Default)]
pub struct LsColors {
    // mapping from file type indicators to ANSI escape sequences
    // di, ln, so, pi, ex, etc.
    map: std::collections::HashMap<String, String>,
}

impl LsColors {
    pub fn from_env() -> Self {
        let mut map = std::collections::HashMap::new();
        // Default color scheme (similar to GNU ls)
        map.insert("di".to_string(), "01;34".to_string());  // bold blue directories
        map.insert("ln".to_string(), "01;36".to_string());  // bold cyan symlinks
        map.insert("so".to_string(), "01;35".to_string());  // bold magenta sockets
        map.insert("pi".to_string(), "01;33".to_string());  // bold yellow pipes
        map.insert("ex".to_string(), "01;32".to_string());  // bold green executables
        map.insert("cd".to_string(), "01;33".to_string());  // bold yellow char devices
        map.insert("bd".to_string(), "01;33".to_string());  // bold yellow block devices

        if let Ok(val) = std::env::var("LS_COLORS") {
            for entry in val.split(':') {
                let mut parts = entry.split('=');
                if let (Some(key), Some(value)) = (parts.next(), parts.next()) {
                    map.insert(key.to_string(), value.to_string());
                }
            }
        }
        Self { map }
    }

    pub fn get_color(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(|s| s.as_str())
    }
}

fn parse_time_style(s: &str) -> Option<TimeStyle> {
    match s {
        "full-iso" => Some(TimeStyle::FullIso),
        "long-iso" => Some(TimeStyle::LongIso),
        "iso" => Some(TimeStyle::Iso),
        "locale" => Some(TimeStyle::Locale),
        s if s.starts_with('+') => Some(TimeStyle::Custom(s[1..].to_string())),
        s if s.starts_with("posix-") => {
            let inner = &s[6..];
            match inner {
                "full-iso" => Some(TimeStyle::PosixFullIso),
                "long-iso" => Some(TimeStyle::PosixLongIso),
                "iso" => Some(TimeStyle::PosixIso),
                "locale" => Some(TimeStyle::PosixLocale),
                s if s.starts_with('+') => Some(TimeStyle::PosixCustom(s[1..].to_string())),
                _ => None,
            }
        }
        _ => None,
    }
}

fn format_time(mtime: u64, time_style: Option<&TimeStyle>) -> String {
    let datetime = match time::OffsetDateTime::from_unix_timestamp(mtime as i64) {
        Ok(dt) => dt,
        Err(_) => return String::new(),
    };
    match time_style {
        Some(TimeStyle::FullIso) | Some(TimeStyle::PosixFullIso) => {
            // %Y-%m-%d %H:%M:%S.%f %z
            // Use format_description! macro
            match time::format_description::parse("[year]-[month]-[day] [hour]:[minute]:[second].[subsecond] [offset_hour sign:mandatory][offset_minute]") {
                Ok(format) => datetime.format(&format).unwrap_or_else(|_| String::new()),
                Err(_) => String::new(),
            }
        }
        Some(TimeStyle::LongIso) | Some(TimeStyle::PosixLongIso) => {
            match time::format_description::parse("[year]-[month]-[day] [hour]:[minute]") {
                Ok(format) => datetime.format(&format).unwrap_or_else(|_| String::new()),
                Err(_) => String::new(),
            }
        }
        Some(TimeStyle::Iso) | Some(TimeStyle::PosixIso) => {
            match time::format_description::parse("[year]-[month]-[day]") {
                Ok(format) => datetime.format(&format).unwrap_or_else(|_| String::new()),
                Err(_) => String::new(),
            }
        }
        Some(TimeStyle::Locale) | Some(TimeStyle::PosixLocale) => {
            // Use existing locale formatting (month name)
            let now = time::OffsetDateTime::now_utc();
            let six_months_ago = now - time::Duration::days(180);
            if datetime > six_months_ago {
                // Recent: show month day hour:minute
                format!("{:>3} {:>2} {:02}:{:02}",
                    format!("{:?}", datetime.month()).chars().take(3).collect::<String>(),
                    datetime.day(),
                    datetime.hour(),
                    datetime.minute())
            } else {
                // Old: show month day year
                format!("{:>3} {:>2} {:>4}",
                    format!("{:?}", datetime.month()).chars().take(3).collect::<String>(),
                    datetime.day(),
                    datetime.year())
            }
        }
        Some(TimeStyle::Custom(ref _fmt)) | Some(TimeStyle::PosixCustom(ref _fmt)) => {
            // fmt is a strftime-like format; we need to convert to time's format_description.
            // For simplicity, we'll use strftime via chrono? Not available.
            // We'll fall back to locale for now.
            // TODO: implement custom format
            String::new()
        }
        None => {
            // Default to locale behavior (same as above)
            let now = time::OffsetDateTime::now_utc();
            let six_months_ago = now - time::Duration::days(180);
            if datetime > six_months_ago {
                format!("{:>3} {:>2} {:02}:{:02}",
                    format!("{:?}", datetime.month()).chars().take(3).collect::<String>(),
                    datetime.day(),
                    datetime.hour(),
                    datetime.minute())
            } else {
                format!("{:>3} {:>2} {:>4}",
                    format!("{:?}", datetime.month()).chars().take(3).collect::<String>(),
                    datetime.day(),
                    datetime.year())
            }
        }
    }
}

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
    pub color: ColorOption,
    pub time_style: Option<TimeStyle>,
    pub ls_colors: LsColors,
}

struct ColumnWidths {
    nlink: usize,
    owner: usize,
    group: usize,
    size: usize,
}

struct LongEntryFields {
    permissions: String,
    nlink: u64,
    owner: String,
    group: String,
    size_str: String,
    date_str: String,
    name: String,
}

impl LsOptions {
    fn color_enabled(&self) -> bool {
        match self.color {
            ColorOption::Always => true,
            ColorOption::Never => false,
            ColorOption::Auto => std::io::stdout().is_terminal(),
        }
    }

    fn color_code_for_entry(&self, entry: &FileEntry) -> Option<String> {
        if !self.color_enabled() {
            return None;
        }
        let key = Self::ls_color_key_for_entry(entry);
        self.ls_colors.get_color(&key).map(|seq| format!("\x1b[{}m", seq))
    }

    fn ls_color_key_for_entry(entry: &FileEntry) -> String {
        Self::ls_color_key_for_metadata(&entry.metadata)
    }

    fn ls_color_key_for_metadata(metadata: &fs::Metadata) -> String {
        use std::os::unix::fs::PermissionsExt;
        if metadata.is_dir() {
            "di".to_string()
        } else if metadata.is_symlink() {
            "ln".to_string()
        } else if metadata.file_type().is_socket() {
            "so".to_string()
        } else if metadata.file_type().is_fifo() {
            "pi".to_string()
        } else if metadata.file_type().is_char_device() {
            "cd".to_string()
        } else if metadata.file_type().is_block_device() {
            "bd".to_string()
        } else {
            // Check for executable
            let mode = metadata.permissions().mode();
            if mode & 0o111 != 0 {
                "ex".to_string()
            } else {
                // default to no color
                "".to_string()
            }
        }
    }

    fn colorize_name(&self, entry: &FileEntry, name: &str) -> String {
        if !self.color_enabled() {
            return name.to_string();
        }

        // For symlinks, try to color based on target type
        if entry.metadata.is_symlink() {
            // Split symlink name and target if present
            if let Some((symlink_name, target_part)) = name.split_once(" -> ") {
                // Try to get target metadata for coloring
                let target_color_key = if let Ok(target_path) = fs::read_link(&entry.path) {
                    // Try to resolve absolute path
                    let resolved_target = if target_path.is_absolute() {
                        target_path.clone()
                    } else {
                        entry.path.parent().map_or(target_path.clone(), |parent| parent.join(&target_path))
                    };

                    // Try to get metadata of target
                    if let Ok(target_metadata) = fs::metadata(&resolved_target) {
                        Some(Self::ls_color_key_for_metadata(&target_metadata))
                    } else {
                        // Fall back to symlink color if target not accessible
                        Some("ln".to_string())
                    }
                } else {
                    // Fall back to symlink color
                    Some("ln".to_string())
                };

                if let Some(key) = target_color_key {
                    if let Some(color_seq) = self.ls_colors.get_color(&key) {
                        let color_code = format!("\x1b[{}m", color_seq);
                        return format!("{}{}\x1b[0m -> {}", color_code, symlink_name, target_part);
                    }
                }
            }
        }

        // Regular file or symlink without target display
        if let Some(code) = self.color_code_for_entry(entry) {
            format!("{}{}\x1b[0m", code, name)
        } else {
            name.to_string()
        }
    }
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

    let color = match matches.get_one::<String>("color") {
        Some(val) => match val.as_str() {
            "always" => ColorOption::Always,
            "auto" => ColorOption::Auto,
            "never" => ColorOption::Never,
            _ => ColorOption::Never, // shouldn't happen
        },
        None => ColorOption::Never,
    };

    let time_style = match matches.get_one::<String>("time_style") {
        Some(val) => parse_time_style(val),
        None => env::var("TIME_STYLE").ok().and_then(|s| parse_time_style(&s)),
    };

    let ls_colors = LsColors::from_env();

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
        color,
        time_style,
        ls_colors,
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
        .arg(Arg::new("color")
            .long("color")
            .value_parser(["never", "always", "auto"])
            .num_args(0..=1)
            .default_missing_value("always")
            .help("Colorize the output; WHEN can be 'never', 'always', or 'auto'"))
        .arg(Arg::new("time_style")
            .long("time-style")
            .help("Time/date format; can be full-iso, long-iso, iso, locale, or +FORMAT"))
        .arg(Arg::new("help").long("help").action(ArgAction::Help).help("Print help information"))
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

fn get_long_entry_fields(entry: &FileEntry, options: &LsOptions) -> Result<LongEntryFields> {
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
    let size_str = format_size(stat.size, options.human_readable);

    // Date formatting
    let date_str = format_time(stat.mtime, options.time_style.as_ref());

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

    Ok(LongEntryFields {
        permissions,
        nlink,
        owner,
        group,
        size_str,
        date_str,
        name,
    })
}

fn format_long_entry(fields: &LongEntryFields, entry: &FileEntry, options: &LsOptions, widths: &ColumnWidths) -> Result<String> {
    let colored_name = options.colorize_name(entry, &fields.name);
    Ok(format!("{} {:>width_nlink$} {:<width_owner$} {:<width_group$} {:>width_size$} {} {}",
        fields.permissions, fields.nlink, fields.owner, fields.group, fields.size_str, fields.date_str, colored_name,
        width_nlink = widths.nlink,
        width_owner = widths.owner,
        width_group = widths.group,
        width_size = widths.size))
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
    if options.long {
        let mut fields_vec = Vec::new();
        let mut widths = ColumnWidths { nlink: 0, owner: 0, group: 0, size: 0 };
        for entry in &file_entries {
            let fields = get_long_entry_fields(entry, options)?;
            widths.nlink = widths.nlink.max(fields.nlink.to_string().len());
            widths.owner = widths.owner.max(fields.owner.len());
            widths.group = widths.group.max(fields.group.len());
            widths.size = widths.size.max(fields.size_str.len());
            fields_vec.push(fields);
        }
        for (entry, fields) in file_entries.iter().zip(fields_vec.iter()) {
            let line = format_long_entry(fields, entry, options, &widths)?;
            println!("{}", line);
        }
    } else {
        for entry in &file_entries {
            let mut display_name = entry.name.clone();
            if options.classify {
                let indicator = get_classify_indicator(entry);
                if indicator != '\0' {
                    display_name.push(indicator);
                }
            }
            let colored_name = options.colorize_name(entry, &display_name);
            println!("{}", colored_name);
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
        let fields = get_long_entry_fields(entry, options)?;
        let widths = ColumnWidths {
            nlink: fields.nlink.to_string().len().max(1),
            owner: fields.owner.len().max(1),
            group: fields.group.len().max(1),
            size: fields.size_str.len().max(1),
        };
        let line = format_long_entry(&fields, entry, options, &widths)?;
        println!("{}", line);
    } else {
        let mut display_name = entry.name.clone();
        if options.classify {
            let indicator = get_classify_indicator(entry);
            if indicator != '\0' {
                display_name.push(indicator);
            }
        }
        let colored_name = options.colorize_name(entry, &display_name);
        println!("{}", colored_name);
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
