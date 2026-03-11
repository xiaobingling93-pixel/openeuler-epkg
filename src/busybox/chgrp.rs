use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::path::Path;
use crate::applets::extract_reference_metadata;

pub struct ChgrpOptions {
    pub group: String,
    pub files: Vec<String>,
    pub recursive: bool,
    pub verbose: bool,
    pub changes: bool,
    pub silent: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<ChgrpOptions> {
    let recursive = matches.get_flag("recursive");
    let verbose = matches.get_flag("verbose");
    let changes = matches.get_flag("changes");
    let silent = matches.get_flag("silent") || matches.get_flag("quiet");
    let reference = matches.get_one::<String>("reference").cloned();

    let args: Vec<String> = matches.get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    if args.is_empty() && reference.is_none() {
        return Err(eyre!("chgrp: missing operand"));
    }

    let (group, files) = if let Some(ref ref_file) = reference {
        // --reference mode: get group from reference file
        if args.is_empty() {
            return Err(eyre!("chgrp: missing operand"));
        }
        let ref_path = Path::new(ref_file);
        let (_, gid, _) = extract_reference_metadata(ref_path)
            .map_err(|e| eyre!("chgrp: {}", e))?;
        (gid.to_string(), args)
    } else {
        let group = args[0].clone();
        let files = args[1..].to_vec();
        if files.is_empty() {
            return Err(eyre!("chgrp: missing operand"));
        }
        (group, files)
    };

    Ok(ChgrpOptions {
        group,
        files,
        recursive,
        verbose,
        changes,
        silent,
    })
}

pub fn command() -> Command {
    Command::new("chgrp")
        .about("Change group ownership")
        .arg(Arg::new("recursive")
            .short('R')
            .long("recursive")
            .help("Change files and directories recursively")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("verbose")
            .short('v')
            .long("verbose")
            .help("Output a diagnostic for every file processed")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("changes")
            .short('c')
            .long("changes")
            .help("Like verbose but report only when a change is made")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("silent")
            .short('f')
            .long("silent")
            .help("Suppress most error messages")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("quiet")
            .long("quiet")
            .help("Suppress most error messages")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("reference")
            .long("reference")
            .help("Use RFILE's group rather than specifying a GROUP value")
            .value_name("RFILE"))
        .arg(Arg::new("args")
            .num_args(1..)
            .help("GROUP and files (or just files with --reference)"))
}

#[cfg(unix)]
fn resolve_group(group: &str) -> Result<nix::unistd::Gid> {
    use nix::unistd::Gid;
    use users::get_group_by_name;

    if let Ok(gid_num) = group.parse::<u32>() {
        Ok(Gid::from_raw(gid_num))
    } else {
        let group = get_group_by_name(group)
            .ok_or_else(|| eyre!("chgrp: invalid group '{}'", group))?;
        Ok(Gid::from_raw(group.gid()))
    }
}

#[cfg(unix)]
fn change_group(path: &Path, gid: nix::unistd::Gid, verbose: bool, changes: bool, silent: bool) -> Result<bool> {
    // Get current group to check if change is needed
    let current_gid = std::fs::metadata(path)
        .map(|m| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                m.gid()
            }
            #[cfg(not(unix))]
            {
                0
            }
        })
        .unwrap_or(0);

    let needs_change = current_gid != gid.as_raw();

    if needs_change {
        nix::unistd::chown(path, None, Some(gid))
            .map_err(|e| {
                if !silent {
                    eyre!("chgrp: cannot change group of '{}': {}", path.display(), e)
                } else {
                    eyre!("")
                }
            })?;

        if verbose || (changes && needs_change) {
            println!("changed group of '{}' to {}", path.display(), gid);
        }
    } else if verbose && !changes {
        // Only print in verbose mode if not using --changes
        println!("group of '{}' retained as {}", path.display(), gid);
    }

    Ok(needs_change)
}

#[cfg(unix)]
fn process_files(options: &ChgrpOptions, gid: nix::unistd::Gid) -> Result<()> {
    use walkdir::WalkDir;

    for file in &options.files {
        let path = Path::new(file);
        if options.recursive && path.is_dir() {
            for entry in WalkDir::new(path).into_iter() {
                let entry = entry.map_err(|e| {
                    if !options.silent {
                        eyre!("chgrp: {}: {}", file, e)
                    } else {
                        eyre!("")
                    }
                })?;
                let _ = change_group(entry.path(), gid, options.verbose, options.changes, options.silent);
            }
        } else {
            let _ = change_group(path, gid, options.verbose, options.changes, options.silent);
        }
    }
    Ok(())
}

pub fn run(options: ChgrpOptions) -> Result<()> {
    #[cfg(unix)]
    {
        let gid = resolve_group(&options.group)?;
        process_files(&options, gid)?;
    }
    #[cfg(not(unix))]
    {
        return Err(eyre!("chgrp: not supported on this platform"));
    }
    Ok(())
}
