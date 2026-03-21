use clap::{Arg, ArgAction, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::env;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::{BufRead, BufReader};
use std::path::Path;
#[cfg(unix)]
use crate::posix::{posix_statfs, PosixStatFs, PosixError};

/// Normalized statfs fields for df output (POSIX statfs on Unix; GetDiskFreeSpaceExW on Windows).
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct DfFsStat {
    f_bsize: u64,
    f_blocks: u64,
    f_bfree: u64,
    f_bavail: u64,
    f_files: u64,
    f_ffree: u64,
}

#[cfg(unix)]
fn statfs_to_df(mut s: PosixStatFs, inodes: bool) -> DfFsStat {
    if inodes {
        s.f_blocks = s.f_files;
        s.f_bfree = s.f_ffree;
        s.f_bavail = s.f_ffree;
        s.f_bsize = 1;
    }
    DfFsStat {
        f_bsize: s.f_bsize,
        f_blocks: s.f_blocks,
        f_bfree: s.f_bfree,
        f_bavail: s.f_bavail,
        f_files: s.f_files,
        f_ffree: s.f_ffree,
    }
}

#[cfg(windows)]
fn statfs_to_df_windows(
    total: u64,
    free_total: u64,
    avail: u64,
    bsize: u64,
    _inodes: bool,
) -> DfFsStat {
    let blocks = (total + bsize - 1) / bsize;
    let bfree = (free_total + bsize - 1) / bsize;
    let bavail = (avail + bsize - 1) / bsize;
    DfFsStat {
        f_bsize: bsize,
        f_blocks: blocks,
        f_bfree: bfree,
        f_bavail: bavail,
        f_files: 0,
        f_ffree: 0,
    }
}

#[derive(Debug)]
pub struct DfOptions {
    pub posix_format: bool,
    pub human_readable: bool,
    pub human_decimal: bool,
    pub mega_bytes: bool,
    pub kilo_bytes: bool,
    pub block_size: Option<u64>,
    pub show_fs_type: bool,
    pub inodes: bool,
    pub all_filesystems: bool,
    pub fs_type_filter: Option<String>,
    pub filesystems: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<DfOptions> {
    let posix_format = matches.get_flag("posix");
    let human_readable = matches.get_flag("human-readable");
    let human_decimal = matches.get_flag("human-decimal");
    let mega_bytes = matches.get_flag("mega-bytes");
    let kilo_bytes = matches.get_flag("kilo-bytes");
    let block_size = matches.get_one::<String>("block-size").map(|s| s.to_string());
    let show_fs_type = matches.get_flag("print-type");
    let inodes = matches.get_flag("inodes");
    let all_filesystems = matches.get_flag("all");
    let fs_type_filter = matches.get_one::<String>("type").map(|s| s.to_string());
    let filesystems: Vec<String> = matches.get_many::<String>("filesystem")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let block_size_parsed = if let Some(bs) = block_size {
        Some(parse_block_size(&bs)?)
    } else {
        None
    };

    Ok(DfOptions {
        posix_format,
        human_readable,
        human_decimal,
        mega_bytes,
        kilo_bytes,
        block_size: block_size_parsed,
        show_fs_type,
        inodes,
        all_filesystems,
        fs_type_filter,
        filesystems,
    })
}

fn parse_block_size(s: &str) -> Result<u64> {
    if s.is_empty() {
        return Err(eyre!("empty block size"));
    }

    let suffix = s.chars().last().unwrap();
    let (num_str, multiplier) = match suffix {
        '0'..='9' => (s, 1),
        'K' | 'k' => (&s[..s.len()-1], 1024),
        'M' | 'm' => (&s[..s.len()-1], 1024 * 1024),
        'G' | 'g' => (&s[..s.len()-1], 1024 * 1024 * 1024),
        'T' | 't' => (&s[..s.len()-1], 1024 * 1024 * 1024 * 1024),
        'P' | 'p' => (&s[..s.len()-1], 1024 * 1024 * 1024 * 1024 * 1024),
        'E' | 'e' => (&s[..s.len()-1], 1024 * 1024 * 1024 * 1024 * 1024 * 1024),
        _ => (s, 1),
    };

    let num: u64 = num_str.parse()
        .map_err(|_| eyre!("invalid block size: '{}'", s))?;
    Ok(num * multiplier)
}

pub fn command() -> Command {
    Command::new("df")
        .about("Report filesystem disk space usage")
        .disable_help_flag(true)
        .arg(
            Arg::new("posix")
                .short('P')
                .long("portability")
                .help("Use POSIX output format")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("human-readable")
                .short('h')
                .help("Print sizes in human readable format (e.g., 1K 234M 2G)")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("human-decimal")
                .short('H')
                .help("Same as -h, but use powers of 1000 instead of 1024")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("mega-bytes")
                .short('m')
                .help("Display values in 1MB blocks")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("kilo-bytes")
                .short('k')
                .help("Display values in 1KB blocks (default)")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("block-size")
                .short('B')
                .long("block-size")
                .help("Scale sizes by SIZE before printing them (e.g., '-BM' prints sizes in megabytes)")
                .value_name("SIZE")
        )
        .arg(
            Arg::new("print-type")
                .short('T')
                .help("Print filesystem type")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("inodes")
                .short('i')
                .help("List inode information instead of block usage (Unix only; not supported on Windows)")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("all")
                .short('a')
                .long("all")
                .help("Include dummy, duplicate or inaccessible filesystems")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("type")
                .short('t')
                .help("Limit listing to filesystems of type TYPE")
                .value_name("TYPE")
        )
        .arg(
            Arg::new("filesystem")
                .num_args(0..)
                .help("Filesystem or mount point to show (default: all mounted filesystems)")
        )
        .arg(Arg::new("help").long("help").action(clap::ArgAction::Help))
}

fn format_blocks(blocks: u64, block_size: u64, display_block_size: u64, human_readable: bool, human_decimal: bool, _posix_format: bool) -> String {
    if human_readable || human_decimal {
        format_size_human(blocks * block_size, human_decimal)
    } else if display_block_size == 1 {
        blocks.to_string()
    } else {
        let total_bytes = blocks as u128 * block_size as u128;
        let display_blocks = (total_bytes + (display_block_size as u128 / 2)) / display_block_size as u128;
        display_blocks.to_string()
    }
}

fn format_size_human(size: u64, decimal: bool) -> String {
    const UNITS_BINARY: &[&str] = &["B", "K", "M", "G", "T", "P", "E"];
    const UNITS_DECIMAL: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB", "EB"];

    let units = if decimal { UNITS_DECIMAL } else { UNITS_BINARY };
    let base = if decimal { 1000.0 } else { 1024.0 };

    let mut size_f = size as f64;
    let mut unit_idx = 0;

    while size_f >= base && unit_idx < units.len() - 1 {
        size_f /= base;
        unit_idx += 1;
    }

    if unit_idx == 0 {
        format!("{}", size as u64)
    } else {
        format!("{:.1}{}", size_f, units[unit_idx])
    }
}

#[cfg(unix)]
fn read_mount_table() -> Result<Vec<(String, String, String)>> {
    let file = File::open("/proc/mounts").or_else(|_| File::open("/etc/mtab"))?;
    let reader = BufReader::new(file);
    let mut mounts = Vec::new();

    for line in reader.lines() {
        let line = line?;
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            let device = parts[0].to_string();
            let mount_point = parts[1].to_string();
            let fs_type = parts[2].to_string();
            mounts.push((device, mount_point, fs_type));
        }
    }

    Ok(mounts)
}

#[cfg(windows)]
mod win_df {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::PathBuf;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetLogicalDriveStringsW(n_buffer_length: u32, lp_buffer: *mut u16) -> u32;
        fn GetDiskFreeSpaceExW(
            lp_directory_name: *const u16,
            lp_free_bytes_available: *mut u64,
            lp_total_number_of_bytes: *mut u64,
            lp_total_free_bytes: *mut u64,
        ) -> i32;
        fn GetVolumeInformationW(
            lp_root_path_name: *const u16,
            lp_volume_name_buffer: *mut u16,
            n_volume_name_size: u32,
            lp_volume_serial_number: *mut u32,
            lp_max_component_len: *mut u32,
            lp_file_system_flags: *mut u32,
            lp_file_system_name_buffer: *mut u16,
            n_file_system_name_size: u32,
        ) -> i32;
    }

    fn to_wide_nul(path: &OsStr) -> Vec<u16> {
        path.encode_wide().chain(std::iter::once(0)).collect()
    }

    pub fn read_mount_table() -> Result<Vec<(String, String, String)>> {
        let mut buf = vec![0u16; 512];
        let n = unsafe { GetLogicalDriveStringsW(buf.len() as u32, buf.as_mut_ptr()) };
        if n == 0 {
            return Err(eyre!("GetLogicalDriveStringsW failed: {}", std::io::Error::last_os_error()));
        }
        let buf = &buf[..n as usize];
        let mut mounts = Vec::new();
        let mut start = 0;
        while start < buf.len() {
            let end = buf[start..].iter().position(|&c| c == 0).map(|i| start + i).unwrap_or(buf.len());
            if end == start {
                break;
            }
            let wide: Vec<u16> = buf[start..end].to_vec();
            let root = OsString::from_wide(&wide)
                .to_string_lossy()
                .into_owned();
            if !root.is_empty() {
                let trimmed = root.trim_end_matches('\\').to_string();
                let device = if trimmed.len() == 2 && trimmed.ends_with(':') {
                    trimmed.clone()
                } else {
                    trimmed.clone()
                };
                let fs_type = volume_fs_type(&root)?;
                mounts.push((device, root, fs_type));
            }
            start = end + 1;
        }
        Ok(mounts)
    }

    pub fn volume_fs_type(root: &str) -> Result<String> {
        let mut root_pb = PathBuf::from(root);
        if !root.ends_with('\\') {
            root_pb.push("");
        }
        let wide = to_wide_nul(root_pb.as_os_str());
        let mut fs_name = vec![0u16; 32];
        let ok = unsafe {
            GetVolumeInformationW(
                wide.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs_name.as_mut_ptr(),
                fs_name.len() as u32,
            )
        };
        if ok == 0 {
            return Ok("unknown".to_string());
        }
        let len = fs_name.iter().position(|&x| x == 0).unwrap_or(fs_name.len());
        Ok(String::from_utf16_lossy(&fs_name[..len]))
    }

    pub fn disk_space_for_root(root: &str) -> Result<(u64, u64, u64)> {
        let mut root_pb = PathBuf::from(root);
        if !root.ends_with('\\') {
            root_pb.push("");
        }
        let wide = to_wide_nul(root_pb.as_os_str());
        let mut avail = 0u64;
        let mut total = 0u64;
        let mut free = 0u64;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut avail,
                &mut total,
                &mut free,
            )
        };
        if ok == 0 {
            return Err(eyre!("GetDiskFreeSpaceExW for '{}': {}", root, std::io::Error::last_os_error()));
        }
        Ok((total, free, avail))
    }

    pub fn volume_root_for_path(path: &Path) -> PathBuf {
        use std::path::Prefix;
        let mut c = path.components();
        match c.next() {
            Some(std::path::Component::Prefix(pref)) => match pref.kind() {
                Prefix::VerbatimDisk(d) | Prefix::Disk(d) => {
                    PathBuf::from(format!("{}:\\", char::from(d)))
                }
                _ => path.to_path_buf(),
            },
            _ => {
                let s = path.to_string_lossy();
                if s.len() >= 2 && s.as_bytes()[1] == b':' {
                    PathBuf::from(format!("{}\\", &s[..2]))
                } else {
                    path.to_path_buf()
                }
            }
        }
    }
}

#[cfg(unix)]
fn get_fs_stats(path: &str, inodes: bool) -> Result<DfFsStat> {
    let stats = posix_statfs(path)
        .map_err(|e| match e {
            PosixError::Io(ioe) => eyre!("cannot read filesystem statistics for '{}': {}", path, ioe),
            PosixError::InvalidArgument(m) => eyre!("cannot read filesystem statistics for '{}': {}", path, m),
            PosixError::NotFound => eyre!("cannot read filesystem statistics for '{}': not found", path),
        })?;
    Ok(statfs_to_df(stats, inodes))
}

#[cfg(windows)]
fn get_fs_stats(path: &str, inodes: bool) -> Result<DfFsStat> {
    let _ = inodes;
    let (total, free, avail) = win_df::disk_space_for_root(path)?;
    let bsize = 512u64;
    Ok(statfs_to_df_windows(total, free, avail, bsize, false))
}

#[cfg(windows)]
fn read_mount_table() -> Result<Vec<(String, String, String)>> {
    win_df::read_mount_table()
}

fn calculate_percent(used: u64, total: u64) -> u64 {
    if total == 0 {
        0
    } else {
        let mut used_scaled = used as u128;
        let mut total_scaled = total as u128;

        while total_scaled >= u64::MAX as u128 / 100 {
            used_scaled >>= 1;
            total_scaled >>= 1;
        }

        let used_scaled_u64 = used_scaled as u64;
        let total_scaled_u64 = total_scaled as u64;

        (used_scaled_u64 * 100 + total_scaled_u64 / 2) / total_scaled_u64
    }
}

fn df_resolve_display_block_size(options: &DfOptions) -> u64 {
    let mut display_block_size = if options.kilo_bytes {
        1024
    } else if options.mega_bytes {
        1024 * 1024
    } else if let Some(bs) = options.block_size {
        bs
    } else if env::var("POSIXLY_CORRECT").is_ok() && !options.kilo_bytes {
        512
    } else {
        1024
    };
    if options.inodes {
        display_block_size = 1;
    }
    display_block_size
}

fn df_resolve_mounts(options: &DfOptions) -> Result<Vec<(String, String, String)>> {
    if options.filesystems.is_empty() {
        return read_mount_table();
    }
    let all_mounts = read_mount_table()?;
    let mut filtered = Vec::new();

    for fs in &options.filesystems {
        let path = Path::new(fs);
        let mut found = None;
        for (device, mount_point, fs_type) in &all_mounts {
            #[cfg(unix)]
            if fs == device || fs == mount_point || path.starts_with(mount_point) {
                found = Some((device.clone(), mount_point.clone(), fs_type.clone()));
                break;
            }
            #[cfg(windows)]
            {
                let fsl = fs.to_lowercase();
                let mpl = mount_point.to_lowercase();
                let pl = path.to_string_lossy().to_lowercase();
                if fsl == device.to_lowercase() || fsl == mpl || pl.starts_with(&mpl.trim_end_matches('\\'))
                    || pl.starts_with(&format!("{}\\", mpl.trim_end_matches('\\')).to_lowercase())
                {
                    found = Some((device.clone(), mount_point.clone(), fs_type.clone()));
                    break;
                }
            }
        }

        if let Some(mount) = found {
            filtered.push(mount);
        } else {
            #[cfg(windows)]
            {
                let root = win_df::volume_root_for_path(path);
                let mut root_str = root.to_string_lossy().into_owned();
                if !root_str.ends_with('\\') {
                    root_str.push('\\');
                }
                let fs_type = win_df::volume_fs_type(&root_str).unwrap_or_else(|_| "unknown".to_string());
                let dev = root_str.trim_end_matches('\\').to_string();
                filtered.push((dev, root_str, fs_type));
            }
            #[cfg(not(windows))]
            return Err(eyre!("cannot find mount point for '{}'", fs));
        }
    }

    Ok(filtered)
}

fn print_df_table_header(options: &DfOptions, display_block_size: u64) {
    if options.posix_format {
        print!("Filesystem          ");
        if options.show_fs_type {
            print!("Type          ");
        }
        print!(
            "{}%-blocks   Used Available Capacity Mounted on\n",
            if display_block_size == 1 {
                String::new()
            } else {
                display_block_size.to_string()
            }
        );
    } else {
        print!("Filesystem          ");
        if options.show_fs_type {
            print!("Type          ");
        }

        if options.human_readable || options.human_decimal {
            print!("     Size      Used   Avail Use% ");
        } else if display_block_size == 1 {
            print!("   Inodes      IUsed    IFree IUse% ");
        } else {
            print!(
                "{}%-blocks      Used Available Use% ",
                if options.posix_format {
                    String::new()
                } else {
                    display_block_size.to_string()
                }
            );
        }

        println!("Mounted on");
    }
}

pub fn run(options: DfOptions) -> Result<()> {
    #[cfg(windows)]
    if options.inodes {
        return Err(eyre!("df: inode counts (-i) are not supported on Windows"));
    }

    let display_block_size = df_resolve_display_block_size(&options);
    let mounts = df_resolve_mounts(&options)?;
    print_df_table_header(&options, display_block_size);

    for (device, mount_point, fs_type) in mounts {
        #[cfg(unix)]
        if !options.all_filesystems && device == "rootfs" {
            continue;
        }

        if let Some(filter_type) = &options.fs_type_filter {
            if &fs_type != filter_type {
                continue;
            }
        }

        let stats = match get_fs_stats(&mount_point, options.inodes) {
            Ok(stats) => stats,
            Err(e) => {
                if options.all_filesystems {
                    eprintln!("df: {}: {}", mount_point, e);
                    continue;
                } else {
                    return Err(e);
                }
            }
        };

        if stats.f_blocks == 0 && !options.all_filesystems && options.filesystems.is_empty() {
            continue;
        }

        let used_blocks = stats.f_blocks - stats.f_bfree;
        let percent_used = calculate_percent(used_blocks, used_blocks + stats.f_bavail);

        let device_display = if device.len() > 20 && !options.posix_format {
            format!("{}\n{:20}", device, "")
        } else {
            format!("{:20}", device)
        };
        print!("{}", device_display);

        if options.show_fs_type {
            let type_display = if fs_type.len() > 10 && !options.posix_format {
                format!(" {}\n{:31}", fs_type, "")
            } else {
                format!(" {:10}", fs_type)
            };
            print!("{}", type_display);
        }

        let total_display = format_blocks(
            stats.f_blocks,
            stats.f_bsize,
            display_block_size,
            options.human_readable,
            options.human_decimal,
            options.posix_format,
        );
        print!(" {:>9}", total_display);

        let used_display = format_blocks(
            used_blocks,
            stats.f_bsize,
            display_block_size,
            options.human_readable,
            options.human_decimal,
            options.posix_format,
        );
        print!(" {:>9}", used_display);

        let avail_display = format_blocks(
            stats.f_bavail,
            stats.f_bsize,
            display_block_size,
            options.human_readable,
            options.human_decimal,
            options.posix_format,
        );
        print!(" {:>9}", avail_display);

        print!(" {:>3}%", percent_used);

        println!(" {}", mount_point);
    }

    Ok(())
}
