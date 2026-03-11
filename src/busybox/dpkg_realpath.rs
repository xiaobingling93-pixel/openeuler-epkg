use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::env;
use std::path::Path;

// Import the realpath module to reuse its functionality
use crate::busybox::realpath;
use crate::busybox::realpath::RealpathOptions;

pub fn parse_options(matches: &clap::ArgMatches) -> Result<RealpathOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let canonicalize = matches.get_flag("canonicalize");
    let quiet = matches.get_flag("quiet");
    let mut root = matches.get_one::<String>("root").cloned();
    let admindir = matches.get_one::<String>("admindir").cloned();

    // If --root not specified, check DPKG_ROOT environment variable
    if root.is_none() {
        if let Ok(env_root) = env::var("DPKG_ROOT") {
            if !env_root.is_empty() {
                root = Some(env_root);
            }
        }
    }

    if files.is_empty() {
        return Err(eyre!("dpkg-realpath: missing operand"));
    }

    Ok(RealpathOptions {
        files,
        canonicalize,
        quiet,
        root,
        admindir,
    })
}

pub fn command() -> Command {
    Command::new("dpkg-realpath")
        .about("Debian package realpath utility")
        .arg(Arg::new("canonicalize")
            .short('e')
            .long("canonicalize-existing")
            .help("All components of the path must exist")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("quiet")
            .short('q')
            .long("quiet")
            .help("Suppress most error messages")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("root")
            .long("root")
            .value_name("DIRECTORY")
            .help("Set root directory"))
        .arg(Arg::new("admindir")
            .long("admindir")
            .value_name("DIRECTORY")
            .help("Use DIRECTORY instead of default dpkg database (ignored for realpath)"))
        .arg(Arg::new("files")
            .num_args(1..)
            .help("Files to resolve"))
}

/// Adjust file paths based on --root option
/// If --root is specified, absolute paths are made relative to root,
/// and relative paths are interpreted relative to root.
fn adjust_path_for_root(file: &str, root: Option<&Path>) -> String {
    if let Some(root_dir) = root {
        let path = Path::new(file);
        if path.is_absolute() {
            // For absolute paths, strip leading slash and join to root
            let relative = path.strip_prefix("/").unwrap_or(path);
            root_dir.join(relative).to_string_lossy().to_string()
        } else {
            // For relative paths, join to root
            root_dir.join(path).to_string_lossy().to_string()
        }
    } else {
        file.to_string()
    }
}

pub fn run(options: RealpathOptions) -> Result<()> {
    // Adjust file paths with --root if specified
    let root_path = options.root.as_deref().map(Path::new);

    let adjusted_files: Vec<String> = options.files.iter()
        .map(|file| adjust_path_for_root(file, root_path))
        .collect();

    // Create new options with adjusted files (root and admindir are ignored by realpath::run)
    let adjusted_options = RealpathOptions {
        files: adjusted_files,
        canonicalize: options.canonicalize,
        quiet: options.quiet,
        root: options.root,
        admindir: options.admindir,
    };

    // Call realpath's run function
    realpath::run(adjusted_options)
}