use clap::Command;
use color_eyre::Result;
#[cfg(unix)]
use std::ffi::CStr;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
use std::io;

pub struct TtyOptions {
    pub silent: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<TtyOptions> {
    let silent = matches.get_flag("silent");

    Ok(TtyOptions { silent })
}

pub fn command() -> Command {
    Command::new("tty")
        .about("Print the file name of the terminal connected to standard input")
        .arg(clap::Arg::new("silent")
            .short('s')
            .long("silent")
            .action(clap::ArgAction::SetTrue)
            .help("Print nothing, only return an exit status"))
}

pub fn run(options: TtyOptions) -> Result<()> {
    let tty_path = std::fs::read_link("/proc/self/fd/0")
        .or_else(|_| std::fs::read_link("/dev/stdin"))
        .or_else(|_| {
            // Fallback: try to get ttyname from libc
            #[cfg(unix)]
            {
                let stdin_fd = io::stdin().as_raw_fd();
                unsafe {
                    let tty_name = libc::ttyname(stdin_fd);
                    if !tty_name.is_null() {
                        let c_str = CStr::from_ptr(tty_name);
                        c_str.to_str()
                            .map(|s| std::path::PathBuf::from(s))
                            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid UTF-8"))
                    } else {
                        Err(std::io::Error::last_os_error())
                    }
                }
            }
            #[cfg(not(unix))]
            {
                Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Not supported on this platform"))
            }
        });

    match tty_path {
        Ok(path) => {
            if !options.silent {
                println!("{}", path.display());
            }
            Ok(())
        }
        Err(_) => {
            if !options.silent {
                println!("not a tty");
            }
            std::process::exit(1)
        }
    }
}
