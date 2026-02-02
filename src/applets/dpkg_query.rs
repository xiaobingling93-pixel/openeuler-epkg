use clap::{Arg, Command};
use color_eyre::Result;
use std::fs;
use crate::models::InstalledPackageInfo;
use crate::models::dirs;
use crate::applets::rpm::{
    path_match_matches, path_match_prepare, pkgline_to_package_and_installed_info,
    resolve_package_spec, select_installed_pkglines_owning_path,
};
use crate::utils::{list_package_file_paths_normalized, truncate_display};

/// Command selected; matches dpkg-query "Commands" (one of -l, -W, -L, -S, -c).
#[derive(Clone, Copy)]
pub enum DpkgQueryCommand {
    List,        // -l, --list [<pattern>...]
    Show,        // -W, --show [<pattern>...]
    Listfiles,   // -L, --listfiles <package>...
    Search,      // -S, --search <pattern>...
    ControlPath, // -c, --control-path <package> [<file>]
}

pub struct DpkgQueryOptions {
    pub command: Option<DpkgQueryCommand>,
    pub showformat: Option<String>,
    /// Positional args: patterns for List/Show, packages for Listfiles, pattern(s) for Search, package [file] for ControlPath.
    pub args: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<DpkgQueryOptions> {
    let showformat = matches.get_one::<String>("showformat").cloned();
    let args: Vec<String> = matches.get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let command = if matches.get_flag("list") {
        Some(DpkgQueryCommand::List)
    } else if matches.get_flag("show") {
        Some(DpkgQueryCommand::Show)
    } else if matches.get_flag("listfiles") {
        Some(DpkgQueryCommand::Listfiles)
    } else if matches.get_flag("search") {
        Some(DpkgQueryCommand::Search)
    } else if matches.get_flag("control-path") {
        Some(DpkgQueryCommand::ControlPath)
    } else {
        None
    };

    Ok(DpkgQueryOptions {
        command,
        showformat,
        args,
    })
}

pub fn command() -> Command {
    Command::new("dpkg-query")
        .about("Query the dpkg database")
        .arg(Arg::new("list")
            .short('l')
            .long("list")
            .action(clap::ArgAction::SetTrue)
            .help("List packages concisely."))
        .arg(Arg::new("show")
            .short('W')
            .long("show")
            .action(clap::ArgAction::SetTrue)
            .help("Show information on package(s)."))
        .arg(Arg::new("showformat")
            .short('f')
            .long("showformat")
            .value_name("format")
            .help("Use alternative format for --show."))
        .arg(Arg::new("listfiles")
            .short('L')
            .long("listfiles")
            .action(clap::ArgAction::SetTrue)
            .help("List files 'owned' by package(s)."))
        .arg(Arg::new("search")
            .short('S')
            .long("search")
            .action(clap::ArgAction::SetTrue)
            .help("Find package(s) owning file(s)."))
        .arg(Arg::new("control-path")
            .short('c')
            .long("control-path")
            .action(clap::ArgAction::SetTrue)
            .help("Print path for package control file."))
        .arg(Arg::new("args")
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
    let pkglines = select_pkglines(pattern)?;

    // Print header
    println!("Desired=Unknown/Install/Remove/Purge/Hold");
    println!("| Status=Not/Inst/Conf-files/Unpacked/halF-conf/Half-inst/trig-aWait/Trig-pend");
    println!("|/ Err?=(none)/Reinst-required (Status,Err: uppercase=bad)");
    println!("||/ Name                                                     Version                              Architecture Description");
    println!("+++-========================================================-====================================-============-================================================================================");

    let mut packages: Vec<(String, String, String, String, String)> = Vec::new();

    for pkgline in pkglines {
        let (package, installed_info) = match pkgline_to_package_and_installed_info(&pkgline) {
            Some(p) => p,
            None => continue,
        };
        let status = get_package_status(installed_info.as_ref());
        packages.push((
            status,
            package.pkgname.clone(),
            package.version.clone(),
            package.arch.clone(),
            package.summary.clone(),
        ));
    }

    packages.sort_by(|a, b| a.1.cmp(&b.1));

    for (status, pkgname, version, arch, summary) in packages {
        let status_display = if status.len() >= 2 {
            format!("{} ", &status[..2])
        } else {
            format!("{} ", status)
        };
        println!("{:<3} {:<55} {:<36} {:<12} {}",
            status_display,
            truncate_display(&pkgname, 55),
            truncate_display(&version, 36),
            truncate_display(&arch, 12),
            truncate_display(&summary, 80)
        );
    }

    Ok(())
}

/// Select installed package pkglines by one pattern. Use None or "*" for all installed.
fn select_pkglines(pattern: Option<&str>) -> Result<Vec<String>> {
    crate::io::load_installed_packages()?;
    let spec = pattern.unwrap_or("*");
    let matches = resolve_package_spec(spec, false);
    let pkglines = matches
        .into_iter()
        .filter_map(|(_, inst)| inst.map(|i| i.pkgline.clone()))
        .collect();
    Ok(pkglines)
}

/// Select installed pkglines by multiple patterns. Returns (pkglines, patterns_that_matched_nothing).
/// When dedup is true, pkglines are ordered by first occurrence across patterns.
fn select_pkglines_by_patterns(
    patterns: &[String],
    dedup: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    use std::collections::HashSet;
    let mut pkglines = Vec::new();
    let mut seen = HashSet::new();
    let mut unmatched = Vec::new();

    for pattern in patterns {
        let matched = select_pkglines(Some(pattern))?;
        if matched.is_empty() {
            unmatched.push(pattern.clone());
            continue;
        }
        for pkgline in matched {
            if dedup {
                if seen.insert(pkgline.clone()) {
                    pkglines.push(pkgline);
                }
            } else {
                pkglines.push(pkgline);
            }
        }
    }

    Ok((pkglines, unmatched))
}

fn show_packages(package_specs: &[String], format: Option<&str>) -> Result<()> {
    let (pkglines, unmatched) = select_pkglines_by_patterns(package_specs, true)?;
    for pattern in &unmatched {
        eprintln!("dpkg-query: no packages found matching '{}'", pattern);
    }

    let default_format = format.unwrap_or("${binary:Package}\t${Version}\n");
    for pkgline in pkglines {
        if let Some((package, installed_info)) = pkgline_to_package_and_installed_info(&pkgline) {
            let output = format_output(default_format, &package, Some(installed_info.as_ref()));
            print!("{}", output);
        }
    }

    Ok(())
}

fn list_files(package_specs: &[String]) -> Result<()> {
    let mut printed_any = false;
    for pkg_spec in package_specs {
        let pkglines = select_pkglines(Some(pkg_spec))?;
        if pkglines.is_empty() {
            eprintln!("dpkg-query: no packages found matching '{}'", pkg_spec);
            continue;
        }
        for pkgline in pkglines {
            let store_path = dirs().epkg_store.join(pkgline);
            if let Ok(paths) = list_package_file_paths_normalized(&store_path) {
                if printed_any {
                    println!();
                }
                printed_any = true;
                for path in paths {
                    if path.starts_with('/') {
                        println!("{}", path);
                    } else {
                        println!("/{}", path);
                    }
                }
            }
        }
    }

    Ok(())
}

fn search_files(pattern: &str) -> Result<()> {
    let pkglines = select_installed_pkglines_owning_path(pattern)?;
    if pkglines.is_empty() {
        eprintln!("dpkg-query: no path found matching pattern '{}'", pattern);
        std::process::exit(1);
    }
    let state = path_match_prepare(pattern);
    for pkgline in pkglines {
        let (package, installed_info) = match pkgline_to_package_and_installed_info(&pkgline) {
            Some(p) => p,
            None => continue,
        };
        let store_path = dirs().epkg_store.join(&installed_info.pkgline);
        if let Ok(paths) = list_package_file_paths_normalized(&store_path) {
            for normalized_path in paths {
                if path_match_matches(&state, &normalized_path) {
                    println!("{}: {}", package.pkgname, normalized_path);
                }
            }
        }
    }
    Ok(())
}

fn show_control_path(package_spec: &str, control_file: Option<&str>) -> Result<()> {
    crate::io::load_installed_packages()?;

    let pkglines = select_pkglines(Some(package_spec))?;
    let maybe_package = pkglines.first().and_then(|pkgline| pkgline_to_package_and_installed_info(pkgline));

    if let Some((_package, installed_info)) = maybe_package {
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
    let cmd = match options.command {
        Some(c) => c,
        None => {
            eprintln!("dpkg-query: no action specified");
            eprintln!("Type dpkg-query --help for help.");
            std::process::exit(2);
        }
    };

    let format = options.showformat.as_deref();
    match cmd {
        DpkgQueryCommand::List => list_packages(options.args.first().map(String::as_str))?,
        DpkgQueryCommand::Show => {
            if options.args.is_empty() {
                show_packages(&[String::from("*")], format)?;
            } else {
                show_packages(&options.args, format)?;
            }
        }
        DpkgQueryCommand::Listfiles => {
            if options.args.is_empty() {
                eprintln!("dpkg-query: --listfiles requires at least one package name");
                std::process::exit(2);
            }
            list_files(&options.args)?;
        }
        DpkgQueryCommand::Search => {
            let pattern = match options.args.first() {
                Some(p) => p.as_str(),
                None => {
                    eprintln!("dpkg-query: --search needs at least one pattern");
                    std::process::exit(2);
                }
            };
            search_files(pattern)?;
        }
        DpkgQueryCommand::ControlPath => {
            let package = match options.args.first() {
                Some(p) => p.as_str(),
                None => {
                    eprintln!("dpkg-query: --control-path needs a package name");
                    std::process::exit(2);
                }
            };
            let file = options.args.get(1).map(String::as_str);
            show_control_path(package, file)?;
        }
    }

    Ok(())
}
