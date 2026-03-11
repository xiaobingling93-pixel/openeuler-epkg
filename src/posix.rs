#![cfg(unix)]
// Standalone Rust implementations of POSIX functions
// These can be tested independently and reused outside of Lua bindings
//
// Compatible with /c/rpm-software-management/rpm/rpmio/lposix.cc
//
// NOTE: epkg is compiled with musl libc (target: x86_64-unknown-linux-musl).
// musl libc returns POSIX-mandated minimum values for some sysconf() and
// pathconf() parameters, while glibc queries actual runtime kernel values.
// Known differences (musl returns POSIX min, glibc returns runtime):
//   - _SC_ARG_MAX:     musl=131072, glibc=runtime (e.g., 2097152)
//   - _SC_NGROUPS_MAX: musl=32, glibc=runtime (e.g., 65536)
//   - _PC_LINK_MAX:    musl=8, glibc=runtime (e.g., 127)
// These are NOT bugs - both behaviors are POSIX compliant. The POSIX spec
// defines these as "minimum values" that must be at least the specified amount.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::io;
use nix::unistd;
use users::{get_user_by_name, get_group_by_name};

pub type PosixResult<T> = Result<T, PosixError>;

#[derive(Debug)]
#[allow(dead_code)]
pub enum PosixError {
    Io(io::Error),
    InvalidArgument(String),
    NotFound,
}

impl From<io::Error> for PosixError {
    fn from(err: io::Error) -> Self {
        PosixError::Io(err)
    }
}

impl From<nix::Error> for PosixError {
    fn from(err: nix::Error) -> Self {
        PosixError::Io(io::Error::from_raw_os_error(err as i32))
    }
}

// Round 1: Basic file operations
// Note: access needs special handling for optional mode, so we don't use #[posix_bind] here
pub fn posix_access(path: &str, mode: &str) -> PosixResult<bool> {
    let path = Path::new(path);
    let mut access_mode = 0;

    for ch in mode.chars() {
        match ch {
            'r' => access_mode |= libc::R_OK,
            'w' => access_mode |= libc::W_OK,
            'x' => access_mode |= libc::X_OK,
            'f' => access_mode |= libc::F_OK,
            ' ' => continue,
            _ => return Err(PosixError::InvalidArgument(format!("unknown mode: {}", ch))),
        }
    }

    if access_mode == 0 {
        access_mode = libc::F_OK;
    }

    use std::ffi::CString;
    let path_cstr = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| PosixError::InvalidArgument("path contains null byte".to_string()))?;
    // Note: libc::access sets errno on failure, which is checked in the Lua binding
    match unsafe { libc::access(path_cstr.as_ptr(), access_mode) } {
        0 => Ok(true),
        _ => Ok(false), // errno is set by access() on failure
    }
}

// Mode munch implementation - parses chmod-style mode strings
pub fn mode_munch(mode: &mut u32, mode_str: &str) -> Result<(), PosixError> {
    let mut p = mode_str.chars().peekable();
    let mut done = false;

    while !done {
        let mut affected_bits = 0u32;
        let mut ch_mode = 0u32;
        let op;

        // Step 1: Parse who's affected (u, g, o, a)
        loop {
            match p.peek() {
                Some(&'u') => {
                    affected_bits |= 0o4700;
                    p.next();
                }
                Some(&'g') => {
                    affected_bits |= 0o2070;
                    p.next();
                }
                Some(&'o') => {
                    affected_bits |= 0o1007;
                    p.next();
                }
                Some(&'a') => {
                    affected_bits |= 0o7777;
                    p.next();
                }
                Some(&' ') => {
                    p.next();
                }
                _ => break,
            }
        }

        // If none specified, affect all bits
        if affected_bits == 0 {
            affected_bits = 0o7777;
        }

        // Check for rwxrwxrwx format (starts with 'r' or '-')
        if let Some(&ch) = p.peek() {
            if ch == 'r' || ch == '-' {
                return rwxrwxrwx(mode, mode_str);
            }
        }

        // Check for octal format (starts with digit 0-7)
        // C++ version: if (*p >= '0' && *p <= '7') { strtol(p, &e, 8); if (*p == 0 || *e != 0) return -5; }
        // This means: parse octal, and the entire remaining string must be consumed (e must point to null)
        if let Some(&ch) = p.peek() {
            if ch >= '0' && ch <= '7' {
                let mut octal_str = String::new();
                // Collect all octal digits
                while let Some(&c) = p.peek() {
                    if c >= '0' && c <= '7' {
                        octal_str.push(p.next().unwrap());
                    } else {
                        break;
                    }
                }
                // Check if we consumed the entire remaining string (matching C++ *e != 0 check)
                // If there are any remaining characters, it's an error
                if p.peek().is_some() {
                    return Err(PosixError::InvalidArgument(format!("invalid octal mode: non-octal character found")));
                }
                // Also check for empty string (matching C++ *p == 0 check)
                if octal_str.is_empty() {
                    return Err(PosixError::InvalidArgument(format!("invalid octal mode: empty string")));
                }
                let mode_num = u32::from_str_radix(&octal_str, 8)
                    .map_err(|_| PosixError::InvalidArgument(format!("invalid octal mode: {}", octal_str)))?;
                *mode = mode_num;
                return Ok(());
            }
        }

        // Step 2: Parse operator (+, -, =)
        match p.next() {
            Some('+') => op = Some('+'),
            Some('-') => op = Some('-'),
            Some('=') => op = Some('='),
            Some(' ') => continue,
            None => {
                break;
            }
            _ => return Err(PosixError::InvalidArgument("bad operator".to_string())),
        }

        // Step 3: Parse what changes (r, w, x, s)
        loop {
            match p.peek() {
                Some(&'r') => {
                    ch_mode |= 0o0444;
                    p.next();
                }
                Some(&'w') => {
                    ch_mode |= 0o0222;
                    p.next();
                }
                Some(&'x') => {
                    ch_mode |= 0o0111;
                    p.next();
                }
                Some(&'s') => {
                    ch_mode |= 0o6000;
                    p.next();
                }
                Some(&' ') => {
                    p.next();
                }
                _ => break,
            }
        }

        // Step 4: Apply changes
        if ch_mode != 0 {
            match op {
                Some('+') => *mode |= ch_mode & affected_bits,
                Some('-') => *mode &= !(ch_mode & affected_bits),
                Some('=') => *mode = (*mode & !affected_bits) | (ch_mode & affected_bits),
                _ => return Err(PosixError::InvalidArgument("bad mode change".to_string())),
            }
        }

        // Check for comma (multiple changes)
        match p.peek() {
            Some(&',') => {
                p.next();
            }
            Some(&' ') => {
                p.next();
            }
            None => done = true,
            _ => done = true,
        }
    }

    Ok(())
}

fn rwxrwxrwx(mode: &mut u32, mode_str: &str) -> Result<(), PosixError> {
    let mut tmp_mode = *mode;
    tmp_mode &= !((libc::S_ISUID as u32) | (libc::S_ISGID as u32)); // Turn off suid and sgid flags

    let chars: Vec<char> = mode_str.chars().take(9).collect();
    if chars.len() != 9 {
        return Err(PosixError::InvalidArgument("rwxrwxrwx format requires 9 characters".to_string()));
    }

    let modesel = [
        ('r', libc::S_IRUSR as u32, 0),
        ('w', libc::S_IWUSR as u32, 1),
        ('x', libc::S_IXUSR as u32, 2),
        ('r', libc::S_IRGRP as u32, 3),
        ('w', libc::S_IWGRP as u32, 4),
        ('x', libc::S_IXGRP as u32, 5),
        ('r', libc::S_IROTH as u32, 6),
        ('w', libc::S_IWOTH as u32, 7),
        ('x', libc::S_IXOTH as u32, 8),
    ];

    for (i, ch) in chars.iter().enumerate() {
        let (expected_ch, bit, pos) = modesel[i];
        match *ch {
            c if c == expected_ch => tmp_mode |= bit,
            '-' => tmp_mode &= !bit,
            's' if pos == 2 => {
                tmp_mode |= (libc::S_ISUID as u32) | (libc::S_IXUSR as u32);
            }
            's' if pos == 5 => {
                tmp_mode |= (libc::S_ISGID as u32) | (libc::S_IXGRP as u32);
            }
            _ => return Err(PosixError::InvalidArgument(format!("bad rwxrwxrwx mode change at position {}", i))),
        }
    }

    *mode = tmp_mode;
    Ok(())
}

pub fn posix_chmod(path: &str, mode_str: &str) -> PosixResult<()> {
    let path = Path::new(path);

    // Get current mode first
    let metadata = fs::metadata(path)?;
    let mut mode = metadata.mode();

    // Parse mode string using mode_munch
    mode_munch(&mut mode, mode_str)?;
    mode &= 0o7777; // Ensure only permission bits

    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

/// Resolve user and group names to UIDs and GIDs
/// Returns (uid, gid) where None means "don't change"
pub fn resolve_user_group_ids(user: Option<&str>, group: Option<&str>) -> (Option<u32>, Option<u32>) {
    // Match C++ behavior: return -1 (None) if user/group not found (means "don't change")
    let uid = if let Some(u) = user {
        if let Ok(uid_num) = u.parse::<u32>() {
            Some(uid_num)
        } else if let Some(user) = get_user_by_name(u) {
            Some(user.uid())
        } else {
            // C++ version returns -1 if not found, which means "don't change"
            None
        }
    } else {
        None
    };

    let gid = if let Some(g) = group {
        if let Ok(gid_num) = g.parse::<u32>() {
            Some(gid_num)
        } else if let Some(group) = get_group_by_name(g) {
            Some(group.gid())
        } else {
            // C++ version returns -1 if not found, which means "don't change"
            None
        }
    } else {
        None
    };

    (uid, gid)
}

pub fn posix_chown(path: &str, user: Option<&str>, group: Option<&str>) -> PosixResult<()> {
    let (uid, gid) = resolve_user_group_ids(user, group);
    let path = Path::new(path);
    unistd::chown(path, uid.map(unistd::Uid::from_raw), gid.map(unistd::Gid::from_raw))?;
    Ok(())
}

// File status information structure
#[derive(Debug, Clone)]
pub struct PosixStat {
    pub mode:      u32,
    pub mode_str:  String,  // String representation like "rwxr-xr-x"
    pub ino:       u64,
    pub dev:       u64,
    pub nlink:     u64,
    pub uid:       u32,
    pub gid:       u32,
    pub size:      u64,
    pub atime:     u64,
    pub mtime:     u64,
    pub ctime:     u64,
    pub file_type: String,
}

fn modechopper(mode: u32) -> String {
    let mut result = String::with_capacity(9);
    let modesel = [
        (libc::S_IRUSR as u32, 'r'),
        (libc::S_IWUSR as u32, 'w'),
        (libc::S_IXUSR as u32, 'x'),
        (libc::S_IRGRP as u32, 'r'),
        (libc::S_IWGRP as u32, 'w'),
        (libc::S_IXGRP as u32, 'x'),
        (libc::S_IROTH as u32, 'r'),
        (libc::S_IWOTH as u32, 'w'),
        (libc::S_IXOTH as u32, 'x'),
    ];

    for (bit, ch) in modesel.iter() {
        if mode & *bit != 0 {
            result.push(*ch);
        } else {
            result.push('-');
        }
    }

    // Handle suid and sgid flags
    let mut chars: Vec<char> = result.chars().collect();
    if mode & libc::S_ISUID as u32 != 0 {
        chars[2] = if mode & libc::S_IXUSR as u32 != 0 { 's' } else { 'S' };
    }
    if mode & libc::S_ISGID as u32 != 0 {
        chars[5] = if mode & libc::S_IXGRP as u32 != 0 { 's' } else { 'S' };
    }

    chars.into_iter().collect()
}

fn filetype(mode: u32) -> &'static str {
    use libc::{S_IFMT, S_IFREG, S_IFLNK, S_IFDIR, S_IFCHR, S_IFBLK, S_IFIFO, S_IFSOCK};
    let file_type = mode & (S_IFMT as u32);
    if file_type == S_IFREG as u32 {
        "regular"
    } else if file_type == S_IFLNK as u32 {
        "link"
    } else if file_type == S_IFDIR as u32 {
        "directory"
    } else if file_type == S_IFCHR as u32 {
        "character device"
    } else if file_type == S_IFBLK as u32 {
        "block device"
    } else if file_type == S_IFIFO as u32 {
        "fifo"
    } else if file_type == S_IFSOCK as u32 {
        "socket"
    } else {
        "?"
    }
}

pub fn posix_stat(path: &str) -> PosixResult<PosixStat> {
    use std::os::unix::fs::MetadataExt;
    // Use lstat (symlink_metadata) to NOT follow symlinks (matching C++ implementation)
    let metadata = fs::symlink_metadata(path)?;

    let mode = metadata.mode();
    let file_type = filetype(mode);

    Ok(PosixStat {
        mode:       mode,
        mode_str:   modechopper(mode),
        ino:        metadata.ino(),
        dev:        metadata.dev(),
        nlink:      metadata.nlink(),
        uid:        metadata.uid(),
        gid:        metadata.gid(),
        size:       metadata.len(),
        atime:      metadata.accessed()?
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
        mtime:      metadata.modified()?
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
        // Match POSIX `st_ctime` (status change time), not creation time
        ctime:      metadata.ctime() as u64,
        file_type:  file_type.to_string(),
    })
}

pub fn posix_umask(mask: Option<&str>) -> PosixResult<String> {
    // Get current umask (umask returns the complement)
    let current_umask = unsafe { libc::umask(0) };
    let mode = (!current_umask) & 0o777;

    if let Some(mask_str) = mask {
        // Parse the new mode string
        let mut new_mode = mode as u32;
        mode_munch(&mut new_mode, mask_str)?;
        let new_mode = (new_mode & 0o777) as libc::mode_t;
        // Set the new umask (umask expects the complement)
        unsafe { libc::umask(!new_mode) };
        Ok(modechopper(new_mode as u32))
    } else {
        // Restore the original umask
        unsafe { libc::umask(current_umask) };
        Ok(modechopper(mode as u32))
    }
}

/// Helper function to call a libc function that takes (path, mode) and returns i32
/// Used by Lua bindings to match C++ pushresult behavior
fn call_libc_path_mode<F>(path: &str, mode: libc::mode_t, f: F) -> io::Result<()>
where
    F: FnOnce(*const libc::c_char, libc::mode_t) -> libc::c_int,
{
    let path_cstr = std::ffi::CString::new(path)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains null byte"))?;
    let result = f(path_cstr.as_ptr(), mode);
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Helper function to wrap libc::mkdir as Result<(), Error>
/// Used by Lua bindings to match C++ pushresult behavior
pub fn posix_mkdir(path: &str) -> io::Result<()> {
    call_libc_path_mode(path, 0o777, |p, m| unsafe { libc::mkdir(p, m) })
}

/// Helper function to wrap libc::mkfifo as Result<(), Error>
/// Used by Lua bindings to match C++ pushresult behavior
pub fn posix_mkfifo(path: &str) -> io::Result<()> {
    call_libc_path_mode(path, 0o777, |p, m| unsafe { libc::mkfifo(p, m) })
}

#[derive(Debug, Clone)]
pub struct PosixUname {
    pub sysname: String,
    pub nodename: String,
    pub release: String,
    pub version: String,
    pub machine: String,
}

pub fn posix_uname() -> PosixResult<PosixUname> {
    let mut uts = std::mem::MaybeUninit::<libc::utsname>::uninit();
    match unsafe { libc::uname(uts.as_mut_ptr()) } {
        0 => {
            let uts = unsafe { uts.assume_init() };
            Ok(PosixUname {
                sysname:    unsafe { std::ffi::CStr::from_ptr(uts.sysname.as_ptr()).to_string_lossy().to_string() },
                nodename:   unsafe { std::ffi::CStr::from_ptr(uts.nodename.as_ptr()).to_string_lossy().to_string() },
                release:    unsafe { std::ffi::CStr::from_ptr(uts.release.as_ptr()).to_string_lossy().to_string() },
                version:    unsafe { std::ffi::CStr::from_ptr(uts.version.as_ptr()).to_string_lossy().to_string() },
                machine:    unsafe { std::ffi::CStr::from_ptr(uts.machine.as_ptr()).to_string_lossy().to_string() },
            })
        }
        _ => Err(PosixError::Io(io::Error::last_os_error())),
    }
}

/// Helper function to wrap libc::ttyname as Option<String>
/// Used by Lua bindings to match C++ behavior (returns string or nil)
pub fn posix_ttyname(fd: i32) -> Option<String> {
    let tty_name = unsafe { libc::ttyname(fd) };
    if tty_name.is_null() {
        None
    } else {
        Some(unsafe { std::ffi::CStr::from_ptr(tty_name).to_string_lossy().to_string() })
    }
}

#[cfg(target_os = "linux")]
pub fn posix_ctermid() -> String {
    // L_ctermid is typically 1024 on most systems
    let mut buf = vec![0u8; 1024];
    let ctermid_str = unsafe { libc::ctermid(buf.as_mut_ptr() as *mut libc::c_char) };
    if ctermid_str.is_null() {
        String::new()
    } else {
        unsafe { std::ffi::CStr::from_ptr(ctermid_str).to_string_lossy().to_string() }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn posix_ctermid() -> String {
    // Stub for non-Linux platforms
    String::new()
}

/// Helper function to wrap libc::getlogin as Option<String>
/// Used by Lua bindings to match C++ behavior (returns string or nil)
pub fn posix_getlogin() -> Option<String> {
    let login_name = unsafe { libc::getlogin() };
    if login_name.is_null() {
        None
    } else {
        Some(unsafe { std::ffi::CStr::from_ptr(login_name).to_string_lossy().to_string() })
    }
}

pub fn posix_dir(path: &str) -> PosixResult<Vec<String>> {
    let dir = fs::read_dir(path)?;
    let mut entries = Vec::new();
    for entry in dir {
        let entry = entry?;
        entries.push(entry.file_name().to_string_lossy().to_string());
    }
    Ok(entries)
}

#[derive(Debug, Clone)]
pub struct PosixStatFs {
    pub f_type:    i64,
    pub f_bsize:   u64,
    pub f_blocks:  u64,
    pub f_bfree:   u64,
    pub f_bavail:  u64,
    pub f_files:   u64,
    pub f_ffree:   u64,
    pub f_namelen: u64,
    pub f_fsid:    u64,
}

#[allow(clippy::unnecessary_transmutes)]
pub fn posix_statfs(path: &str) -> PosixResult<PosixStatFs> {
    use std::ffi::CString;
    let path_cstr = CString::new(path)
        .map_err(|_| PosixError::InvalidArgument("path contains null byte".to_string()))?;

    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::statfs(path_cstr.as_ptr(), &mut st) };
    if ret != 0 {
        return Err(PosixError::Io(io::Error::last_os_error()));
    }

    Ok(PosixStatFs {
        f_type:    st.f_type    as i64,
        f_bsize:   st.f_bsize   as u64,
        f_blocks:  st.f_blocks  as u64,
        f_bfree:   st.f_bfree   as u64,
        f_bavail:  st.f_bavail  as u64,
        f_files:   st.f_files   as u64,
        f_ffree:   st.f_ffree   as u64,
        #[cfg(target_os = "linux")]
        f_namelen: st.f_namelen as u64,
        #[cfg(not(target_os = "linux"))]
        f_namelen: 0,
        // fsid encoding is platform-specific; extract from f_fsid
        f_fsid: {
            #[cfg(target_os = "linux")]
            {
                use std::convert::TryInto;
                // On Linux, f_fsid is fsid_t { int val[2]; }
                // Combine as high and low 32-bit parts (matching GNU stat)
                let bytes = unsafe { std::mem::transmute::<_, [u8; 8]>(st.f_fsid) };
                let val0 = u32::from_ne_bytes(bytes[0..4].try_into().unwrap()) as u64;
                let val1 = u32::from_ne_bytes(bytes[4..8].try_into().unwrap()) as u64;
                (val0 << 32) | val1
            }
            #[cfg(not(target_os = "linux"))]
            {
                // Fallback to f_type for now (matches previous behavior)
                st.f_type as u64
            }
        },
    })
}

pub fn posix_mkstemp(template: &str) -> PosixResult<(String, std::fs::File)> {
    let template_bytes = template.as_bytes();
    let mut template_vec = template_bytes.to_vec();
    template_vec.push(0); // null terminator

    let fd = unsafe { libc::mkstemp(template_vec.as_mut_ptr() as *mut libc::c_char) };
    if fd == -1 {
        return Err(PosixError::Io(io::Error::last_os_error()));
    }

    // Remove null terminator and convert back to string
    template_vec.pop();
    let path = String::from_utf8(template_vec)
        .map_err(|_| PosixError::InvalidArgument("invalid UTF-8 in template".to_string()))?;

    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    Ok((path, file))
}

pub fn posix_utime(path: impl AsRef<Path>, mtime: Option<u64>, atime: Option<u64>) -> PosixResult<()> {
    let path = path.as_ref();
    let path_cstr = std::ffi::CString::new(path.as_os_str().as_bytes().to_vec())
        .map_err(|_| PosixError::InvalidArgument("path contains null byte".to_string()))?;

    let currtime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let times = libc::utimbuf {
        modtime: mtime.map(|t| t as i64).unwrap_or(currtime),
        actime:  atime.map(|t| t as i64).unwrap_or(currtime),
    };

    match unsafe { libc::utime(path_cstr.as_ptr(), &times) } {
        0 => Ok(()),
        _ => Err(PosixError::Io(io::Error::last_os_error())),
    }
}

#[derive(Debug, Clone)]
pub struct PosixPasswd {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub dir: String,
    pub shell: String,
    pub gecos: String,
    pub passwd: String,
}

pub fn posix_getpasswd(name: Option<&str>, uid: Option<u32>) -> PosixResult<PosixPasswd> {
    use std::ffi::CString;
    use libc::{getpwnam, getpwuid, geteuid};

    let pwd = if let Some(n) = name {
        let name_cstr = CString::new(n)
            .map_err(|_| PosixError::InvalidArgument("name contains null byte".to_string()))?;
        unsafe { getpwnam(name_cstr.as_ptr()) }
    } else if let Some(u) = uid {
        unsafe { getpwuid(u) }
    } else {
        let euid = unsafe { geteuid() };
        unsafe { getpwuid(euid) }
    };

    if pwd.is_null() {
        return Err(PosixError::NotFound);
    }

    let pwd = unsafe { *pwd };
    Ok(PosixPasswd {
        name: unsafe    { std::ffi::CStr::from_ptr(pwd.pw_name).to_string_lossy().to_string() },
        uid: pwd.pw_uid,
        gid: pwd.pw_gid,
        dir: unsafe     { std::ffi::CStr::from_ptr(pwd.pw_dir).to_string_lossy().to_string() },
        shell: unsafe   { std::ffi::CStr::from_ptr(pwd.pw_shell).to_string_lossy().to_string() },
        gecos: unsafe   { std::ffi::CStr::from_ptr(pwd.pw_gecos).to_string_lossy().to_string() },
        passwd: unsafe  { std::ffi::CStr::from_ptr(pwd.pw_passwd).to_string_lossy().to_string() },
    })
}

#[derive(Debug, Clone)]
pub struct PosixGroup {
    pub name: String,
    pub gid: u32,
    pub members: Vec<String>,
}

pub fn posix_getgroup(name: Option<&str>, gid: Option<u32>) -> PosixResult<PosixGroup> {
    use std::ffi::CString;
    use libc::{getgrnam, getgrgid};

    let grp = if let Some(n) = name {
        let name_cstr = CString::new(n)
            .map_err(|_| PosixError::InvalidArgument("name contains null byte".to_string()))?;
        unsafe { getgrnam(name_cstr.as_ptr()) }
    } else if let Some(g) = gid {
        unsafe { getgrgid(g) }
    } else {
        return Err(PosixError::InvalidArgument("name or gid required".to_string()));
    };

    if grp.is_null() {
        return Err(PosixError::NotFound);
    }

    let grp = unsafe { *grp };
    let mut members = Vec::new();
    let mut mem_ptr = grp.gr_mem;
    while !mem_ptr.is_null() && !unsafe { *mem_ptr }.is_null() {
        let member = unsafe { std::ffi::CStr::from_ptr(*mem_ptr).to_string_lossy().to_string() };
        members.push(member);
        mem_ptr = unsafe { mem_ptr.add(1) };
    }

    Ok(PosixGroup {
        name: unsafe { std::ffi::CStr::from_ptr(grp.gr_name).to_string_lossy().to_string() },
        gid: grp.gr_gid,
        members,
    })
}


#[derive(Debug, Clone)]
pub struct PosixTimes {
    pub utime: f64,
    pub stime: f64,
    pub cutime: f64,
    pub cstime: f64,
    pub elapsed: f64,
}

pub fn posix_times() -> PosixResult<PosixTimes> {
    let mut tms = std::mem::MaybeUninit::<libc::tms>::uninit();
    let elapsed = unsafe { libc::times(tms.as_mut_ptr()) };
    if elapsed == (!0 as libc::clock_t) {
        return Err(PosixError::Io(io::Error::last_os_error()));
    }
    let tms = unsafe { tms.assume_init() };
    // C++ version uses CLOCKS_PER_SEC (line 642: #define pushtime(L,x) lua_pushnumber(L,((lua_Number)x)/CLOCKS_PER_SEC))
    // For compatibility with RPM lua scripts, we match this behavior exactly
    // Note: CLOCKS_PER_SEC is standardized to 1000000 on all POSIX systems
    // (even though times() technically uses clock ticks, the C++ code divides by CLOCKS_PER_SEC)
    const CLOCKS_PER_SEC: f64 = 1_000_000.0;
    let clk_tck = CLOCKS_PER_SEC;
    Ok(PosixTimes {
        utime:  tms.tms_utime   as f64 / clk_tck,
        stime:  tms.tms_stime   as f64 / clk_tck,
        cutime: tms.tms_cutime  as f64 / clk_tck,
        cstime: tms.tms_cstime  as f64 / clk_tck,
        elapsed: elapsed        as f64 / clk_tck,
    })
}

#[derive(Debug, Clone)]
pub struct PosixPathconf {
    pub link_max: i64,
    pub max_canon: i64,
    pub max_input: i64,
    pub name_max: i64,
    pub path_max: i64,
    pub pipe_buf: i64,
    pub chown_restricted: i64,
    pub no_trunc: i64,
    pub vdisable: i64,
}

pub fn posix_pathconf(path: &str) -> PosixResult<PosixPathconf> {
    use std::ffi::CString;
    let path_cstr = CString::new(path)
        .map_err(|_| PosixError::InvalidArgument("path contains null byte".to_string()))?;

    Ok(PosixPathconf {
        link_max:           unsafe { libc::pathconf(path_cstr.as_ptr(), libc::_PC_LINK_MAX) },
        max_canon:          unsafe { libc::pathconf(path_cstr.as_ptr(), libc::_PC_MAX_CANON) },
        max_input:          unsafe { libc::pathconf(path_cstr.as_ptr(), libc::_PC_MAX_INPUT) },
        name_max:           unsafe { libc::pathconf(path_cstr.as_ptr(), libc::_PC_NAME_MAX) },
        path_max:           unsafe { libc::pathconf(path_cstr.as_ptr(), libc::_PC_PATH_MAX) },
        pipe_buf:           unsafe { libc::pathconf(path_cstr.as_ptr(), libc::_PC_PIPE_BUF) },
        chown_restricted:   unsafe { libc::pathconf(path_cstr.as_ptr(), libc::_PC_CHOWN_RESTRICTED) },
        no_trunc:           unsafe { libc::pathconf(path_cstr.as_ptr(), libc::_PC_NO_TRUNC) },
        vdisable:           unsafe { libc::pathconf(path_cstr.as_ptr(), libc::_PC_VDISABLE) },
    })
}

#[derive(Debug, Clone)]
pub struct PosixSysconf {
    pub arg_max: i64,
    pub child_max: i64,
    pub clk_tck: i64,
    pub ngroups_max: i64,
    pub stream_max: i64,
    pub tzname_max: i64,
    pub open_max: i64,
    pub job_control: i64,
    pub saved_ids: i64,
    pub version: i64,
}

pub fn posix_sysconf() -> PosixResult<PosixSysconf> {
    Ok(PosixSysconf {
        arg_max:        unsafe { libc::sysconf(libc::_SC_ARG_MAX) },
        child_max:      unsafe { libc::sysconf(libc::_SC_CHILD_MAX) },
        clk_tck:        unsafe { libc::sysconf(libc::_SC_CLK_TCK) },
        ngroups_max:    unsafe { libc::sysconf(libc::_SC_NGROUPS_MAX) },
        stream_max:     unsafe { libc::sysconf(libc::_SC_STREAM_MAX) },
        tzname_max:     unsafe { libc::sysconf(libc::_SC_TZNAME_MAX) },
        open_max:       unsafe { libc::sysconf(libc::_SC_OPEN_MAX) },
        job_control:    unsafe { libc::sysconf(libc::_SC_JOB_CONTROL) },
        saved_ids:      unsafe { libc::sysconf(libc::_SC_SAVED_IDS) },
        version:        unsafe { libc::sysconf(libc::_SC_VERSION) },
    })
}

pub fn posix_getuid_by_name(name: &str) -> Option<u32> {
    get_user_by_name(name).map(|u| u.uid())
}

pub fn posix_getgid_by_name(name: &str) -> Option<u32> {
    get_group_by_name(name).map(|g| g.gid())
}

pub fn posix_setuid(uid: u32) -> PosixResult<()> {
    match unsafe { libc::setuid(uid) } {
        0 => Ok(()),
        _ => Err(PosixError::Io(io::Error::last_os_error())),
    }
}

pub fn posix_setgid(gid: u32) -> PosixResult<()> {
    match unsafe { libc::setgid(gid) } {
        0 => Ok(()),
        _ => Err(PosixError::Io(io::Error::last_os_error())),
    }
}

#[cfg(test)]
mod tests {

    #[test]
    #[allow(clippy::unnecessary_transmutes)]
    fn test_statfs_fsid() {
        use std::ffi::CString;
        use std::convert::TryInto;
        let path = CString::new(".").unwrap();
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::statfs(path.as_ptr(), &mut st) };
        if ret != 0 {
            panic!("statfs failed");
        }
        println!("f_type: 0x{:x}", st.f_type);
        // inspect f_fsid
        let bytes = unsafe { std::mem::transmute::<_, [u8; 8]>(st.f_fsid) };
        println!("f_fsid bytes: {:02x?}", bytes);
        // Extract two 32-bit integers in native byte order
        let val0 = u32::from_ne_bytes(bytes[0..4].try_into().unwrap()) as u64;
        let val1 = u32::from_ne_bytes(bytes[4..8].try_into().unwrap()) as u64;
        println!("val0: 0x{:x}, val1: 0x{:x}", val0, val1);
        let gnu_id = (val0 << 32) | val1;
        println!("GNU stat ID: 0x{:x}", gnu_id);
        let le = u64::from_le_bytes(bytes);
        println!("le: 0x{:x}", le);
        let be = u64::from_be_bytes(bytes);
        println!("be: 0x{:x}", be);
        // check if f_fsid is zero
        if le != 0 {
            println!("non-zero fsid");
        }
    }
}
