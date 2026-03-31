use clap::Command;
use color_eyre::Result;

pub struct WhoamiOptions;

pub fn parse_options(_matches: &clap::ArgMatches) -> Result<WhoamiOptions> {
    Ok(WhoamiOptions)
}

pub fn command() -> Command {
    Command::new("whoami")
        .about("Print the effective userid")
}

pub fn run(_options: WhoamiOptions) -> Result<()> {
    #[cfg(unix)]
    {
        use users::get_current_uid;
        use crate::busybox::get_uid_name;

        let uid = get_current_uid();
        println!("{}", get_uid_name(uid));
    }
    #[cfg(not(unix))]
    {
        use std::env;
        let user = env::var("USER")
            .or_else(|_| env::var("USERNAME"))
            .map_err(|_| color_eyre::eyre::eyre!("Cannot determine user"))?;
        println!("{}", user);
    }
    Ok(())
}
