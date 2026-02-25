use clap::{Arg, ArgAction, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use crate::posix::{posix_statfs, PosixStatFs};

#[derive(Debug)]
pub struct DfOptions {
    // Display options
    pub posix_format: bool,
    pub human_readable: bool,
    pub human_decimal: bool,
    pub mega_bytes: bool,
    pub kilo_bytes: bool,
    pub block_size: Option<u64>,
    pub show_fs_type: bool,
    pub inodes: bool,
    pub all_filesystems: bool,

    // Filter options
    pub fs_type_filter: Option<String>,

    // Input
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

    // Parse block size if specified
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

/// Parse block size string with optional suffix (K, M, G, T, P, E)
fn parse_block_size(s: &str) -> Result<u64> {
    if s.is_empty() {
        return Err(eyre!("empty block size"));
    }

    // Check for suffix
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
                .help("List inode information instead of block usage")
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

/// Format size for display based on options
fn format_blocks(blocks: u64, block_size: u64, display_block_size: u64, human_readable: bool, human_decimal: bool, _posix_format: bool) -> String {
    if human_readable || human_decimal {
        format_size_human(blocks * block_size, human_decimal)
    } else if display_block_size == 1 {
        // For inodes mode or raw counts
        blocks.to_string()
    } else {
        // Convert to display blocks
        let total_bytes = blocks as u128 * block_size as u128;
        let display_blocks = (total_bytes + (display_block_size as u128 / 2)) / display_block_size as u128;
        display_blocks.to_string()
    }
}

/// Human readable size formatting (similar to ls.rs)
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

/// Read mount table from /proc/mounts or /etc/mtab
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

/// Get filesystem statistics for a mount point
fn get_fs_stats(path: &str, inodes: bool) -> Result<PosixStatFs> {
    let mut stats = posix_statfs(path)
        .map_err(|e| eyre!("cannot read filesystem statistics for '{}': {:?}", path, e))?;

    if inodes {
        // For inode mode, repurpose block fields as inode counts
        stats.f_blocks = stats.f_files;
        stats.f_bfree = stats.f_ffree;
        stats.f_bavail = stats.f_ffree;
        stats.f_bsize = 1; // Each "block" is one inode
    }

    Ok(stats)
}

/// Calculate usage percentage
fn calculate_percent(used: u64, total: u64) -> u64 {
    if total == 0 {
        0
    } else {
        // Scale down to avoid overflow
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

pub fn run(options: DfOptions) -> Result<()> {
    // Determine display block size based on options and environment
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

    // If showing inodes, display block size should be 1 (one inode per "block")
    if options.inodes {
        display_block_size = 1;
    }

    // Get mount list
    let mounts = if options.filesystems.is_empty() {
        read_mount_table()?
    } else {
        // For specified filesystems/mount points, we need to find their mount entries
        let all_mounts = read_mount_table()?;
        let mut filtered = Vec::new();

        for fs in &options.filesystems {
            let path = Path::new(fs);
            // Try to find mount point containing this path
            let mut found = None;
            for (device, mount_point, fs_type) in &all_mounts {
                if fs == device || fs == mount_point || path.starts_with(mount_point) {
                    // Use the mount point itself, not the subdirectory
                    found = Some((device.clone(), mount_point.clone(), fs_type.clone()));
                    break;
                }
            }

            if let Some(mount) = found {
                filtered.push(mount);
            } else {
                return Err(eyre!("cannot find mount point for '{}'", fs));
            }
        }

        filtered
    };

    // Print header
    if options.posix_format {
        print!("Filesystem          ");
        if options.show_fs_type {
            print!("Type          ");
        }
        print!("{}%-blocks   Used Available Capacity Mounted on\n",
            if display_block_size == 1 { String::new() } else { display_block_size.to_string() });
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
            print!("{}%-blocks      Used Available Use% ",
                if options.posix_format { String::new() } else { display_block_size.to_string() });
        }

        println!("Mounted on");
    }

    // Process each mount
    for (device, mount_point, fs_type) in mounts {
        // Skip rootfs if not showing all
        if !options.all_filesystems && device == "rootfs" {
            continue;
        }

        // Filter by filesystem type if specified
        if let Some(filter_type) = &options.fs_type_filter {
            if &fs_type != filter_type {
                continue;
            }
        }

        // Get filesystem statistics
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

        // Skip filesystems with 0 blocks unless specifically listed or -a
        if stats.f_blocks == 0 && !options.all_filesystems && options.filesystems.is_empty() {
            continue;
        }

        let used_blocks = stats.f_blocks - stats.f_bfree;
        let percent_used = calculate_percent(used_blocks, used_blocks + stats.f_bavail);

        // Format device name (truncate or pad to 20 chars)
        let device_display = if device.len() > 20 && !options.posix_format {
            format!("{}\n{:20}", device, "")
        } else {
            format!("{:20}", device)
        };
        print!("{}", device_display);

        // Filesystem type if requested
        if options.show_fs_type {
            let type_display = if fs_type.len() > 10 && !options.posix_format {
                format!(" {}\n{:31}", fs_type, "")
            } else {
                format!(" {:10}", fs_type)
            };
            print!("{}", type_display);
        }

        // Total blocks
        let total_display = format_blocks(
            stats.f_blocks,
            stats.f_bsize,
            display_block_size,
            options.human_readable,
            options.human_decimal,
            options.posix_format,
        );
        print!(" {:>9}", total_display);

        // Used blocks
        let used_display = format_blocks(
            used_blocks,
            stats.f_bsize,
            display_block_size,
            options.human_readable,
            options.human_decimal,
            options.posix_format,
        );
        print!(" {:>9}", used_display);

        // Available blocks
        let avail_display = format_blocks(
            stats.f_bavail,
            stats.f_bsize,
            display_block_size,
            options.human_readable,
            options.human_decimal,
            options.posix_format,
        );
        print!(" {:>9}", avail_display);

        // Percentage
        print!(" {:>3}%", percent_used);

        // Mount point
        println!(" {}", mount_point);
    }

    Ok(())
}
