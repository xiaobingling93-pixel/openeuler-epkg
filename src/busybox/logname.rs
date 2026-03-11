use clap::Command;
use color_eyre::Result;

pub struct LognameOptions;

pub fn parse_options(_matches: &clap::ArgMatches) -> Result<LognameOptions> {
    Ok(LognameOptions)
}

pub fn command() -> Command {
    Command::new("logname")
        .about("Print the user's login name")
}

pub fn run(_options: LognameOptions) -> Result<()> {
    #[cfg(unix)]
    {
        use users::get_user_by_uid;
        use users::get_current_uid;

        let uid = get_current_uid();
        let user = get_user_by_uid(uid)
            .ok_or_else(|| color_eyre::eyre::eyre!("logname: cannot determine login name"))?;
        println!("{}", user.name().to_string_lossy());
    }
    #[cfg(not(unix))]
    {
        use std::env;
        let logname = env::var("LOGNAME")
            .or_else(|_| env::var("USER"))
            .or_else(|_| env::var("USERNAME"))
            .map_err(|_| color_eyre::eyre::eyre!("logname: cannot determine login name"))?;
        println!("{}", logname);
    }
    Ok(())
}
