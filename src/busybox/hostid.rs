use clap::Command;
use color_eyre::Result;

pub struct HostidOptions;

pub fn parse_options(_matches: &clap::ArgMatches) -> Result<HostidOptions> {
    Ok(HostidOptions)
}

pub fn command() -> Command {
    Command::new("hostid")
        .about("Print the numeric identifier for the current host")
}

pub fn run(_options: HostidOptions) -> Result<()> {
    #[cfg(unix)]
    {
        unsafe {
            let hostid = libc::gethostid();
            println!("{:08x}", hostid as u32);
        }
    }
    #[cfg(not(unix))]
    {
        // On non-Unix systems, use a simple default
        println!("00000000");
    }
    Ok(())
}
