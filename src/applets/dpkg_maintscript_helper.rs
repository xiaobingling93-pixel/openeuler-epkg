//! dpkg-maintscript-helper: works around known dpkg limitations in maintainer scripts.
//!
//! Upstream reference: `/usr/bin/dpkg-maintscript-helper` (POSIX shell script from dpkg).
//! Implements rm_conffile, mv_conffile, symlink_to_dir, and dir_to_symlink.
//! Behavior is keyed off DPKG_MAINTSCRIPT_NAME and script arguments after "--".
//! Paths are resolved under DPKG_ROOT when set (upstream uses empty when DPKG_ROOT is "/").

use clap::{Arg, Command};
use color_eyre::Result;
use std::cmp::Ordering;
use std::env;
use std::fs;
use crate::lfs;
use std::path::{Path, PathBuf};

use crate::models::PackageFormat;
use crate::version_compare::compare_versions;

#[derive(Debug, Clone)]
pub struct DpkgMaintscriptHelperOptions {
    pub subcommand: Option<String>,
    pub args: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<DpkgMaintscriptHelperOptions> {
    let args: Vec<String> = matches
        .get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let subcommand = args.get(0).cloned();

    Ok(DpkgMaintscriptHelperOptions { subcommand, args })
}

/// Build options from raw argv (e.g. from env::args_os). Use this when invoking as an
/// applet so that "--" and script args after it are preserved; clap consumes "--" and
/// drops following args for the positional, which breaks maintscript calls like
/// `dpkg-maintscript-helper rm_conffile ... -- install`.
pub fn options_from_raw_args(raw_args: &[std::ffi::OsString]) -> DpkgMaintscriptHelperOptions {
    let args: Vec<String> = raw_args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let subcommand = args.first().cloned();
    DpkgMaintscriptHelperOptions { subcommand, args }
}

pub fn command() -> Command {
    Command::new("dpkg-maintscript-helper")
        .about("Helper for maintainer scripts (rm_conffile, mv_conffile, symlink_to_dir, dir_to_symlink)")
        .arg(
            Arg::new("args")
                .value_name("ARGS")
                .num_args(1..)
                .help("Subcommand and arguments as used in maintainer scripts (use -- before script args)"),
        )
}

/// Resolve an absolute path under DPKG_ROOT. When DPKG_ROOT is unset or "/", path is used as-is.
fn resolve_root(path: &str) -> PathBuf {
    let root = env::var("DPKG_ROOT").unwrap_or_default();
    let path_trimmed = path.trim_start_matches('/');
    if root.is_empty() || root == "/" {
        if path.starts_with('/') {
            PathBuf::from(path)
        } else {
            PathBuf::from("/").join(path_trimmed)
        }
    } else {
        Path::new(&root).join(path_trimmed)
    }
}

/// Upstream: preinst/postinst run when $1 is install|upgrade or configure, [ -n "$2" ], and
/// dpkg --compare-versions -- "$2" le-nl "$LASTVERSION". So $2 (old version) must be present.
/// Returns true when we should run the upgrade-triggered operation.
fn should_run_for_upgrade(script_args: &[String], prior_version: Option<&str>) -> bool {
    let first = script_args.first().map(String::as_str);
    let ver = script_args.get(1).map(String::as_str);
    let ver = match ver {
        Some(v) if !v.is_empty() => v,
        _ => return false,
    };
    match first {
        Some("upgrade") | Some("install") | Some("configure") => match prior_version {
            None | Some("") => true,
            Some(prior) => match compare_versions(ver, prior, PackageFormat::Deb) {
                Some(Ordering::Less) | Some(Ordering::Equal) => true,
                _ => false,
            },
        },
        _ => false,
    }
}

/// Upstream: postrm abort runs when $1 is abort-install|abort-upgrade, [ -n "$2" ], and
/// dpkg --compare-versions -- "$2" le-nl "$LASTVERSION". So we run when version <= prior.
fn should_run_abort(script_args: &[String], prior_version: Option<&str>) -> bool {
    let first = script_args.first().map(String::as_str);
    let ver = script_args.get(1).map(String::as_str);
    let ver = match ver {
        Some(v) if !v.is_empty() => v,
        _ => return false,
    };
    if first != Some("abort-install") && first != Some("abort-upgrade") {
        return false;
    }
    match prior_version {
        None | Some("") => true,
        Some(prior) => match compare_versions(ver, prior, PackageFormat::Deb) {
            Some(Ordering::Less) | Some(Ordering::Equal) => true,
            _ => false,
        },
    }
}

fn handle_supports(args: &[String]) -> i32 {
    let supported = ["mv_conffile", "rm_conffile", "dir_to_symlink", "symlink_to_dir"];
    let name_ok = args.get(1).map_or(false, |n| supported.contains(&n.as_str()));
    if !name_ok {
        return 1;
    }
    if env::var("DPKG_MAINTSCRIPT_NAME").unwrap_or_default().is_empty() {
        eprintln!("dpkg-maintscript-helper: warning: environment variable DPKG_MAINTSCRIPT_NAME missing");
        return 1;
    }
    if env::var("DPKG_MAINTSCRIPT_PACKAGE").unwrap_or_default().is_empty() {
        eprintln!("dpkg-maintscript-helper: warning: environment variable DPKG_MAINTSCRIPT_PACKAGE missing");
        return 1;
    }
    0
}

/// Split args into helper args (before "--") and script args (after "--").
fn split_args(args: &[String]) -> (Vec<String>, Vec<String>) {
    let sep = args.iter().position(|a| a == "--");
    match sep {
        Some(i) => (args[..i].to_vec(), args[i + 1..].to_vec()),
        None => (args.to_vec(), vec![]),
    }
}

/// Upstream: require arguments after "--" and DPKG_MAINTSCRIPT_NAME / DPKG_MAINTSCRIPT_PACKAGE.
fn require_script_env(script_args: &[String]) -> Result<(), i32> {
    if script_args.is_empty() {
        eprintln!("dpkg-maintscript-helper: error: missing arguments after --");
        return Err(1);
    }
    if env::var("DPKG_MAINTSCRIPT_NAME").unwrap_or_default().is_empty() {
        eprintln!("dpkg-maintscript-helper: error: environment variable DPKG_MAINTSCRIPT_NAME is required");
        return Err(1);
    }
    if env::var("DPKG_MAINTSCRIPT_PACKAGE").unwrap_or_default().is_empty() {
        eprintln!("dpkg-maintscript-helper: error: environment variable DPKG_MAINTSCRIPT_PACKAGE is required");
        return Err(1);
    }
    Ok(())
}

fn require_absolute_path(path: &str, name: &str) -> Result<(), i32> {
    if path.is_empty() || !path.starts_with('/') {
        eprintln!("dpkg-maintscript-helper: error: {} '{}' is not an absolute path", name, path);
        return Err(1);
    }
    Ok(())
}

// --- rm_conffile: conffile [prior-version [package]] -- "$@"
// Upstream: preinst prepare_rm_conffile (md5: backup or remove); postinst finish_rm_conffile;
// postrm purge: rm .dpkg-bak .dpkg-remove .dpkg-backup; postrm abort: restore both.
// We don't have dpkg conffile md5, so we treat as not modified (use .dpkg-remove only).

fn do_rm_conffile(helper_args: &[String], script_args: &[String], script_name: &str) -> Result<i32> {
    if let Err(c) = require_script_env(script_args) {
        return Ok(c);
    }
    let conffile = match helper_args.get(1) {
        Some(p) => p.as_str(),
        None => {
            log::debug!("dpkg-maintscript-helper rm_conffile: missing conffile");
            return Ok(1);
        }
    };
    if let Err(c) = require_absolute_path(conffile, "conffile") {
        return Ok(c);
    }
    let prior_version = helper_args.get(2).filter(|s| *s != "--" && !s.is_empty()).map(String::as_str);
    if prior_version.is_some() && !should_run_for_upgrade(script_args, prior_version) {
        return Ok(0);
    }
    let path = resolve_root(conffile);
    let remove_marker = path.clone().with_extension("dpkg-remove");
    let backup_marker = path.clone().with_extension("dpkg-backup");
    let bak_path = path.clone().with_extension("dpkg-bak");

    let exit_ok = match script_name {
        "preinst" => {
            if path.exists() {
                if let Err(e) = lfs::rename(&path, &remove_marker) {
                    log::warn!("dpkg-maintscript-helper rm_conffile preinst rename: {}", e);
                    return Ok(1);
                }
            }
            0
        }
        "postinst" => {
            let _ = lfs::remove_file(&remove_marker);
            if backup_marker.exists() {
                let _ = lfs::rename(&backup_marker, &bak_path);
            }
            0
        }
        "postrm" => {
            let first = script_args.first().map(String::as_str);
            if first == Some("purge") {
                let _ = lfs::remove_file(&bak_path);
                let _ = lfs::remove_file(&remove_marker);
                let _ = lfs::remove_file(&backup_marker);
            } else if should_run_abort(script_args, prior_version) {
                if remove_marker.exists() {
                    let _ = lfs::rename(&remove_marker, &path);
                }
                if backup_marker.exists() {
                    let _ = lfs::rename(&backup_marker, &path);
                }
            }
            0
        }
        _ => 0,
    };
    Ok(exit_ok)
}

// --- mv_conffile: old-conffile new-conffile [prior-version [package]] -- "$@"
// preinst: if not modified, old -> old.dpkg-remove
// postinst: remove old.dpkg-remove; if old exists, rename old -> new
// postrm abort: old.dpkg-remove -> old

fn do_mv_conffile(helper_args: &[String], script_args: &[String], script_name: &str) -> Result<i32> {
    if let Err(c) = require_script_env(script_args) {
        return Ok(c);
    }
    let old_path_str = match helper_args.get(1) {
        Some(p) => p.as_str(),
        None => {
            log::debug!("dpkg-maintscript-helper mv_conffile: missing old-conffile");
            return Ok(1);
        }
    };
    let new_path_str = match helper_args.get(2) {
        Some(p) => p.as_str(),
        None => {
            log::debug!("dpkg-maintscript-helper mv_conffile: missing new-conffile");
            return Ok(1);
        }
    };
    if let Err(c) = require_absolute_path(old_path_str, "old-conffile") {
        return Ok(c);
    }
    if let Err(c) = require_absolute_path(new_path_str, "new-conffile") {
        return Ok(c);
    }
    let prior_version = helper_args.get(3).filter(|s| *s != "--" && !s.is_empty()).map(String::as_str);
    if prior_version.is_some() && !should_run_for_upgrade(script_args, prior_version) {
        return Ok(0);
    }
    let old_path = resolve_root(old_path_str);
    let new_path = resolve_root(new_path_str);
    let remove_marker = {
        let mut p = old_path.clone();
        p.set_extension("dpkg-remove");
        p
    };
    let new_backup = new_path.clone().with_extension("dpkg-new");

    match script_name {
        "preinst" => {
            if old_path.exists() {
                if let Err(e) = lfs::rename(&old_path, &remove_marker) {
                    log::warn!("dpkg-maintscript-helper mv_conffile preinst: {}", e);
                    return Ok(1);
                }
            }
        }
        "postinst" => {
            let _ = lfs::remove_file(&remove_marker);
            if old_path.exists() {
                if new_path.exists() {
                    let _ = lfs::rename(&new_path, &new_backup);
                }
                if let Err(e) = lfs::rename(&old_path, &new_path) {
                    log::warn!("dpkg-maintscript-helper mv_conffile postinst: {}", e);
                    return Ok(1);
                }
            }
        }
        "postrm" => {
            if should_run_abort(script_args, prior_version) && remove_marker.exists() {
                let _ = lfs::rename(&remove_marker, &old_path);
            }
        }
        _ => {}
    }
    Ok(0)
}

// --- symlink_to_dir: pathname old-target [prior-version [package]] -- "$@"
// Upstream: preinst mv symlink to .dpkg-backup if target matches; postinst rm .dpkg-backup if symlink;
// postrm purge: rm .dpkg-backup if symlink; postrm abort: restore .dpkg-backup -> pathname.

fn do_symlink_to_dir(helper_args: &[String], script_args: &[String], script_name: &str) -> Result<i32> {
    if let Err(c) = require_script_env(script_args) {
        return Ok(c);
    }
    let pathname = match helper_args.get(1) {
        Some(p) => p.as_str(),
        None => {
            log::debug!("dpkg-maintscript-helper symlink_to_dir: missing pathname");
            return Ok(1);
        }
    };
    let old_target = match helper_args.get(2) {
        Some(p) => p.as_str(),
        None => {
            log::debug!("dpkg-maintscript-helper symlink_to_dir: missing old-target");
            return Ok(1);
        }
    };
    if pathname.ends_with('/') {
        eprintln!("dpkg-maintscript-helper: error: symlink pathname ends with a slash");
        return Ok(1);
    }
    if let Err(c) = require_absolute_path(pathname, "symlink pathname") {
        return Ok(c);
    }
    let prior_version = helper_args.get(3).filter(|s| *s != "--" && !s.is_empty()).map(String::as_str);
    if prior_version.is_some() && !should_run_for_upgrade(script_args, prior_version) {
        return Ok(0);
    }
    let path = resolve_root(pathname);
    let backup_path = path.clone().with_extension("dpkg-backup");

    match script_name {
        "preinst" => {
            if path.is_symlink() {
                if let Ok(target) = fs::read_link(&path) {
                    let target_str = target.to_string_lossy();
                    if target_str == old_target || target_str.ends_with(old_target.trim_start_matches('/')) {
                        if let Err(e) = lfs::rename(&path, &backup_path) {
                            log::warn!("dpkg-maintscript-helper symlink_to_dir preinst: {}", e);
                            return Ok(1);
                        }
                    }
                }
            }
        }
        "postinst" => {
            if backup_path.exists() {
                let meta = fs::metadata(&backup_path).ok();
                if meta.map(|m| m.is_symlink()).unwrap_or(false) {
                    let _ = lfs::remove_file(&backup_path);
                }
            }
        }
        "postrm" => {
            let first = script_args.first().map(String::as_str);
            if first == Some("purge") && backup_path.is_symlink() {
                let _ = lfs::remove_file(&backup_path);
            } else if should_run_abort(script_args, prior_version) && backup_path.exists() {
                let _ = lfs::rename(&backup_path, &path);
            }
        }
        _ => {}
    }
    Ok(0)
}

/// Path + ".dpkg-backup" (path is a directory name like /usr/share/foo)
fn backup_path_for_dir(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    p.set_extension("dpkg-backup");
    p
}

/// Staging marker file inside the empty directory we create in preinst for dir_to_symlink.
const STAGING_MARKER: &str = ".dpkg-staging-dir";

// --- dir_to_symlink: pathname new-target [prior-version [package]] -- "$@"
// Upstream: preinst prepare_dir_to_symlink (backup dir, mkdir pathname, touch .dpkg-staging-dir);
// postinst finish_dir_to_symlink (move pathname/* to abs(new_target), rmdir, ln -s, rm backup);
// postrm purge: rm -rf .dpkg-backup; postrm abort: restore .dpkg-backup -> pathname.
// Upstream resolves relative new_target as (dirname pathname)/new_target.

fn do_dir_to_symlink(helper_args: &[String], script_args: &[String], script_name: &str) -> Result<i32> {
    if let Err(c) = require_script_env(script_args) {
        return Ok(c);
    }
    let pathname = match helper_args.get(1) {
        Some(p) => p.as_str().trim_end_matches('/'),
        None => {
            log::debug!("dpkg-maintscript-helper dir_to_symlink: missing pathname");
            return Ok(1);
        }
    };
    let new_target = match helper_args.get(2) {
        Some(p) => p.as_str(),
        None => {
            log::debug!("dpkg-maintscript-helper dir_to_symlink: missing new-target");
            return Ok(1);
        }
    };
    if let Err(c) = require_absolute_path(pathname, "directory parameter") {
        return Ok(c);
    }
    let prior_version = helper_args.get(3).filter(|s| *s != "--" && !s.is_empty()).map(String::as_str);
    if prior_version.is_some() && !should_run_for_upgrade(script_args, prior_version) {
        return Ok(0);
    }
    let path = resolve_root(pathname);
    let backup_path = backup_path_for_dir(&path);
    let staging_marker_path = path.join(STAGING_MARKER);

    match script_name {
        "preinst" => {
            if path.is_dir() && !path.is_symlink() {
                if let Err(e) = lfs::rename(&path, &backup_path) {
                    log::warn!("dpkg-maintscript-helper dir_to_symlink preinst rename: {}", e);
                    return Ok(1);
                }
                if let Err(e) = lfs::create_dir_all(&path) {
                    log::warn!("dpkg-maintscript-helper dir_to_symlink preinst mkdir: {}", e);
                    let _ = lfs::rename(&backup_path, &path);
                    return Ok(1);
                }
                if let Err(e) = lfs::file_create(&staging_marker_path) {
                    log::warn!("dpkg-maintscript-helper dir_to_symlink preinst marker: {}", e);
                    let _ = lfs::remove_dir(&path);
                    let _ = lfs::rename(&backup_path, &path);
                    return Ok(1);
                }
            }
        }
        "postinst" => {
            if backup_path.is_dir()
                && staging_marker_path.exists()
            {
                let _ = lfs::remove_file(&staging_marker_path);
                let new_target_path = if new_target.starts_with('/') {
                    resolve_root(new_target)
                } else {
                    path.parent().map_or_else(|| PathBuf::from(new_target), |parent| parent.join(new_target))
                };
                let _ = lfs::create_dir_all(&new_target_path);
                if let Ok(entries) = fs::read_dir(&path) {
                    for entry in entries.flatten() {
                        let name = entry.file_name();
                        let dest = new_target_path.join(&name);
                        if let Err(e) = lfs::rename(entry.path(), &dest) {
                            log::warn!("dpkg-maintscript-helper dir_to_symlink postinst move {:?}: {}", name, e);
                        }
                    }
                }
                if let Err(e) = lfs::remove_dir(&path) {
                    log::warn!("dpkg-maintscript-helper dir_to_symlink postinst rmdir: {}", e);
                } else {
                    if let Err(e) = crate::utils::force_symlink(new_target, &path) {
                        log::warn!("dpkg-maintscript-helper dir_to_symlink postinst symlink: {}", e);
                    } else if let Err(e) = lfs::remove_dir_all(&backup_path) {
                        log::warn!("dpkg-maintscript-helper dir_to_symlink postinst remove backup: {}", e);
                    }
                }
            }
        }
        "postrm" => {
            let first = script_args.first().map(String::as_str);
            if first == Some("purge") && backup_path.is_dir() {
                let _ = lfs::remove_dir_all(&backup_path);
            } else if should_run_abort(script_args, prior_version) && backup_path.exists() {
                if path.is_symlink() {
                    let _ = lfs::remove_file(&path);
                } else if staging_marker_path.exists() {
                    let _ = lfs::remove_file(&staging_marker_path);
                    let _ = lfs::remove_dir(&path);
                }
                let _ = lfs::rename(&backup_path, &path);
            }
        }
        _ => {}
    }
    Ok(0)
}

pub fn run(options: DpkgMaintscriptHelperOptions) -> Result<()> {
    let (helper_args, script_args) = split_args(&options.args);
    let script_name = env::var("DPKG_MAINTSCRIPT_NAME").unwrap_or_default();

    let code = match options.subcommand.as_deref() {
        Some("supports") => {
            let c = handle_supports(&helper_args);
            std::process::exit(c);
        }
        Some("rm_conffile") => do_rm_conffile(&helper_args, &script_args, &script_name)?,
        Some("mv_conffile") => do_mv_conffile(&helper_args, &script_args, &script_name)?,
        Some("symlink_to_dir") => do_symlink_to_dir(&helper_args, &script_args, &script_name)?,
        Some("dir_to_symlink") => do_dir_to_symlink(&helper_args, &script_args, &script_name)?,
        Some(_) | None => {
            eprintln!("dpkg-maintscript-helper: unsupported or missing command");
            1
        }
    };
    std::process::exit(code);
}
