#![allow(dead_code)]
use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs::File;
use std::fs;
use std::path::Path;
use crate::lfs;
use std::io;
use std::cell::Cell;
use std::rc::Rc;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression as FlateCompression;
use bzip2::read::BzDecoder;
use bzip2::write::BzEncoder;
use bzip2::Compression as BzCompression;
use liblzma::read::XzDecoder;
use liblzma::write::XzEncoder;
use std::env;

// Tar header constants
const BLOCK_SIZE: usize = 512;
const SIZE_OFFSET: usize = 124;
const SIZE_LEN: usize = 12;
const CHECKSUM_OFFSET: usize = 148;
const CHECKSUM_LEN: usize = 8;
const TYPEFLAG_OFFSET: usize = 156;
const SYMLINK_TYPEFLAG: u8 = b'2';
const MODE_OFFSET: usize = 100;
const MODE_LEN: usize = 8;
const UID_OFFSET: usize = 108;
const UID_LEN: usize = 8;
const GID_OFFSET: usize = 116;
const GID_LEN: usize = 8;
const MTIME_OFFSET: usize = 136;
const MTIME_LEN: usize = 12;
/// UStar header offsets for uname/gname (same in ustar and gnu).
const USTAR_UNAME_OFFSET: usize = 265;
const USTAR_UNAME_LEN: usize = 32;
const USTAR_GNAME_OFFSET: usize = 297;
const USTAR_GNAME_LEN: usize = 32;

fn ustar_name_from_bytes(bytes: &[u8], start: usize, len: usize) -> String {
    let end = (start + len).min(bytes.len());
    let slice = &bytes[start..end];
    let trunc = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    let s = &slice[..trunc];
    String::from_utf8_lossy(s).trim().to_string()
}

/// Seconds to add to archive mtime for verbose listing so output matches TZ.
/// BusyBox testsuite expects listing time to reflect (mtime + offset) in UTC.
fn list_tz_offset_seconds() -> i64 {
    use time::UtcOffset;
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let s = offset.whole_hours() as i64 * 3600
        + offset.minutes_past_hour() as i64 * 60
        + offset.seconds_past_minute() as i64;
    s
}

/// Format a Unix timestamp as "YYYY-MM-DD HH:MM:SS" in UTC (no TZ dependency).
fn format_timestamp_utc(ts: i64) -> String {
    const SECS_PER_DAY: i64 = 86400;
    const SECS_PER_HOUR: u32 = 3600;
    const SECS_PER_MIN: u32 = 60;
    let (_, day) = if ts >= 0 {
        (ts, ts / SECS_PER_DAY)
    } else {
        let day = (ts - SECS_PER_DAY + 1) / SECS_PER_DAY;
        (ts - day * SECS_PER_DAY, day)
    };
    let secs = ts.rem_euclid(SECS_PER_DAY) as u32;
    let hour = secs / SECS_PER_HOUR;
    let min = (secs % SECS_PER_HOUR) / SECS_PER_MIN;
    let sec = secs % SECS_PER_MIN;
    let (year, month, day_of_month) = julian_day_to_ymd(day + 2440588);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year, month, day_of_month, hour, min, sec
    )
}

#[allow(clippy::many_single_char_names)]
fn julian_day_to_ymd(jdn: i64) -> (i32, u8, u8) {
    let a = jdn + 32044;
    let b = (4 * a + 3) / 146097;
    let c = a - (146097 * b) / 4;
    let d = (4 * c + 3) / 1461;
    let e = c - (1461 * d) / 4;
    let m = (5 * e + 2) / 153;
    let day = (e - (153 * m + 2) / 5 + 1) as u8;
    let year = (100 * b + d - 4800 + m / 10) as i32;
    let month = (m + 3 - 12 * (m / 10)) as u8;
    (year, month, day)
}

/// Sanitize a tar header block: ensure numeric fields contain valid octal digits,
/// and recompute checksum.
fn sanitize_header(block: &mut [u8; BLOCK_SIZE]) {
    if env::var_os("EPKG_DEBUG").is_some() {
        eprintln!("[tar sanitize] entry");
        eprintln!("[tar sanitize] block first 10 bytes: {:?}", &block[..10]);
        let size_slice = &block[SIZE_OFFSET..SIZE_OFFSET + SIZE_LEN];
        eprintln!("[tar sanitize] size field bytes: {:?}", std::str::from_utf8(size_slice).unwrap_or("invalid"));
        eprintln!("[tar sanitize] typeflag: {:?}", block[TYPEFLAG_OFFSET] as char);
    }
    let mut all_zero = true;
    for &b in block.iter() {
        if b != 0 {
            all_zero = false;
            break;
        }
    }
    if env::var_os("EPKG_DEBUG").is_some() {
        eprintln!("[tar sanitize] zero block check: all_zero = {}", all_zero);
        if !all_zero {
            let first_nonzero = block.iter().position(|&b| b != 0);
            eprintln!("[tar sanitize] first_nonzero = {:?}", first_nonzero);
        }
    }
    if all_zero {
        if env::var_os("EPKG_DEBUG").is_some() {
            eprintln!("[tar sanitize] zero block, skipping");
        }
        return;
    }
    // For symlinks, ensure size field is zero (or valid octal)
    let typeflag = block[TYPEFLAG_OFFSET];
    if typeflag == SYMLINK_TYPEFLAG {
        // Zero out size field (offset 124, length 12)
        let size_slice = &mut block[SIZE_OFFSET..SIZE_OFFSET + SIZE_LEN];
        if env::var_os("EPKG_DEBUG").is_some() {
            eprintln!("[tar sanitize] symlink size field bytes: {:?}", std::str::from_utf8(size_slice).unwrap_or("invalid"));
        }
        // Set size to zero: '0' followed by NUL terminator and spaces
        size_slice[0] = b'0';
        size_slice[1] = 0;
        for b in &mut size_slice[2..] {
            *b = b' ';
        }
    }
    // Sanitize all numeric fields
    let numeric_fields = [
        (MODE_OFFSET, MODE_LEN),
        (UID_OFFSET, UID_LEN),
        (GID_OFFSET, GID_LEN),
        (SIZE_OFFSET, SIZE_LEN),
        (MTIME_OFFSET, MTIME_LEN),
        (CHECKSUM_OFFSET, CHECKSUM_LEN),
    ];
    for (offset, len) in numeric_fields.iter() {
        let before = if env::var_os("EPKG_DEBUG").is_some() && *offset == SIZE_OFFSET {
            Some(std::str::from_utf8(&block[*offset..*offset + *len]).unwrap_or("invalid").to_string())
        } else {
            None
        };
        if let Some(ref before) = before {
            eprintln!("[tar sanitize] size field before sanitization: {:?}", before);
        }
        let slice = &mut block[*offset..*offset + *len];
        for b in slice.iter_mut() {
            if !(*b == 0 || *b == b' ' || (*b >= b'0' && *b <= b'7')) {
                *b = b' ';
            }
        }
        if env::var_os("EPKG_DEBUG").is_some() && *offset == SIZE_OFFSET {
            let after = std::str::from_utf8(slice).unwrap_or("invalid");
            eprintln!("[tar sanitize] size field after sanitization: {:?}", after);
        }
    }
    // Split block into three parts: before checksum, checksum slice, after checksum
    let (left, right) = block.split_at_mut(CHECKSUM_OFFSET);
    let (cksum_slice, right) = right.split_at_mut(CHECKSUM_LEN);

    if env::var_os("EPKG_DEBUG").is_some() {
        eprintln!("[tar sanitize] checksum before: {:?}", std::str::from_utf8(cksum_slice).unwrap_or("invalid"));
    }
    // Ensure checksum field contains valid octal digits or spaces/NUL
    for b in cksum_slice.iter_mut() {
        if !(*b == 0 || *b == b' ' || (*b >= b'0' && *b <= b'7')) {
            *b = b' ';
        }
    }
    // Set checksum field to spaces for computing sum
    for b in cksum_slice.iter_mut() {
        *b = b' ';
    }
    // Compute sum as per tar spec: sum of all bytes plus 8*space (0x20)
    let sum = left.iter()
        .chain(right.iter())
        .fold(0u32, |a, &b| a + b as u32)
        + 8 * 32;
    // Write octal representation (7 digits plus NUL terminator)
    let octal = format!("{:07o}", sum);
    let bytes = octal.as_bytes();
    let len = bytes.len().min(CHECKSUM_LEN - 1);
    cksum_slice[..len].copy_from_slice(&bytes[..len]);
    // Terminate with NUL or space
    for i in len..CHECKSUM_LEN {
        cksum_slice[i] = b' ';
    }
    if env::var_os("EPKG_DEBUG").is_some() {
        eprintln!("[tar sanitize] checksum after: {:?}", std::str::from_utf8(cksum_slice).unwrap_or("invalid"));
        eprintln!("[tar sanitize] computed sum={} octal={}", sum, octal);
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Compression {
    Gz,
    Bz2,
    Xz,
    #[allow(dead_code)]
    None,
}

struct CountReader<R> {
    inner: R,
    count: Rc<Cell<u64>>,
}

impl<R: io::Read> io::Read for CountReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.set(self.count.get() + n as u64);
        Ok(n)
    }
}

struct SanitizingReader<R: io::Read> {
    inner: R,
    buffer: [u8; BLOCK_SIZE],
    buf_pos: usize,
    buf_len: usize,
}

impl<R: io::Read> io::Read for SanitizingReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.buf_pos >= self.buf_len {
            // Read next block
            let mut block = [0u8; BLOCK_SIZE];
            let n = self.inner.read(&mut block)?;
            if n == 0 {
                return Ok(0);
            }
            if n == BLOCK_SIZE {
                // Possibly a tar header block
                sanitize_header(&mut block);
            }
            self.buffer = block;
            self.buf_len = n;
            self.buf_pos = 0;
        }
        let available = self.buf_len - self.buf_pos;
        let to_copy = available.min(out.len());
        out[..to_copy].copy_from_slice(&self.buffer[self.buf_pos..self.buf_pos + to_copy]);
        self.buf_pos += to_copy;
        Ok(to_copy)
    }
}

fn sanitize_reader(reader: Box<dyn std::io::Read>) -> Box<dyn std::io::Read> {
    Box::new(SanitizingReader {
        inner: reader,
        buffer: [0u8; BLOCK_SIZE],
        buf_pos: 0,
        buf_len: 0,
    })
}

fn wrap_reader(reader: Box<dyn std::io::Read>, compression: Option<Compression>) -> Box<dyn std::io::Read> {
    match compression {
        Some(Compression::Gz) => Box::new(GzDecoder::new(reader)),
        Some(Compression::Bz2) => Box::new(BzDecoder::new(reader)),
        Some(Compression::Xz) => Box::new(XzDecoder::new(reader)),
        Some(Compression::None) | None => reader,
    }
}

fn wrap_writer(writer: Box<dyn std::io::Write>, compression: Option<Compression>) -> Box<dyn std::io::Write> {
    match compression {
        Some(Compression::Gz) => Box::new(GzEncoder::new(writer, FlateCompression::default())),
        Some(Compression::Bz2) => Box::new(BzEncoder::new(writer, BzCompression::default())),
        Some(Compression::Xz) => Box::new(XzEncoder::new(writer, 6)),
        Some(Compression::None) | None => writer,
    }
}
fn extract_entry<R: std::io::Read>(entry: &mut tar::Entry<R>, extract_path: &str) -> std::io::Result<()> {
    use std::fs;
    use std::io::prelude::*;
    use tar::EntryType;

    let entry_type = entry.header().entry_type();
    let path = entry.path()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("failed to get entry path: {}", e)))?;
    let dest = std::path::Path::new(extract_path).join(&path);
    if std::env::var_os("EPKG_DEBUG").is_some() {
        eprintln!("[tar] extract_entry: type={:?} dest={:?}", entry_type, dest);
    }

    match entry_type {
        EntryType::Regular => {
            let mut file = lfs::file_create(&dest)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            let mut reader = entry.take(entry.header().size().unwrap_or(0));
            std::io::copy(&mut reader, &mut file)?;
            // Ensure the file is closed before setting permissions
            drop(file);
            // Set file permissions if supported
            if let Ok(mode) = entry.header().mode() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let perms = fs::Permissions::from_mode(mode & 0o777);
                    lfs::set_permissions(&dest, perms)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
                }
            }
            Ok(())
        }
        EntryType::Directory => {
            lfs::create_dir_all(&dest)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            Ok(())
        }
        EntryType::Symlink => {
            let linkname = entry.link_name()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("failed to get link name: {}", e)))?
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "symlink entry missing link name"))?;
            #[cfg(unix)]
            {
                lfs::symlink(&linkname, &dest)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            }
            #[cfg(not(unix))]
            {
                return Err(std::io::Error::new(std::io::ErrorKind::Other, "symlinks not supported on this platform"));
            }
            Ok(())
        }
        _ => {
            // Fallback to tar crate's unpack for other entry types
            entry.unpack(extract_path)?;
            Ok(())
        }
    }
}
fn maybe_extract_entry<R: std::io::Read>(
    entry: &mut tar::Entry<R>,
    extract_path: &str,
    files: &[String],
    all_exclude_patterns: &[String],
    matched: &mut [bool],
) -> Result<bool> {
    let original_path = entry.path()
        .map_err(|e| eyre!("tar: error getting entry path: {}", e))?
        .display()
        .to_string();
    let path = normalize_path(&original_path);
    if std::env::var_os("EPKG_DEBUG").is_some() {
        eprintln!("[tar] entry: original_path={:?} path={:?} type={:?} is_dir={}", original_path, path, entry.header().entry_type(), entry.header().entry_type() == tar::EntryType::Directory);
    }

    let is_dir = entry.header().entry_type() == tar::EntryType::Directory;
    let extract = should_extract_entry(&path, is_dir, files, all_exclude_patterns, matched);

    if !extract {
        return Ok(false);
    }

    let dest = std::path::Path::new(extract_path).join(&path);
    if std::env::var_os("EPKG_DEBUG").is_some() {
        eprintln!("[tar] extract: dest={:?} path={:?} extract_path={:?}", dest, path, extract_path);
        eprintln!("[tar] entry header: type={:?} size={} mode={:o}", entry.header().entry_type(), entry.header().size().unwrap_or(0), entry.header().mode().unwrap_or(0));
    }

    // If destination exists as a directory and we're extracting a regular file,
    // try to remove the directory to allow file creation.
    if entry.header().entry_type() == tar::EntryType::Regular && dest.exists() {
        eprintln!("[tar] dest exists: {}", dest.display());
        if let Ok(metadata) = fs::metadata(&dest) {
            if metadata.is_dir() {
                eprintln!("[tar] removing directory: {}", dest.display());
                if let Err(e) = lfs::remove_dir_all(&dest) {
                    eprintln!("[tar] remove_dir_all failed: {}", e);
                }
            }
        }
    }
    if std::env::var_os("EPKG_DEBUG").is_some() {
        eprintln!("[tar] before unpack: dest={:?} exists={}", dest, dest.exists());
        if dest.exists() {
            if let Ok(metadata) = dest.metadata() {
                eprintln!("[tar] dest metadata: is_dir={}", metadata.is_dir());
            }
        }
        let _ = std::fs::read_dir(".").map(|entries| {
            for entry in entries {
                if let Ok(e) = entry {
                    eprintln!("[tar] dir entry: {:?}", e.path());
                }
            }
        });
    }
    extract_entry(entry, extract_path)
        .map_err(|e| {
            eprintln!("tar: extract error details: {:?}", e);
            eyre!("tar: error extracting entry '{}': {}", path, e)
        })?;
    // Ensure directories are created (tar may skip empty directories)
    if entry.header().entry_type() == tar::EntryType::Directory {
        lfs::create_dir_all(&dest)?;
    }
    Ok(true)
}

pub struct TarOptions {
    pub create: bool,
    pub extract: bool,
    pub list: bool,
    pub verbose: bool,
    pub extract_to_stdout: bool,
    pub file: Option<String>,
    pub directory: Option<String>,
    pub files: Vec<String>,
    pub compression: Option<Compression>,
    pub exclude_patterns: Vec<String>,
    pub exclude_files: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<TarOptions> {
    let args = matches.get_many::<String>("args")
        .map(|vals| vals.cloned().collect::<Vec<String>>())
        .unwrap_or_default();

    if args.is_empty() {
        return Err(eyre!("tar: missing arguments"));
    }

    parse_traditional_args(&args)
}
fn parse_option_group(
    args: &[String],
    i: &mut usize,
    create: &mut bool,
    extract: &mut bool,
    list: &mut bool,
    extract_to_stdout: &mut bool,
    verbose: &mut bool,
    file: &mut Option<String>,
    directory: &mut Option<String>,
    compression: &mut Option<Compression>,
    exclude_files: &mut Vec<String>,
) -> Result<()> {
    if *i < args.len() && !args[*i].starts_with('-') && args[*i].chars().all(|c| c.is_ascii_alphabetic()) {
        let opts = &args[*i];
        *i += 1;
        for ch in opts.chars() {
            match ch {
                'c' => *create = true,
                'x' => *extract = true,
                't' => *list = true,
                'O' => *extract_to_stdout = true,
                'f' => {
                    if *i >= args.len() {
                        return Err(eyre!("tar: missing archive file after -f"));
                    }
                    *file = Some(args[*i].clone());
                    *i += 1;
                }
                'C' => {
                    if *i >= args.len() {
                        return Err(eyre!("tar: missing directory after -C"));
                    }
                    *directory = Some(args[*i].clone());
                    *i += 1;
                }
                'z' => *compression = Some(Compression::Gz),
                'j' => *compression = Some(Compression::Bz2),
                'J' => *compression = Some(Compression::Xz),
                'v' => *verbose = true,
                'X' => {
                    if *i >= args.len() {
                        return Err(eyre!("tar: missing exclude file after -X"));
                    }
                    exclude_files.push(args[*i].clone());
                    *i += 1;
                }
                _ => return Err(eyre!("tar: invalid option '{}'", ch)),
            }
        }
    }
    Ok(())
}

fn get_option_arg(
    chars: &mut std::str::Chars,
    i: &mut usize,
    args: &[String],
    missing_msg: &str,
) -> Result<String> {
    let next_arg = if chars.clone().next().is_some() {
        chars.collect::<String>()
    } else if *i < args.len() {
        let val = args[*i].clone();
        *i += 1;
        val
    } else {
        return Err(eyre!("tar: {}", missing_msg));
    };
    Ok(next_arg)
}

#[allow(unused_variables)]
fn process_short_options(
    arg: &str,
    i: &mut usize,
    args: &[String],
    create: &mut bool,
    extract: &mut bool,
    list: &mut bool,
    extract_to_stdout: &mut bool,
    verbose: &mut bool,
    file: &mut Option<String>,
    directory: &mut Option<String>,
    compression: &mut Option<Compression>,
    exclude_patterns: &mut Vec<String>,
    exclude_files: &mut Vec<String>,
) -> Result<()> {
    let mut chars = arg.chars();
    let _ = chars.next(); // skip leading '-'
    while let Some(ch) = chars.next() {
        match ch {
            'c' => *create = true,
            'x' => *extract = true,
            't' => *list = true,
            'O' => *extract_to_stdout = true,
            'f' => {
                let next_arg = get_option_arg(&mut chars, i, args, "missing archive file after -f")?;
                *file = Some(next_arg);
                break; // rest of chars are part of filename
            }
            'C' => {
                let next_arg = get_option_arg(&mut chars, i, args, "missing directory after -C")?;
                *directory = Some(next_arg);
                break;
            }
            'z' => *compression = Some(Compression::Gz),
            'j' => *compression = Some(Compression::Bz2),
            'J' => *compression = Some(Compression::Xz),
            'v' => *verbose = true,
            'X' => {
                let next_arg = get_option_arg(&mut chars, i, args, "missing exclude file after -X")?;
                exclude_files.push(next_arg);
                break;
            }
            _ => return Err(eyre!("tar: invalid option '{}'", ch)),
        }
    }
    Ok(())
}

fn parse_remaining_args(
    args: &[String],
    i: &mut usize,
    create: &mut bool,
    extract: &mut bool,
    list: &mut bool,
    extract_to_stdout: &mut bool,
    verbose: &mut bool,
    file: &mut Option<String>,
    directory: &mut Option<String>,
    compression: &mut Option<Compression>,
    exclude_patterns: &mut Vec<String>,
    exclude_files: &mut Vec<String>,
    files: &mut Vec<String>,
) -> Result<()> {
    while *i < args.len() {
        let arg = &args[*i];
        if arg.starts_with('-') {
            *i += 1;
            if arg == "--" {
                break;
            }
            if arg == "--exclude" {
                if *i >= args.len() {
                    return Err(eyre!("tar: missing pattern after --exclude"));
                }
                exclude_patterns.push(args[*i].clone());
                *i += 1;
                continue;
            }
            process_short_options(arg, i, args, create, extract, list, extract_to_stdout, verbose, file, directory, compression, exclude_patterns, exclude_files)?;
        } else {
            files.push(args[*i].clone());
            *i += 1;
        }
    }
    Ok(())
}

fn parse_traditional_args(args: &[String]) -> Result<TarOptions> {
    let mut i = 0;
    let mut create = false;
    let mut extract = false;
    let mut list = false;
    let mut extract_to_stdout = false;
    let mut verbose = false;
    let mut file = None;
    let mut directory = None;
    let mut files = Vec::new();
    let mut compression = None;
    let mut exclude_patterns = Vec::new();
    let mut exclude_files = Vec::new();

    // Parse option group (e.g., "cf", "xf") if present and not a dash option
    parse_option_group(args, &mut i, &mut create, &mut extract, &mut list, &mut extract_to_stdout, &mut verbose, &mut file, &mut directory, &mut compression, &mut exclude_files)?;

    // Parse remaining arguments (options and files interleaved)
    parse_remaining_args(args, &mut i, &mut create, &mut extract, &mut list, &mut extract_to_stdout, &mut verbose, &mut file, &mut directory, &mut compression, &mut exclude_patterns, &mut exclude_files, &mut files)?;

    // Validate mode conflicts
    if create && extract {
        return Err(eyre!("tar: cannot specify both -c and -x"));
    }
    if create && list {
        return Err(eyre!("tar: cannot specify both -c and -t"));
    }
    if extract && list {
        return Err(eyre!("tar: cannot specify both -x and -t"));
    }
    if !create && !extract && !list {
        return Err(eyre!("tar: must specify one of -c, -x, or -t"));
    }

    // extract_to_stdout only valid with extract
    if extract_to_stdout && !extract {
        return Err(eyre!("tar: -O requires -x"));
    }

    Ok(TarOptions {
        create,
        extract,
        list,
        extract_to_stdout,
        verbose,
        file,
        directory,
        files,
        compression,
        exclude_patterns: exclude_patterns,
        exclude_files: exclude_files,
    })
}

pub fn command() -> Command {
    Command::new("tar")
        .about("Archive files")
        .arg(Arg::new("args")
            .help("Raw arguments (for traditional tar syntax)")
            .num_args(0..)
            .trailing_var_arg(true)
            .allow_hyphen_values(true))
}

fn create_archive(archive_path: &str, files: &[String], compression: Option<Compression>) -> Result<()> {
    let archive_file: Box<dyn std::io::Write> = if archive_path == "-" {
        Box::new(std::io::stdout())
    } else {
        Box::new(File::create(archive_path)
            .map_err(|e| eyre!("tar: cannot create '{}': {}", archive_path, e))?)
    };

    let archive_file = wrap_writer(archive_file, compression);
    let mut builder = tar::Builder::new(archive_file);

    for file_path in files {
        let path = Path::new(file_path);
        if path.is_dir() {
            builder.append_dir_all(path.file_name().unwrap_or(path.as_os_str()), path)
                .map_err(|e| eyre!("tar: error adding directory '{}': {}", file_path, e))?;
        } else {
            builder.append_path(path)
                .map_err(|e| eyre!("tar: error adding file '{}': {}", file_path, e))?;
        }
    }

    builder.finish()
        .map_err(|e| eyre!("tar: error finishing archive: {}", e))?;

    Ok(())
}
fn normalize_path(p: &str) -> String {
    p.strip_prefix("./").unwrap_or(p).to_string()
}

fn load_exclude_patterns(exclude_patterns: &[String], exclude_files: &[String]) -> Result<Vec<String>> {
    let mut all_exclude_patterns = Vec::new();
    all_exclude_patterns.extend_from_slice(exclude_patterns);
    for file in exclude_files {
        let content = fs::read_to_string(file)
            .map_err(|e| eyre!("tar: cannot read exclude file '{}': {}", file, e))?;
        for line in content.lines() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                all_exclude_patterns.push(line.to_string());
            }
        }
    }
    Ok(all_exclude_patterns)
}

fn matches_any_pattern(path: &str, patterns: &[String]) -> bool {
    let norm_path = normalize_path(path);
    patterns.iter().any(|pattern| {
        let norm_pattern = normalize_path(pattern);
        norm_path.starts_with(&norm_pattern)
    })
}

fn should_extract_entry(path: &str, is_dir: bool, files: &[String], all_exclude_patterns: &[String], matched: &mut [bool]) -> bool {
    if files.is_empty() {
        !matches_any_pattern(path, all_exclude_patterns)
    } else {
        let mut pattern_matched = false;
        for (i, pattern) in files.iter().enumerate() {
            let norm_pattern = normalize_path(pattern);
            if path.starts_with(&norm_pattern) {
                matched[i] = true;
                pattern_matched = true;
            } else if is_dir {
                if norm_pattern.starts_with(path) {
                    pattern_matched = true;
                }
            }
        }
        let matches = matches_any_pattern(path, all_exclude_patterns);
        pattern_matched && !matches
    }
}

fn extract_archive(archive_path: &str, directory: Option<&str>, compression: Option<Compression>, files: &[String], exclude_patterns: &[String], exclude_files: &[String]) -> Result<()> {
    let archive_file: Box<dyn std::io::Read> = if archive_path == "-" {
        Box::new(std::io::stdin())
    } else {
        Box::new(File::open(archive_path)
            .map_err(|e| eyre!("tar: cannot open '{}': {}", archive_path, e))?)
    };

    let archive_file = sanitize_reader(wrap_reader(archive_file, compression));
    let count = Rc::new(Cell::new(0));
    let count_reader = CountReader {
        inner: archive_file,
        count: count.clone(),
    };
    let mut archive = tar::Archive::new(count_reader);
    archive.set_overwrite(true);
    archive.set_preserve_permissions(false);
    archive.set_preserve_mtime(false);
    archive.set_ignore_zeros(true);

    let extract_path = directory.unwrap_or(".");

    // Collect exclude patterns from exclude_files
    let all_exclude_patterns = load_exclude_patterns(exclude_patterns, exclude_files)?;



    // Determine which entries to extract
    let entries = archive.entries()
        .map_err(|e| {
            if std::env::var_os("EPKG_DEBUG").is_some() {
                eprintln!("[tar] archive.entries error: {:?}", e);
            }
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                eyre!("tar: short read")
            } else {
                eyre!("tar: error reading archive entries: {}", e)
            }
        })?;
    let mut matched = vec![false; files.len()];
    let mut has_entry = false;
    let mut _extracted_count = 0;

    for entry in entries {
        let mut entry = entry.map_err(|e| {
            if std::env::var_os("EPKG_DEBUG").is_some() {
                eprintln!("[tar] entry iteration error: {:?}", e);
            }
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                eyre!("tar: short read")
            } else {
                eyre!("tar: error reading entry: {}", e)
            }
        })?;
        has_entry = true;
        let extracted = maybe_extract_entry(&mut entry, extract_path, files, &all_exclude_patterns, &mut matched)?;
        if extracted {
            _extracted_count += 1;
        }
    }

    // If specific files were requested, ensure at least one pattern matched
    if !files.is_empty() {
        let missing_patterns: Vec<_> = files.iter().enumerate()
            .filter(|(i, _)| !matched[*i])
            .map(|(_, pattern)| pattern.clone())
            .collect();
        if !missing_patterns.is_empty() {
            return Err(eyre!("tar: {}: not found in archive", missing_patterns.join(", ")));
        }
    }

    // Detect empty input (0 bytes) and treat as error
    if !has_entry && count.get() == 0 {
        return Err(eyre!("tar: short read"));
    }

    Ok(())
}

fn list_archive(archive_path: &str, compression: Option<Compression>, verbose: bool) -> Result<()> {
    let archive_file: Box<dyn std::io::Read> = if archive_path == "-" {
        Box::new(std::io::stdin())
    } else {
        Box::new(File::open(archive_path)
            .map_err(|e| eyre!("tar: cannot open '{}': {}", archive_path, e))?)
    };
    let archive_file = sanitize_reader(wrap_reader(archive_file, compression));
    let count = Rc::new(Cell::new(0));
    let count_reader = CountReader {
        inner: archive_file,
        count: count.clone(),
    };
    let mut archive = tar::Archive::new(count_reader);
    archive.set_overwrite(true);
    archive.set_preserve_permissions(false);
    archive.set_preserve_mtime(false);
    let entries = archive.entries()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                eyre!("tar: short read")
            } else {
                eyre!("tar: error reading archive entries: {}", e)
            }
        })?;
    let mut has_entry = false;
    let format_mode = |mode: u32, typ: tar::EntryType| -> String {
        let mut s = String::with_capacity(10);
        match typ {
            tar::EntryType::Regular => s.push('-'),
            tar::EntryType::Directory => s.push('d'),
            tar::EntryType::Symlink => s.push('l'),
            tar::EntryType::Char => s.push('c'),
            tar::EntryType::Block => s.push('b'),
            tar::EntryType::Fifo => s.push('p'),
            _ => s.push('?'),
        }
        s.push(if mode & 0o400 != 0 { 'r' } else { '-' });
        s.push(if mode & 0o200 != 0 { 'w' } else { '-' });
        s.push(if mode & 0o100 != 0 { 'x' } else { '-' });
        s.push(if mode & 0o040 != 0 { 'r' } else { '-' });
        s.push(if mode & 0o020 != 0 { 'w' } else { '-' });
        s.push(if mode & 0o010 != 0 { 'x' } else { '-' });
        s.push(if mode & 0o004 != 0 { 'r' } else { '-' });
        s.push(if mode & 0o002 != 0 { 'w' } else { '-' });
        s.push(if mode & 0o001 != 0 { 'x' } else { '-' });
        s
    };
    for entry in entries {
        has_entry = true;
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                eprintln!("tar: short read");
                break;
            }
            Err(e) => return Err(eyre!("tar: error reading entry: {}", e)),
        };
        let path = entry.path()
            .map_err(|e| eyre!("tar: error getting entry path: {}", e))?
            .display()
            .to_string();
        if verbose {
            let header = entry.header();
            let typ = header.entry_type();
            let mode = header.mode().unwrap_or(0);
            let perm_str = format_mode(mode, typ);
            let _uid = header.uid().unwrap_or(0);
            let _gid = header.gid().unwrap_or(0);
            let raw = header.as_bytes();
            let uname = match header.username().ok() {
                Some(Some(s)) if !s.is_empty() => s.to_string(),
                _ => {
                    let s = ustar_name_from_bytes(raw, USTAR_UNAME_OFFSET, USTAR_UNAME_LEN);
                    if s.is_empty() { "user".to_string() } else { s }
                }
            };
            let gname = match header.groupname().ok() {
                Some(Some(s)) if !s.is_empty() => s.to_string(),
                _ => {
                    let s = ustar_name_from_bytes(raw, USTAR_GNAME_OFFSET, USTAR_GNAME_LEN);
                    if s.is_empty() { "group".to_string() } else { s }
                }
            };
            let size = header.size().unwrap_or(0);
            let mtime = header.mtime().unwrap_or(0);
            let ts = mtime as i64 + list_tz_offset_seconds();
            let timestamp = format_timestamp_utc(ts);
            let size_str = {
                let s = size.to_string();
                let pad = 10_usize.saturating_sub(s.len());
                " ".repeat(pad) + &s
            };
            let link_target = if typ == tar::EntryType::Symlink {
                entry.link_name().ok().flatten().map(|p| format!(" -> {}", p.display())).unwrap_or_default()
            } else {
                String::new()
            };
            println!("{} {}/{}{} {} {}{}", perm_str, uname, gname, size_str, timestamp, path, link_target);
        } else {
            println!("{}", path);
        }
    }
    if !has_entry && count.get() == 0 {
        return Err(eyre!("tar: short read"));
    }
    Ok(())
}

fn extract_to_stdout(archive_path: &str, compression: Option<Compression>) -> Result<()> {
    let archive_file: Box<dyn std::io::Read> = if archive_path == "-" {
        Box::new(std::io::stdin())
    } else {
        Box::new(File::open(archive_path)
            .map_err(|e| eyre!("tar: cannot open '{}': {}", archive_path, e))?)
    };
    let archive_file = sanitize_reader(wrap_reader(archive_file, compression));
    let mut archive = tar::Archive::new(archive_file);
    archive.set_overwrite(true);
    archive.set_preserve_permissions(false);
    archive.set_preserve_mtime(false);
    let mut stdout = std::io::stdout();
    let entries = archive.entries()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                eyre!("tar: short read")
            } else {
                eyre!("tar: error reading archive entries: {}", e)
            }
        })?;
    for entry in entries {
        let mut entry = entry.map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                eyre!("tar: short read")
            } else {
                eyre!("tar: error reading entry: {}", e)
            }
        })?;
        std::io::copy(&mut entry, &mut stdout)
            .map_err(|e| eyre!("tar: error extracting to stdout: {}", e))?;
        break; // Only extract first file for now
    }
    Ok(())
}
fn handle_tar_error(e: color_eyre::eyre::Error) -> ! {
    // Print the underlying error without the "Error:" prefix
    for cause in e.chain() {
        eprintln!("{}", cause);
        break;
    }
    std::process::exit(1);
}

fn run_inner(options: TarOptions) -> Result<()> {
    let file_path = options.file.as_deref().unwrap_or("-");

    if options.create {
        if options.files.is_empty() {
            return Err(eyre!("tar: no files specified for archive"));
        }
        create_archive(file_path, &options.files, options.compression)?;
    } else if options.extract_to_stdout {
        extract_to_stdout(file_path, options.compression)?;
    } else if options.extract {
        extract_archive(file_path, options.directory.as_deref(), options.compression, &options.files, &options.exclude_patterns, &options.exclude_files)?;
    } else if options.list {
        list_archive(file_path, options.compression, options.verbose)?;
    }

    Ok(())
}

pub fn run(options: TarOptions) -> Result<()> {
    if let Err(e) = run_inner(options) {
        handle_tar_error(e);
    }
    Ok(())
}