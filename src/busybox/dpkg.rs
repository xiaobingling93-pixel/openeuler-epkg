use clap::{Arg, Command};
use color_eyre::Result;
use std::cmp::Ordering;

use crate::models::{PackageFormat};
use crate::applets::rpm::{pkgline_to_package_and_installed_info, select_installed_packages_by_predicate};
use crate::version_compare::compare_versions;

#[derive(Debug, Clone)]
pub struct DpkgOptions {
    pub configure: bool,
    #[allow(dead_code)]
    pub configure_all: bool,
    pub status: bool,
    pub list: bool,
    pub listfiles: bool,
    pub search: bool,
    pub print_arch: bool,
    pub print_foreign_arch: bool,
    pub compare_versions: bool,
    pub compare_args: Vec<String>,
    pub patterns: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<DpkgOptions> {
    let configure = matches.get_flag("configure");
    let configure_all = matches.get_flag("configure-all");
    let status = matches.get_flag("status");
    let list = matches.get_flag("list");
    let listfiles = matches.get_flag("listfiles");
    let search = matches.get_flag("search");
    let print_arch = matches.get_flag("print-architecture");
    let print_foreign_arch = matches.get_flag("print-foreign-architectures");

    let compare_versions = matches.get_flag("compare-versions");
    let trailing: Vec<String> = matches
        .get_many::<String>("trailing")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();
    let (compare_args, patterns) = if compare_versions && trailing.len() >= 3 {
        (trailing[..3].to_vec(), trailing[3..].to_vec())
    } else {
        (Vec::new(), trailing)
    };

    Ok(DpkgOptions {
        configure,
        configure_all,
        status,
        list,
        listfiles,
        search,
        print_arch,
        print_foreign_arch,
        compare_versions,
        compare_args,
        patterns,
    })
}

pub fn command() -> Command {
    Command::new("dpkg")
        .about("Debian package manager compatibility shim (epkg)")
        .arg(
            Arg::new("configure")
                .long("configure")
                .action(clap::ArgAction::SetTrue)
                .help("No-op in epkg: configuration is handled by epkg itself"),
        )
        .arg(
            Arg::new("configure-all")
                .short('a')
                .action(clap::ArgAction::SetTrue)
                .requires("configure")
                .help("Configure all unpacked but unconfigured packages (no-op in epkg)"),
        )
        .arg(
            Arg::new("status")
                .short('s')
                .action(clap::ArgAction::SetTrue)
                .help("Report status of specified package(s)"),
        )
        .arg(
            Arg::new("list")
                .short('l')
                .action(clap::ArgAction::SetTrue)
                .help("List packages matching pattern"),
        )
        .arg(
            Arg::new("listfiles")
                .short('L')
                .action(clap::ArgAction::SetTrue)
                .help("List files installed by package(s)"),
        )
        .arg(
            Arg::new("search")
                .short('S')
                .action(clap::ArgAction::SetTrue)
                .help("Search for which package owns a file"),
        )
        .arg(
            Arg::new("print-architecture")
                .long("print-architecture")
                .action(clap::ArgAction::SetTrue)
                .help("Print primary architecture"),
        )
        .arg(
            Arg::new("print-foreign-architectures")
                .long("print-foreign-architectures")
                .action(clap::ArgAction::SetTrue)
                .help("Print foreign architectures (none in epkg by default)"),
        )
        .arg(
            Arg::new("compare-versions")
                .long("compare-versions")
                .action(clap::ArgAction::SetTrue)
                .help("Compare version numbers (Debian semantics)"),
        )
        .arg(
            Arg::new("trailing")
                .value_name("ARGS")
                .index(1)
                .num_args(0..)
                .help("For --compare-versions: VER1 OP VER2; for -s/-l/-L/-S: packages or patterns"),
        )
}

fn run_print_arch() {
    let cfg = crate::models::config();
    println!("{}", cfg.common.arch);
}

fn find_installed_by_name(pkgname: &str) -> Option<(crate::models::Package, crate::models::InstalledPackageInfo)> {
    let pkglines = select_installed_packages_by_predicate(
        |package, _installed_info| package.pkgname == pkgname,
    ).ok()?;
    let first = pkglines.first()?;
    let (pkg, info_arc) = pkgline_to_package_and_installed_info(first)?;
    Some((pkg, (*info_arc).clone()))
}

fn print_status_line(pkgname: &str) -> i32 {
    if let Some((package, _info)) = find_installed_by_name(pkgname) {
        println!("Package: {}", package.pkgname);
        println!("Status: install ok installed");
        if let Some(section) = package.section {
            println!("Section: {}", section);
        }
        if let Some(priority) = package.priority {
            println!("Priority: {}", priority);
        }
        if package.installed_size != 0 {
            println!("Installed-Size: {}", package.installed_size);
        }
        println!("Architecture: {}", package.arch);
        println!("Version: {}", package.version);
        println!("Description: {}", package.summary);
        if let Some(desc) = package.description {
            for line in desc.lines() {
                println!(" {}", line);
            }
        }
        0
    } else {
        eprintln!("dpkg-query: package '{}' is not installed", pkgname);
        1
    }
}

fn run_status(patterns: &[String]) -> i32 {
    if patterns.is_empty() {
        eprintln!("dpkg: error: -s needs at least one package name");
        return 2;
    }
    let mut exit_code = 0;
    for pkg in patterns {
        let code = print_status_line(pkg);
        if code != 0 {
            exit_code = code;
        }
    }
    exit_code
}

fn run_dpkg_query(args: Vec<String>) -> i32 {
    let cmd = crate::applets::dpkg_query::command();
    let matches = match cmd.try_get_matches_from(args.clone()) {
        Ok(m) => m,
        Err(e) => {
            crate::utils::handle_clap_error_with_cmdline(e, args.join(" "));
        }
    };
    let opts = match crate::applets::dpkg_query::parse_options(&matches) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("dpkg-query: {}", e);
            return 2;
        }
    };
    if let Err(e) = crate::applets::dpkg_query::run(opts) {
        eprintln!("dpkg-query: {}", e);
        return 2;
    }
    0
}

fn run_list(patterns: &[String]) -> i32 {
    let mut args = vec!["dpkg-query".to_string(), "-l".to_string()];
    args.extend(patterns.iter().cloned());
    run_dpkg_query(args)
}

fn run_listfiles(patterns: &[String]) -> i32 {
    if patterns.is_empty() {
        eprintln!("dpkg: error: -L needs at least one package name");
        return 2;
    }
    let mut args = vec!["dpkg-query".to_string(), "-L".to_string()];
    args.extend(patterns.iter().cloned());
    run_dpkg_query(args)
}

fn run_search(patterns: &[String]) -> i32 {
    if patterns.is_empty() {
        eprintln!("dpkg: error: -S needs at least one pattern");
        return 2;
    }
    let mut args = vec!["dpkg-query".to_string(), "-S".to_string()];
    // dpkg -S only uses first pattern; we mimic dpkg-query behavior here.
    args.push(patterns[0].clone());
    run_dpkg_query(args)
}

fn eval_compare_op(ord: Ordering, op: &str) -> Option<bool> {
    match op {
        "lt" => Some(ord == Ordering::Less),
        "le" => Some(ord != Ordering::Greater),
        "gt" => Some(ord == Ordering::Greater),
        "ge" => Some(ord != Ordering::Less),
        "eq" => Some(ord == Ordering::Equal),
        "ne" => Some(ord != Ordering::Equal),
        _ => None,
    }
}

fn eval_compare_op_nl(ver1: &str, ver2: &str, op: &str, ord: Ordering) -> Option<bool> {
    let v1_empty = ver1.is_empty();
    let v2_empty = ver2.is_empty();
    match op {
        "lt-nl" => {
            if v1_empty && !v2_empty {
                Some(true)
            } else if v1_empty && v2_empty {
                Some(false)
            } else if !v1_empty && v2_empty {
                Some(false)
            } else {
                Some(ord == Ordering::Less)
            }
        }
        "le-nl" => {
            if v1_empty && !v2_empty {
                Some(true)
            } else if v1_empty && v2_empty {
                Some(true)
            } else if !v1_empty && v2_empty {
                Some(false)
            } else {
                Some(ord != Ordering::Greater)
            }
        }
        _ => None,
    }
}

fn run_compare_versions(args: &[String]) -> i32 {
    if args.len() != 3 {
        eprintln!("dpkg: error: --compare-versions needs exactly three arguments");
        return 2;
    }
    let ver1 = &args[0];
    let op = &args[1];
    let ver2 = &args[2];

    let cmp = match compare_versions(ver1, ver2, PackageFormat::Deb) {
        Some(o) => o,
        None => {
            eprintln!("dpkg: error: failed to parse versions '{}' and '{}'", ver1, ver2);
            return 2;
        }
    };

    let result = if op.ends_with("-nl") {
        eval_compare_op_nl(ver1, ver2, op, cmp)
    } else {
        eval_compare_op(cmp, op)
    };

    match result {
        Some(true) => 0,
        Some(false) => 1,
        None => {
            eprintln!("dpkg: error: unsupported comparison operator '{}'", op);
            2
        }
    }
}

pub fn run(options: DpkgOptions) -> Result<()> {
    if options.compare_versions {
        let code = run_compare_versions(&options.compare_args);
        std::process::exit(code);
    }

    if options.print_arch {
        run_print_arch();
        return Ok(());
    }

    if options.print_foreign_arch {
        // epkg environments typically do not enable foreign architectures;
        // print nothing and succeed.
        return Ok(());
    }

    if options.configure {
        // epkg already runs maintainer scripts during install/upgrade,
        // so treating dpkg --configure as a no-op is sufficient for scripts
        // that expect it to succeed.
        return Ok(());
    }

    if options.status {
        let code = run_status(&options.patterns);
        std::process::exit(code);
    }

    if options.list {
        let code = run_list(&options.patterns);
        std::process::exit(code);
    }

    if options.listfiles {
        let code = run_listfiles(&options.patterns);
        std::process::exit(code);
    }

    if options.search {
        let code = run_search(&options.patterns);
        std::process::exit(code);
    }

    // If called without any recognized action, mirror dpkg's style.
    eprintln!("dpkg: error: need an action option");
    eprintln!("Try 'dpkg --help' for more information.");
    std::process::exit(2);
}

