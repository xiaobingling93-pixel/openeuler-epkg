use clap::{Arg, Command};
use color_eyre::Result;
use std::fs;
use std::path::Path;
use crate::models::{PackageManager, InstalledPackageInfo, Package};
use crate::mmio;
use crate::list;
use crate::models::dirs;
use crate::models::PACKAGE_CACHE;

pub struct DpkgQueryOptions {
    pub list: bool,
    pub list_pattern: Option<String>,
    pub show: bool,
    pub showformat: Option<String>,
    pub listfiles: bool,
    pub search: Option<String>,
    pub control_path: Option<String>,
    pub control_file: Option<String>,
    pub packages: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<DpkgQueryOptions> {
    let list = matches.get_flag("list");
    let list_pattern = matches.get_one::<String>("list-pattern").cloned();
    let show = matches.get_flag("show");
    let showformat = matches.get_one::<String>("showformat").cloned();
    let listfiles = matches.get_flag("listfiles");
    let search = matches.get_one::<String>("search").cloned();
    let control_path = matches.get_one::<String>("control-path").cloned();
    let control_file = matches.get_one::<String>("control-file").cloned();
    let packages: Vec<String> = matches.get_many::<String>("packages")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(DpkgQueryOptions {
        list,
        list_pattern,
        show,
        showformat,
        listfiles,
        search,
        control_path,
        control_file,
        packages,
    })
}

pub fn command() -> Command {
    Command::new("dpkg-query")
        .about("Query the dpkg database")
        .arg(Arg::new("list")
            .short('l')
            .long("list")
            .action(clap::ArgAction::SetTrue)
            .help("List packages matching pattern"))
        .arg(Arg::new("list-pattern")
            .help("Pattern to match package names (used with --list)"))
        .arg(Arg::new("show")
            .short('W')
            .long("show")
            .action(clap::ArgAction::SetTrue)
            .help("Show information about packages"))
        .arg(Arg::new("showformat")
            .short('f')
            .long("showformat")
            .value_name("FORMAT")
            .help("Use alternative format for output"))
        .arg(Arg::new("listfiles")
            .short('L')
            .long("listfiles")
            .action(clap::ArgAction::SetTrue)
            .help("List files installed by package"))
        .arg(Arg::new("search")
            .short('S')
            .long("search")
            .value_name("FILENAME-PATTERN")
            .help("Search for packages owning files"))
        .arg(Arg::new("control-path")
            .long("control-path")
            .value_name("PACKAGE")
            .help("Print path to control file"))
        .arg(Arg::new("control-file")
            .long("control-file")
            .value_name("FILE")
            .help("Control file name (used with --control-path)"))
        .arg(Arg::new("packages")
            .help("Package names to query")
            .num_args(0..))
}

fn get_package_status(installed_info: &InstalledPackageInfo) -> String {
    // dpkg status format: ii = installed, config-files, etc.
    // For epkg, we use a simplified status
    if installed_info.config_failed {
        String::from("CF") // Config-failed
    } else if installed_info.triggers_awaited {
        String::from("TA") // Triggers-awaited
    } else if installed_info.ebin_exposure {
        String::from("ii") // Installed
    } else {
        String::from("ii") // Installed (simplified)
    }
}

fn get_status_abbrev(installed_info: &InstalledPackageInfo) -> String {
    // Status abbreviation: i = installed
    if installed_info.ebin_exposure || !installed_info.rdepends.is_empty() {
        String::from("i")
    } else {
        String::from("i")
    }
}

fn format_field(field: &str, package: &crate::models::Package, installed_info: Option<&InstalledPackageInfo>) -> String {
    match field {
        "Package" | "binary:Package" => package.pkgname.clone(),
        "Version" => package.version.clone(),
        "Architecture" | "Arch" => package.arch.clone(),
        "Status" => {
            if let Some(info) = installed_info {
                get_package_status(info)
            } else {
                String::from("un") // uninstalled
            }
        },
        "db:Status-Abbrev" => {
            if let Some(info) = installed_info {
                get_status_abbrev(info)
            } else {
                String::new()
            }
        },
        "db:Status-Want" => {
            if installed_info.is_some() {
                String::from("install")
            } else {
                String::from("unknown")
            }
        },
        "Description" => package.summary.clone(),
        "Summary" => package.summary.clone(),
        "Conffiles" => {
            // Read conffiles from info/deb/conffiles
            if let Some(info) = installed_info {
                let store_path = dirs().epkg_store.join(&info.pkgline);
                let conffiles_path = store_path.join("info/deb/conffiles");
                if conffiles_path.exists() {
                    if let Ok(content) = fs::read_to_string(&conffiles_path) {
                        // Format: each line is "path md5sum" or "path md5sum obsolete"
                        return content.trim().to_string();
                    }
                }
            }
            String::new()
        },
        _ => {
            // Try to match field names with colons (e.g., "binary:Package")
            if let Some((prefix, suffix)) = field.split_once(':') {
                match prefix {
                    "binary" => match suffix {
                        "Package" => package.pkgname.clone(),
                        _ => String::new(),
                    },
                    "db" => match suffix {
                        "Status-Abbrev" => {
                            if let Some(info) = installed_info {
                                get_status_abbrev(info)
                            } else {
                                String::new()
                            }
                        },
                        "Status-Want" => {
                            if installed_info.is_some() {
                                String::from("install")
                            } else {
                                String::from("unknown")
                            }
                        },
                        _ => String::new(),
                    },
                    _ => String::new(),
                }
            } else {
                String::new()
            }
        }
    }
}

fn format_output(format_str: &str, package: &crate::models::Package, installed_info: Option<&InstalledPackageInfo>) -> String {
    let mut result = String::new();
    let mut i = 0;
    let chars: Vec<char> = format_str.chars().collect();

    while i < chars.len() {
        if chars[i] == '$' && i + 1 < chars.len() && chars[i + 1] == '{' {
            // Find closing brace
            let mut j = i + 2;
            while j < chars.len() && chars[j] != '}' {
                j += 1;
            }
            if j < chars.len() {
                let field = &format_str[i + 2..j];
                let value = format_field(field, package, installed_info);
                result.push_str(&value);
                i = j + 1;
            } else {
                result.push(chars[i]);
                i += 1;
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

fn list_packages(pattern: Option<&str>) -> Result<()> {
    let mut pm = PackageManager::default();
    pm.load_installed_packages()?;

    // Print header
    println!("Desired=Unknown/Install/Remove/Purge/Hold");
    println!("| Status=Not/Inst/Conf-files/Unpacked/halF-conf/Half-inst/trig-aWait/Trig-pend");
    println!("|/ Err?=(none)/Reinst-required (Status,Err: uppercase=bad)");
    println!("||/ Name                                                     Version                              Architecture Description");
    println!("+++-========================================================-====================================-============-================================================================================");

    let mut packages: Vec<(String, String, String, String, String)> = Vec::new();

    for (pkgkey, installed_info) in PACKAGE_CACHE.installed_packages.read().unwrap().iter() {
        // Get package info
        let package = match mmio::map_pkgline2package(&installed_info.pkgline) {
            Ok(pkg) => pkg,
            Err(_) => {
                // Try to get from repository
                match mmio::map_pkgkey2package(pkgkey) {
                    Ok(pkg) => pkg,
                    Err(_) => continue,
                }
            }
        };

        let pkgname = &package.pkgname;

        // Apply pattern filter if provided
        if let Some(pat) = pattern {
            if !list::matches_glob_pattern(pkgname, pat) {
                continue;
            }
        }

        let status = get_package_status(installed_info);
        let version = &package.version;
        let arch = &package.arch;
        let summary = &package.summary;

        packages.push((
            status,
            pkgname.clone(),
            version.clone(),
            arch.clone(),
            summary.clone(),
        ));
    }

    // Sort by package name
    packages.sort_by(|a, b| a.1.cmp(&b.1));

    // Print packages
    for (status, pkgname, version, arch, summary) in packages {
        let status_display = if status.len() >= 2 {
            format!("{} ", &status[..2])
        } else {
            format!("{} ", status)
        };
        println!("{:<3} {:<55} {:<36} {:<12} {}",
            status_display,
            truncate(&pkgname, 55),
            truncate(&version, 36),
            truncate(&arch, 12),
            truncate(&summary, 80)
        );
    }

    Ok(())
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
/// Returns (Package, InstalledPackageInfo) if found
fn find_installed_package_by_name(
    _pm: &PackageManager,
    pkgname: &str,
    arch_suffix: Option<&str>,
) -> Option<(Package, InstalledPackageInfo)> {
    for (pkgkey, installed_info) in PACKAGE_CACHE.installed_packages.read().unwrap().iter() {
        let package = match load_package_info(pkgkey, installed_info) {
            Ok(pkg) => pkg,
            Err(_) => continue,
        };

        if package.pkgname == pkgname {
            if let Some(arch) = arch_suffix {
                if package.arch != arch {
                    continue;
                }
            }
            return Some((package, installed_info.clone()));
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

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

fn show_packages(packages: &[String], format: Option<&str>) -> Result<()> {
    let mut pm = PackageManager::default();
    pm.load_installed_packages()?;

    let default_format = format.unwrap_or("${binary:Package}\t${Version}\n");

    for pkg_spec in packages {
        let (pkgname, arch_suffix) = parse_package_spec(pkg_spec);

        if let Some((package, installed_info)) = find_installed_package_by_name(&pm, pkgname, arch_suffix) {
            let output = format_output(default_format, &package, Some(&installed_info));
            print!("{}", output);
        } else {
            eprintln!("dpkg-query: no packages found matching '{}'", pkg_spec);
        }
    }

    Ok(())
}

fn list_files(packages: &[String]) -> Result<()> {
    let mut pm = PackageManager::default();
    pm.load_installed_packages()?;

    for pkg_spec in packages {
        let (pkgname, arch_suffix) = parse_package_spec(pkg_spec);

        if let Some((_package, installed_info)) = find_installed_package_by_name(&pm, pkgname, arch_suffix) {
            // Read filelist
            let store_path = dirs().epkg_store.join(&installed_info.pkgline);
            let filelist_path = store_path.join("info/filelist.txt");

            if filelist_path.exists() {
                if let Ok(content) = fs::read_to_string(&filelist_path) {
                    // Parse mtree format and extract file paths
                    for line in content.lines() {
                        if line.trim().is_empty() || line.starts_with('#') {
                            continue;
                        }
                        // mtree format: path type=file ...
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
            eprintln!("dpkg-query: no packages found matching '{}'", pkg_spec);
        }
    }

    Ok(())
}

fn search_files(pattern: &str) -> Result<()> {
    let mut pm = PackageManager::default();
    pm.load_installed_packages()?;

    let pattern_path = Path::new(pattern);
    let mut found_any = false;

    for (pkgkey, installed_info) in PACKAGE_CACHE.installed_packages.read().unwrap().iter() {
        let package = match load_package_info(pkgkey, installed_info) {
            Ok(pkg) => pkg,
            Err(_) => continue,
        };

        // Read filelist
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

                    // Check if pattern matches
                    if file_path_obj == pattern_path ||
                       file_path_obj.starts_with(pattern_path) ||
                       normalized_path.contains(pattern) {
                        println!("{}: {}", package.pkgname, normalized_path);
                        found_any = true;
                    }
                }
            }
        }
    }

    if !found_any {
        eprintln!("dpkg-query: no path found matching pattern '{}'", pattern);
        std::process::exit(1);
    }

    Ok(())
}

fn show_control_path(package_spec: &str, control_file: Option<&str>) -> Result<()> {
    let mut pm = PackageManager::default();
    pm.load_installed_packages()?;

    let (pkgname, arch_suffix) = parse_package_spec(package_spec);

    if let Some((_package, installed_info)) = find_installed_package_by_name(&pm, pkgname, arch_suffix) {
        let store_path = dirs().epkg_store.join(&installed_info.pkgline);
        let control_file_name = control_file.unwrap_or("control");
        let control_path = store_path.join("info/deb").join(control_file_name);

        if control_path.exists() {
            println!("{}", control_path.display());
            return Ok(());
        } else {
            eprintln!("dpkg-query: control file '{}' not found for package '{}'", control_file_name, package_spec);
            std::process::exit(1);
        }
    }

    eprintln!("dpkg-query: no packages found matching '{}'", package_spec);
    std::process::exit(1);
}

pub fn run(options: DpkgQueryOptions) -> Result<()> {
    if options.list {
        list_packages(options.list_pattern.as_deref())?;
    } else if options.listfiles {
        if options.packages.is_empty() {
            eprintln!("dpkg-query: --listfiles requires at least one package name");
            std::process::exit(2);
        }
        list_files(&options.packages)?;
    } else if let Some(search_pattern) = &options.search {
        search_files(search_pattern)?;
    } else if let Some(control_pkg) = &options.control_path {
        show_control_path(control_pkg, options.control_file.as_deref())?;
    } else if options.show || !options.packages.is_empty() {
        // Default to --show if packages are specified
        let format = options.showformat.as_deref();
        if options.packages.is_empty() {
            eprintln!("dpkg-query: --show requires at least one package name");
            std::process::exit(2);
        }
        show_packages(&options.packages, format)?;
    } else {
        eprintln!("dpkg-query: no action specified");
        eprintln!("Type dpkg-query --help for help.");
        std::process::exit(2);
    }

    Ok(())
}

