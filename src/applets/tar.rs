use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs::File;
use std::path::Path;

pub struct TarOptions {
    pub create: bool,
    pub extract: bool,
    pub file: String,
    pub directory: Option<String>,
    pub files: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<TarOptions> {
    let create = matches.get_flag("create");
    let extract = matches.get_flag("extract");

    if create && extract {
        return Err(eyre!("tar: cannot specify both -c and -x"));
    }

    if !create && !extract {
        return Err(eyre!("tar: must specify either -c or -x"));
    }

    let file = matches.get_one::<String>("file")
        .ok_or_else(|| eyre!("tar: missing archive file"))?
        .clone();

    let directory = matches.get_one::<String>("directory").cloned();

    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(TarOptions {
        create,
        extract,
        file,
        directory,
        files,
    })
}

pub fn command() -> Command {
    Command::new("tar")
        .about("Archive files")
        .arg(Arg::new("create")
            .short('c')
            .long("create")
            .help("Create a new archive")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("extract")
            .short('x')
            .long("extract")
            .help("Extract files from an archive")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("directory")
            .short('C')
            .long("directory")
            .help("Change to directory before performing operation")
            .value_name("DIR"))
        .arg(Arg::new("file")
            .short('f')
            .long("file")
            .help("Archive file")
            .value_name("ARCHIVE")
            .required(true))
        .arg(Arg::new("files")
            .help("Files to archive or extract")
            .num_args(0..))
}

fn create_archive(archive_path: &str, files: &[String]) -> Result<()> {
    let archive_file = File::create(archive_path)
        .map_err(|e| eyre!("tar: cannot create '{}': {}", archive_path, e))?;

    let mut builder = tar::Builder::new(archive_file);

    for file_path in files {
        let path = Path::new(file_path);
        if path.is_dir() {
            builder.append_dir_all(path.file_name().unwrap_or(path.as_os_str()), path)
                .map_err(|e| eyre!("tar: error adding directory '{}': {}", file_path, e))?;
        } else {
            builder.append_path(path)
                .map_err(|e| eyre!("tar: error adding file '{}': {}", file_path, e))?;
        }
    }

    builder.finish()
        .map_err(|e| eyre!("tar: error finishing archive: {}", e))?;

    Ok(())
}

fn extract_archive(archive_path: &str, directory: Option<&str>) -> Result<()> {
    let archive_file = File::open(archive_path)
        .map_err(|e| eyre!("tar: cannot open '{}': {}", archive_path, e))?;

    let mut archive = tar::Archive::new(archive_file);

    let extract_path = directory.unwrap_or(".");

    archive.unpack(extract_path)
        .map_err(|e| eyre!("tar: error extracting archive: {}", e))?;

    Ok(())
}

pub fn run(options: TarOptions) -> Result<()> {
    if options.create {
        if options.files.is_empty() {
            return Err(eyre!("tar: no files specified for archive"));
        }
        create_archive(&options.file, &options.files)?;
    } else if options.extract {
        extract_archive(&options.file, options.directory.as_deref())?;
    }

    Ok(())
}