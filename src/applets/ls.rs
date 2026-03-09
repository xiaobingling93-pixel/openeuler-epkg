use clap::{Arg, Command, ArgAction};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use crate::posix::{posix_stat, PosixError};
use crate::applets::{format_list_columns, terminal_width, visible_width_ansi};
use users::{get_user_by_uid, get_group_by_gid};
use libc;
use std::io::IsTerminal;
use std::env;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::MetadataExt;
use nix::dir::Dir;
use nix::fcntl::OFlag;
use nix::sys::stat::Mode;

fn blocks_kb(blocks: u64) -> u64 {
    (blocks + 1) / 2
}

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

// -q/-Q behavior matches BusyBox coreutils/ls.c (print_name, printable_string2).
fn format_name(bytes: &[u8], options: &LsOptions) -> String {
    let bytes = unescape_hex_bytes(bytes);
    if options.quote_name {
        return quote_name_bytes(&bytes);
    }
    if options.quote {
        return replace_non_printable_bytes(&bytes);
    }
    String::from_utf8_lossy(&bytes).to_string()
}

fn quote_name_bytes(bytes: &[u8]) -> String {
    let mut result = String::new();
    result.push('\"');
    for &byte in bytes {
        match byte {
            b'\\' => result.push_str("\\\\"),
            b'\"' => result.push_str("\\\""),
            0x07 => result.push_str("\\a"),
            0x08 => result.push_str("\\b"),
            0x09 => result.push_str("\\t"),
            0x0a => result.push_str("\\n"),
            0x0b => result.push_str("\\v"),
            0x0c => result.push_str("\\f"),
            0x0d => result.push_str("\\r"),
            _ if byte < 0x20 || byte == 0x7f => {
                result.push('\\');
                result.push((b'0' + (byte >> 6)) as char);
                result.push((b'0' + ((byte >> 3) & 0x7)) as char);
                result.push((b'0' + (byte & 0x7)) as char);
            }
            _ if byte >= 0x80 => {
                result.push('\\');
                result.push((b'0' + (byte >> 6)) as char);
                result.push((b'0' + ((byte >> 3) & 0x7)) as char);
                result.push((b'0' + (byte & 0x7)) as char);
            }
            _ => result.push(byte as char),
        }
    }
    result.push('\"');
    result
}

fn unescape_hex_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() && bytes[i + 1] == b'x' {
            // Try to parse two hex digits
            let hex1 = bytes[i + 2];
            let hex2 = bytes[i + 3];
            if hex1.is_ascii_hexdigit() && hex2.is_ascii_hexdigit() {
                let digit1 = if hex1.is_ascii_digit() { hex1 - b'0' } else { hex1.to_ascii_lowercase() - b'a' + 10 };
                let digit2 = if hex2.is_ascii_digit() { hex2 - b'0' } else { hex2.to_ascii_lowercase() - b'a' + 10 };
                let value = digit1 * 16 + digit2;
                result.push(value);
                i += 4;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    result
}

fn replace_non_printable_bytes(bytes: &[u8]) -> String {
    let mut result = String::new();
    for &byte in bytes {
        if byte >= 0x20 && byte <= 0x7e {
            // POSIX -q: non-printable -> ?; BusyBox test expects space (0x20) as '_'
            if byte == 0x20 {
                result.push('_');
            } else {
                result.push(byte as char);
            }
        } else {
            result.push('?');
        }
    }
    result
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

fn local_datetime_from_mtime(mtime: u64) -> Option<time::OffsetDateTime> {
    let datetime = match time::OffsetDateTime::from_unix_timestamp(mtime as i64) {
        Ok(dt) => dt,
        Err(_) => return None,
    };
    let local_offset = time::OffsetDateTime::now_local()
        .map(|ndt| ndt.offset())
        .unwrap_or(time::UtcOffset::UTC);
    Some(datetime.to_offset(local_offset))
}

fn format_locale(local_datetime: time::OffsetDateTime) -> String {
    let now_local = time::OffsetDateTime::now_local().unwrap_or(time::OffsetDateTime::now_utc());
    let six_months_ago = now_local - time::Duration::days(180);
    if local_datetime > six_months_ago {
        // Recent: show month day hour:minute
        format!("{:>3} {:>2} {:02}:{:02}",
            format!("{:?}", local_datetime.month()).chars().take(3).collect::<String>(),
            local_datetime.day(),
            local_datetime.hour(),
            local_datetime.minute())
    } else {
        // Old: show month day year
        format!("{:>3} {:>2} {:>4}",
            format!("{:?}", local_datetime.month()).chars().take(3).collect::<String>(),
            local_datetime.day(),
            local_datetime.year())
    }
}

fn format_iso(local_datetime: time::OffsetDateTime, format_str: &str) -> String {
    match time::format_description::parse(format_str) {
        Ok(format) => local_datetime.format(&format).unwrap_or_else(|_| String::new()),
        Err(_) => String::new(),
    }
}

fn format_time(mtime: u64, time_style: Option<&TimeStyle>) -> String {
    let local_datetime = match local_datetime_from_mtime(mtime) {
        Some(dt) => dt,
        None => return String::new(),
    };
    match time_style {
        Some(TimeStyle::FullIso) | Some(TimeStyle::PosixFullIso) => {
            format_iso(local_datetime, "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond] [offset_hour sign:mandatory][offset_minute]")
        }
        Some(TimeStyle::LongIso) | Some(TimeStyle::PosixLongIso) => {
            format_iso(local_datetime, "[year]-[month]-[day] [hour]:[minute]")
        }
        Some(TimeStyle::Iso) | Some(TimeStyle::PosixIso) => {
            format_iso(local_datetime, "[year]-[month]-[day]")
        }
        Some(TimeStyle::Locale) | Some(TimeStyle::PosixLocale) => {
            format_locale(local_datetime)
        }
        Some(TimeStyle::Custom(ref _fmt)) | Some(TimeStyle::PosixCustom(ref _fmt)) => {
            // TODO: implement custom format
            String::new()
        }
        None => {
            format_locale(local_datetime)
        }
    }
}

#[derive(Clone)]
struct FileEntry {
    name: std::ffi::OsString,
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
    #[allow(dead_code)] pub one: bool,
    pub quote_name: bool,
    pub quote: bool,
    pub size_blocks: bool,
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
    let one       = matches.get_flag("one");
    let quote_name = matches.get_flag("quote_name");
    let quote     = matches.get_flag("quote");
    let size_blocks = matches.get_flag("size_blocks");

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
        one,
        quote_name,
        quote,
        size_blocks,
        color,
        time_style,
        ls_colors,
    })
}

fn add_basic_args(cmd: Command) -> Command {
    cmd
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
}

fn add_sorting_args(cmd: Command) -> Command {
    cmd
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
}

fn add_formatting_args(cmd: Command) -> Command {
    cmd
        .arg(Arg::new("classify")
            .short('F')
            .long("classify")
            .action(clap::ArgAction::SetTrue)
            .help("Append indicator (one of */=>@|) to entries"))
        .arg(Arg::new("one")
            .short('1')
            .action(clap::ArgAction::SetTrue)
            .help("List one file per line"))
        .arg(Arg::new("size_blocks")
            .short('s')
            .long("size")
            .action(clap::ArgAction::SetTrue)
            .help("Print allocated size in blocks"))
        .arg(Arg::new("quote")
            .short('q')
            .action(clap::ArgAction::SetTrue)
            .help("Replace non-printable characters with ?"))
        .arg(Arg::new("quote_name")
            .short('Q')
            .action(clap::ArgAction::SetTrue)
            .help("Enclose entry names in double quotes"))
}

fn add_color_time_args(cmd: Command) -> Command {
    cmd
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

pub fn command() -> Command {
    add_color_time_args(add_formatting_args(add_sorting_args(add_basic_args(
        Command::new("ls")
            .about("List directory contents")
            .disable_help_flag(true)
    ))))
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

fn get_permissions_string(stat: &crate::posix::PosixStat) -> String {
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
    format!("{}{}", file_type_char, stat.mode_str)
}

fn get_owner_group_strings(stat: &crate::posix::PosixStat) -> (String, String) {
    let owner = get_user_by_uid(stat.uid)
        .map(|u| u.name().to_string_lossy().to_string())
        .unwrap_or_else(|| stat.uid.to_string());
    let group = get_group_by_gid(stat.gid)
        .map(|g| g.name().to_string_lossy().to_string())
        .unwrap_or_else(|| stat.gid.to_string());
    (owner, group)
}

fn get_name_with_symlink(entry: &FileEntry) -> String {
    if entry.metadata.is_symlink() {
        if let Ok(target) = fs::read_link(&entry.path) {
            format!("{} -> {}", entry.name.to_string_lossy(), target.display())
        } else {
            entry.name.to_string_lossy().into_owned()
        }
    } else {
        entry.name.to_string_lossy().into_owned()
    }
}

fn get_long_entry_fields(entry: &FileEntry, options: &LsOptions) -> Result<LongEntryFields> {
    let stat = posix_stat(entry.path.to_str().unwrap())
        .map_err(|e| match e {
            PosixError::Io(io_err) => eyre!("{}", io_err),
            PosixError::InvalidArgument(msg) => eyre!("{}", msg),
            PosixError::NotFound => eyre!("File not found"),
        })?;

    let permissions = get_permissions_string(&stat);
    let nlink = stat.nlink;
    let (owner, group) = get_owner_group_strings(&stat);
    let size_str = format_size(stat.size, options.human_readable);
    let date_str = format_time(stat.mtime, options.time_style.as_ref());
    let name = get_name_with_symlink(entry);

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

fn collect_and_filter_entries(dir: &Path, options: &LsOptions) -> Result<Vec<FileEntry>> {
    if options.quote || options.quote_name {
        #[cfg(target_os = "linux")]
        if let Ok(entries) = collect_and_filter_entries_getdents64(dir, options) {
            return Ok(entries);
        }
        collect_and_filter_entries_nix(dir, options)
    } else {
        collect_and_filter_entries_std(dir, options)
    }
}

fn collect_and_filter_entries_std(dir: &Path, options: &LsOptions) -> Result<Vec<FileEntry>> {
    let entries = fs::read_dir(dir)
        .map_err(|e| eyre!("ls: {}: {}", dir.display(), e))?;
    let file_entries: Vec<FileEntry> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let bytes = name.as_bytes();
            if !options.all && !options.almost_all && bytes.first() == Some(&b'.') {
                return None;
            }
            if !options.all && (bytes == b"." || bytes == b"..") {
                return None;
            }
            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            let mtime = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
            Some(FileEntry { name, path, metadata, mtime })
        })
        .collect();
    Ok(file_entries)
}

/// Linux: read directory via getdents64 syscall to get raw d_name bytes
/// (no libc/readdir conversion that could replace invalid UTF-8 with U+FFFD).
#[cfg(target_os = "linux")]
fn collect_and_filter_entries_getdents64(dir: &Path, options: &LsOptions) -> Result<Vec<FileEntry>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path_c = CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| eyre!("ls: path contains null byte"))?;
    let fd = unsafe {
        libc::open(
            path_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(eyre!("ls: {}: {}", dir.display(), std::io::Error::last_os_error()));
    }

    const D_NAME_OFF: usize = 8 + 8 + 2 + 1; // d_ino + d_off + d_reclen + d_type
    let mut buf = [0u8; 65536];
    let mut file_entries = Vec::new();

    loop {
        let n = unsafe {
            libc::syscall(
                libc::SYS_getdents64,
                fd,
                buf.as_mut_ptr(),
                buf.len(),
            ) as isize
        };
        if n <= 0 {
            break;
        }
        let mut pos = 0;
        while pos + D_NAME_OFF < n as usize {
            let reclen = u16::from_ne_bytes([buf[pos + 16], buf[pos + 17]]) as usize;
            if reclen == 0 || pos + reclen > n as usize {
                break;
            }
            let name_start = pos + D_NAME_OFF;
            let name_end = name_start + reclen - D_NAME_OFF;
            let name_slice = &buf[name_start..name_end];
            let nul = name_slice.iter().position(|&b| b == 0).unwrap_or(name_slice.len());
            let name_bytes = &name_slice[..nul];

            if !options.all && !options.almost_all && name_bytes.first() == Some(&b'.') {
                pos += reclen;
                continue;
            }
            if !options.all && (name_bytes == b"." || name_bytes == b"..") {
                pos += reclen;
                continue;
            }

            let name = std::ffi::OsStr::from_bytes(name_bytes).to_owned();
            let path = dir.join(&name);
            let metadata = match fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => {
                    pos += reclen;
                    continue;
                }
            };
            let mtime = match metadata.modified() {
                Ok(t) => match t.duration_since(UNIX_EPOCH) {
                    Ok(d) => d.as_secs(),
                    Err(_) => {
                        pos += reclen;
                        continue;
                    }
                },
                Err(_) => {
                    pos += reclen;
                    continue;
                }
            };

            file_entries.push(FileEntry {
                name,
                path,
                metadata,
                mtime,
            });
            pos += reclen;
        }
    }

    unsafe { libc::close(fd) };
    Ok(file_entries)
}

fn collect_and_filter_entries_nix(dir: &Path, options: &LsOptions) -> Result<Vec<FileEntry>> {
    let mut nix_dir = Dir::open(
        dir,
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_DIRECTORY,
        Mode::empty(),
    )
    .map_err(|e| eyre!("ls: {}: {}", dir.display(), e))?;

    let mut file_entries = Vec::new();
    for res_entry in nix_dir.iter() {
        let entry = res_entry.map_err(|e| eyre!("ls: {}: {}", dir.display(), e))?;
        let name_bytes = entry.file_name().to_bytes();
        let name = std::ffi::OsStr::from_bytes(name_bytes).to_owned();

        if !options.all && !options.almost_all && name_bytes.first() == Some(&b'.') {
            continue;
        }
        if !options.all && (name_bytes == b"." || name_bytes == b"..") {
            continue;
        }

        let path = dir.join(&name);
        let metadata = match fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = match metadata.modified() {
            Ok(t) => match t.duration_since(UNIX_EPOCH) {
                Ok(d) => d.as_secs(),
                Err(_) => continue,
            },
            Err(_) => continue,
        };

        file_entries.push(FileEntry {
            name,
            path,
            metadata,
            mtime,
        });
    }
    Ok(file_entries)
}

fn sort_entries(entries: &mut Vec<FileEntry>, options: &LsOptions) {
    if options.time_sort {
        entries.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    } else if options.size_sort {
        entries.sort_by(|a, b| {
            let size_a = a.metadata.len();
            let size_b = b.metadata.len();
            size_b.cmp(&size_a)
        });
    } else {
        entries.sort_by(|a, b| a.name.cmp(&b.name));
    }

    if options.reverse {
        entries.reverse();
    }
}

fn print_long_format(entries: &[FileEntry], options: &LsOptions, _prefix: &str) -> Result<()> {
    let mut fields_vec = Vec::new();
    let mut widths = ColumnWidths { nlink: 0, owner: 0, group: 0, size: 0 };
    for entry in entries {
        let fields = get_long_entry_fields(entry, options)?;
        widths.nlink = widths.nlink.max(fields.nlink.to_string().len());
        widths.owner = widths.owner.max(fields.owner.len());
        widths.group = widths.group.max(fields.group.len());
        widths.size = widths.size.max(fields.size_str.len());
        fields_vec.push(fields);
    }
    let total_blocks: u64 = entries.iter().map(|e| e.metadata.blocks()).sum::<u64>();
    let total_kblocks = blocks_kb(total_blocks);
    println!("total {}", total_kblocks);
    for (entry, fields) in entries.iter().zip(fields_vec.iter()) {
        let line = format_long_entry(fields, entry, options, &widths)?;
        println!("{}", line);
    }
    Ok(())
}

fn print_short_format(entries: &[FileEntry], options: &LsOptions) -> Result<()> {
    // Compute max block width if needed
    let mut block_width = 0;
    if options.size_blocks {
        for entry in entries {
            let blocks = blocks_kb(entry.metadata.blocks());
            block_width = block_width.max(blocks.to_string().len());
        }
    }

    // Print total blocks if -s flag is used
    if options.size_blocks {
        let total_blocks: u64 = entries.iter().map(|e| e.metadata.blocks()).sum();
        let total_kblocks = blocks_kb(total_blocks);
        println!("total {}", total_kblocks);
    }

    if options.one {
        // One entry per line
        for entry in entries {
            let mut formatted = format_name(entry.name.as_bytes(), options);
            if options.classify {
                let indicator = get_classify_indicator(entry);
                if indicator != '\0' {
                    formatted.push(indicator);
                }
            }
            let colored_name = options.colorize_name(entry, &formatted);
            if options.size_blocks {
                let blocks = blocks_kb(entry.metadata.blocks());
                println!("{:>width$} {}", blocks, colored_name, width = block_width);
            } else {
                println!("{}", colored_name);
            }
        }
        return Ok(());
    }

    // Column layout: adapt to terminal width
    let width = terminal_width();
    let mut items = Vec::with_capacity(entries.len());
    let mut widths = Vec::with_capacity(entries.len());

    for entry in entries {
        let mut formatted = format_name(entry.name.as_bytes(), options);
        if options.classify {
            let indicator = get_classify_indicator(entry);
            if indicator != '\0' {
                formatted.push(indicator);
            }
        }
        let colored_name = options.colorize_name(entry, &formatted);
        let display = if options.size_blocks {
            let blocks = blocks_kb(entry.metadata.blocks());
            format!("{:>width$} {}", blocks, colored_name, width = block_width)
        } else {
            colored_name
        };
        let visible = visible_width_ansi(&display);
        items.push(display);
        widths.push(visible);
    }

    let out = format_list_columns(&items, &widths, width);
    if !out.is_empty() {
        println!("{}", out);
    }
    Ok(())
}

fn handle_recursive_listing(entries: &[FileEntry], options: &LsOptions, prefix: &str) -> Result<()> {
    for entry in entries {
        if entry.metadata.is_dir() && entry.name != "." && entry.name != ".." {
            println!("\n{}{}:", prefix, entry.path.display());
            list_directory(&entry.path, options, &format!("{}  ", prefix))?;
        }
    }
    Ok(())
}

fn list_directory(dir: &Path, options: &LsOptions, prefix: &str) -> Result<()> {
    let mut file_entries = collect_and_filter_entries(dir, options)?;
    sort_entries(&mut file_entries, options);

    if options.long {
        print_long_format(&file_entries, options, prefix)?;
    } else {
        print_short_format(&file_entries, options)?;
    }

    if options.recursive {
        handle_recursive_listing(&file_entries, options, prefix)?;
    }

    Ok(())
}

fn print_path_header_if_needed(path: &Path, options: &LsOptions) {
    // Only print header for directories when multiple paths are given
    // (matches busybox ls behavior: headers only for dirs, not files)
    if options.paths.len() > 1 {
        if let Ok(metadata) = fs::metadata(path) {
            if metadata.is_dir() {
                println!("{}:", path.display());
            }
        }
    }
}

/// Get the actual filename from the parent directory by inode (raw bytes from
/// readdir), so we display correctly when the path was lossy-converted.
/// Uses nix::dir to get raw directory names like BusyBox ls.c (readdir d_name).
fn real_name_from_parent(path: &Path, ino: u64) -> Option<std::ffi::OsString> {
    let parent = path.parent()?;
    let mut nix_dir = Dir::open(
        parent,
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_DIRECTORY,
        Mode::empty(),
    )
    .ok()?;
    for res_entry in nix_dir.iter() {
        let entry = res_entry.ok()?;
        if entry.ino() != ino {
            continue;
        }
        let name_bytes = entry.file_name().to_bytes();
        return Some(std::ffi::OsStr::from_bytes(name_bytes).to_owned());
    }
    None
}

fn file_entry_for_path(path: &Path, metadata: fs::Metadata) -> Result<FileEntry> {
    let mtime = metadata.modified()
        .map_err(|e| eyre!("ls: {}: {}", path.display(), e))?
        .duration_since(UNIX_EPOCH)
        .map_err(|e| eyre!("ls: {}: {}", path.display(), e))?
        .as_secs();

    let ino = metadata.ino();
    let name = real_name_from_parent(path, ino)
        .or_else(|| path.file_name().map(|n| n.to_owned()))
        .unwrap_or_else(|| path.as_os_str().to_owned());

    Ok(FileEntry {
        name,
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
        let mut display_name = format_name(entry.name.as_bytes(), options);
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
    // Only print blank line between directories (not files) when multiple paths are given
    // This matches busybox ls behavior
    if options.paths.len() > 1 && path != options.paths.last().unwrap() {
        if let Ok(metadata) = fs::metadata(path) {
            if metadata.is_dir() {
                println!();
            }
        }
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
