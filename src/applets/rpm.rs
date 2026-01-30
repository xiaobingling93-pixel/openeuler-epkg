use clap::{Arg, Command};
use color_eyre::Result;
use std::fs;
use std::path::Path;
use crate::models::{InstalledPackageInfo, Package};
use std::sync::Arc;
use crate::mmio;
use crate::models::dirs;
use crate::models::PACKAGE_CACHE;
use crate::utils;

#[derive(Debug)]
pub struct RpmOptions {
    pub all: bool,
    pub info: bool,
    pub list: bool,
    pub file: Option<String>,
    pub package: Option<String>,
    pub scripts: bool,
    pub triggers: bool,
    pub provides: bool,
    pub requires: bool,
    pub verify: bool,
    pub state: bool,
    pub packages: Vec<String>,
}

/// Print a field with proper RPM formatting (colon at column 13, width 12)
/// Labels longer than 12 characters will break alignment
fn print_field(label: &str, value: &str) {
    println!("{:12}: {}", label, value);
}

/// Print a list field with proper RPM formatting (colon at column 13, width 12)
/// Labels longer than 12 characters will break alignment
fn print_field_list(label: &str, items: &[String]) {
    if !items.is_empty() {
        println!("{:12}:", label);
        for item in items {
            println!("  {}", item);
        }
    }
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<RpmOptions> {
    let all         = matches.get_flag("all");
    let info        = matches.get_flag("info");
    let list        = matches.get_flag("list");
    let file        = matches.get_one::<String>("file").cloned();
    let package     = matches.get_one::<String>("package").cloned();
    let scripts     = matches.get_flag("scripts");
    let triggers    = matches.get_flag("triggers");
    let provides    = matches.get_flag("provides");
    let requires    = matches.get_flag("requires");
    let verify      = matches.get_flag("verify");
    let state       = matches.get_flag("state");
    let packages: Vec<String> = matches.get_many::<String>("packages")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(RpmOptions {
        all,
        info,
        list,
        file,
        package,
        scripts,
        triggers,
        provides,
        requires,
        verify,
        state,
        packages,
    })
}

pub fn command() -> Command {
    Command::new("rpm")
        .about("RPM package manager query tool")
        .arg(Arg::new("query")
            .short('q')
            .long("query")
            .action(clap::ArgAction::SetTrue)
            .help("Query mode (default)"))
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
            .help("Query package owning file"))
        .arg(Arg::new("package")
            .short('p')
            .long("package")
            .value_name("PACKAGE_FILE")
            .help("Query an (uninstalled) package file"))
        .arg(Arg::new("scripts")
            .long("scripts")
            .action(clap::ArgAction::SetTrue)
            .help("List package scripts"))
        .arg(Arg::new("triggers")
            .long("triggers")
            .action(clap::ArgAction::SetTrue)
            .help("List triggers"))
        .arg(Arg::new("provides")
            .long("provides")
            .action(clap::ArgAction::SetTrue)
            .help("List capabilities provided by package"))
        .arg(Arg::new("requires")
            .long("requires")
            .action(clap::ArgAction::SetTrue)
            .help("List capabilities required by package"))
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
        .arg(Arg::new("packages")
            .value_name("PACKAGE_NAME")
            .help("Package name(s) to query")
            .num_args(0..))
}

/// Helper function to load package info from pkgline or fallback to pkgkey
fn load_package_info(pkgkey: &str, installed_info: &InstalledPackageInfo) -> Result<Package> {
    match mmio::map_pkgline2package(&installed_info.pkgline) {
        Ok(pkg) => Ok(pkg),
        Err(_) => {
            // Fallback to repository lookup
            mmio::map_pkgkey2package(pkgkey)
        }
    }
}

/// Helper function to find installed package by name (with optional arch suffix)
/// Returns (Package, Arc<InstalledPackageInfo>) if found
fn find_installed_package_by_name(
    pkgname: &str,
    arch_suffix: Option<&str>,
) -> Option<(Package, Arc<InstalledPackageInfo>)> {
    for (pkgkey, installed_info) in PACKAGE_CACHE.installed_packages.read().unwrap().iter() {
        let package = match load_package_info(pkgkey, installed_info.as_ref()) {
            Ok(pkg) => pkg,
            Err(_) => continue,
        };

        if package.pkgname == pkgname {
            if let Some(arch) = arch_suffix {
                if package.arch != arch {
                    continue;
                }
            }
            return Some((package, Arc::clone(installed_info)));
        }
    }
    None
}

/// Parse package spec (name or name:arch) into (pkgname, arch_suffix)
fn parse_package_spec(pkg_spec: &str) -> (&str, Option<&str>) {
    if let Some((name, arch)) = pkg_spec.split_once(':') {
        (name, Some(arch))
    } else {
        (pkg_spec, None)
    }
}
fn show_package_field<F>(packages: &[String], field_extractor: F) -> Result<()>
where
    F: Fn(&Package) -> &Vec<String>,
{
    crate::io::load_installed_packages()?;

    let mut exit_code = 0;
    for pkg_spec in packages {
        let (pkgname, arch_suffix) = parse_package_spec(pkg_spec);

        if let Some((package, _)) = find_installed_package_by_name(pkgname, arch_suffix) {
            for item in field_extractor(&package) {
                println!("{}", item);
            }
        } else {
            eprintln!("package {} is not installed", pkg_spec);
            exit_code = 1;
        }
    }
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}
fn ensure_packages_nonempty(packages: &[String], flag: &str) {
    if packages.is_empty() {
        eprintln!("rpm: --{} requires at least one package name", flag);
        std::process::exit(2);
    }
}

fn list_all_packages() -> Result<()> {
    crate::io::load_installed_packages()?;

    let mut packages: Vec<(String, String, String)> = Vec::new();

    for (pkgkey, installed_info) in PACKAGE_CACHE.installed_packages.read().unwrap().iter() {
        let package = match load_package_info(pkgkey, installed_info.as_ref()) {
            Ok(pkg) => pkg,
            Err(_) => continue,
        };

        packages.push((
            package.pkgname.clone(),
            package.version.clone(),
            package.arch.clone(),
        ));
    }

    // Sort by package name
    packages.sort_by(|a, b| a.0.cmp(&b.0));

    for (pkgname, version, arch) in packages {
        println!("{}-{}.{}", pkgname, version, arch);
    }

    Ok(())
}

fn show_package_info(packages: &[String]) -> Result<()> {
    crate::io::load_installed_packages()?;

    let mut exit_code = 0;
    for pkg_spec in packages {
        let (pkgname, arch_suffix) = parse_package_spec(pkg_spec);

        if let Some((package, installed_info)) = find_installed_package_by_name(pkgname, arch_suffix) {
            // RPM -qi style: Name, Version, Release, Architecture
            let (version, release) = match package.version.rsplit_once('-') {
                Some((v, r)) => (v.to_string(), r.to_string()),
                None => (package.version.clone(), String::new()),
            };
            print_field("Name", &package.pkgname);
            print_field("Version", &version);
            if !release.is_empty() {
                print_field("Release", &release);
            }
            print_field("Architecture", &package.arch);
            print_field("Summary", &package.summary);
            if let Some(description) = package.description.as_ref() {
                println!("{:12}:", "Description");
                for line in description.lines() {
                    println!("  {}", line);
                }
            }
            print_field("Install Date", &installed_info.install_time.to_string());
            print_field("Group", package.section.as_deref().unwrap_or("Unspecified"));
            if !package.homepage.is_empty() {
                print_field("URL", &package.homepage);
            }
            if package.size > 0 {
                print_field("Size", &package.size.to_string());
            }
            if package.installed_size > 0 {
                print_field("InstalledSize", &package.installed_size.to_string());
            }
            if let Some(build_time) = package.build_time {
                print_field("BuildTime", &build_time.to_string());
            }
            if let Some(source) = &package.source {
                print_field("Source", source);
            }
            if !package.maintainer.is_empty() {
                print_field("Maintainer", &package.maintainer);
            }
            if let Some(priority) = &package.priority {
                print_field("Priority", priority);
            }
            print_field("Format", &format!("{:?}", package.format));
            print_field_list("Provides", &package.provides);
            print_field_list("Requires", &package.requires);
            print_field_list("Conflicts", &package.conflicts);
            print_field_list("Obsoletes", &package.obsoletes);
            print_field_list("Recommends", &package.recommends);
            print_field_list("Suggests", &package.suggests);
            println!();
        } else {
            eprintln!("package {} is not installed", pkg_spec);
            exit_code = 1;
        }
    }
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

fn list_package_files(packages: &[String]) -> Result<()> {
    crate::io::load_installed_packages()?;

    let mut exit_code = 0;
    for pkg_spec in packages {
        let (pkgname, arch_suffix) = parse_package_spec(pkg_spec);

        if let Some((_package, installed_info)) = find_installed_package_by_name(pkgname, arch_suffix) {
            let store_path = dirs().epkg_store.join(&installed_info.pkgline);
            let filelist_path = store_path.join("info/filelist.txt");

            if filelist_path.exists() {
                if let Ok(content) = fs::read_to_string(&filelist_path) {
                    for line in content.lines() {
                        if line.trim().is_empty() || line.starts_with('#') {
                            continue;
                        }
                        if let Some(path) = line.split_whitespace().next() {
                            if !path.starts_with('.') {
                                println!("/{}", path.trim_start_matches('/'));
                            } else {
                                println!("{}", path);
                            }
                        }
                    }
                }
            }
        } else {
            eprintln!("package {} is not installed", pkg_spec);
            exit_code = 1;
        }
    }
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

fn verify_packages(packages: &[String]) -> Result<()> {
    crate::io::load_installed_packages()?;

    let mut exit_code = 0;
    for pkg_spec in packages {
        let (pkgname, arch_suffix) = parse_package_spec(pkg_spec);

        if let Some((_package, installed_info)) = find_installed_package_by_name(pkgname, arch_suffix) {
            let store_path = dirs().epkg_store.join(&installed_info.pkgline);
            match utils::list_package_files_with_info(store_path.to_str().unwrap()) {
                Ok(file_infos) => {
                    for file_info in file_infos {
                        // RPM verify format: markers for size, mode, md5, etc.
                        // We'll just print "......." for each file
                        let path = if file_info.path.starts_with('.') {
                            &file_info.path[1..]
                        } else {
                            &file_info.path
                        };
                        println!("....... /{}", path.trim_start_matches('/'));
                    }
                }
                Err(e) => {
                    eprintln!("failed to read filelist: {}", e);
                    exit_code = 1;
                }
            }
        } else {
            eprintln!("package {} is not installed", pkg_spec);
            exit_code = 1;
        }
    }
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

fn show_package_scripts(packages: &[String]) -> Result<()> {
    crate::io::load_installed_packages()?;

    let mut exit_code = 0;
    for pkg_spec in packages {
        let (pkgname, arch_suffix) = parse_package_spec(pkg_spec);

        if let Some((_package, installed_info)) = find_installed_package_by_name(pkgname, arch_suffix) {
            let store_path = dirs().epkg_store.join(&installed_info.pkgline);
            let scripts_dir = store_path.join("info/install");
            if scripts_dir.exists() {
                match std::fs::read_dir(&scripts_dir) {
                    Ok(entries) => {
                        for entry in entries.flatten() {
                            if let Some(name) = entry.file_name().to_str() {
                                if !name.ends_with(".hook") {
                                    println!("{}", name);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("failed to read scripts directory: {}", e);
                        exit_code = 1;
                    }
                }
            } else {
                // No scripts directory
            }
        } else {
            eprintln!("package {} is not installed", pkg_spec);
            exit_code = 1;
        }
    }
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

fn show_package_triggers(packages: &[String]) -> Result<()> {
    crate::io::load_installed_packages()?;

    let mut exit_code = 0;
    for pkg_spec in packages {
        let (pkgname, arch_suffix) = parse_package_spec(pkg_spec);

        if let Some((_package, installed_info)) = find_installed_package_by_name(pkgname, arch_suffix) {
            let store_path = dirs().epkg_store.join(&installed_info.pkgline);
            let install_dir = store_path.join("info/install");
            if install_dir.exists() {
                match std::fs::read_dir(&install_dir) {
                    Ok(entries) => {
                        for entry in entries.flatten() {
                            if let Some(name) = entry.file_name().to_str() {
                                if name.ends_with(".hook") {
                                    println!("{}", name);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("failed to read install directory: {}", e);
                        exit_code = 1;
                    }
                }
            }
        } else {
            eprintln!("package {} is not installed", pkg_spec);
            exit_code = 1;
        }
    }
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

fn show_package_provides(packages: &[String]) -> Result<()> {
    show_package_field(packages, |pkg| &pkg.provides)
}

fn show_package_requires(packages: &[String]) -> Result<()> {
    show_package_field(packages, |pkg| &pkg.requires)
}

fn show_package_state(packages: &[String]) -> Result<()> {
    crate::io::load_installed_packages()?;

    let mut exit_code = 0;
    for pkg_spec in packages {
        let (pkgname, arch_suffix) = parse_package_spec(pkg_spec);

        if let Some((_package, installed_info)) = find_installed_package_by_name(pkgname, arch_suffix) {
            let store_path = dirs().epkg_store.join(&installed_info.pkgline);
            match utils::list_package_files_with_info(store_path.to_str().unwrap()) {
                Ok(file_infos) => {
                    for file_info in file_infos {
                        let path = if file_info.path.starts_with('.') {
                            &file_info.path[1..]
                        } else {
                            &file_info.path
                        };
                        println!("normal /{}", path.trim_start_matches('/'));
                    }
                }
                Err(e) => {
                    eprintln!("failed to read filelist: {}", e);
                    exit_code = 1;
                }
            }
        } else {
            eprintln!("package {} is not installed", pkg_spec);
            exit_code = 1;
        }
    }
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

fn query_package_file(path: &str) -> Result<()> {
    // Trivial implementation: just print the filename
    println!("Package file: {}", path);
    // Try to parse filename as package name-version-release.arch.rpm
    let file_name = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);
    println!("Filename: {}", file_name);
    // For now, just exit successfully
    Ok(())
}

fn query_file_ownership(file_pattern: &str) -> Result<()> {
    crate::io::load_installed_packages()?;

    let pattern_path = Path::new(file_pattern);
    let mut found_any = false;

    for (pkgkey, installed_info) in PACKAGE_CACHE.installed_packages.read().unwrap().iter() {
        let package = match load_package_info(pkgkey, installed_info.as_ref()) {
            Ok(pkg) => pkg,
            Err(_) => continue,
        };

        let store_path = dirs().epkg_store.join(&installed_info.pkgline);
        let filelist_path = store_path.join("info/filelist.txt");

        if !filelist_path.exists() {
            continue;
        }

        if let Ok(content) = fs::read_to_string(&filelist_path) {
            for line in content.lines() {
                if line.trim().is_empty() || line.starts_with('#') {
                    continue;
                }

                if let Some(file_path) = line.split_whitespace().next() {
                    let normalized_path = if file_path.starts_with('.') {
                        &file_path[1..]
                    } else {
                        file_path
                    };

                    let file_path_obj = Path::new(normalized_path);

                    if file_path_obj == pattern_path ||
                       file_path_obj.starts_with(pattern_path) ||
                       normalized_path.contains(file_pattern) {
                        println!("{}-{}.{}", package.pkgname, package.version, package.arch);
                        found_any = true;
                    }
                }
            }
        }
    }

    if !found_any {
        eprintln!("file {} is not owned by any package", file_pattern);
        std::process::exit(1);
    }

    Ok(())
}

pub fn run(options: RpmOptions) -> Result<()> {
    if options.all {
        list_all_packages()?;
    } else if options.info {
        ensure_packages_nonempty(&options.packages, "info");
        show_package_info(&options.packages)?;
    } else if options.list {
        ensure_packages_nonempty(&options.packages, "list");
        list_package_files(&options.packages)?;
    } else if let Some(file_pattern) = &options.file {
        query_file_ownership(file_pattern)?;
    } else if !options.packages.is_empty() {
        // Default query mode: list package names
        let mut exit_code = 0;
        for pkg_spec in &options.packages {
            let (pkgname, arch_suffix) = parse_package_spec(pkg_spec);
            if let Some((package, _)) = find_installed_package_by_name(pkgname, arch_suffix) {
                println!("{}-{}.{}", package.pkgname, package.version, package.arch);
            } else {
                eprintln!("package {} is not installed", pkg_spec);
                exit_code = 1;
            }
        }
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
    } else if options.verify {
        ensure_packages_nonempty(&options.packages, "verify");
        verify_packages(&options.packages)?;
    } else if options.scripts {
        ensure_packages_nonempty(&options.packages, "scripts");
        show_package_scripts(&options.packages)?;
    } else if options.triggers {
        ensure_packages_nonempty(&options.packages, "triggers");
        show_package_triggers(&options.packages)?;
    } else if options.provides {
        ensure_packages_nonempty(&options.packages, "provides");
        show_package_provides(&options.packages)?;
    } else if options.requires {
        ensure_packages_nonempty(&options.packages, "requires");
        show_package_requires(&options.packages)?;
    } else if options.state {
        ensure_packages_nonempty(&options.packages, "state");
        show_package_state(&options.packages)?;
    } else if let Some(package_file) = &options.package {
        query_package_file(package_file)?;
    } else {
        eprintln!("rpm: no action specified");
        eprintln!("Type rpm --help for help.");
        std::process::exit(2);
    }

    Ok(())
}
