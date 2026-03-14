use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;

pub struct MkfifoOptions {
    pub files: Vec<String>,
    pub mode: Option<u32>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<MkfifoOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let mode = matches.get_one::<String>("mode")
        .and_then(|s| u32::from_str_radix(s, 8).ok());

    if files.is_empty() {
        return Err(eyre!("mkfifo: missing operand"));
    }

    Ok(MkfifoOptions { files, mode })
}

pub fn command() -> Command {
    Command::new("mkfifo")
        .about("Create FIFO special files")
        .arg(Arg::new("mode")
            .short('m')
            .long("mode")
            .help("Set file permission bits to MODE (octal)")
            .value_name("MODE"))
        .arg(Arg::new("files")
            .num_args(1..)
            .required(true)
            .help("FIFO files to create"))
}

pub fn run(options: MkfifoOptions) -> Result<()> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        let default_mode = 0o666;

        for file in &options.files {
            let path_cstr = CString::new(file.as_str())
                .map_err(|e| eyre!("mkfifo: invalid path '{}': {}", file, e))?;
            let mode = options.mode.unwrap_or(default_mode) as libc::mode_t;

            let result = unsafe { libc::mkfifo(path_cstr.as_ptr(), mode) };
            if result != 0 {
                return Err(eyre!("mkfifo: cannot create fifo '{}': {}", file, std::io::Error::last_os_error()));
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = options;
        Err(eyre!("mkfifo: not supported on this platform"))
    }
}
