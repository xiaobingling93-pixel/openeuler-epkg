use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs::File;
use std::io::{self};

pub struct CatOptions {
    pub files: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<CatOptions> {
    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(CatOptions { files })
}

pub fn command() -> Command {
    Command::new("cat")
        .about("Concatenate and print files")
        .arg(Arg::new("files")
            .num_args(0..)
            .help("Files to concatenate (if none, read from stdin; - means stdin)"))
}

pub fn run(options: CatOptions) -> Result<()> {
    let stdout = io::stdout();
    let mut stdout_handle = stdout.lock();

    if options.files.is_empty() {
        // Read from stdin
        let stdin = io::stdin();
        let mut stdin_handle = stdin.lock();
        io::copy(&mut stdin_handle, &mut stdout_handle)
            .map_err(|e| eyre!("cat: failed to read from stdin: {}", e))?;
    } else {
        // Read from files (- means stdin)
        for file_path in &options.files {
            let mut input: Box<dyn io::Read> = if file_path == "-" {
                Box::new(io::stdin())
            } else {
                let file = File::open(file_path)
                    .map_err(|e| eyre!("cat: {}: {}", file_path, e))?;
                Box::new(file)
            };
            io::copy(&mut input, &mut stdout_handle)
                .map_err(|e| eyre!("cat: failed to write {}: {}", file_path, e))?;
        }
    }
    Ok(())
}

