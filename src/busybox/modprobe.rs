//! Load/remove kernel modules with dependency resolution (modules.dep with recursive deps, fallback recursive search).
//!
//! **Debug-friendly logging:** This code runs in constrained environments (e.g. VM init) where
//! failures are hard to root-cause. At every possible failure point add `log::debug!` with rich
//! context: module name, paths, base_path existence, which file failed to open, errno, and
//! whether we used dep file vs fallback search. Do not fail silently; keep this file debuggable.

use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::{eyre, Context};
use glob::glob;
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

// Linux kernel module syscall numbers (shared with insmod). Per-arch from Linux unistd.
#[cfg(target_arch = "x86_64")]
pub(crate) const SYS_FINIT_MODULE: i32 = 313;
#[cfg(target_arch = "x86_64")]
pub(crate) const SYS_INIT_MODULE: i32 = 175;
#[cfg(target_arch = "x86_64")]
pub(crate) const SYS_DELETE_MODULE: i32 = 176;
#[cfg(any(target_arch = "x86", target_arch = "arm", target_arch = "powerpc", target_arch = "mips"))]
pub(crate) const SYS_FINIT_MODULE: i32 = 379;
#[cfg(any(target_arch = "x86", target_arch = "arm", target_arch = "powerpc", target_arch = "mips"))]
pub(crate) const SYS_INIT_MODULE: i32 = 128;
#[cfg(any(target_arch = "x86", target_arch = "arm", target_arch = "powerpc", target_arch = "mips"))]
pub(crate) const SYS_DELETE_MODULE: i32 = 129;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64", target_arch = "loongarch64"))]
pub(crate) const SYS_FINIT_MODULE: i32 = 379;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64", target_arch = "loongarch64"))]
pub(crate) const SYS_INIT_MODULE: i32 = 105;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64", target_arch = "loongarch64"))]
pub(crate) const SYS_DELETE_MODULE: i32 = 106;
#[cfg(any(target_arch = "powerpc64", target_arch = "mips64"))]
pub(crate) const SYS_FINIT_MODULE: i32 = 379;
#[cfg(any(target_arch = "powerpc64", target_arch = "mips64"))]
pub(crate) const SYS_INIT_MODULE: i32 = 175;
#[cfg(any(target_arch = "powerpc64", target_arch = "mips64"))]
pub(crate) const SYS_DELETE_MODULE: i32 = 176;

/// Flag for finit_module when the module file is compressed (e.g. .ko.xz).
pub(crate) const MODULE_INIT_COMPRESSED_FILE: i32 = 4;

pub(crate) fn module_load_flags(is_compressed: bool) -> i32 {
    if is_compressed {
        MODULE_INIT_COMPRESSED_FILE
    } else {
        0
    }
}

#[cfg(target_os = "linux")]
#[inline(always)]
pub(crate) unsafe fn syscall_finit_module(
    fd: libc::c_int,
    param_values: *const libc::c_char,
    flags: libc::c_int,
) -> libc::c_long {
    libc::syscall(
        SYS_FINIT_MODULE as libc::c_long,
        fd as libc::c_long,
        param_values as libc::c_long,
        flags as libc::c_long,
    )
}

#[cfg(target_os = "linux")]
#[inline(always)]
pub(crate) unsafe fn syscall_init_module(
    image: *const libc::c_void,
    len: usize,
    param_values: *const libc::c_char,
) -> libc::c_long {
    libc::syscall(
        SYS_INIT_MODULE as libc::c_long,
        image as libc::c_long,
        len as libc::c_long,
        param_values as libc::c_long,
    )
}

#[cfg(target_os = "linux")]
#[inline(always)]
pub(crate) unsafe fn syscall_delete_module(
    name: *const libc::c_char,
    flags: libc::c_int,
) -> libc::c_long {
    libc::syscall(
        SYS_DELETE_MODULE as libc::c_long,
        name as libc::c_long,
        flags as libc::c_long,
    )
}

pub struct ModuleInfo {
    pub pathname: String,
    pub deps: Vec<String>,
}

pub struct ModuleDb {
    modules: HashMap<String, ModuleInfo>,
}

impl ModuleDb {
    fn new() -> Self {
        Self {
            modules: HashMap::new(),
        }
    }

    /// Load module path -> deps from modules.dep (plain text, standard depmod output).
    /// Format: "path/to/module.ko.xz: dep1.ko.xz dep2.ko.xz"
    fn load_dep(&mut self, base_path: &Path) -> Result<()> {
        let dep_path = base_path.join("modules.dep");
        let file = match self.open_dep_file(&dep_path) {
            Some(f) => f,
            None => return Ok(()),
        };
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line.map_err(|e| eyre!("read modules.dep: {}", e))?;
            if let Some((pathname, deps)) = self.parse_dep_line(&line) {
                if let Some(stem) = extract_module_name(&pathname) {
                    self.modules.insert(stem, ModuleInfo { pathname, deps });
                }
            }
        }
        Ok(())
    }

    fn open_dep_file(&self, dep_path: &Path) -> Option<File> {
        match File::open(dep_path) {
            Ok(f) => Some(f),
            Err(e) => {
                log::debug!("modprobe: cannot open {}: {} (will use fallback search)", dep_path.display(), e);
                None
            }
        }
    }

    fn parse_dep_line(&self, line: &str) -> Option<(String, Vec<String>)> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (path, deps_str) = line.split_once(':')?;
        let pathname = path.trim().to_string();
        let deps: Vec<String> = deps_str
            .split_whitespace()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Some((pathname, deps))
    }

    fn find_module(&self, name: &str) -> Option<&ModuleInfo> {
        self.modules.get(name)
    }

    fn load_module_with_deps(&self, base_path: &Path, name: &str, params: &[String], quiet: bool) -> Result<()> {
        let mut loaded = HashSet::new();
        self.load_module_recursive(base_path, name, params, quiet, &mut loaded)
    }

    fn load_module_recursive(&self, base_path: &Path, name: &str, params: &[String], quiet: bool, loaded: &mut HashSet<String>) -> Result<()> {
        if self.is_already_loaded(name, loaded) {
            return Ok(());
        }

        if let Some(info) = self.find_module(name) {
            self.load_module_from_db(base_path, name, &info, params, quiet, loaded)
        } else {
            self.load_module_fallback(base_path, name, params, loaded)
        }
    }

    fn is_already_loaded(&self, name: &str, loaded: &HashSet<String>) -> bool {
        if loaded.contains(name) {
            log::debug!("modprobe: module {} already loaded in this session, skipping", name);
            true
        } else {
            false
        }
    }

    fn load_module_from_db(&self, base_path: &Path, name: &str, info: &ModuleInfo, params: &[String], quiet: bool, loaded: &mut HashSet<String>) -> Result<()> {
        self.load_deps_for_module(base_path, name, &info.deps, quiet, loaded)?;
        self.load_single_module(base_path, &info.pathname, name, params, quiet)?;
        self.mark_as_loaded(name, loaded);
        Ok(())
    }

    fn mark_as_loaded(&self, name: &str, loaded: &mut HashSet<String>) {
        loaded.insert(name.to_string());
    }

    fn load_deps_for_module(&self, base_path: &Path, name: &str, deps: &[String], quiet: bool, loaded: &mut HashSet<String>) -> Result<()> {
        for dep_path in deps {
            self.load_single_dependency(base_path, name, dep_path, quiet, loaded)?;
        }
        Ok(())
    }

    fn load_single_dependency(&self, base_path: &Path, parent_name: &str, dep_path: &str, quiet: bool, loaded: &mut HashSet<String>) -> Result<()> {
        if let Some(dep_name) = extract_module_name(dep_path) {
            log::debug!("modprobe: loading dependency {} for {}", dep_name, parent_name);
            self.load_module_recursive(base_path, &dep_name, &[], quiet, loaded)?;
        }
        Ok(())
    }

    fn load_single_module(&self, base_path: &Path, pathname: &str, name: &str, params: &[String], quiet: bool) -> Result<()> {
        let path = base_path.join(pathname);
        log::debug!("modprobe: loading {} from dep path {}", name, path.display());
        self.try_load_module(&path, params, quiet)
    }

    fn try_load_module(&self, path: &Path, params: &[String], quiet: bool) -> Result<()> {
        if let Err(e) = load_module(path, params) {
            if !quiet {
                return Err(e);
            }
            log::debug!("modprobe: load {} failed (quiet): {}", path.display(), e);
        }
        Ok(())
    }

    fn load_module_fallback(&self, base_path: &Path, name: &str, params: &[String], loaded: &mut HashSet<String>) -> Result<()> {
        log::debug!("modprobe: module {} not in db, using fallback search in {}", name, base_path.display());
        let path = self.find_module_by_search(name, base_path)?;
        load_module(&path, params)?;
        self.mark_as_loaded(name, loaded);
        Ok(())
    }

    fn find_module_by_search(&self, name: &str, base_path: &Path) -> Result<PathBuf> {
        find_module_file(name, Some(self), base_path)
    }
}

pub struct ModprobeOptions {
    pub remove: bool,
    pub quiet: bool,
    pub module: String,
    pub params: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<ModprobeOptions> {
    let remove = matches.get_flag("remove");
    let quiet = matches.get_flag("quiet");
    let module = matches
        .get_one::<String>("module")
        .ok_or_else(|| eyre!("Missing module name argument"))?
        .to_string();

    let params: Vec<String> = matches
        .get_many::<String>("params")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(ModprobeOptions {
        remove,
        quiet,
        module,
        params,
    })
}

fn extract_module_name(pathname: &str) -> Option<String> {
    Path::new(pathname)
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(|s| {
            s.strip_suffix(".ko")
                .or_else(|| s.strip_suffix(".ko.xz"))
                .or_else(|| s.strip_suffix(".ko.zst"))
                .or_else(|| s.strip_suffix(".ko.gz"))
        })
        .filter(|s| !s.is_empty())
        .map(String::from)
}

pub fn command() -> Command {
    Command::new("modprobe")
        .about("Load or remove kernel modules with dependency resolution")
        .arg(
            Arg::new("remove")
                .short('r')
                .long("remove")
                .action(clap::ArgAction::SetTrue)
                .help("Remove module instead of loading"),
        )
        .arg(
            Arg::new("quiet")
                .short('q')
                .long("quiet")
                .action(clap::ArgAction::SetTrue)
                .help("Be quiet"),
        )
        .arg(
            Arg::new("module")
                .required(true)
                .help("Module name to load or remove"),
        )
        .arg(
            Arg::new("params")
                .num_args(0..)
                .help("Module parameters as SYMBOL=VALUE"),
        )
}

pub fn find_module_file(module_name: &str, db: Option<&ModuleDb>, base_path: &Path) -> Result<PathBuf> {
    if let Some(db) = db {
        if let Some(info) = db.find_module(module_name) {
            return Ok(base_path.join(&info.pathname));
        }
    }

    let base_exists = base_path.exists();
    log::debug!("modprobe: looking for module {} in {} (base exists={})", module_name, base_path.display(), base_exists);

    // Early exit: if base path doesn't exist, skip all expensive searches
    if !base_exists {
        log::debug!("modprobe: module {} not found - base path {} does not exist", module_name, base_path.display());
        return Err(eyre!("Module {} not found - base path {} does not exist", module_name, base_path.display()));
    }

    if let Some(path) = try_exact_module_paths(module_name, base_path) {
        return Ok(path);
    }

    recursive_module_search(module_name, base_path)
}

fn try_exact_module_paths(module_name: &str, base_path: &Path) -> Option<PathBuf> {
    for ext in ["ko", "ko.xz", "ko.zst", "ko.gz"] {
        let p = base_path.join(format!("{}.{}", module_name, ext));
        if p.exists() {
            log::debug!("modprobe: module {} found at {}", module_name, p.display());
            return Some(p);
        }
    }
    None
}

fn recursive_module_search(module_name: &str, base_path: &Path) -> Result<PathBuf> {
    for ext in ["ko", "ko.xz", "ko.zst", "ko.gz"] {
        let pattern = format!("{}/**/{}.{}", base_path.display(), module_name, ext);
        log::debug!("modprobe: searching with pattern {}", pattern);

        if let Ok(paths) = glob(&pattern) {
            for entry in paths.flatten() {
                log::debug!("modprobe: module {} found at {} (glob)", module_name, entry.display());
                return Ok(entry);
            }
        }
    }

    log::debug!("modprobe: module {} not found in {} via glob search", module_name, base_path.display());
    Err(eyre!("Module {} not found in {}", module_name, base_path.display()))
}

pub fn load_module(path: &Path, params: &[String]) -> Result<()> {
    log::debug!("modprobe: loading {}", path.display());
    let opts_cstr = prepare_module_options(params)?;
    let is_compressed = is_compressed_module(path);

    if try_finit_module(path, &opts_cstr, is_compressed)? {
        return Ok(());
    }

    if is_compressed {
        return Err(eyre!("Failed to load compressed module '{}' via finit_module", path.display()));
    }

    load_via_init_module(path, &opts_cstr)
}

fn prepare_module_options(params: &[String]) -> Result<CString> {
    let opts_str = if params.is_empty() {
        String::new()
    } else {
        params.join(" ")
    };
    Ok(CString::new(opts_str)?)
}

fn is_compressed_module(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    path_str.ends_with(".ko.xz") || path_str.ends_with(".ko.zst") || path_str.ends_with(".ko.gz")
}

fn try_finit_module(path: &Path, opts_cstr: &CString, is_compressed: bool) -> Result<bool> {
    let fd = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            log::debug!("modprobe: open {} for finit_module failed: {}, will try init_module path", path.display(), e);
            return Ok(false);
        }
    };

    log::debug!("modprobe: opened {} (compressed={}), calling finit_module", path.display(), is_compressed);

    let flags = module_load_flags(is_compressed);
    let result = unsafe {
        syscall_finit_module(
            fd.as_raw_fd(),
            opts_cstr.as_ptr(),
            flags,
        )
    };

    if result == 0 {
        log::debug!("modprobe: loaded {}", path.display());
        return Ok(true);
    }

    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    if errno == libc::EEXIST {
        log::debug!("modprobe: module {} already loaded (EEXIST), treating as success", path.display());
        return Ok(true);
    }

    if is_compressed {
        log::debug!("modprobe: load {} failed: {} (errno={})", path.display(), std::io::Error::last_os_error(), errno);
        return Err(eyre!("Failed to load module '{}': {} (errno={})",
            path.display(), std::io::Error::last_os_error(), errno));
    }

    Ok(false)
}

fn load_via_init_module(path: &Path, opts_cstr: &CString) -> Result<()> {
    log::debug!("modprobe: loading {} via init_module (read into memory)", path.display());
    let mut file = File::open(path)
        .with_context(|| format!("Failed to open module file: {}", path.display()))?;

    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)
        .with_context(|| format!("Failed to read module file: {}", path.display()))?;

    if buffer.is_empty() {
        log::debug!("modprobe: module file {} is empty", path.display());
        return Err(eyre!("Module file {} is empty", path.display()));
    }

    let result = unsafe {
        syscall_init_module(
            buffer.as_ptr() as *const libc::c_void,
            buffer.len(),
            opts_cstr.as_ptr(),
        )
    };

    if result != 0 {
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if errno == libc::EEXIST {
            log::debug!("modprobe: module {} already loaded (init_module EEXIST), treating as success", path.display());
            return Ok(());
        }
        log::debug!("modprobe: load {} failed (init_module): {} (errno={})", path.display(), std::io::Error::last_os_error(), errno);
        return Err(eyre!("Failed to load module '{}': {} (errno={})",
            path.display(), std::io::Error::last_os_error(), errno));
    }

    log::debug!("modprobe: loaded {}", path.display());
    Ok(())
}

fn remove_module(module_name: &str, quiet: bool) -> Result<()> {
    // Convert module name to C string
    let modname_cstr = CString::new(module_name)?;

    const O_NONBLOCK: i32 = 0x00004000;
    const O_EXCL: i32 = 0x00000080;

    let result = unsafe {
        syscall_delete_module(modname_cstr.as_ptr(), O_NONBLOCK | O_EXCL)
    };

    if result != 0 {
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if !quiet || errno != libc::ENOENT {
            log::debug!("modprobe: delete_module {} failed: {} (errno={})", module_name, std::io::Error::last_os_error(), errno);
            return Err(eyre!("Failed to remove module '{}': {} (errno={})",
                module_name,
                std::io::Error::last_os_error(),
                errno));
        }
        log::debug!("modprobe: remove {} not loaded (errno ENOENT, quiet)", module_name);
    } else {
        log::debug!("modprobe: removed module {}", module_name);
    }

    Ok(())
}

pub fn run(options: ModprobeOptions) -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = options;
        return Err(eyre!("modprobe is only supported on Linux"));
    }

    #[cfg(target_os = "linux")]
    {
        let release = get_kernel_release()?;
        let (db, base_path) = setup_module_db(&release)?;

        if options.remove {
            log::debug!("modprobe: remove module {}", options.module);
            remove_module(&options.module, options.quiet)?;
        } else {
            log::debug!("modprobe: load module {} (params: {:?})", options.module, options.params);
            db.load_module_with_deps(&base_path, &options.module, &options.params, options.quiet)?;
        }
        Ok(())
    }
}

fn get_kernel_release() -> Result<String> {
    let mut uts = libc::utsname {
        sysname: [0; 65],
        nodename: [0; 65],
        release: [0; 65],
        version: [0; 65],
        machine: [0; 65],
        domainname: [0; 65],
    };

    if unsafe { libc::uname(&mut uts) } != 0 {
        let e = std::io::Error::last_os_error();
        log::debug!("modprobe: uname() failed: {}", e);
        return Err(eyre!("Failed to get kernel version: {}", e));
    }

    Ok(unsafe {
        std::ffi::CStr::from_ptr(uts.release.as_ptr())
            .to_string_lossy()
            .into_owned()
    })
}

fn setup_module_db(release: &str) -> Result<(ModuleDb, PathBuf)> {
    let base_paths = [
        Path::new("/lib/modules").join(release),
        Path::new("/usr/lib/modules").join(release),
    ];

    let mut db = ModuleDb::new();
    let mut found_modules_dir = false;

    for base_path in &base_paths {
        log::debug!("modprobe: trying base_path={} exists={}", base_path.display(), base_path.exists());
        if base_path.exists() {
            found_modules_dir = true;
            let _ = db.load_dep(base_path);
            break;
        }
    }

    if !found_modules_dir {
        log::debug!("modprobe: no modules directory found in /lib/modules or /usr/lib/modules for kernel {}", release);
    }

    let base_path = base_paths.iter()
        .find(|p| p.exists())
        .unwrap_or(&base_paths[0])
        .clone();

    Ok((db, base_path))
}
