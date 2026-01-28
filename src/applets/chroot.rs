use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::path::Path;

pub struct ChrootOptions {
    pub newroot: String,
    pub command: Vec<String>,
    pub skip_chdir: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<ChrootOptions> {
    let newroot = matches.get_one::<String>("newroot")
        .ok_or_else(|| eyre!("chroot: missing operand"))?
        .clone();

    let command: Vec<String> = matches.get_many::<String>("command")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let skip_chdir = matches.get_flag("skip-chdir");

    Ok(ChrootOptions { newroot, command, skip_chdir })
}

pub fn command() -> Command {
    Command::new("chroot")
        .about("Run command or interactive shell with special root directory")
        .arg(Arg::new("skip-chdir")
            .long("skip-chdir")
            .help("Do not change working directory to '/'")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("newroot")
            .required(true)
            .help("New root directory"))
        .arg(Arg::new("command")
            .num_args(0..)
            .help("Command to run (default: $SHELL -i)"))
}

pub fn run(options: ChrootOptions) -> Result<()> {
    #[cfg(unix)]
    {
        use nix::unistd::{chroot, execvp};
        use std::ffi::CString;

        let newroot_path = Path::new(&options.newroot);
        chroot(newroot_path)
            .map_err(|e| eyre!("chroot: cannot change root directory to '{}': {}", options.newroot, e))?;

        // Change to root directory
        if !options.skip_chdir {
            std::env::set_current_dir("/")
                .map_err(|e| eyre!("chroot: cannot change directory to '/': {}", e))?;
        }

        if options.command.is_empty() {
            // Default to /bin/sh -i if no command specified
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
            let default_cmd = vec![shell.clone(), "-i".to_string()];
            let cmd_args: Vec<CString> = default_cmd.iter()
                .map(|s| CString::new(s.as_str()).unwrap())
                .collect();
            execvp(&cmd_args[0], &cmd_args)
                .map_err(|e| eyre!("chroot: cannot execute shell: {}", e))?;
        } else {
            let cmd_args: Vec<CString> = options.command.iter()
                .map(|s| CString::new(s.as_str()).unwrap())
                .collect();
            execvp(&cmd_args[0], &cmd_args)
                .map_err(|e| eyre!("chroot: cannot execute command: {}", e))?;
        }
    }
    #[cfg(not(unix))]
    {
        return Err(eyre!("chroot: not supported on this platform"));
    }
    Ok(())
}
