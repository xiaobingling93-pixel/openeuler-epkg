use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use nix::unistd;
use std::path::Path;
use walkdir::WalkDir;
use crate::posix::resolve_user_group_ids;

pub struct ChownOptions {
    pub owner: String,
    pub files: Vec<String>,
    pub recursive: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<ChownOptions> {
    let recursive = matches.get_flag("recursive");
    let reference = matches.get_one::<String>("reference").cloned();

    let args: Vec<String> = matches.get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    if args.is_empty() && reference.is_none() {
        return Err(eyre!("chown: missing operand"));
    }

    let (owner, files) = if let Some(ref ref_file) = reference {
        // --reference mode: get owner and group from reference file
        if args.is_empty() {
            return Err(eyre!("chown: missing operand"));
        }
        let ref_path = std::path::Path::new(ref_file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::metadata(ref_path)
                .map_err(|e| eyre!("chown: cannot stat '{}': {}", ref_file, e))?;
            let uid = metadata.uid();
            let gid = metadata.gid();
            (format!("{}:{}", uid, gid), args)
        }
        #[cfg(not(unix))]
        {
            return Err(eyre!("chown: --reference not supported on this platform"));
        }
    } else {
        let owner = args[0].clone();
        let files = args[1..].to_vec();
        if files.is_empty() {
            return Err(eyre!("chown: missing operand"));
        }
        (owner, files)
    };

    Ok(ChownOptions { owner, files, recursive })
}

pub fn command() -> Command {
    Command::new("chown")
        .about("Change file owner and group")
        .arg(Arg::new("recursive")
            .short('R')
            .long("recursive")
            .help("Operate on files and directories recursively")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("reference")
            .long("reference")
            .help("Use RFILE's owner and group rather than specifying an OWNER value")
            .value_name("RFILE"))
        .arg(Arg::new("args")
            .num_args(1..)
            .help("OWNER and files (or just files with --reference)"))
}

fn parse_owner_spec(owner_spec: &str) -> Result<(Option<&str>, Option<&str>)> {
    match owner_spec.split_once(':') {
        Some((user, group)) => {
            let user = (!user.is_empty()).then_some(user);
            let group = (!group.is_empty()).then_some(group);
            Ok((user, group))
        }
        None => Ok((Some(owner_spec), None)),
    }
}

fn change_ownership(path: &Path, uid: Option<u32>, gid: Option<u32>) -> Result<()> {
    unistd::chown(path, uid.map(unistd::Uid::from_raw), gid.map(unistd::Gid::from_raw))
        .map_err(|e| eyre!("chown: cannot change ownership of '{}': {:?}", path.display(), e))?;
    Ok(())
}

fn change_ownership_recursive(path: &Path, uid: Option<u32>, gid: Option<u32>) -> Result<()> {
    for entry in WalkDir::new(path) {
        let entry = entry.map_err(|e| eyre!("chown: error walking directory: {}", e))?;
        let entry_path = entry.path();
        change_ownership(entry_path, uid, gid)?;
    }
    Ok(())
}

pub fn run(options: ChownOptions) -> Result<()> {
    let (user, group) = parse_owner_spec(&options.owner)?;
    let (uid, gid) = resolve_user_group_ids(user, group);

    for file_path in &options.files {
        let path = Path::new(file_path);

        if options.recursive {
            if path.is_dir() {
                change_ownership_recursive(path, uid, gid)?;
            } else {
                change_ownership(path, uid, gid)?;
            }
        } else {
            // For non-recursive, we can still use the original posix_chown for backward compatibility
            // but since we already resolved the IDs, use the efficient version
            change_ownership(path, uid, gid)?;
        }
    }

    Ok(())
}