use clap::{Arg, Command};
use color_eyre::Result;
use std::fs;
use std::io::{self, Read};

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
            .help("Files to concatenate (if none, read from stdin)"))
}

pub fn run(options: CatOptions) -> Result<()> {
    if options.files.is_empty() {
        // Read from stdin
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)
            .map_err(|e| color_eyre::eyre::eyre!("cat: failed to read from stdin: {}", e))?;
        print!("{}", buffer);
    } else {
        // Read from files
        for file_path in &options.files {
            let content = fs::read_to_string(file_path)
                .map_err(|e| color_eyre::eyre::eyre!("cat: {}: {}", file_path, e))?;
            print!("{}", content);
        }
    }
    Ok(())
}

