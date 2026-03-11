use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs::OpenOptions;
use std::io::Seek;
use std::path::Path;
use crate::utils::{split_number_and_suffix, apply_suffix};

pub struct TruncateOptions {
    pub size: Option<String>,
    pub reference: Option<String>,
    pub files: Vec<String>,
    pub no_create: bool,
    pub io_blocks: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<TruncateOptions> {
    let size = matches.get_one::<String>("size").cloned();
    let reference = matches.get_one::<String>("reference").cloned();

    let files: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let no_create = matches.get_flag("no-create");
    let io_blocks = matches.get_flag("io-blocks");

    if files.is_empty() {
        return Err(eyre!("truncate: missing operand"));
    }

    if size.is_none() && reference.is_none() {
        return Err(eyre!("truncate: you must specify either --size or --reference"));
    }

    Ok(TruncateOptions {
        size,
        reference,
        files,
        no_create,
        io_blocks,
    })
}

pub fn command() -> Command {
    Command::new("truncate")
        .about("Shrink or extend the size of each FILE to the specified size")
        .arg(Arg::new("size")
            .short('s')
            .long("size")
            .help("Set or adjust the file size by SIZE bytes")
            .value_name("SIZE"))
        .arg(Arg::new("reference")
            .short('r')
            .long("reference")
            .help("Base size on RFILE")
            .value_name("RFILE"))
        .arg(Arg::new("no-create")
            .short('c')
            .long("no-create")
            .help("Do not create any files")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("io-blocks")
            .short('o')
            .long("io-blocks")
            .help("Treat SIZE as number of IO blocks instead of bytes")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files")
            .num_args(1..)
            .required(true)
            .help("Files to truncate"))
}

#[derive(Debug, Clone, Copy)]
enum SizeModifier {
    Set,      // No modifier
    Extend,   // '+'
    Reduce,   // '-'
    AtMost,   // '<'
    AtLeast,  // '>'
    RoundDown, // '/'
    RoundUp,  // '%'
}


#[allow(dead_code)]

fn parse_size(size_str: &str) -> Result<(i64, SizeModifier)> {
    if size_str.is_empty() {
        return Err(eyre!("truncate: invalid size '{}'", size_str));
    }

    // Check for modifier prefix
    let (modifier, size_part) = match size_str.chars().next() {
        Some('+') => (SizeModifier::Extend, &size_str[1..]),
        Some('-') => (SizeModifier::Reduce, &size_str[1..]),
        Some('<') => (SizeModifier::AtMost, &size_str[1..]),
        Some('>') => (SizeModifier::AtLeast, &size_str[1..]),
        Some('/') => (SizeModifier::RoundDown, &size_str[1..]),
        Some('%') => (SizeModifier::RoundUp, &size_str[1..]),
        _ => (SizeModifier::Set, size_str),
    };

    // Parse suffix (K, M, G, etc. or KB, MB, etc.)
    let (number_str, suffix) = split_number_and_suffix(size_part);

    let number = number_str.parse::<i64>()
        .map_err(|e| eyre!("truncate: invalid size '{}': {}", size_str, e))?;

    let bytes = apply_suffix(number, suffix, size_str)?;

    Ok((bytes, modifier))
}

fn get_base_size(options: &TruncateOptions) -> Result<i64> {
    if let Some(ref ref_file) = options.reference {
        let ref_path = Path::new(ref_file);
        if !ref_path.exists() {
            return Err(eyre!("truncate: cannot stat '{}': No such file or directory", ref_file));
        }
        let metadata = std::fs::metadata(ref_path)
            .map_err(|e| eyre!("truncate: cannot stat '{}': {}", ref_file, e))?;
        Ok(metadata.len() as i64)
    } else if let Some(ref size_str) = options.size {
        let (size, _) = parse_size(size_str)?;
        Ok(size)
    } else {
        Err(eyre!("truncate: you must specify either --size or --reference"))
    }
}

fn get_block_size() -> Result<i64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(".")
            .map_err(|e| eyre!("truncate: cannot determine block size: {}", e))?;
        Ok(metadata.blksize() as i64)
    }
    #[cfg(not(unix))]
    {
        Ok(512i64) // Default block size on non-Unix
    }
}

fn calculate_final_size(
    path: &Path,
    base_size: i64,
    block_size: i64,
    io_blocks: bool,
    size_str: Option<&String>,
) -> Result<i64> {
    // Calculate target size
    let target_size = if io_blocks {
        base_size * block_size
    } else {
        base_size
    };

    // Apply size modifier if we have a size string (not reference)
    let final_size = if let Some(size_str) = size_str {
        let (_, modifier) = parse_size(size_str)?;

        if path.exists() {
            let current_metadata = std::fs::metadata(path)
                .map_err(|e| eyre!("truncate: cannot stat '{}': {}", path.display(), e))?;
            let current_size = current_metadata.len() as i64;

            match modifier {
                SizeModifier::Set => target_size,
                SizeModifier::Extend => current_size + target_size,
                SizeModifier::Reduce => current_size - target_size,
                SizeModifier::AtMost => current_size.min(target_size),
                SizeModifier::AtLeast => current_size.max(target_size),
                SizeModifier::RoundDown => (current_size / target_size) * target_size,
                SizeModifier::RoundUp => {
                    if current_size % target_size == 0 {
                        current_size
                    } else {
                        ((current_size / target_size) + 1) * target_size
                    }
                },
            }
        } else {
            // File doesn't exist, modifiers don't apply (except Set)
            match modifier {
                SizeModifier::Set => target_size,
                _ => return Err(eyre!("truncate: cannot apply modifier to non-existent file '{}'", path.display())),
            }
        }
    } else {
        // Using reference file, just use the size directly
        target_size
    };

    if final_size < 0 {
        return Err(eyre!("truncate: invalid size: {}", final_size));
    }

    Ok(final_size)
}

fn truncate_file(path: &Path, final_size: u64, no_create: bool) -> Result<()> {
    let mut file_handle = OpenOptions::new()
        .write(true)
        .create(!no_create)
        .open(path)
        .map_err(|e| eyre!("truncate: cannot open '{}': {}", path.display(), e))?;

    file_handle.seek(std::io::SeekFrom::Start(final_size))
        .map_err(|e| eyre!("truncate: cannot seek in '{}': {}", path.display(), e))?;

    file_handle.set_len(final_size)
        .map_err(|e| eyre!("truncate: cannot truncate '{}': {}", path.display(), e))?;

    Ok(())
}

pub fn run(options: TruncateOptions) -> Result<()> {
    // Get base size from reference file or parse from size string
    let base_size = get_base_size(&options)?;

    // Get IO block size if needed
    let block_size = if options.io_blocks {
        get_block_size()?
    } else {
        1i64
    };

    for file in &options.files {
        let path = Path::new(file);

        // Check if file exists when --no-create is specified
        if options.no_create && !path.exists() {
            return Err(eyre!("truncate: cannot open '{}': No such file or directory", file));
        }

        // Calculate final size with modifiers
        let final_size = calculate_final_size(
            path,
            base_size,
            block_size,
            options.io_blocks,
            options.size.as_ref(),
        )?;

        // Truncate the file
        truncate_file(path, final_size as u64, options.no_create)?;
    }

    Ok(())
}
