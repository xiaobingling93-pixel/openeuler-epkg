use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::path::Path;
use pathdiff;

pub struct LnOptions {
    pub target: String,
    pub link_name: String,
    pub symbolic: bool,
    pub force: bool,
    pub relative: bool,
    pub no_dereference: bool,
    pub verbose: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<LnOptions> {
    let symbolic = matches.get_flag("symbolic");
    let force = matches.get_flag("force");
    let relative = matches.get_flag("relative");
    let no_dereference = matches.get_flag("no-dereference");
    let verbose = matches.get_flag("verbose");

    let args: Vec<String> = matches.get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    if args.len() != 2 {
        return Err(eyre!("ln: incorrect number of arguments"));
    }

    let target = args[0].clone();
    let link_name = args[1].clone();

    Ok(LnOptions {
        target,
        link_name,
        symbolic,
        force,
        relative,
        no_dereference,
        verbose,
    })
}

pub fn command() -> Command {
    Command::new("ln")
        .about("Create links")
        .arg(Arg::new("symbolic")
            .short('s')
            .long("symbolic")
            .help("Create symbolic links instead of hard links")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("force")
            .short('f')
            .long("force")
            .help("Remove existing destination files")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("relative")
            .short('r')
            .long("relative")
            .help("Create symbolic links relative to link location")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("no-dereference")
            .short('n')
            .long("no-dereference")
            .help("Treat LINK_NAME as a normal file if it is a symbolic link to a directory")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("verbose")
            .short('v')
            .long("verbose")
            .help("Print name of each linked file")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("args")
            .num_args(2)
            .help("Target and link name")
            .required(true))
}

pub fn run(options: LnOptions) -> Result<()> {
    let target_path = Path::new(&options.target);
    let link_path = Path::new(&options.link_name);

    // Handle force option - remove existing destination
    if options.force && link_path.exists() {
        if link_path.is_dir() {
            fs::remove_dir_all(link_path)
                .map_err(|e| eyre!("ln: cannot remove '{}': {}", options.link_name, e))?;
        } else {
            fs::remove_file(link_path)
                .map_err(|e| eyre!("ln: cannot remove '{}': {}", options.link_name, e))?;
        }
    }

    // Handle no-dereference option - if link_name is a symlink to a directory, treat it as a file
    if options.no_dereference && link_path.is_symlink() {
        // For no-dereference, we need to remove the symlink first if it exists
        if link_path.exists() {
            fs::remove_file(link_path)
                .map_err(|e| eyre!("ln: cannot remove '{}': {}", options.link_name, e))?;
        }
    }

    let actual_target = if options.symbolic && options.relative {
        // Calculate relative path from link directory to target
        let link_dir = link_path.parent().unwrap_or(Path::new("."));
        pathdiff::diff_paths(target_path, link_dir)
            .unwrap_or_else(|| target_path.to_path_buf())
    } else {
        target_path.to_path_buf()
    };

    if options.symbolic {
        std::os::unix::fs::symlink(&actual_target, link_path)
            .map_err(|e| eyre!("ln: cannot create symbolic link '{}': {}", options.link_name, e))?;
    } else {
        fs::hard_link(&actual_target, link_path)
            .map_err(|e| eyre!("ln: cannot create hard link '{}': {}", options.link_name, e))?;
    }

    // Handle verbose option
    if options.verbose {
        println!("{} -> {}", options.link_name, actual_target.display());
    }

    Ok(())
}