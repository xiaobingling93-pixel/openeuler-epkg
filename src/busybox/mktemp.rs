use clap::{value_parser, Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::env;
use std::fs::OpenOptions;
#[cfg(not(unix))]
use std::io::ErrorKind;
use std::path::Path;
#[cfg(unix)]
use crate::posix::posix_mkstemp;

const MIN_X_COUNT: usize = 3;
const MKSTEMP_X_COUNT: usize = 6;
const DEFAULT_TEMPLATE: &str = "tmp.XXXXXXXXXX";

pub struct MktempOptions {
    pub templates: Vec<String>,
    pub directory: bool,
    pub dry_run: bool,
    pub quiet: bool,
    pub tmpdir_requested: bool,
    pub tmpdir_value: Option<String>,
    pub suffix: Option<String>,
    pub t_flag: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<MktempOptions> {
    let templates: Vec<String> = matches.get_many::<String>("template")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let directory = matches.get_flag("directory");
    let dry_run = matches.get_flag("dry-run");
    let quiet = matches.get_flag("quiet");
    let tmpdir_requested = matches.contains_id("tmpdir");
    let tmpdir_value = matches.get_one::<String>("tmpdir").cloned();
    let suffix = matches.get_one::<String>("suffix").cloned();
    let t_flag = matches.get_flag("t");

    Ok(MktempOptions {
        templates,
        directory,
        dry_run,
        quiet,
        tmpdir_requested,
        tmpdir_value,
        suffix,
        t_flag,
    })
}

pub fn command() -> Command {
    Command::new("mktemp")
        .about("Create a temporary file or directory, safely, and print its name.")
        .arg(Arg::new("directory")
            .short('d')
            .long("directory")
            .help("create a directory, not a file")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("dry-run")
            .short('u')
            .long("dry-run")
            .help("do not create anything; merely print a name (unsafe)")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("quiet")
            .short('q')
            .long("quiet")
            .help("suppress diagnostics about file/dir-creation failure")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("suffix")
            .long("suffix")
            .help("append SUFF to TEMPLATE; SUFF must not contain a slash")
            .value_name("SUFF"))
        .arg(Arg::new("tmpdir")
            .short('p')
            .long("tmpdir")
            .help("interpret TEMPLATE relative to DIR; if DIR not specified, use $TMPDIR/$TMP/$TEMP or system temp")
            .value_name("DIR")
            .num_args(0..=1))
        .arg(Arg::new("t")
            .short('t')
            .help("interpret TEMPLATE as single file name component relative to tmpdir [deprecated]")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("template")
            .num_args(0..)
            .value_parser(value_parser!(String))
            .help("TEMPLATE must contain at least 3 consecutive 'X's in last component; default tmp.XXXXXXXXXX with --tmpdir implied"))
}

fn fallback_tmpdir() -> String {
    #[cfg(unix)]
    {
        "/tmp".to_string()
    }
    #[cfg(windows)]
    {
        env::temp_dir().to_string_lossy().into_owned()
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        env::temp_dir().to_string_lossy().into_owned()
    }
}

fn env_tmpdir_chain() -> String {
    env::var("TMPDIR")
        .or_else(|_| env::var("TMP"))
        .or_else(|_| env::var("TEMP"))
        .unwrap_or_else(|_| fallback_tmpdir())
}

fn get_tmpdir(options: &MktempOptions) -> String {
    if options.tmpdir_requested {
        options
            .tmpdir_value
            .clone()
            .unwrap_or_else(env_tmpdir_chain)
    } else {
        env_tmpdir_chain()
    }
}

fn default_template(options: &MktempOptions) -> String {
    let tmpdir = get_tmpdir(options);
    Path::new(&tmpdir).join(DEFAULT_TEMPLATE).to_string_lossy().into_owned()
}

/// Last path component (handles `/` and `\\`).
fn last_component(template: &str) -> &str {
    let trimmed = template.trim_end_matches(&['/', '\\'][..]);
    Path::new(trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(trimmed)
}

/// Count consecutive 'X's at the end of the string.
fn count_trailing_x(s: &str) -> usize {
    s.chars().rev().take_while(|&c| c == 'X').count()
}

/// Require at least MIN_X_COUNT consecutive X's in the last component.
fn check_template(template: &str) -> Result<()> {
    let last = last_component(template);
    let n = count_trailing_x(last);
    if n < MIN_X_COUNT {
        return Err(eyre!(
            "mktemp: template must contain at least {} consecutive 'X's in last component: {}",
            MIN_X_COUNT,
            template
        ));
    }
    Ok(())
}

/// Pad template so it ends with at least MKSTEMP_X_COUNT X's (for mkstemp/mkdtemp).
fn template_for_mkstemp(template: &str) -> String {
    let n = count_trailing_x(template);
    if n >= MKSTEMP_X_COUNT {
        template.to_string()
    } else {
        let extra = MKSTEMP_X_COUNT - n;
        format!("{}{}", template, "X".repeat(extra))
    }
}

fn replace_trailing_x_with_random(template: &str, x_count: usize) -> Result<String> {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let (base, _) = template.split_at(template.len() - x_count);

    let mut bytes = vec![0u8; x_count];
    getrandom::fill(&mut bytes)
        .map_err(|e| eyre!("mktemp: getrandom failed: {}", e))?;
    let suffix: String = bytes
        .iter()
        .map(|&b| CHARS[(b as usize) % 62] as char)
        .collect();
    Ok(format!("{}{}", base, suffix))
}

fn create_temp_file(
    template: &str,
    dry_run: bool,
    quiet: bool,
    append_suffix: Option<&str>,
) -> Result<String> {
    let x_count = count_trailing_x(template);

    if dry_run {
        let path = replace_trailing_x_with_random(template, x_count)?;
        return Ok(match append_suffix {
            Some(s) => format!("{}{}", path, s),
            None => path,
        });
    }

    if let Some(suffix) = append_suffix {
        if suffix.contains('/') {
            return Err(eyre!("mktemp: suffix must not contain a slash"));
        }
        let path = replace_trailing_x_with_random(template, x_count)?;
        let path_with_suffix = format!("{}{}", path, suffix);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path_with_suffix)
            .map_err(|e| {
                if quiet {
                    eyre!("")
                } else {
                    eyre!("mktemp: failed to create file '{}': {}", path_with_suffix, e)
                }
            })?;
        return Ok(path_with_suffix);
    }

    let tpl = template_for_mkstemp(template);
    #[cfg(unix)]
    {
        let (path, file) = posix_mkstemp(&tpl).map_err(|e| {
            if quiet {
                eyre!("")
            } else {
                eyre!("mktemp: failed to create file via template '{}': {:?}", template, e)
            }
        })?;
        drop(file);
        return Ok(path);
    }
    #[cfg(not(unix))]
    {
        let x_ct = count_trailing_x(&tpl);
        for _ in 0..128 {
            let path = replace_trailing_x_with_random(&tpl, x_ct)?;
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    drop(file);
                    return Ok(path);
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                Err(e) if quiet => return Err(eyre!("")),
                Err(e) => {
                    return Err(eyre!(
                        "mktemp: failed to create file '{}': {}",
                        path,
                        e
                    ));
                }
            }
        }
        return Err(if quiet {
            eyre!("")
        } else {
            eyre!(
                "mktemp: failed to create unique file for template '{}'",
                template
            )
        });
    }
}

fn create_temp_dir(
    template: &str,
    dry_run: bool,
    quiet: bool,
    append_suffix: Option<&str>,
) -> Result<String> {
    let x_count = count_trailing_x(template);

    if dry_run {
        let path = replace_trailing_x_with_random(template, x_count)?;
        return Ok(match append_suffix {
            Some(s) => format!("{}{}", path, s),
            None => path,
        });
    }

    if let Some(suffix) = append_suffix {
        if suffix.contains('/') {
            return Err(eyre!("mktemp: suffix must not contain a slash"));
        }
        let path = replace_trailing_x_with_random(template, count_trailing_x(template))?;
        let path_with_suffix = format!("{}{}", path, suffix);
        std::fs::create_dir(&path_with_suffix).map_err(|e| {
            if quiet {
                eyre!("")
            } else {
                eyre!("mktemp: failed to create directory '{}': {}", path_with_suffix, e)
            }
        })?;
        return Ok(path_with_suffix);
    }

    #[cfg(unix)]
    {
        let tpl = template_for_mkstemp(template);
        let mut template_vec = tpl.as_bytes().to_vec();
        template_vec.push(0);
        let result = unsafe { libc::mkdtemp(template_vec.as_mut_ptr() as *mut libc::c_char) };
        if result.is_null() {
            return Err(if quiet {
                eyre!("")
            } else {
                eyre!(
                    "mktemp: failed to create directory via template '{}': {}",
                    template,
                    std::io::Error::last_os_error()
                )
            });
        }
        template_vec.pop();
        let path_str = String::from_utf8(template_vec)
            .map_err(|_| eyre!("mktemp: invalid UTF-8 in path"))?;
        Ok(path_str)
    }
    #[cfg(not(unix))]
    {
        let tpl = template_for_mkstemp(template);
        let x_ct = count_trailing_x(&tpl);
        for _ in 0..128 {
            let path = replace_trailing_x_with_random(&tpl, x_ct)?;
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(path),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                Err(e) if quiet => return Err(eyre!("")),
                Err(e) => {
                    return Err(eyre!(
                        "mktemp: failed to create directory '{}': {}",
                        path,
                        e
                    ));
                }
            }
        }
        Err(if quiet {
            eyre!("")
        } else {
            eyre!(
                "mktemp: failed to create unique directory for template '{}'",
                template
            )
        })
    }
}

pub fn run(options: MktempOptions) -> Result<()> {
    let tmpdir = get_tmpdir(&options);
    let use_tmpdir_implied = options.templates.is_empty();

    let templates: Vec<String> = if options.templates.is_empty() {
        vec![default_template(&options)]
    } else {
        options.templates
    };

    if options.suffix.as_ref().map_or(false, |s| s.contains('/')) {
        return Err(eyre!("mktemp: suffix must not contain a slash"));
    }

    for template in &templates {
        if options.t_flag && template.contains('/') {
            return Err(eyre!("mktemp: with -t, TEMPLATE must not contain a slash: {}", template));
        }

        if options.tmpdir_requested && Path::new(template).is_absolute() {
            return Err(eyre!("mktemp: with --tmpdir, TEMPLATE must not be an absolute name: {}", template));
        }

        let effective_template = if options.t_flag || (use_tmpdir_implied && options.tmpdir_requested) {
            let base = if options.t_flag { template.as_str() } else { DEFAULT_TEMPLATE };
            Path::new(&tmpdir).join(base).to_string_lossy().into_owned()
        } else if options.tmpdir_requested && !template.is_empty() {
            Path::new(&tmpdir).join(template).to_string_lossy().into_owned()
        } else {
            template.clone()
        };

        check_template(&effective_template)?;

        if options.suffix.is_some() && !effective_template.ends_with('X') {
            return Err(eyre!("mktemp: with --suffix, TEMPLATE must end in 'X': {}", effective_template));
        }

        let suffix_ref = options.suffix.as_deref();

        let path = if options.directory {
            create_temp_dir(&effective_template, options.dry_run, options.quiet, suffix_ref)?
        } else {
            create_temp_file(&effective_template, options.dry_run, options.quiet, suffix_ref)?
        };
        println!("{}", path);
    }
    Ok(())
}
