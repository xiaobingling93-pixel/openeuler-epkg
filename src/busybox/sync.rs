use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use libc;
use std::fs;
use std::os::unix::io::AsRawFd;
use std::path::Path;

pub struct SyncOptions {
    pub files: Vec<String>,
    pub data_only: bool,
    pub file_system: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<SyncOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();
    let data_only = matches.get_flag("data");
    let file_system = matches.get_flag("file-system");

    Ok(SyncOptions {
        files,
        data_only,
        file_system,
    })
}

pub fn command() -> Command {
    Command::new("sync")
        .about("Synchronize cached writes to persistent storage")
        .long_about(
            "If one or more files are specified, sync only them,\n\
             or their containing file systems."
        )
        .arg(Arg::new("data")
            .short('d')
            .long("data")
            .help("sync only file data, no unneeded metadata")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("file-system")
            .short('f')
            .long("file-system")
            .help("Sync filesystems containing the files (Linux syncfs(2) only; unsupported on macOS and others)")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files or directories to sync"))
}

pub fn run(options: SyncOptions) -> Result<()> {
    // If no files specified, sync the entire system
    if options.files.is_empty() {
        unsafe {
            libc::sync();
        }
        return Ok(());
    }

    #[cfg(not(target_os = "linux"))]
    if options.file_system {
        return Err(eyre!(
            "sync: --file-system is not supported on this platform (requires Linux syncfs(2)); \
             omit -f, or use plain `sync` with no file operands"
        ));
    }

    // Process each file
    for file_path in &options.files {
        let path = Path::new(file_path);

        // Open file/directory for syncing (read-only is sufficient)
        let file = fs::OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(|e| eyre!("sync: cannot open '{}': {}", file_path, e))?;

        let fd = file.as_raw_fd();

        if options.file_system {
            #[cfg(target_os = "linux")]
            {
                let result = unsafe { libc::syncfs(fd) };
                if result != 0 {
                    let err = std::io::Error::last_os_error();
                    return Err(eyre!("sync: failed to sync file system for '{}': {}", file_path, err));
                }
            }
        } else if options.data_only {
            // Sync file data only (no metadata on Linux). macOS libc has no fdatasync; fsync is stronger.
            #[cfg(target_os = "linux")]
            let result = unsafe { libc::fdatasync(fd) };
            #[cfg(not(target_os = "linux"))]
            let result = unsafe { libc::fsync(fd) };
            if result != 0 {
                let err = std::io::Error::last_os_error();
                return Err(eyre!("sync: failed to sync data for '{}': {}", file_path, err));
            }
        } else {
            // Sync file data and metadata
            let result = unsafe { libc::fsync(fd) };
            if result != 0 {
                let err = std::io::Error::last_os_error();
                return Err(eyre!("sync: failed to sync '{}': {}", file_path, err));
            }
        }
    }

    Ok(())
}