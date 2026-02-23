use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use users::{get_group_by_name, get_user_by_name};

#[derive(Debug, Clone)]
pub struct DpkgStatOverrideOptions {
    pub list: Option<String>,
    pub add: bool,
    pub remove: bool,
    pub update: bool,
    pub owner: Option<String>,
    pub group: Option<String>,
    pub mode: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone)]
struct StatOverrideRecord {
    owner: String,
    group: String,
    mode: u32,
    path: String,
}

fn db_path() -> PathBuf {
    PathBuf::from("/var/lib/dpkg/statoverride")
}

fn load_overrides() -> Vec<StatOverrideRecord> {
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
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            continue;
        }
        let owner = parts[0].to_string();
        let group = parts[1].to_string();
        let mode = u32::from_str_radix(parts[2], 8).unwrap_or(0o755);
        let path = parts[3].to_string();
        records.push(StatOverrideRecord {
            owner,
            group,
            mode,
            path,
        });
    }
    records
}

fn save_overrides(records: &[StatOverrideRecord]) -> Result<()> {
    let path = db_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut content = String::new();
    content.push_str("# epkg dpkg-statoverride database: owner group mode path\n");
    for r in records {
        content.push_str(&format!(
            "{} {} {:04o} {}\n",
            r.owner, r.group, r.mode, r.path
        ));
    }
    fs::write(path, content)?;
    Ok(())
}

fn list_override(path: &str) -> i32 {
    let records = load_overrides();
    for r in records {
        if r.path == path {
            println!("{} {} {:04o} {}", r.owner, r.group, r.mode, r.path);
            return 0;
        }
    }
    // Upstream prints nothing and exits 1 when no override exists.
    1
}

fn chown_if_possible(owner: &str, group: &str, path: &Path) {
    let uid = get_user_by_name(owner).map(|u| u.uid());
    let gid = get_group_by_name(group).map(|g| g.gid());
    if uid.is_none() && gid.is_none() {
        return;
    }
    let uid = uid.unwrap_or(u32::MAX);
    let gid = gid.unwrap_or(u32::MAX);
    // Ignore failures; this is a best-effort helper inside an isolated env.
    let _ = nix::unistd::chown(path, if uid == u32::MAX { None } else { Some(nix::unistd::Uid::from_raw(uid)) }, if gid == u32::MAX { None } else { Some(nix::unistd::Gid::from_raw(gid)) });
}

fn chmod_if_possible(mode: u32, path: &Path) {
    if let Ok(metadata) = fs::metadata(path) {
        let mut perms = metadata.permissions();
        perms.set_mode(mode);
        let _ = fs::set_permissions(path, perms);
    }
}

fn add_override(opts: &DpkgStatOverrideOptions) -> Result<i32> {
    let owner = match &opts.owner {
        Some(o) => o.clone(),
        None => {
            return Err(eyre!(
                "dpkg-statoverride: error: --add requires owner group mode path"
            ));
        }
    };
    let group = match &opts.group {
        Some(g) => g.clone(),
        None => {
            return Err(eyre!(
                "dpkg-statoverride: error: --add requires owner group mode path"
            ));
        }
    };
    let mode_str = match &opts.mode {
        Some(m) => m.clone(),
        None => {
            return Err(eyre!(
                "dpkg-statoverride: error: --add requires owner group mode path"
            ));
        }
    };
    let mode = u32::from_str_radix(&mode_str, 8).unwrap_or(0o755);
    let path = match &opts.path {
        Some(p) => p.clone(),
        None => {
            return Err(eyre!(
                "dpkg-statoverride: error: --add requires owner group mode path"
            ));
        }
    };

    let mut records = load_overrides();
    // Replace any existing override for this path.
    records.retain(|r| r.path != path);
    records.push(StatOverrideRecord {
        owner: owner.clone(),
        group: group.clone(),
        mode,
        path: path.clone(),
    });
    save_overrides(&records)?;

    if opts.update {
        let p = Path::new(&path);
        if p.exists() {
            chown_if_possible(&owner, &group, p);
            chmod_if_possible(mode, p);
        }
    }

    Ok(0)
}

fn remove_override(opts: &DpkgStatOverrideOptions) -> Result<i32> {
    let path = match &opts.path {
        Some(p) => p.clone(),
        None => {
            return Err(eyre!(
                "dpkg-statoverride: error: --remove requires path argument"
            ));
        }
    };
    let mut records = load_overrides();
    let before = records.len();
    records.retain(|r| r.path != path);
    if records.len() == before {
        // Nothing removed; upstream exits 1 in this case.
        return Ok(1);
    }
    save_overrides(&records)?;
    Ok(0)
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<DpkgStatOverrideOptions> {
    // dpkg-statoverride syntax is positional for add/remove; we keep a small, explicit subset.
    let list = matches.get_one::<String>("list").cloned();
    let add = matches.get_flag("add");
    let remove = matches.get_flag("remove");
    let update = matches.get_flag("update");

    let owner = matches.get_one::<String>("owner").cloned();
    let group = matches.get_one::<String>("group").cloned();
    let mode = matches.get_one::<String>("mode").cloned();
    let path = matches.get_one::<String>("path").cloned();

    Ok(DpkgStatOverrideOptions {
        list,
        add,
        remove,
        update,
        owner,
        group,
        mode,
        path,
    })
}

pub fn command() -> Command {
    Command::new("dpkg-statoverride")
        .about("Override file ownership and permissions (epkg-compatible subset)")
        .arg(
            Arg::new("list")
                .long("list")
                .value_name("FILE")
                .help("List any override for FILE"),
        )
        .arg(
            Arg::new("add")
                .long("add")
                .action(clap::ArgAction::SetTrue)
                .help("Add a stat override"),
        )
        .arg(
            Arg::new("remove")
                .long("remove")
                .action(clap::ArgAction::SetTrue)
                .help("Remove a stat override"),
        )
        .arg(
            Arg::new("update")
                .long("update")
                .action(clap::ArgAction::SetTrue)
                .help("Immediately apply changes to the filesystem (best-effort)"),
        )
        // For --add/--remove we take simple positional fields: owner group mode path.
        .arg(
            Arg::new("owner")
                .value_name("OWNER")
                .requires("add")
                .index(1),
        )
        .arg(
            Arg::new("group")
                .value_name("GROUP")
                .requires("add")
                .index(2),
        )
        .arg(
            Arg::new("mode")
                .value_name("MODE")
                .requires("add")
                .index(3),
        )
        .arg(
            Arg::new("path")
                .value_name("FILE")
                .index(4),
        )
}

pub fn run(options: DpkgStatOverrideOptions) -> Result<()> {
    if let Some(path) = options.list.as_deref() {
        let code = list_override(path);
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }

    if options.add {
        let code = add_override(&options)?;
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }

    if options.remove {
        let code = remove_override(&options)?;
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }

    eprintln!("dpkg-statoverride: error: no action specified");
    eprintln!("Usage:");
    eprintln!("  dpkg-statoverride --list FILE");
    eprintln!("  dpkg-statoverride --add [--update] OWNER GROUP MODE FILE");
    eprintln!("  dpkg-statoverride --remove FILE");
    std::process::exit(2);
}

