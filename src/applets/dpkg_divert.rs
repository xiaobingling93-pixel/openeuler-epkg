use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DpkgDivertOptions {
    pub add: bool,
    pub remove: bool,
    pub listpackage: Option<String>,
    pub truename: Option<String>,
    pub rename: bool,
    pub no_rename: bool,
    pub package: Option<String>,
    pub divert: Option<String>,
    pub quiet: bool,
    pub files: Vec<String>,
}

#[derive(Debug, Clone)]
struct DiversionRecord {
    original: String,
    diverted: String,
    package: Option<String>,
    rename: bool,
}

fn db_path() -> PathBuf {
    PathBuf::from("/var/lib/dpkg/diversions")
}

fn load_diversions() -> Vec<DiversionRecord> {
    let path = db_path();
    if !path.exists() {
        return Vec::new();
    }
    let file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let mut records = Vec::new();
    for line in reader.lines().flatten() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 4 {
            continue;
        }
        let original = parts[0].to_string();
        let diverted = parts[1].to_string();
        let package = if parts[2].is_empty() {
            None
        } else {
            Some(parts[2].to_string())
        };
        let rename = parts[3] == "1";
        records.push(DiversionRecord {
            original,
            diverted,
            package,
            rename,
        });
    }
    records
}

fn save_diversions(records: &[DiversionRecord]) -> Result<()> {
    let path = db_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut content = String::new();
    content.push_str("# epkg dpkg-divert database: original<TAB>diverted<TAB>package<TAB>rename(0|1)\n");
    for r in records {
        let pkg = r.package.clone().unwrap_or_default();
        let rename_flag = if r.rename { "1" } else { "0" };
        content.push_str(&format!("{}\t{}\t{}\t{}\n", r.original, r.diverted, pkg, rename_flag));
    }
    fs::write(path, content)?;
    Ok(())
}

fn find_diversion<'a>(records: &'a [DiversionRecord], path: &str) -> Option<&'a DiversionRecord> {
    records
        .iter()
        .find(|r| r.original == path || r.diverted == path)
}

fn listpackage(path: &str) -> i32 {
    let records = load_diversions();
    if let Some(r) = find_diversion(&records, path) {
        if let Some(pkg) = &r.package {
            println!("{}", pkg);
        }
        0
    } else {
        // Upstream prints empty output but success when no diversion exists.
        0
    }
}

fn truename(path: &str) -> i32 {
    let records = load_diversions();
    if let Some(r) = find_diversion(&records, path) {
        println!("{}", r.diverted);
    } else {
        println!("{}", path);
    }
    0
}

fn add_diversion(opts: &DpkgDivertOptions) -> Result<i32> {
    let path = match opts.files.first() {
        Some(p) => p.clone(),
        None => {
            return Err(eyre!(
                "dpkg-divert: error: --add needs a file path argument"
            ));
        }
    };

    let divert_to = match &opts.divert {
        Some(d) => d.clone(),
        None => {
            // Upstream defaults to adding .distrib; we keep it simple and require explicit --divert
            return Err(eyre!(
                "dpkg-divert: error: --divert is required with --add in epkg"
            ));
        }
    };

    let mut records = load_diversions();

    if let Some(existing) = records.iter().find(|r| r.original == path) {
        // If the same diversion already exists, treat as no-op.
        if existing.diverted == divert_to && existing.package == opts.package && existing.rename == (opts.rename && !opts.no_rename) {
            return Ok(0);
        }
        // Different diversion on same path – behave similar to dpkg and report a clash.
        eprintln!(
            "dpkg-divert: error: rename involves overwriting '{}' with\n  '{}', which is also diverted",
            existing.diverted,
            divert_to
        );
        return Ok(2);
    }

    let do_rename = opts.rename && !opts.no_rename;
    if do_rename {
        let src = Path::new(&path);
        let dst = Path::new(&divert_to);
        if src.exists() {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
            // Best-effort; if rename fails, keep going but report error and non-zero exit.
            if let Err(e) = fs::rename(src, dst) {
                eprintln!(
                    "dpkg-divert: error: failed to rename '{}' to '{}': {}",
                    path,
                    divert_to,
                    e
                );
                return Ok(2);
            }
        }
    }

    records.push(DiversionRecord {
        original: path,
        diverted: divert_to,
        package: opts.package.clone(),
        rename: do_rename,
    });
    save_diversions(&records)?;
    Ok(0)
}

fn remove_diversion(opts: &DpkgDivertOptions) -> Result<i32> {
    let path = match opts.files.first() {
        Some(p) => p.clone(),
        None => {
            return Err(eyre!(
                "dpkg-divert: error: --remove needs a file path argument"
            ));
        }
    };

    let mut records = load_diversions();
    let mut idx = None;
    for (i, r) in records.iter().enumerate() {
        if r.original == path {
            if let Some(pkg) = &opts.package {
                if r.package.as_deref() != Some(pkg.as_str()) {
                    continue;
                }
            }
            idx = Some(i);
            break;
        }
    }

    let Some(i) = idx else {
        // Nothing to remove – match Debian behavior and succeed silently.
        return Ok(0);
    };

    let record = records.remove(i);

    let do_rename_back = record.rename && !opts.no_rename;
    if do_rename_back {
        let src = Path::new(&record.diverted);
        let dst = Path::new(&record.original);
        if src.exists() && !dst.exists() {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
            if let Err(e) = fs::rename(src, dst) {
                eprintln!(
                    "dpkg-divert: error: failed to rename '{}' back to '{}': {}",
                    record.diverted,
                    record.original,
                    e
                );
                // Keep going; state file is still updated.
            }
        }
    }

    save_diversions(&records)?;
    Ok(0)
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<DpkgDivertOptions> {
    let add = matches.get_flag("add");
    let remove = matches.get_flag("remove");
    let listpackage = matches.get_one::<String>("listpackage").cloned();
    let truename = matches.get_one::<String>("truename").cloned();
    let rename = matches.get_flag("rename");
    let no_rename = matches.get_flag("no-rename");
    let package = matches.get_one::<String>("package").cloned();
    let divert = matches.get_one::<String>("divert").cloned();
    let quiet = matches.get_flag("quiet");
    let files: Vec<String> = matches
        .get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(DpkgDivertOptions {
        add,
        remove,
        listpackage,
        truename,
        rename,
        no_rename,
        package,
        divert,
        quiet,
        files,
    })
}

pub fn command() -> Command {
    Command::new("dpkg-divert")
        .about("Manage diverted files (epkg-compatible subset)")
        .arg(
            Arg::new("add")
                .long("add")
                .action(clap::ArgAction::SetTrue)
                .help("Add a diversion"),
        )
        .arg(
            Arg::new("remove")
                .long("remove")
                .action(clap::ArgAction::SetTrue)
                .help("Remove a diversion"),
        )
        .arg(
            Arg::new("listpackage")
                .long("listpackage")
                .value_name("FILE")
                .help("Print the package name that diverts FILE"),
        )
        .arg(
            Arg::new("truename")
                .long("truename")
                .value_name("FILE")
                .help("Print the real name for FILE after applying diversions"),
        )
        .arg(
            Arg::new("rename")
                .long("rename")
                .action(clap::ArgAction::SetTrue)
                .help("Rename the file when adding/removing diversion"),
        )
        .arg(
            Arg::new("no-rename")
                .long("no-rename")
                .action(clap::ArgAction::SetTrue)
                .help("Do not rename the file when adding/removing diversion"),
        )
        .arg(
            Arg::new("package")
                .long("package")
                .value_name("PACKAGE")
                .help("Associate diversion with PACKAGE"),
        )
        .arg(
            Arg::new("divert")
                .long("divert")
                .value_name("DIVERT_PATH")
                .help("Divert to this path"),
        )
        .arg(
            Arg::new("quiet")
                .long("quiet")
                .action(clap::ArgAction::SetTrue)
                .help("Suppress non-essential output"),
        )
        .arg(
            Arg::new("files")
                .value_name("FILE")
                .num_args(0..)
                .help("File(s) to divert"),
        )
}

pub fn run(options: DpkgDivertOptions) -> Result<()> {
    // Handle query-style commands first; they do not modify state.
    if let Some(path) = options.listpackage.as_deref() {
        let code = listpackage(path);
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }

    if let Some(path) = options.truename.as_deref() {
        let code = truename(path);
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }

    // Modification commands
    if options.add {
        let code = add_diversion(&options)?;
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }

    if options.remove {
        let code = remove_diversion(&options)?;
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }

    if !options.quiet {
        eprintln!("dpkg-divert: error: no action specified");
        eprintln!("Usage: dpkg-divert [--add|--remove|--listpackage|--truename] [options] FILE...");
    }
    std::process::exit(2);
}

