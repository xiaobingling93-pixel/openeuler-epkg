use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::path::{Component, Path, PathBuf};
use std::fs;

fn format_io_error(e: &std::io::Error) -> String {
    let msg = e.to_string();
    // Remove trailing "(os error XX)" suffix if present
    if let Some(pos) = msg.rfind(" (os error ") {
        msg[..pos].to_string()
    } else {
        msg
    }
}

fn strip_trailing_slashes(s: &str) -> &str {
    let mut end = s.len();
    while end > 1 && s.as_bytes()[end-1] == b'/' {
        end -= 1;
    }
    &s[..end]
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => continue,
            Component::ParentDir => {
                match components.last() {
                    Some(Component::Normal(_)) => {
                        components.pop();
                    }
                    Some(Component::RootDir) => {
                        // /.. -> /
                        continue;
                    }
                    _ => components.push(component),
                }
            }
            _ => components.push(component),
        }
    }
    if components.is_empty() {
        components.push(Component::CurDir);
    }
    components.iter().collect()
}

fn resolve_symlinks(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    let mut iterations = 0;
    while let Ok(target) = fs::read_link(&current) {
        if iterations > 20 {
            break;
        }
        if target.is_absolute() {
            current = target;
        } else {
            let parent = current.parent().unwrap_or_else(|| Path::new("."));
            current = parent.join(target);
        }
        iterations += 1;
    }
    current
}

pub struct RealpathOptions {
    pub files: Vec<String>,
    pub canonicalize: bool,
    pub quiet: bool,
    pub root: Option<String>,
    pub admindir: Option<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<RealpathOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let canonicalize = matches.get_flag("canonicalize");
    let quiet = matches.get_flag("quiet");

    if files.is_empty() {
        return Err(eyre!("realpath: missing operand"));
    }

    Ok(RealpathOptions {
        files,
        canonicalize,
        quiet,
        root: None,
        admindir: None,
    })
}

pub fn command() -> Command {
    Command::new("realpath")
        .about("Print the resolved absolute file name")
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
        .arg(Arg::new("files")
            .num_args(1..)
            .help("Files to resolve"))
}

fn resolve_canonicalize(path: &Path, file: &str, quiet: bool) -> PathBuf {
    match path.canonicalize() {
        Ok(canonical) => canonical,
        Err(e) => {
            if !quiet {
                eprintln!("realpath: {}: {}", file, format_io_error(&e));
            }
            std::process::exit(1);
        }
    }
}

fn resolve_non_canonicalize(path: &Path, file: &str, quiet: bool) -> PathBuf {
    // First, resolve symlinks
    let resolved = resolve_symlinks(path);
    // Make path absolute
    let abs_path = if resolved.is_absolute() {
        resolved
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(resolved),
            Err(e) => {
                if !quiet {
                    eprintln!("realpath: {}: {}", file, format_io_error(&e));
                }
                std::process::exit(1);
            }
        }
    };
    // Normalize . and .. components
    let normalized = normalize_path(&abs_path);
    // Split into parent and last component
    let parent = normalized.parent();
    let last = normalized.file_name();
    match (parent, last) {
        (Some(parent), Some(last)) => {
            // Canonicalize parent (resolve symlinks, ensure it exists)
            match fs::canonicalize(parent) {
                Ok(canonical_parent) => canonical_parent.join(last),
                Err(e) => {
                    if !quiet {
                        eprintln!("realpath: {}: {}", file, format_io_error(&e));
                    }
                    std::process::exit(1);
                }
            }
        }
        (Some(parent), None) => {
            // Path ends with slash (should have been stripped)
            // Treat as directory, canonicalize it
            match fs::canonicalize(parent) {
                Ok(canonical_parent) => canonical_parent,
                Err(e) => {
                    if !quiet {
                        eprintln!("realpath: {}: {}", file, format_io_error(&e));
                    }
                    std::process::exit(1);
                }
            }
        }
        (None, _) => {
            // Root directory
            PathBuf::from("/")
        }
    }
}

pub fn run(options: RealpathOptions) -> Result<()> {
    for file in &options.files {
        // Strip trailing slashes (except root)
        let file = strip_trailing_slashes(file);
        let path = Path::new(file);

        let real_path = if options.canonicalize {
            resolve_canonicalize(path, file, options.quiet)
        } else {
            resolve_non_canonicalize(path, file, options.quiet)
        };
        println!("{}", real_path.display());
    }
    Ok(())
}
