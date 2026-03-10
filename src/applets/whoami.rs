use clap::Command;
use color_eyre::Result;
#[cfg(unix)]
use users::get_current_username;

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

        let username = match get_current_username() {
            Some(name) => name.to_string_lossy().to_string(),
            None => {
                if get_current_uid() == 0 {
                    "root".to_string()
                } else {
                    return Err(color_eyre::eyre::eyre!("Cannot determine user"));
                }
            }
        };
        println!("{}", username);
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
