use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use std::path::Path;
use std::collections::{HashMap, HashSet};
use crate::models::{InstalledPackageInfo, Package, PackageFormat, SUPPORT_ARCH_LIST};
use std::sync::{Arc, Mutex};
use crate::mmio;
use crate::models::dirs;
use crate::models::PACKAGE_CACHE;
use crate::utils;
use time::OffsetDateTime;
use time::macros::format_description;
use time::UtcOffset;
use crate::store::unpack_package;
use crate::search::SearchOptions;
use crate::package::{self, PackageNVRA};
use crate::package_cache::{map_pkgline2package, map_pkgname2packages, load_package_info};
use crate::hooks::parse_hook_file;
use crate::repo::sync_channel_metadata;
use glob::Pattern;

#[derive(Debug)]
pub struct RpmOptions {
    pub all: bool,
    pub info: bool,
    pub list: bool,
    pub file: Option<String>,
    pub path: Option<String>,
    pub package: Option<String>,
    pub whatprovides: Option<String>,
    pub whatrequires: Option<String>,
    pub whatconflicts: Option<String>,
    pub whatobsoletes: Option<String>,
    pub whatrecommends: Option<String>,
    pub whatsuggests: Option<String>,
    pub whatsupplements: Option<String>,
    pub whatenhances: Option<String>,
    pub scripts: bool,
    pub triggers: bool,
    pub filetriggers: bool,
    pub provides: bool,
    pub requires: bool,
    pub conflicts: bool,
    pub enhances: bool,
    pub obsoletes: bool,
    pub recommends: bool,
    pub suggests: bool,
    pub supplements: bool,
    pub verify: bool,
    pub state: bool,
    pub install: bool,
    pub upgrade: bool,
    pub erase: bool,
    pub package_specs: Vec<String>,
    pub allow_repo_query: bool,
}

/// Print a field with proper RPM formatting (colon at column 13, width 12)
/// Labels longer than 12 characters will break alignment
/// Empty values are printed as empty string
fn print_field(label: &str, value: &str) {
    println!("{:12}: {}", label, value);
}

/// Print an optional field if it contains Some(value)
macro_rules! print_optional_field {
    ($opt:expr, $label:expr) => {
        if let Some(value) = $opt {
            print_field($label, value);
        }
    };
}

/// Extract interpreter from script content (first line shebang)
fn extract_interpreter(content: &str) -> &str {
    if let Some(first_line) = content.lines().next() {
        if first_line.starts_with("#!") {
            first_line[2..].trim()
        } else {
            "/bin/sh"
        }
    } else {
        "/bin/sh"
    }
}

/// Print script content excluding shebang line. Trailing blank lines are skipped so
/// output matches host rpm (no extra newline between trigger blocks).
fn print_script_content(content: &str) {
    let lines: Vec<&str> = content.lines().collect();
    let start = if lines.get(0).map(|l| l.starts_with("#!")).unwrap_or(false) {
        1
    } else {
        0
    };
    let mut end = lines.len();
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    for line in lines.iter().skip(start).take(end - start) {
        println!("{}", line);
    }
}



/// Generic function to select installed packages by predicate
/// Returns vector of pkgline strings for matching packages
pub(crate) fn select_installed_packages_by_predicate<P>(
    predicate: P,
) -> Result<Vec<String>>
where
    P: Fn(&Package, &InstalledPackageInfo) -> bool,
{
    // Load installed packages including pending packages from current transaction
    crate::io::load_installed_packages()?;

    let mut pkglines = Vec::new();
    let mut found_keys = HashSet::new();

    for (pkgkey, installed_info) in PACKAGE_CACHE.installed_packages.read().unwrap().iter() {
        let package = match load_package_info(pkgkey) {
            Ok(pkg) => pkg,
            Err(_) => continue,
        };

        if predicate(&package, installed_info.as_ref()) {
            // Deduplicate by pkgkey
            if found_keys.insert(pkgkey.clone()) {
                pkglines.push(installed_info.pkgline.clone());
            }
        }
    }

    Ok(pkglines)
}

/// Process package paths with a custom formatter
fn process_package_paths<F>(
    store_path: &Path,
    formatter: F,
) -> Result<()>
where
    F: Fn(&str),
{
    for path in normalized_package_file_paths(store_path)? {
        formatter(&path);
    }
    Ok(())
}

/// Display package information in RPM format
/// If installed_info is None, fields that require installation (Install Date) are omitted
fn display_package_info(package: &Package, installed_info: Option<&InstalledPackageInfo>) {
    // RPM -qi style: Name, Version, Release, Architecture
    let (version, release) = match package.version.rsplit_once('-') {
        Some((v, r)) => (v.to_string(), r.to_string()),
        None => (package.version.clone(), String::new()),
    };
    print_field("Name", &package.pkgname);
    print_field("Version", &version);
    print_field("Release", &release);
    print_field("Architecture", &package.arch);

    // Install Date
    let install_date = if let Some(info) = installed_info {
        format_rpm_date(info.install_time)
    } else {
        "(not installed)".to_string()
    };
    print_field("Install Date", &install_date);

    // Group
    print_field("Group", package.section.as_deref().unwrap_or("Unspecified"));

    // Size (installed size)
    print_field("Size", &package.installed_size.to_string());

    // Optional fields: License, Signature, Source RPM, Build Date, Build Host, Packager, Vendor, URL
    print_optional_field!(&package.license, "License");
    print_optional_field!(&package.signature, "Signature");
    print_optional_field!(&package.source, "Source RPM");

    // Build Date (renamed from BuildTime)
    let build_date = package.build_time.map(|t| format_rpm_date(t.into())).unwrap_or_default();
    print_field("Build Date", &build_date);

    print_optional_field!(&package.build_host, "Build Host");

    // Packager (renamed from Maintainer)
    print_field("Packager", &package.maintainer);

    print_optional_field!(&package.vendor, "Vendor");

    // URL
    print_field("URL", &package.homepage);

    // Summary and Description
    print_field("Summary", &package.summary);
    if let Some(description) = package.description.as_ref() {
        println!("{:12}:", "Description");
        for line in description.lines() {
            println!("  {}", line);
        }
    }

    print_optional_field!(&package.relocations, "Relocations");

}

fn format_rpm_date(timestamp: u64) -> String {
    match OffsetDateTime::from_unix_timestamp(timestamp as i64) {
        Ok(utc) => {
            // Try to get local offset, fallback to UTC
            let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
            let local = utc.to_offset(offset);
            // Format as "Wed 24 Dec 2025 08:54:12 PM CST"
            // Try to get timezone abbreviation
            let tz_abbr = if offset == UtcOffset::UTC {
                "UTC".to_string()
            } else {
                // Common offsets
                let h = offset.whole_hours();
                let m_abs = offset.whole_minutes().abs() % 60;
                let s_abs = offset.whole_seconds().abs() % 60;
                match (h, m_abs, s_abs) {
                    (8, 0, 0) => "CST".to_string(),
                    (-5, 0, 0) => "EST".to_string(),
                    (0, 0, 0) => "UTC".to_string(),
                    _ => offset.to_string(),
                }
            };
            // Build format string
            let fmt = format_description!("[weekday repr:short] [day] [month repr:short] [year] [hour repr:12]:[minute]:[second] [period case:upper]");
            match local.format(&fmt) {
                Ok(s) => format!("{} {}", s, tz_abbr),
                Err(_) => timestamp.to_string(),
            }
        },
        Err(_) => timestamp.to_string(),
    }
}

/// Get normalized file paths for a package store directory (package root).
pub(crate) fn normalized_package_file_paths(store_path: &Path) -> Result<Vec<String>> {
    utils::list_package_file_paths_normalized(store_path)
}

/// Unpack an RPM file or load a pkgline from store for querying.
/// Returns Package with pkgline set (use dirs().epkg_store.join(pkgline) for store path).
fn unpack_rpm_for_query(rpm_path: &str) -> Result<Package> {
    let parts: Vec<&str> = rpm_path.split("__").collect();
    match parts.len() {
        4 => {
            // pkgline format: ca_hash__name__version__arch
            mmio::map_pkgline2package(rpm_path)
                .wrap_err_with(|| format!("Failed to load package from pkgline: {}", rpm_path))
        }
        1 => {
            let path = Path::new(rpm_path);
            if !path.exists() {
                eprintln!("error: open of {} failed: No such file or directory", rpm_path);
                std::process::exit(1);
            }

            // Input is a package file, unpack it
            let store_pkglines_by_pkgname = HashMap::new();
            let dummy_pkgkey = ""; // pkgkey not needed for query unpacking

            let (_actual_pkgkey, pkgline) = unpack_package(
                rpm_path,
                dummy_pkgkey,
                &store_pkglines_by_pkgname,
                Some(PackageFormat::Rpm),
            ).wrap_err_with(|| format!("Failed to unpack RPM file: {}", rpm_path))?;

            // Load package from store using pkgline (sets package.pkgline)
            mmio::map_pkgline2package(&pkgline)
                .wrap_err_with(|| format!("Failed to load package from store pkgline: {}", pkgline))
        }
        _ => {
            Err(color_eyre::eyre::eyre!("Invalid package or path: {}", rpm_path))
        }
    }
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<RpmOptions> {
    let all             = matches.get_flag("all");
    let info            = matches.get_flag("info");
    let list            = matches.get_flag("list");
    let file            = matches.get_one::<String>("file").cloned();
    if matches.contains_id("file") && file.is_none() {
        // Match system rpm error message format for missing argument
        eprintln!("rpm: no arguments given for query");
        std::process::exit(1);
    }
    let path            = matches.get_one::<String>("path").cloned();
    let package         = matches.get_one::<String>("package").cloned();
    if matches.contains_id("package") && package.is_none() {
        // Match system rpm error message format for missing argument
        eprintln!("rpm: no arguments given for query");
        std::process::exit(1);
    }
    let whatprovides    = matches.get_one::<String>("whatprovides").cloned();
    let whatrequires    = matches.get_one::<String>("whatrequires").cloned();
    let whatconflicts   = matches.get_one::<String>("whatconflicts").cloned();
    let whatobsoletes   = matches.get_one::<String>("whatobsoletes").cloned();
    let whatrecommends  = matches.get_one::<String>("whatrecommends").cloned();
    let whatsuggests    = matches.get_one::<String>("whatsuggests").cloned();
    let whatsupplements = matches.get_one::<String>("whatsupplements").cloned();
    let whatenhances    = matches.get_one::<String>("whatenhances").cloned();
    let scripts         = matches.get_flag("scripts");
    let triggers        = matches.get_flag("triggers");
    let filetriggers    = matches.get_flag("filetriggers");
    let provides        = matches.get_flag("provides");
    let requires        = matches.get_flag("requires");
    let conflicts       = matches.get_flag("conflicts");
    let enhances        = matches.get_flag("enhances");
    let obsoletes       = matches.get_flag("obsoletes");
    let recommends      = matches.get_flag("recommends");
    let suggests        = matches.get_flag("suggests");
    let supplements     = matches.get_flag("supplements");
    let verify          = matches.get_flag("verify");
    let state           = matches.get_flag("state");
    let install         = matches.get_flag("install");
    let upgrade         = matches.get_flag("upgrade");
    let erase           = matches.get_flag("erase");
    let package_specs: Vec<String> = matches.get_many::<String>("packages")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(RpmOptions {
        all,
        info,
        list,
        file,
        path,
        package,
        whatprovides,
        whatrequires,
        whatconflicts,
        whatobsoletes,
        whatrecommends,
        whatsuggests,
        whatsupplements,
        whatenhances,
        scripts,
        triggers,
        filetriggers,
        provides,
        requires,
        conflicts,
        enhances,
        obsoletes,
        recommends,
        suggests,
        supplements,
        verify,
        state,
        install,
        upgrade,
        erase,
        package_specs,
        allow_repo_query: false,
    })
}


fn add_query_flags(cmd: Command) -> Command {
    cmd
        .arg(Arg::new("all")
            .short('a')
            .long("all")
            .action(clap::ArgAction::SetTrue)
            .help("List all installed packages"))
        .arg(Arg::new("info")
            .short('i')
            .long("info")
            .action(clap::ArgAction::SetTrue)
            .help("Display package information"))
        .arg(Arg::new("list")
            .short('l')
            .long("list")
            .action(clap::ArgAction::SetTrue)
            .help("List files in package"))
        .arg(Arg::new("file")
            .short('f')
            .long("file")
            .value_name("FILE")
            .num_args(1)
            .action(clap::ArgAction::Set)
            .value_parser(clap::value_parser!(String))
            .help("Query package owning FILE"))
        .arg(Arg::new("path")
            .long("path")
            .value_name("PATH")
            .num_args(1)
            .action(clap::ArgAction::Set)
            .value_parser(clap::value_parser!(String))
            .help("Query package(s) owning PATH (installed or in repos)"))
        .arg(Arg::new("package")
            .short('p')
            .long("package")
            .value_name("PACKAGE_FILE")
            .num_args(1)
            .action(clap::ArgAction::Set)
            .value_parser(clap::value_parser!(String))
            .help("Query an (uninstalled) package file"))
}

fn add_what_query_flags(cmd: Command) -> Command {
    cmd
        .arg(Arg::new("whatprovides")
            .long("whatprovides")
            .value_name("CAPABILITY")
            .num_args(1)
            .action(clap::ArgAction::Set)
            .value_parser(clap::value_parser!(String))
            .help("Query packages that provide CAPABILITY"))
        .arg(Arg::new("whatrequires")
            .long("whatrequires")
            .value_name("CAPABILITY")
            .num_args(1)
            .action(clap::ArgAction::Set)
            .value_parser(clap::value_parser!(String))
            .help("Query packages that require CAPABILITY"))
        .arg(Arg::new("whatconflicts")
            .long("whatconflicts")
            .value_name("CAPABILITY")
            .num_args(1)
            .action(clap::ArgAction::Set)
            .value_parser(clap::value_parser!(String))
            .help("Query packages that conflict with CAPABILITY"))
        .arg(Arg::new("whatobsoletes")
            .long("whatobsoletes")
            .value_name("CAPABILITY")
            .num_args(1)
            .action(clap::ArgAction::Set)
            .value_parser(clap::value_parser!(String))
            .help("Query packages that obsolete CAPABILITY"))
        .arg(Arg::new("whatrecommends")
            .long("whatrecommends")
            .value_name("CAPABILITY")
            .num_args(1)
            .action(clap::ArgAction::Set)
            .value_parser(clap::value_parser!(String))
            .help("Query packages that recommend CAPABILITY"))
        .arg(Arg::new("whatsuggests")
            .long("whatsuggests")
            .value_name("CAPABILITY")
            .num_args(1)
            .action(clap::ArgAction::Set)
            .value_parser(clap::value_parser!(String))
            .help("Query packages that suggest CAPABILITY"))
        .arg(Arg::new("whatsupplements")
            .long("whatsupplements")
            .value_name("CAPABILITY")
            .num_args(1)
            .action(clap::ArgAction::Set)
            .value_parser(clap::value_parser!(String))
            .help("Query packages that supplement CAPABILITY"))
        .arg(Arg::new("whatenhances")
            .long("whatenhances")
            .value_name("CAPABILITY")
            .num_args(1)
            .action(clap::ArgAction::Set)
            .value_parser(clap::value_parser!(String))
            .help("Query packages that enhance CAPABILITY"))
}

fn add_script_flags(cmd: Command) -> Command {
    cmd
        .arg(Arg::new("scripts")
            .long("scripts")
            .action(clap::ArgAction::SetTrue)
            .help("List package scripts"))
        .arg(Arg::new("triggers")
            .long("triggers")
            .visible_alias("triggerscripts")
            .action(clap::ArgAction::SetTrue)
            .help("List package triggers"))
        .arg(Arg::new("filetriggers")
            .long("filetriggers")
            .action(clap::ArgAction::SetTrue)
            .help("List file triggers"))
}

fn add_dependency_flags(cmd: Command) -> Command {
    cmd
        .arg(Arg::new("provides")
            .long("provides")
            .action(clap::ArgAction::SetTrue)
            .help("List capabilities provided by package"))
        .arg(Arg::new("requires")
            .short('R')
            .long("requires")
            .action(clap::ArgAction::SetTrue)
            .help("List capabilities required by package"))
        .arg(Arg::new("conflicts")
            .long("conflicts")
            .action(clap::ArgAction::SetTrue)
            .help("List packages conflicting with package"))
        .arg(Arg::new("enhances")
            .long("enhances")
            .action(clap::ArgAction::SetTrue)
            .help("List packages enhanced by package"))
        .arg(Arg::new("obsoletes")
            .long("obsoletes")
            .action(clap::ArgAction::SetTrue)
            .help("List packages obsoleted by package"))
        .arg(Arg::new("recommends")
            .long("recommends")
            .action(clap::ArgAction::SetTrue)
            .help("List packages recommended by package"))
        .arg(Arg::new("suggests")
            .long("suggests")
            .action(clap::ArgAction::SetTrue)
            .help("List packages suggested by package"))
        .arg(Arg::new("supplements")
            .long("supplements")
            .action(clap::ArgAction::SetTrue)
            .help("List packages supplemented by package"))
}

fn add_verify_state_flags(cmd: Command) -> Command {
    cmd
        .arg(Arg::new("verify")
            .short('V')
            .long("verify")
            .action(clap::ArgAction::SetTrue)
            .help("Verify package integrity"))
        .arg(Arg::new("state")
            .short('s')
            .long("state")
            .action(clap::ArgAction::SetTrue)
            .help("Display file states"))
}

fn add_install_flags(cmd: Command) -> Command {
    cmd
        .arg(Arg::new("install")
            .short('I')
            .long("install")
            .action(clap::ArgAction::SetTrue)
            .help("Install package(s)"))
        .arg(Arg::new("upgrade")
            .short('U')
            .long("upgrade")
            .action(clap::ArgAction::SetTrue)
            .help("Upgrade package(s)"))
        .arg(Arg::new("erase")
            .short('e')
            .long("erase")
            .action(clap::ArgAction::SetTrue)
            .help("Remove package(s)"))
}

pub fn command() -> Command {
    let cmd = Command::new("rpm")
        .about("RPM package manager query tool")
        .arg_required_else_help(true) // This will show help if no args are provided
        .arg(Arg::new("query")
            .short('q')
            .long("query")
            .action(clap::ArgAction::SetTrue)
            .help("Query mode (default)"));
    let cmd = add_query_flags(cmd);
    let cmd = add_what_query_flags(cmd);
    let cmd = add_script_flags(cmd);
    let cmd = add_dependency_flags(cmd);
    let cmd = add_verify_state_flags(cmd);
    let cmd = add_install_flags(cmd);
    cmd.arg(Arg::new("packages")
        .value_name("PACKAGE_SPECS")
        .help("Package name(s) to query")
        .num_args(0..))
}

/// Resolve pkgline to (Package, InstalledPackageInfo). Returns None if not installed or load fails.
/// Shared by rpm and dpkg-query applets; other distro query applets should use this.
pub(crate) fn pkgline_to_package_and_installed_info(pkgline: &str) -> Option<(Package, Arc<InstalledPackageInfo>)> {
    let installed_info = PACKAGE_CACHE.pkgline2installed.read().unwrap().get(pkgline).cloned()
        .or_else(|| {
            let pkgkey = package::pkgline2pkgkey(pkgline).ok()?;
            PACKAGE_CACHE.installed_packages.read().unwrap().get(&pkgkey).cloned()
        })?;
    let package = map_pkgline2package(pkgline).ok()?;
    Some((package.as_ref().clone(), installed_info))
}

/// Select installed pkglines by PackageNVRA (name as glob, optional version and arch).
/// Use name "*" and version/arch None for all installed.
/// Shared by rpm and dpkg-query applets; other distro query applets should use this.
pub(crate) fn select_installed_pkglines_by_nvra(spec: &PackageNVRA) -> Result<Vec<String>> {
    let name_pattern_glob = Pattern::new(&spec.name).ok();
    let name_pattern = spec.name.clone();
    select_installed_packages_by_predicate(move |package, _installed_info| {
        let name_ok = if let Some(ref pat) = name_pattern_glob {
            pat.matches(&package.pkgname)
        } else {
            package.pkgname == name_pattern
        };
        if !name_ok {
            return false;
        }
        if let Some(ref arch) = spec.arch {
            if package.arch != *arch {
                return false;
            }
        }
        if let Some(ref version) = spec.version {
            if package.version != *version {
                return false;
            }
        }
        true
    })
}

/// Select installed pkglines that own a path matching the given pattern.
/// Pattern can be exact path, path prefix, substring, or glob (*?[).
/// Shared by rpm and dpkg-query applets; other distro query applets should use this.
pub(crate) fn select_installed_pkglines_owning_path(path_pattern: &str) -> Result<Vec<String>> {
    // Load installed packages including pending packages from current transaction
    crate::io::load_installed_packages()?;
    let state = path_match_prepare(path_pattern);
    let mut pkglines = Vec::new();
    for (_pkgkey, installed_info) in PACKAGE_CACHE.installed_packages.read().unwrap().iter() {
        let store_path = dirs().epkg_store.join(&installed_info.pkgline);
        let paths = match utils::list_package_file_paths_normalized(&store_path) {
            Ok(p) => p,
            Err(_) => continue,
        };
        for normalized_path in paths {
            if path_match_matches(&state, &normalized_path) {
                pkglines.push(installed_info.pkgline.clone());
                break;
            }
        }
    }
    Ok(pkglines)
}

/// True if the string contains glob metacharacters (*?[).
fn is_glob_pattern(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[')
}

/// Pre-compiled path pattern for matching many paths without re-parsing.
/// Build once with path_match_prepare(), then call path_match_matches() in a loop.
pub(crate) enum PathMatchState {
    Glob(Pattern),
    Literal(String),
}

/// Setup path matching once; use path_match_matches() in the loop.
pub(crate) fn path_match_prepare(path_pattern: &str) -> PathMatchState {
    if is_glob_pattern(path_pattern) {
        if let Ok(pat) = Pattern::new(path_pattern) {
            return PathMatchState::Glob(pat);
        }
    }
    PathMatchState::Literal(path_pattern.to_string())
}

/// Returns true if normalized_path matches the pre-prepared state.
pub(crate) fn path_match_matches(state: &PathMatchState, normalized_path: &str) -> bool {
    match state {
        PathMatchState::Glob(pat) => pat.matches(normalized_path),
        PathMatchState::Literal(path_pattern) => {
            normalized_path.contains(path_pattern.as_str())
        }
    }
}

/// Resolve pkgline (ca_hash__name__version__arch) to (Package, Option<InstalledPackageInfo>).
/// Uses pkgline2installed for O(1) lookup when populated; falls back to pkgkey lookup.
/// Returned Package has pkgline set (from store load).
/// If package is not installed, installed_info is None.
fn resolve_from_pkgline(pkgline: &str) -> Option<(Package, Option<Arc<InstalledPackageInfo>>)> {
    let installed_info = PACKAGE_CACHE.pkgline2installed.read().unwrap().get(pkgline).cloned();
    let package_arc = map_pkgline2package(pkgline).ok()?;
    Some((package_arc.as_ref().clone(), installed_info))
}

/// Resolve installed packages by PackageNVRA (name/version/arch spec).
/// Returns (Package, Some(InstalledPackageInfo)) for each matching installed package.
fn resolve_installed_by_spec(spec: &PackageNVRA) -> Vec<(Package, Option<Arc<InstalledPackageInfo>>)> {
    let pkglines = match select_installed_pkglines_by_nvra(spec) {
        Ok(p) => p,
        Err(_) => return vec![],
    };
    pkglines
        .into_iter()
        .filter_map(|pkgline| resolve_from_pkgline(&pkgline))
        .collect()
}

/// Resolve pkgkey (name__version__arch). If installed, returns (Package, Some(info));
/// if only in repo and allow_repo_query, returns (Package, None).
fn resolve_from_pkgkey(
    pkgkey: &str,
    allow_repo_query: bool,
) -> Option<(Package, Option<Arc<InstalledPackageInfo>>)> {
    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
    if let Some(info) = installed.get(pkgkey) {
        let pkgline = info.pkgline.clone();
        drop(installed);
        return resolve_from_pkgline(&pkgline);
    }
    drop(installed);
    if allow_repo_query {
        return load_package_info(pkgkey).ok().map(|pkg| (pkg.as_ref().clone(), None));
    }
    None
}

/// Resolve a package specification to a Package and optional installed info.
/// Package has pkgline set when in store; use dirs().epkg_store.join(pkgline) for store path.
///
/// Supported package specification formats:
/// 1. Simple package name: "bash"
/// 2. Package name with architecture suffix separated by dot: "bash.x86_64"
/// 3. Package name-version: "bash-5.2.15"
/// 4. Package name-version-release: "bash-5.2.15-9.oe2403"
/// 5. Package name-version-release.arch: "bash-5.2.15-9.oe2403.x86_64"
/// 6. pkgkey format: "bash__5.2.15-9.oe2403__x86_64"
/// 7. pkgline format: "ca_hash__bash__5.2.15-9.oe2403__x86_64"
/// 8. Legacy format with colon separator: "bash:x86_64"
///
/// Returns matches: (Package, Option<Arc<InstalledPackageInfo>>). Package has pkgline set when in store.
/// For repo-only packages, installed_info is None and pkgline is None.
pub(crate) fn resolve_package_spec(
    pkg_spec: &str,
    allow_repo_query: bool,
) -> Vec<(Package, Option<Arc<InstalledPackageInfo>>)> {
    let parts: Vec<&str> = pkg_spec.split("__").collect();
    match parts.len() {
        4 => { // pkgline format: ca_hash__name__version__arch
            resolve_from_pkgline(pkg_spec)
                .into_iter()
                .collect()
        }
        3 => { // pkgkey format: name__version__arch
            resolve_from_pkgkey(pkg_spec, allow_repo_query).into_iter().collect()
        }
        1 => { // pkgname
            let _ = sync_channel_metadata();
            if is_glob_pattern(pkg_spec) {
                resolve_installed_by_spec(&parse_rpm_nvra(pkg_spec))
            } else {
                let repo_packages = match map_pkgname2packages(pkg_spec) {
                    Ok(pkgs) => pkgs,
                    Err(_) => vec![],
                };
                let mut results = Vec::new();
                for pkg in &repo_packages {
                    if let Some(r) = resolve_from_pkgkey(&pkg.pkgkey, allow_repo_query) {
                        results.push(r);
                    }
                }
                if results.is_empty() {
                    resolve_installed_by_spec(&parse_rpm_nvra(pkg_spec))
                } else {
                    results
                }
            }
        }
        _ => {
            log::warn!("invalid pkgkey format {}", pkg_spec);
            vec![]
        }
    }
}

/// Extract architecture suffix from spec using colon or dot separator.
/// Returns (architecture, remaining_spec) if suffix is a known architecture.
fn extract_arch_suffix(spec: &str) -> (Option<String>, &str) {
    // Check for colon separator first (legacy format)
    if let Some(colon_pos) = spec.rfind(':') {
        let suffix = &spec[colon_pos + 1..];
        if SUPPORT_ARCH_LIST.contains(&suffix) || suffix == "noarch" || suffix == "any" {
            return (Some(suffix.to_string()), &spec[..colon_pos]);
        }
    }
    // If no colon architecture, try dot separator
    if let Some(dot_pos) = spec.rfind('.') {
        let suffix = &spec[dot_pos + 1..];
        if SUPPORT_ARCH_LIST.contains(&suffix) || suffix == "noarch" || suffix == "any" {
            return (Some(suffix.to_string()), &spec[..dot_pos]);
        }
    }
    (None, spec)
}

/// Split version from name by finding first hyphen followed by a digit.
/// Returns (version, remaining_name) if found.
fn split_version_from_name(name: &str) -> (Option<String>, &str) {
    for (i, c) in name.char_indices() {
        if c == '-' && i + 1 < name.len() {
            let next_char = name[i + 1..].chars().next().unwrap();
            if next_char.is_ascii_digit() {
                let version = name[i + 1..].to_string();
                return (Some(version), &name[..i]);
            }
        }
    }
    (None, name)
}

/// Parse RPM style spec: name-version-release.arch or name-version-release or name.arch
fn parse_rpm_nvra(spec: &str) -> PackageNVRA {
    let (arch, name_without_arch) = extract_arch_suffix(spec);
    let (version, name_only) = split_version_from_name(name_without_arch);
    PackageNVRA {
        name: name_only.to_string(),
        version,
        arch,
    }
}

/// Generic function to process each package in a list.
/// Loads installed packages, resolves each spec, and calls the closure.
/// Handles error reporting and exit codes.
/// store_path is Some when package.pkgline is set (in-store packages).
fn for_each_package<F>(
    package_specs: &[String],
    options: &RpmOptions,
    mut f: F,
) -> Result<()>
where
    F: FnMut(&Package, Option<&Path>, Option<&InstalledPackageInfo>) -> Result<()>,
{
    // Load installed packages including pending packages from current transaction
    crate::io::load_installed_packages()?;
    let mut exit_code = 0;
    for pkg_spec in package_specs {
        let matches = resolve_package_spec(pkg_spec, options.allow_repo_query);
        if matches.is_empty() {
            eprintln!("package {} is not installed", pkg_spec);
            exit_code = 1;
        } else {
            for (package, installed_info) in matches {
                let store_path = package.pkgline.as_ref().map(|p| dirs().epkg_store.join(p));
                let store_path = store_path.as_deref();
                if let Err(e) = f(&package, store_path, installed_info.as_deref()) {
                    eprintln!("error processing {}: {}", pkg_spec, e);
                    exit_code = 1;
                }
            }
        }
    }
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

fn get_all_package_specs() -> Result<Vec<String>> {
    // Load installed packages including pending packages from current transaction
    crate::io::load_installed_packages()?;

    let mut specs = Vec::new();
    for pkgkey in PACKAGE_CACHE.installed_packages.read().unwrap().keys() {
        specs.push(pkgkey.clone());
    }
    // Sort by package name (extract name from pkgkey)
    specs.sort_by(|a, b| {
        let a_name = a.split("__").next().unwrap_or(a);
        let b_name = b.split("__").next().unwrap_or(b);
        a_name.cmp(b_name)
    });
    Ok(specs)
}

fn list_package_files(store_path: &Path) -> Result<()> {
    process_package_paths(store_path, |path| println!("/{}", path))
}

fn show_package_state(store_path: &Path) -> Result<()> {
    process_package_paths(store_path, |path| println!("normal        {}", path))
}

fn verify_packages(store_path: &Path) -> Result<()> {
    process_package_paths(store_path, |path| println!("....... {}", path))
}

fn show_package_scripts(store_path: &Path) -> Result<()> {
    // Order of scriptlets as defined by RPM convention
    const SCRIPT_ORDER: [&str; 8] = [
        "pre_install",
        "post_install",
        "pre_uninstall",
        "post_uninstall",
        "pre_trans",
        "post_trans",
        "pre_untrans",
        "post_untrans",
    ];
    // Mapping from stored filename prefix to RPM scriptlet name
    let scriptlet_mapping: HashMap<&str, &str> = [
        ("pre_install", "preinstall"),
        ("post_install", "postinstall"),
        ("pre_uninstall", "preuninstall"),
        ("post_uninstall", "postuninstall"),
        ("pre_trans", "pretrans"),
        ("post_trans", "posttrans"),
        ("pre_untrans", "preuntrans"),
        ("post_untrans", "postuntrans"),
    ].into_iter().collect();

    let scripts_dir = crate::dirs::path_join(store_path, &["info", "install"]);
    if !scripts_dir.exists() {
        return Ok(());
    }

    // Collect and sort script files
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&scripts_dir)?.flatten() {
        let filename = entry.file_name();
        let filename_str = filename.to_string_lossy();

        // Filter out .hook files and rpm-* files
        if filename_str.ends_with(".hook") || filename_str.starts_with("rpm-") {
            continue;
        }

        let base_name = filename_str.split('.').next().unwrap_or(&filename_str);
        let order = SCRIPT_ORDER.iter().position(|&s| s == base_name).unwrap_or(usize::MAX);
        entries.push((order, filename_str.to_string()));
    }

    // Sort by order, then by filename for equal order (unknown scripts)
    entries.sort_by_key(|(order, filename)| (*order, filename.clone()));

    // Process each script file in sorted order
    for (_, filename) in entries {
        let path = scripts_dir.join(&filename);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("failed to read script file {}: {}", filename, e);
                continue;
            }
        };

        let base_name = filename.split('.').next().unwrap_or(&filename);
        let scriptlet_name = scriptlet_mapping.get(base_name).map(|&s| s.to_string()).unwrap_or_else(|| "unknown".to_string());
        let interpreter = extract_interpreter(&content);

        println!("{} scriptlet (using {}):", scriptlet_name, interpreter);
        print_script_content(&content);
    }

    Ok(())
}

fn show_package_triggers(store_path: &Path) -> Result<()> {
    let filter = |filename: &str| filename.starts_with("rpm-trigger");
    show_triggers_generic(store_path, filter)
}

fn show_file_triggers(store_path: &Path) -> Result<()> {
    let filter = |filename: &str| {
        filename.starts_with("rpm-filetrigger") || filename.starts_with("rpm-transfiletrigger")
    };
    show_triggers_generic(store_path, filter)
}

/// Generic function to show triggers (package or file triggers).
/// `filter` decides which script files to include based on filename.
/// Calls parse_hook_file() to extract script_order and targets from the corresponding .hook file.
fn collect_trigger_items<F>(scripts_dir: &Path, filter: F) -> Vec<(u32, String, String, Vec<String>)>
where
    F: Fn(&str) -> bool,
{
    let mut items = Vec::new();
    let entries = match std::fs::read_dir(scripts_dir) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("failed to read scripts directory: {}", e);
            return items;
        }
    };
    for entry in entries.flatten() {
        let filename = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };

        if !filter(&filename) || filename.ends_with(".hook") || filename.split('-').count() < 3 {
            continue;
        }

        let path = scripts_dir.join(&filename);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("failed to read script file {}: {}", filename, e);
                continue;
            }
        };

        let hook_filename = Path::new(&filename).with_extension("");
        let hook_path = scripts_dir.join(format!("{}.hook", hook_filename.to_string_lossy()));
        let (script_order, targets) = match parse_hook_file(&hook_path, None) {
            Ok(hook) => {
                let script_order = hook.action.script_order;
                let targets: Vec<String> = hook.triggers.iter()
                    .flat_map(|trigger| trigger.targets.iter().cloned())
                    .collect();
                (script_order, targets)
            }
            Err(_) => (0, Vec::new()),
        };
        items.push((script_order, filename, content, targets));
    }
    items
}

fn print_trigger_items(items: &[(u32, String, String, Vec<String>)]) {
    for (_order, filename, content, targets) in items {
        let scriptlet_name = filename
            .split('-')
            .nth(1)
            .unwrap_or("trigger")
            .to_string();
        let interpreter = extract_interpreter(&content);

        if targets.is_empty() {
            println!("{} scriptlet (using {}):", scriptlet_name, interpreter);
        } else {
            // Normalize targets: add leading '/' if missing
            let normalized_targets: Vec<String> = targets.iter()
                .map(|target| {
                    if target.starts_with('/') {
                        target.clone()
                    } else {
                        format!("/{}", target)
                    }
                })
                .collect();
            let target_str = normalized_targets.join(", ");
            println!("{} scriptlet (using {}) -- {}", scriptlet_name, interpreter, target_str);
        }

        print_script_content(&content);
    }
}

fn show_triggers_generic<F>(
    store_path: &Path,
    filter: F,
) -> Result<()>
where
    F: Fn(&str) -> bool,
{
    let scripts_dir = crate::dirs::path_join(store_path, &["info", "install"]);
    if !scripts_dir.exists() {
        return Ok(());
    }

    let mut items = collect_trigger_items(&scripts_dir, filter);
    items.sort_by_key(|(ord, _, _, _)| *ord);
    print_trigger_items(&items);

    Ok(())
}

fn show_dependency_list(items: &[String]) -> Result<()> {
    for item in items {
        println!("{}", item);
    }
    Ok(())
}

fn select_installed_packages_by_file(file_pattern: &str, options: &mut RpmOptions) -> Result<usize> {
    let pkglines = select_installed_pkglines_owning_path(file_pattern)?;
    let count = pkglines.len();
    options.package_specs.extend(pkglines);
    Ok(count)
}

/// Find repository packages that own the given path.
/// Returns a de-duplicated list of package names.
fn select_repo_packages_by_path(path: &str) -> Result<Vec<String>> {
    let mut search_opts = SearchOptions {
        paths: true,
        origin_pattern: path.to_string(),
        collected_results: Some(Arc::new(Mutex::new(Vec::new()))),
        ..Default::default()
    };

    // search_repo_cache will fill collected_results
    log::debug!("Searching repositories for path: {}", path);
    crate::search::search_repo_cache(&mut search_opts)?;

    let results_arc = search_opts.collected_results.take()
        .expect("collected_results should be Some");
    let results = results_arc.lock().unwrap();

    // For each matching package, collect pkgname
    // Deduplicate by pkgname first
    let unique_pkgnames: HashSet<String> = results.iter().map(|(pkgname, _)|
        pkgname.split('/').last().unwrap_or(pkgname).to_string()    // Deb may return "section/pkgname"
    ).collect();

    log::debug!("Adding repository packages for path query: '{:?}'", unique_pkgnames);
    Ok(unique_pkgnames.into_iter().collect())
}

fn select_packages_by_path(path: &str, options: &mut RpmOptions) -> Result<()> {
    let repo_pkgnames = select_repo_packages_by_path(path)?;

    if repo_pkgnames.is_empty() {
        eprintln!("error: path {}: No package owns this path", path);
        return Ok(());
    }

    // Set flag to allow repository queries
    options.allow_repo_query = true;

    // Add repository packages to package specs
    options.package_specs.extend(repo_pkgnames);

    Ok(())
}

fn select_packages_by_dependency<F>(
    capability: &str,
    get_field: F,
    options: &mut RpmOptions
) -> Result<usize>
where
    F: Fn(&Package) -> &[String]
{
    let pkglines = select_installed_packages_by_predicate(
        |package, _installed_info| {
            get_field(package).iter().any(|dep| dep == capability)
        },
    )?;

    let count = pkglines.len();
    // Directly append all pkglines as package specs
    options.package_specs.extend(pkglines);
    Ok(count)
}

pub fn run(mut options: RpmOptions) -> Result<()> {
    if options.install {
        crate::install::install_packages(options.package_specs.clone())?;
    } else if options.upgrade {
        crate::upgrade::upgrade_packages(options.package_specs.clone())?;
    } else if options.erase {
        crate::remove::remove_packages(options.package_specs.clone())?;
    } else {
        query_verify_packages(&mut options)?;
    }
    Ok(())
}

fn handle_package_file_query(options: &mut RpmOptions) -> Result<()> {
    if let Some(package_file) = options.package.take() {
        let package = unpack_rpm_for_query(&package_file)?;
        let pkgline = package.pkgline
            .as_ref()
            .expect("unpack_rpm_for_query sets pkgline")
            .clone();
        options.package_specs.push(pkgline);
    }
    Ok(())
}

fn validate_query_options(options: &mut RpmOptions) -> Result<()> {
    // Determine which combinable flags are set
    let has_action = options.info || options.list || options.verify || options.scripts ||
                     options.triggers || options.filetriggers || options.provides || options.requires || options.conflicts ||
                     options.enhances || options.obsoletes || options.recommends || options.suggests ||
                     options.supplements || options.state;

    // Default query mode if no action specified but packages given
    if !has_action && !options.package_specs.is_empty() {
        for_each_package(&options.package_specs, &options, |package, _, _| {
            println!("{}-{}.{}", package.pkgname, package.version, package.arch);
            Ok(())
        })?;
        return Ok(());
    }

    // Ensure packages are specified for any action
    if options.package_specs.is_empty() {
        if !has_action {
            eprintln!("rpm: no action specified");
            eprintln!("Type rpm --help for help.");
        } else {
            // Match system rpm error message format
            eprintln!("rpm: no arguments given for query");
        }
        std::process::exit(1);
    }
    Ok(())
}

fn execute_query_actions(options: &RpmOptions) -> Result<()> {
    for_each_package(&options.package_specs, &options, |package, store_path, installed_info| {
        if options.info {
            display_package_info(package, installed_info);
        }
        if let Some(store_path) = store_path {
            if options.list {
                list_package_files(store_path)?;
            }
            if options.verify {
                verify_packages(store_path)?;
            }
            if options.scripts {
                show_package_scripts(store_path)?;
            }
            if options.triggers {
                show_package_triggers(store_path)?;
            }
            if options.filetriggers {
                show_file_triggers(store_path)?;
            }
            if options.state {
                show_package_state(store_path)?;
            }
        }

        /// Execute show_dependency_list if the corresponding flag is true
        macro_rules! show_if_flag {
            ($field:ident) => {
                if options.$field {
                    show_dependency_list(&package.$field)?;
                }
            };
        }
        show_if_flag!(provides);
        show_if_flag!(requires);
        show_if_flag!(conflicts);
        show_if_flag!(enhances);
        show_if_flag!(obsoletes);
        show_if_flag!(recommends);
        show_if_flag!(suggests);
        show_if_flag!(supplements);
        Ok(())
    })
}

fn handle_what_query<F>(
    capability: Option<String>,
    get_field: F,
    verb: &str,
    options: &mut RpmOptions
) -> Result<()>
where
    F: Fn(&Package) -> &[String]
{
    if let Some(capability) = capability {
        let found = select_packages_by_dependency(&capability, get_field, options)?;
        if found == 0 {
            eprintln!("error: capability {}: No package {} this capability", capability, verb);
            std::process::exit(1);
        }
    }
    Ok(())
}

fn query_verify_packages(options: &mut RpmOptions) -> Result<()> {
    // Handle package file query (-p flag)
    handle_package_file_query(options)?;

    // Rest of the logic for installed package queries

    // Handle file query (select-option)
    if let Some(file_pattern) = options.file.clone() {
        let found = select_installed_packages_by_file(&file_pattern, options)?;
        if found == 0 {
            // No package owns the file, exit with error (maintains current behavior)
            eprintln!("error: file {}: No such file or directory", file_pattern);
            std::process::exit(1);
        }
    }

    // Handle path query (select-option)
    if let Some(path) = options.path.clone() {
        select_packages_by_path(&path, options)?;
    }

    // Handle whatXXX capability queries (select-options)
    let what_queries: Vec<(Option<String>, Box<dyn Fn(&Package) -> &[String]>, &str)> = vec![
        (options.whatprovides.clone(),      Box::new(|pkg| &pkg.provides),      "provides"),
        (options.whatrequires.clone(),      Box::new(|pkg| &pkg.requires),      "requires"),
        (options.whatconflicts.clone(),     Box::new(|pkg| &pkg.conflicts),     "conflicts"),
        (options.whatobsoletes.clone(),     Box::new(|pkg| &pkg.obsoletes),     "obsoletes"),
        (options.whatrecommends.clone(),    Box::new(|pkg| &pkg.recommends),    "recommends"),
        (options.whatsuggests.clone(),      Box::new(|pkg| &pkg.suggests),      "suggests"),
        (options.whatsupplements.clone(),   Box::new(|pkg| &pkg.supplements),   "supplements"),
        (options.whatenhances.clone(),      Box::new(|pkg| &pkg.enhances),      "enhances"),
    ];

    for (capability_opt, get_field, verb) in what_queries.into_iter() {
        handle_what_query(capability_opt, get_field, verb, options)?;
    }

    // Expand --all to all package specs
    if options.all {
        options.package_specs = get_all_package_specs()?;
    }

    validate_query_options(options)?;

    // Build closure that runs all requested actions
    execute_query_actions(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rpm_package_spec() {
        // Simple name
        let spec = parse_rpm_nvra("bash");
        assert_eq!(spec.name, "bash");
        assert_eq!(spec.version, None);
        assert_eq!(spec.arch, None);

        // Name with architecture suffix
        let spec = parse_rpm_nvra("bash.x86_64");
        assert_eq!(spec.name, "bash");
        assert_eq!(spec.version, None);
        assert_eq!(spec.arch, Some("x86_64".to_string()));

        // Name with colon architecture (legacy)
        let spec = parse_rpm_nvra("bash:x86_64");
        assert_eq!(spec.name, "bash");
        assert_eq!(spec.version, None);
        assert_eq!(spec.arch, Some("x86_64".to_string()));

        // Name-version
        let spec = parse_rpm_nvra("bash-5.2.15");
        assert_eq!(spec.name, "bash");
        assert_eq!(spec.version, Some("5.2.15".to_string()));
        assert_eq!(spec.arch, None);

        // Name-version-release
        let spec = parse_rpm_nvra("bash-5.2.15-9.oe2403");
        assert_eq!(spec.name, "bash");
        assert_eq!(spec.version, Some("5.2.15-9.oe2403".to_string()));
        assert_eq!(spec.arch, None);

        // Name-version-release.arch
        let spec = parse_rpm_nvra("bash-5.2.15-9.oe2403.x86_64");
        assert_eq!(spec.name, "bash");
        assert_eq!(spec.version, Some("5.2.15-9.oe2403".to_string()));
        assert_eq!(spec.arch, Some("x86_64".to_string()));

        // Package name with hyphen (ncurses-base)
        let spec = parse_rpm_nvra("ncurses-base");
        assert_eq!(spec.name, "ncurses-base");
        assert_eq!(spec.version, None);
        assert_eq!(spec.arch, None);

        // Package name with hyphen and version
        let spec = parse_rpm_nvra("ncurses-base-6.4-8.oe2403");
        assert_eq!(spec.name, "ncurses-base");
        assert_eq!(spec.version, Some("6.4-8.oe2403".to_string()));
        assert_eq!(spec.arch, None);

        // Package name with hyphen, version, and arch
        let spec = parse_rpm_nvra("ncurses-base-6.4-8.oe2403.noarch");
        assert_eq!(spec.name, "ncurses-base");
        assert_eq!(spec.version, Some("6.4-8.oe2403".to_string()));
        assert_eq!(spec.arch, Some("noarch".to_string()));

        // Glob patterns (name only, no version/arch)
        let spec = parse_rpm_nvra("*bash");
        assert_eq!(spec.name, "*bash");
        let spec = parse_rpm_nvra("bash*");
        assert_eq!(spec.name, "bash*");
    }

    #[test]
    fn test_glob_pattern_matches_package_names() {
        assert!(Pattern::new("*bash").unwrap().matches("bash"));
        assert!(Pattern::new("bash*").unwrap().matches("bash"));
        assert!(Pattern::new("bash*").unwrap().matches("bash-completion"));
    }
}
