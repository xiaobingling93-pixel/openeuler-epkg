use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::path::Path;

pub struct InsmodOptions {
    pub filename: String,
    pub params: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<InsmodOptions> {
    let filename = matches
        .get_one::<String>("filename")
        .ok_or_else(|| eyre!("Missing filename argument"))?
        .to_string();

    let params: Vec<String> = matches
        .get_many::<String>("params")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(InsmodOptions { filename, params })
}

pub fn command() -> Command {
    Command::new("insmod")
        .about("Load kernel module")
        .arg(
            Arg::new("filename")
                .required(true)
                .help("Module file to load (.ko or compressed format)"),
        )
        .arg(
            Arg::new("params")
                .num_args(0..)
                .help("Module parameters as SYMBOL=VALUE"),
        )
}

pub fn run(options: InsmodOptions) -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = options;
        return Err(eyre!("insmod is only supported on Linux"));
    }

    #[cfg(target_os = "linux")]
    {
        let path = Path::new(&options.filename);
        crate::busybox::modprobe::load_module(path, &options.params)
    }
}