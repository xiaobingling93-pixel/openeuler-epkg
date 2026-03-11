use clap::Command;
use color_eyre::Result;

pub struct PwdOptions {
    pub logical: bool,
    pub physical: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<PwdOptions> {
    let logical = matches.get_flag("logical");
    let physical = matches.get_flag("physical");

    Ok(PwdOptions { logical, physical })
}

pub fn command() -> Command {
    Command::new("pwd")
        .about("Print the name of the current working directory")
        .arg(clap::Arg::new("logical")
            .short('L')
            .long("logical")
            .help("Use PWD from environment, even if it contains symlinks")
            .action(clap::ArgAction::SetTrue))
        .arg(clap::Arg::new("physical")
            .short('P')
            .long("physical")
            .help("Avoid all symlinks")
            .action(clap::ArgAction::SetTrue))
}

pub fn run(options: PwdOptions) -> Result<()> {
    if options.physical {
        // Get physical current directory (canonicalize)
        let current_dir = std::env::current_dir()
            .map_err(|e| color_eyre::eyre::eyre!("pwd: {}", e))?;
        let canonical = current_dir.canonicalize()
            .map_err(|e| color_eyre::eyre::eyre!("pwd: {}", e))?;
        println!("{}", canonical.display());
    } else if options.logical {
        // Use PWD from environment if available
        if let Ok(pwd) = std::env::var("PWD") {
            println!("{}", pwd);
            return Ok(());
        }
        // Fall through to physical
        let current_dir = std::env::current_dir()
            .map_err(|e| color_eyre::eyre::eyre!("pwd: {}", e))?;
        println!("{}", current_dir.display());
    } else {
        // Default: use PWD from environment if available, otherwise physical
        if let Ok(pwd) = std::env::var("PWD") {
            println!("{}", pwd);
        } else {
            let current_dir = std::env::current_dir()
                .map_err(|e| color_eyre::eyre::eyre!("pwd: {}", e))?;
            println!("{}", current_dir.display());
        }
    }
    Ok(())
}
