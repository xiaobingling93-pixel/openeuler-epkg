//! update-alternatives - manage symbolic links determining default commands
//!
//! Subset compatible with dpkg's update-alternatives: --install, --remove, --remove-all,
//! --auto, --display, --list, --query. Options: --quiet, --force, --slave (with --install).
//! Uses /etc/alternatives and DPKG_ADMINDIR/var/lib/dpkg for alternatives state.

use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::{eyre, WrapErr};
use pathdiff;
use std::collections::HashMap;
use std::env;
use std::fs;
use crate::lfs;
use std::io::Write;
use std::path::{Path, PathBuf};

const DEFAULT_ALTDIR: &str = "/etc/alternatives";
const DEFAULT_ADMINDIR: &str = "/var/lib/dpkg/alternatives";
const ADMINDIR_ENVVAR: &str = "DPKG_ADMINDIR";
const INSTDIR_ENVVAR: &str = "DPKG_ROOT";

#[derive(Debug, Clone)]
pub struct UpdateAlternativesOptions {
    pub quiet: bool,
    pub force: bool,
    pub action: String,
    pub install_link: Option<String>,
    pub install_name: Option<String>,
    pub install_path: Option<String>,
    pub install_priority: Option<i32>,
    pub slaves: Vec<(String, String, String)>, // (link, name, path)
    pub name: Option<String>,
    pub path: Option<String>,
}

fn instdir() -> PathBuf {
    env::var_os(INSTDIR_ENVVAR)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(""))
}

fn admindir() -> PathBuf {
    env::var_os(ADMINDIR_ENVVAR)
        .map(|v| PathBuf::from(v).join("alternatives"))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ADMINDIR))
}

fn root_join(root: &Path, p: &str) -> PathBuf {
    let p_trim = p.trim_start_matches('/');
    if root.as_os_str().is_empty() || p_trim.is_empty() {
        PathBuf::from(p)
    } else {
        root.join(p_trim)
    }
}

/// Create a relative target path for a symlink within an environment root
///
/// Given a target path (relative to root) and the symlink location,
/// computes a relative path from symlink to target.
/// Example:
/// - root: /home/wfg/.epkg/envs/oe
/// - target_str: /usr/lib/golang/bin/go
/// - link_path: /home/wfg/.epkg/envs/oe/etc/alternatives/go
/// - Returns: ../../../usr/lib/golang/bin/go
fn make_relative_target(root: &Path, target_str: &str, link_path: &Path) -> PathBuf {
    let target_path = root_join(root, target_str);
    let link_dir = link_path.parent().unwrap_or(Path::new("."));
    pathdiff::diff_paths(&target_path, link_dir)
        .unwrap_or_else(|| target_path.clone())
}

/// Make an existing path relative to a symlink location
///
/// Given an already-computed target path and symlink location,
/// computes relative path.
/// Example:
/// - target_path: /home/wfg/.epkg/envs/oe/etc/alternatives/go
/// - link_path: /home/wfg/.epkg/envs/oe/usr/bin/go
/// - Returns: ../etc/alternatives/go
fn make_relative_existing(target_path: &Path, link_path: &Path) -> PathBuf {
    let link_dir = link_path.parent().unwrap_or(Path::new("."));
    pathdiff::diff_paths(target_path, link_dir)
        .unwrap_or_else(|| target_path.to_path_buf())
}

struct SlaveLink {
    name: String,
    link: String,
}

struct Choice {
    master_file: String,
    priority: i32,
    slave_files: HashMap<String, String>,
}

struct Alternative {
    master_name: String,
    master_link: String,
    status: String,
    current: Option<String>,
    slaves: Vec<SlaveLink>,
    choices: Vec<Choice>,
}

fn read_admin_file(admdir: &Path, name: &str, root: &Path) -> Result<Option<Alternative>> {
    let fpath = admdir.join(name);
    let content = match fs::read_to_string(&fpath) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).wrap_err_with(|| format!("read {}", fpath.display())),
    };
    let mut lines = content.lines().map(str::trim);
    let status = lines.next().ok_or_else(|| eyre!("empty admin file"))?.to_string();
    let master_link = lines.next().ok_or_else(|| eyre!("truncated admin file"))?.to_string();
    let mut slaves = Vec::new();
    loop {
        let sname = match lines.next() {
            Some("") => break,
            Some(s) => s.to_string(),
            None => break,
        };
        let slink = lines.next().ok_or_else(|| eyre!("truncated slaves"))?.to_string();
        slaves.push(SlaveLink { name: sname, link: slink });
    }
    let mut choices = Vec::new();
    loop {
        let master_file = match lines.next() {
            Some("") => break,
            Some(s) => s.to_string(),
            None => break,
        };
        let prio_str = lines.next().ok_or_else(|| eyre!("truncated choices"))?;
        let priority: i32 = prio_str.parse().map_err(|_| eyre!("invalid priority"))?;
        let mut slave_files = HashMap::new();
        for sl in &slaves {
            let spath = lines.next().unwrap_or("").to_string();
            slave_files.insert(sl.name.clone(), spath);
        }
        choices.push(Choice {
            master_file,
            priority,
            slave_files,
        });
        if lines.next() != Some("") {
            break;
        }
    }
    let current = read_current(admdir, name, root)?;
    Ok(Some(Alternative {
        master_name: name.to_string(),
        master_link,
        status,
        current,
        slaves,
        choices,
    }))
}

fn read_current(_admdir: &Path, name: &str, root: &Path) -> Result<Option<String>> {
    let alt_link = root_join(root, &format!("{}/{}", DEFAULT_ALTDIR, name));
    if !alt_link.is_symlink() {
        return Ok(None);
    }
    let target = fs::read_link(&alt_link)
        .wrap_err_with(|| format!("readlink {}", alt_link.display()))?;
    Ok(Some(
        target.to_string_lossy().into_owned()
    ))
}

fn write_admin_file(admdir: &Path, a: &Alternative) -> Result<()> {
    let fpath = admdir.join(&a.master_name);
    if let Some(parent) = fpath.parent() {
        lfs::create_dir_all(parent)?;
    }
    let mut f = lfs::file_create(&fpath)?;
    writeln!(f, "{}", a.status)?;
    writeln!(f, "{}", a.master_link)?;
    for sl in &a.slaves {
        writeln!(f, "{}", sl.name)?;
        writeln!(f, "{}", sl.link)?;
    }
    writeln!(f, "")?;
    for choice in &a.choices {
        writeln!(f, "{}", choice.master_file)?;
        writeln!(f, "{}", choice.priority)?;
        for sl in &a.slaves {
            let sp = choice.slave_files.get(&sl.name).map(String::as_str).unwrap_or("");
            writeln!(f, "{}", sp)?;
        }
        writeln!(f, "")?;
    }
    Ok(())
}

fn path_exists(root: &Path, p: &str) -> bool {
    lfs::exists_or_any_symlink(&root_join(root, p))
}

fn best_choice(a: &Alternative) -> Option<&Choice> {
    a.choices.iter().max_by_key(|c| c.priority)
}

fn apply_alternative(
    root: &Path,
    altdir: &str,
    a: &Alternative,
    choice_path: &str,
    force: bool,
) -> Result<()> {
    // Create symlinks with relative paths so they work both inside and outside chroot.
    // Absolute symlinks like /usr/lib/golang/bin/go would point to host root when viewed
    // from outside the environment root. Relative symlinks work in both contexts.
    let alt_name_path = root_join(root, &format!("{}/{}", altdir, a.master_name));
    let choice = a.choices.iter().find(|c| c.master_file == choice_path)
        .ok_or_else(|| eyre!("choice not found: {}", choice_path))?;

    if let Some(parent) = alt_name_path.parent() {
        lfs::create_dir_all(parent)?;
    }
    if lfs::exists_or_any_symlink(&alt_name_path) {
        lfs::remove_file(&alt_name_path)?;
    }
    // Compute target path relative to root, then make it relative to symlink location
    let _relative_target = make_relative_target(root, choice_path, &alt_name_path);
    #[cfg(unix)]
    lfs::symlink(&_relative_target, &alt_name_path)?;

    let master_link_full = root_join(root, &a.master_link);
    if lfs::exists_or_any_symlink(&master_link_full) {
        if !force && !master_link_full.is_symlink() {
            return Err(eyre!("not replacing {} with a link (use --force)", a.master_link));
        }
        lfs::remove_file(&master_link_full).ok();
    }
    if let Some(parent) = master_link_full.parent() {
        lfs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        let alt_name_relative = make_relative_existing(&alt_name_path, &master_link_full);
        lfs::symlink(&alt_name_relative, &master_link_full)?;
    }

    for sl in &a.slaves {
        if let Some(spath) = choice.slave_files.get(&sl.name) {
            if spath.is_empty() || !path_exists(root, spath) {
                continue;
            }
            let slave_alt = root_join(root, &format!("{}/{}", altdir, sl.name));
            let slave_link_full = root_join(root, &sl.link);
            if lfs::exists_or_any_symlink(&slave_alt) {
                lfs::remove_file(&slave_alt).ok();
            }
            #[cfg(unix)]
            {
                let slave_relative = make_relative_target(root, spath, &slave_alt);
                lfs::symlink(&slave_relative, &slave_alt).ok();
            }
            if lfs::exists_or_any_symlink(&slave_link_full) {
                if !force && !slave_link_full.is_symlink() {
                    continue;
                }
                lfs::remove_file(&slave_link_full).ok();
            }
            if let Some(p) = slave_link_full.parent() {
                lfs::create_dir_all(p).ok();
            }
            #[cfg(unix)]
            {
                let slave_alt_relative = make_relative_existing(&slave_alt, &slave_link_full);
                lfs::symlink(&slave_alt_relative, &slave_link_full).ok();
            }
        }
    }
    Ok(())
}

fn remove_links(root: &Path, altdir: &str, a: &Alternative) -> Result<()> {
    let alt_path = root_join(root, &format!("{}/{}", altdir, a.master_name));
    lfs::remove_file(&alt_path).ok();
    if a.master_link.starts_with('/') {
        let link_full = root_join(root, &a.master_link);
        if link_full.is_symlink() {
            lfs::remove_file(&link_full).ok();
        }
    }
    for sl in &a.slaves {
        let slave_alt = root_join(root, &format!("{}/{}", altdir, sl.name));
        lfs::remove_file(&slave_alt).ok();
        let link_full = root_join(root, &sl.link);
        if link_full.is_symlink() {
            lfs::remove_file(&link_full).ok();
        }
    }
    Ok(())
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<UpdateAlternativesOptions> {
    let quiet = matches.get_flag("quiet");
    let force = matches.get_flag("force");

    let (action, install_link, install_name, install_path, install_priority, name, path) =
        if let Some(v) = matches.get_many::<String>("install") {
            let vec: Vec<String> = v.cloned().collect();
            if vec.len() >= 4 {
                (
                    "install".to_string(),
                    Some(vec[0].clone()),
                    Some(vec[1].clone()),
                    Some(vec[2].clone()),
                    vec[3].parse().ok(),
                    None,
                    None,
                )
            } else {
                (String::new(), None, None, None, None, None, None)
            }
        } else if let Some(v) = matches.get_many::<String>("remove") {
            let vec: Vec<String> = v.cloned().collect();
            if vec.len() >= 2 {
                (
                    "remove".to_string(),
                    None,
                    Some(vec[0].clone()),
                    Some(vec[1].clone()),
                    None,
                    Some(vec[0].clone()),
                    Some(vec[1].clone()),
                )
            } else {
                (String::new(), None, None, None, None, None, None)
            }
        } else if let Some(n) = matches.get_one::<String>("remove_all") {
            ("remove-all".to_string(), None, Some(n.clone()), None, None, Some(n.clone()), None)
        } else if let Some(n) = matches.get_one::<String>("auto") {
            ("auto".to_string(), None, Some(n.clone()), None, None, Some(n.clone()), None)
        } else if let Some(n) = matches.get_one::<String>("display") {
            ("display".to_string(), None, Some(n.clone()), None, None, Some(n.clone()), None)
        } else if let Some(n) = matches.get_one::<String>("list") {
            ("list".to_string(), None, Some(n.clone()), None, None, Some(n.clone()), None)
        } else if let Some(n) = matches.get_one::<String>("query") {
            ("query".to_string(), None, Some(n.clone()), None, None, Some(n.clone()), None)
        } else {
            (String::new(), None, None, None, None, None, None)
        };

    let slaves: Vec<(String, String, String)> = matches
        .get_many::<String>("slave")
        .map(|v| v.cloned().collect::<Vec<_>>())
        .unwrap_or_default()
        .chunks(3)
        .filter(|c| c.len() == 3)
        .map(|c| (c[0].clone(), c[1].clone(), c[2].clone()))
        .collect();

    Ok(UpdateAlternativesOptions {
        quiet,
        force,
        action,
        install_link,
        install_name,
        install_path,
        install_priority,
        slaves,
        name,
        path,
    })
}

pub fn command() -> Command {
    Command::new("update-alternatives")
        .about("Manage symbolic links determining default commands")
        .arg(Arg::new("quiet").long("quiet").action(clap::ArgAction::SetTrue))
        .arg(Arg::new("force").long("force").action(clap::ArgAction::SetTrue))
        .arg(
            Arg::new("install")
                .long("install")
                .num_args(4)
                .value_names(["link", "name", "path", "priority"]),
        )
        .arg(
            Arg::new("slave")
                .long("slave")
                .num_args(3)
                .value_names(["link", "name", "path"])
                .action(clap::ArgAction::Append),
        )
        .arg(
            Arg::new("remove")
                .long("remove")
                .num_args(2)
                .value_names(["name", "path"]),
        )
        .arg(
            Arg::new("remove_all")
                .long("remove-all")
                .num_args(1)
                .value_name("name"),
        )
        .arg(Arg::new("auto").long("auto").num_args(1).value_name("name"))
        .arg(Arg::new("display").long("display").num_args(1).value_name("name"))
        .arg(Arg::new("list").long("list").num_args(1).value_name("name"))
        .arg(Arg::new("query").long("query").num_args(1).value_name("name"))
}

pub fn run(options: UpdateAlternativesOptions) -> Result<()> {
    let root = instdir();
    let adm = admindir();
    let altdir = DEFAULT_ALTDIR;

    match options.action.as_str() {
        "install" => {
            let link = options.install_link.as_ref().ok_or_else(|| eyre!("--install needs <link> <name> <path> <priority>"))?;
            let name = options.install_name.as_ref().ok_or_else(|| eyre!("--install needs name"))?;
            let path = options.install_path.as_ref().ok_or_else(|| eyre!("--install needs path"))?;
            let priority = options.install_priority.ok_or_else(|| eyre!("--install needs priority"))?;
            if !path_exists(&root, path) {
                return Err(eyre!("alternative path {} does not exist", path));
            }
            let mut a = read_admin_file(&adm, name, &root)?.unwrap_or_else(|| Alternative {
                master_name: name.clone(),
                master_link: link.clone(),
                status: "auto".to_string(),
                current: None,
                slaves: Vec::new(),
                choices: Vec::new(),
            });
            a.master_link = link.clone();
            for (slink, sname, _spath) in &options.slaves {
                if !a.slaves.iter().any(|s| s.name == *sname) {
                    a.slaves.push(SlaveLink { name: sname.clone(), link: slink.clone() });
                }
            }
            let mut slave_files = HashMap::new();
            for (_, sname, spath) in &options.slaves {
                slave_files.insert(sname.clone(), spath.clone());
            }
            let mut found = false;
            for c in &mut a.choices {
                if c.master_file == *path {
                    c.priority = priority;
                    for (k, v) in &slave_files {
                        c.slave_files.insert(k.clone(), v.clone());
                    }
                    found = true;
                    break;
                }
            }
            if !found {
                a.choices.push(Choice {
                    master_file: path.clone(),
                    priority,
                    slave_files,
                });
            }
            let new_current = if a.status == "auto" {
                best_choice(&a).map(|c| c.master_file.clone())
            } else {
                a.current.clone()
            };
            if let Some(ref cur) = new_current {
                apply_alternative(&root, altdir, &a, cur, options.force)?;
            }
            a.current = new_current;
            write_admin_file(&adm, &a)?;
        }
        "remove" => {
            let name = options.name.as_ref().ok_or_else(|| eyre!("--remove needs <name> <path>"))?;
            let path = options.path.as_ref().ok_or_else(|| eyre!("--remove needs path"))?;
            let mut a = read_admin_file(&adm, name, &root)?
                .ok_or_else(|| eyre!("no alternatives for {}", name))?;
            a.choices.retain(|c| c.master_file != *path);
            let new_current = if a.status == "auto" {
                best_choice(&a).map(|c| c.master_file.clone())
            } else if a.current.as_deref() == Some(path.as_str()) {
                best_choice(&a).map(|c| c.master_file.clone())
            } else {
                a.current.clone()
            };
            if a.choices.is_empty() {
                remove_links(&root, altdir, &a)?;
                lfs::remove_file(adm.join(name)).ok();
            } else if let Some(ref cur) = new_current {
                apply_alternative(&root, altdir, &a, cur, options.force)?;
                a.current = new_current;
                write_admin_file(&adm, &a)?;
            }
        }
        "remove-all" => {
            let name = options.name.as_ref().ok_or_else(|| eyre!("--remove-all needs <name>"))?;
            if let Some(a) = read_admin_file(&adm, name, &root)? {
                remove_links(&root, altdir, &a)?;
            }
            lfs::remove_file(adm.join(name)).ok();
        }
        "auto" => {
            let name = options.name.as_ref().ok_or_else(|| eyre!("--auto needs <name>"))?;
            let mut a = read_admin_file(&adm, name, &root)?
                .ok_or_else(|| eyre!("no alternatives for {}", name))?;
            a.status = "auto".to_string();
            if let Some(best) = best_choice(&a) {
                let cur = best.master_file.clone();
                apply_alternative(&root, altdir, &a, &cur, options.force)?;
                a.current = Some(cur);
            }
            write_admin_file(&adm, &a)?;
        }
        "display" => {
            let name = options.name.as_ref().ok_or_else(|| eyre!("--display needs <name>"))?;
            let a = read_admin_file(&adm, name, &root)?
                .ok_or_else(|| eyre!("no alternatives for {}", name))?;
            let best = best_choice(&a);
            if !options.quiet {
                println!("{} - {}", a.master_name, if a.status == "auto" { "auto mode" } else { "manual mode" });
                if let Some(b) = best {
                    println!("  link best version is {}", b.master_file);
                }
                println!("  link currently points to {}", a.current.as_deref().unwrap_or("(none)"));
                println!("  link {} is {}", a.master_name, a.master_link);
                for sl in &a.slaves {
                    println!("  slave {} is {}", sl.name, sl.link);
                }
                for c in &a.choices {
                    println!("{} - priority {}", c.master_file, c.priority);
                }
            }
        }
        "list" => {
            let name = options.name.as_ref().ok_or_else(|| eyre!("--list needs <name>"))?;
            let a = read_admin_file(&adm, name, &root)?
                .ok_or_else(|| eyre!("no alternatives for {}", name))?;
            for c in &a.choices {
                println!("{}", c.master_file);
            }
        }
        "query" => {
            let name = options.name.as_ref().ok_or_else(|| eyre!("--query needs <name>"))?;
            let a = read_admin_file(&adm, name, &root)?
                .ok_or_else(|| eyre!("no alternatives for {}", name))?;
            let best = best_choice(&a);
            println!("Name: {}", a.master_name);
            println!("Link: {}", a.master_link);
            if !a.slaves.is_empty() {
                print!("Slaves:");
                for sl in &a.slaves {
                    print!(" {} {}", sl.name, sl.link);
                }
                println!();
            }
            println!("Status: {}", a.status);
            if let Some(b) = best {
                println!("Best: {}", b.master_file);
            }
            println!("Value: {}", a.current.as_deref().unwrap_or("none"));
            for c in &a.choices {
                println!();
                println!("Alternative: {}", c.master_file);
                println!("Priority: {}", c.priority);
            }
        }
        _ => return Err(eyre!("need --install, --remove, --remove-all, --auto, --display, --list or --query")),
    }
    Ok(())
}
