//! deb-systemd-helper - subset of systemctl for maintainer scripts (non-systemd systems)
//!
//! Implements: enable, disable, unmask, purge, update-state, was-enabled, debian-installed.
//! Options: --quiet, --user. Respects DPKG_ROOT. Intended for use from dpkg maintscripts only.

use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::{eyre, WrapErr};
use std::collections::HashSet;
use std::env;
use std::fs;
use crate::lfs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

const SYSTEM_ENABLED_STATE_DIR: &str = "/var/lib/systemd/deb-systemd-helper-enabled";
const USER_ENABLED_STATE_DIR: &str = "/var/lib/systemd/deb-systemd-user-helper-enabled";
const SYSTEM_MASKED_STATE_DIR: &str = "/var/lib/systemd/deb-systemd-helper-masked";
const USER_MASKED_STATE_DIR: &str = "/var/lib/systemd/deb-systemd-user-helper-masked";


#[derive(Debug, Clone)]
pub struct DebSystemdHelperOptions {
    pub quiet: bool,
    pub user: bool,
    pub action: String,
    pub units: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<DebSystemdHelperOptions> {
    let quiet = matches.get_flag("quiet");
    let user = matches.get_flag("user");
    let action = matches
        .get_one::<String>("action")
        .cloned()
        .ok_or_else(|| eyre!("missing action"))?;
    let units: Vec<String> = matches
        .get_many::<String>("units")
        .map(|v| v.cloned().collect())
        .unwrap_or_default();

    Ok(DebSystemdHelperOptions {
        quiet,
        user,
        action,
        units,
    })
}

pub fn command() -> Command {
    Command::new("deb-systemd-helper")
        .about("Helper for systemd unit files in Debian packages (enable/disable/unmask/purge/update-state/was-enabled/debian-installed)")
        .arg(
            Arg::new("quiet")
                .long("quiet")
                .action(clap::ArgAction::SetTrue)
                .help("Suppress output"),
        )
        .arg(
            Arg::new("user")
                .long("user")
                .action(clap::ArgAction::SetTrue)
                .help("Handle user units"),
        )
        .arg(Arg::new("action").required(true).help(
            "enable | disable | purge | unmask | update-state | was-enabled | debian-installed",
        ))
        .arg(
            Arg::new("units")
                .num_args(0..)
                .help("Unit names (e.g. ssh.service)"),
        )
}

fn dpkg_root() -> PathBuf {
    env::var_os("DPKG_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(""))
}

/// Path under DPKG_ROOT. When root is empty, p is used as absolute path.
fn root_join(root: &Path, p: &str) -> PathBuf {
    let p_trim = p.trim_start_matches('/');
    if root.as_os_str().is_empty() || p_trim.is_empty() {
        PathBuf::from(p)
    } else {
        root.join(p_trim)
    }
}

fn require_maintscript() -> Result<()> {
    if env::var_os("DPKG_MAINTSCRIPT_PACKAGE").is_none() {
        return Err(eyre!(
            "deb-systemd-helper was not called from dpkg. Exiting."
        ));
    }
    Ok(())
}

fn enabled_state_dir(user: bool, root: &Path) -> PathBuf {
    let sub = if user {
        USER_ENABLED_STATE_DIR
    } else {
        SYSTEM_ENABLED_STATE_DIR
    };
    root_join(root, sub)
}

fn masked_state_dir(user: bool, root: &Path) -> PathBuf {
    let sub = if user {
        USER_MASKED_STATE_DIR
    } else {
        SYSTEM_MASKED_STATE_DIR
    };
    root_join(root, sub)
}

fn etc_systemd_instance(user: bool) -> &'static str {
    if user {
        "user"
    } else {
        "system"
    }
}

fn find_unit(root: &Path, unit_name: &str, user: bool) -> Option<PathBuf> {
    let instance = etc_systemd_instance(user);
    let search = [
        format!("/etc/systemd/{}/{}", instance, unit_name),
        format!("/lib/systemd/{}/{}", instance, unit_name),
        format!("/usr/lib/systemd/{}/{}", instance, unit_name),
    ];
    for p in &search {
        let full = root_join(root, p);
        if full.exists() {
            return Some(PathBuf::from(p));
        }
    }
    None
}

fn dsh_state_path(unit_name: &str, user: bool, root: &Path) -> PathBuf {
    let basename = Path::new(unit_name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(unit_name);
    let state_dir = enabled_state_dir(user, root);
    state_dir.join(format!("{}.dsh-also", basename))
}

#[derive(Clone)]
struct UnitLink {
    /// Symlink target (path to unit file, e.g. /usr/lib/systemd/system/ssh.service)
    dest: String,
    /// Symlink path under etc (e.g. /etc/systemd/system/multi-user.target.wants/ssh.service)
    src: String,
}

fn parse_wanted_required(line: &str) -> Option<(bool, Vec<String>)> {
    let line = line.trim();
    if line.starts_with("WantedBy=") {
        let v = line["WantedBy=".len()..].trim();
        let values: Vec<String> = v
            .split_whitespace()
            .map(|s| s.trim_matches(|c| c == '"' || c == '\'').to_string())
            .collect();
        return Some((true, values));
    }
    if line.starts_with("RequiredBy=") {
        let v = line["RequiredBy=".len()..].trim();
        let values: Vec<String> = v
            .split_whitespace()
            .map(|s| s.trim_matches(|c| c == '"' || c == '\'').to_string())
            .collect();
        return Some((false, values));
    }
    None
}

fn get_link_closure(
    root: &Path,
    service_path: &Path,
    instance: &str,
    _visited: &mut HashSet<String>,
) -> Result<Vec<UnitLink>> {
    let unit_name = service_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let full_path = root_join(root, &service_path.to_string_lossy());
    let content = fs::read_to_string(&full_path)
        .wrap_err_with(|| format!("unable to read {}", full_path.display()))?;

    let mut wants_dirs: Vec<String> = Vec::new();
    let wanted_target = unit_name;

    for line in content.lines() {
        if let Some((is_wants, values)) = parse_wanted_required(line) {
            for value in values {
                let dir = if is_wants {
                    format!("/etc/systemd/{}/{}.wants/", instance, value)
                } else {
                    format!("/etc/systemd/{}/{}.requires/", instance, value)
                };
                wants_dirs.push(dir);
            }
        }
    }

    let dest_str = service_path.to_string_lossy().to_string();
    let mut links = Vec::new();
    for wants_dir in wants_dirs {
        let src = format!("{}{}", wants_dir, wanted_target);
        links.push(UnitLink {
            dest: dest_str.clone(),
            src,
        });
    }

    Ok(links)
}

fn state_file_entries(state_path: &Path) -> Vec<String> {
    let file = match fs::File::open(state_path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    BufReader::new(file)
        .lines()
        .filter_map(|r| r.ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn record_in_statefile(state_path: &Path, service_link: &str) -> Result<()> {
    let entries = state_file_entries(state_path);
    if entries.contains(&service_link.to_string()) {
        return Ok(());
    }
    if let Some(parent) = state_path.parent() {
        lfs::create_dir_all(parent)?;
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_path)
        .wrap_err("open state file")?;
    writeln!(f, "{}", service_link).wrap_err("write state file")?;
    Ok(())
}

fn link_state_path(link_src: &str, user: bool, root: &Path) -> PathBuf {
    let instance = etc_systemd_instance(user);
    let prefix = format!("/etc/systemd/{}/", instance);
    let rel = link_src.strip_prefix(&prefix).unwrap_or(link_src);
    let state_dir = enabled_state_dir(user, root);
    state_dir.join(rel.trim_start_matches('/'))
}

fn dsh_action_enable(
    root: &Path,
    user: bool,
    unit_name: &str,
    service_path: &Path,
) -> Result<()> {
    let mut visited = HashSet::new();
    let links = get_link_closure(
        root,
        service_path,
        etc_systemd_instance(user),
        &mut visited,
    )?;
    let dsh_state = dsh_state_path(unit_name, user, root);
    for link in &links {
        record_in_statefile(&dsh_state, &link.src)?;
        let link_full = root_join(root, &link.src);
        if link_full.exists() {
            continue;
        }
        if let Some(parent) = link_full.parent() {
            lfs::create_dir_all(parent)?;
        }
        let _target = Path::new(&link.dest);
        #[cfg(unix)]
        {
            lfs::symlink_file_for_virtiofs(_target, &link_full)?;
        }
        #[cfg(not(unix))]
        {
            return Err(eyre!("symlink not supported on this platform"));
        }
    }
    for link in &links {
        let statefile = link_state_path(&link.src, user, root);
        if let Some(p) = statefile.parent() {
            lfs::create_dir_all(p).ok();
        }
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&statefile)
            .ok();
    }
    Ok(())
}

fn dsh_action_disable(root: &Path, user: bool, unit_name: &str, purge: bool) -> Result<()> {
    let dsh_state = dsh_state_path(unit_name, user, root);
    let entries = state_file_entries(&dsh_state);
    if purge && dsh_state.exists() {
        lfs::remove_file(&dsh_state).ok();
    }
    for link_src in &entries {
        let link_state = link_state_path(link_src, user, root);
        if purge || link_state.exists() {
            lfs::remove_file(&link_state).ok();
        }
        let link_full = root_join(root, link_src);
        if link_full.is_symlink() {
            let _ = lfs::remove_file(&link_full);
        }
    }
    Ok(())
}

/// Returns `true` if the caller should `continue` to the next unit.
fn dsh_action_unmask(root: &Path, user: bool, unit_name: &str) -> Result<bool> {
    let basename = Path::new(unit_name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(unit_name);
    let mask_link = root_join(
        root,
        &format!(
            "/etc/systemd/{}/{}",
            etc_systemd_instance(user),
            basename
        ),
    );
    if !mask_link.exists() {
        return Ok(true);
    }
    let target = fs::read_link(&mask_link).ok();
    if target.as_ref().map(|t| t != Path::new("/dev/null")).unwrap_or(true) {
        return Ok(true);
    }
    let statefile = masked_state_dir(user, root).join(basename);
    if statefile.exists() {
        lfs::remove_file(&mask_link).ok();
        lfs::remove_file(&statefile).ok();
    }
    Ok(false)
}

fn dsh_action_update_state(
    root: &Path,
    user: bool,
    unit_name: &str,
    service_path: &Path,
) -> Result<()> {
    let mut visited = HashSet::new();
    let links = get_link_closure(
        root,
        service_path,
        etc_systemd_instance(user),
        &mut visited,
    )?;
    let dsh_state = dsh_state_path(unit_name, user, root);
    if let Some(parent) = dsh_state.parent() {
        lfs::create_dir_all(parent)?;
    }
    let mut f = lfs::file_create(&dsh_state)?;
    for link in &links {
        writeln!(f, "{}", link.src).wrap_err("write state")?;
    }
    Ok(())
}

fn dsh_action_was_enabled(
    root: &Path,
    user: bool,
    unit_name: &str,
    quiet: bool,
    exit_rc: &mut i32,
) -> Result<()> {
    let dsh_state = dsh_state_path(unit_name, user, root);
    let entries = state_file_entries(&dsh_state);
    let all_present = entries
        .iter()
        .all(|link_src| root_join(root, link_src).is_symlink());
    if all_present && !entries.is_empty() {
        *exit_rc = 0;
        if !quiet {
            eprintln!("enabled");
        }
    } else if !quiet {
        eprintln!("disabled");
    }
    Ok(())
}

fn dsh_action_debian_installed(root: &Path, user: bool, unit_name: &str, exit_rc: &mut i32) -> Result<()> {
    let dsh_state = dsh_state_path(unit_name, user, root);
    if dsh_state.exists() {
        *exit_rc = 0;
    }
    Ok(())
}

pub fn run(options: DebSystemdHelperOptions) -> Result<()> {
    require_maintscript()?;

    let root = dpkg_root();
    let purge = options.action == "purge";
    let action = if purge {
        "disable"
    } else {
        options.action.as_str()
    };

    let mut exit_rc = 1;
    if matches!(
        action,
        "was-enabled" | "debian-installed" | "is-enabled"
    ) {
        exit_rc = 0;
    }

    for unit_name in &options.units {
        let service_path = match find_unit(&root, unit_name, options.user) {
            Some(p) => p,
            None => {
                if action == "debian-installed" {
                    continue;
                }
                return Err(eyre!("unit file not found: {}", unit_name));
            }
        };

        match action {
            "enable" => {
                dsh_action_enable(&root, options.user, unit_name, &service_path)?;
            }
            "disable" => {
                dsh_action_disable(&root, options.user, unit_name, purge)?;
            }
            "unmask" => {
                if dsh_action_unmask(&root, options.user, unit_name)? {
                    continue;
                }
            }
            "update-state" => {
                dsh_action_update_state(&root, options.user, unit_name, &service_path)?;
            }
            "was-enabled" => {
                dsh_action_was_enabled(
                    &root,
                    options.user,
                    unit_name,
                    options.quiet,
                    &mut exit_rc,
                )?;
            }
            "debian-installed" => {
                dsh_action_debian_installed(&root, options.user, unit_name, &mut exit_rc)?;
            }
            _ => {}
        }
    }

    if options.units.is_empty() && (action == "was-enabled" || action == "debian-installed") {
        // No units -> exit 1 for was-enabled, 1 for debian-installed
    }

    std::process::exit(exit_rc);
}
