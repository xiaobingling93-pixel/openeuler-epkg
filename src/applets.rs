use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use color_eyre::Result;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;

use crate::utils;

/// Parse arguments for the wget command.
/// Returns (output_file, prefix_dir, urls) on success.
/// - output_file: Some(path) if -O specified, None otherwise
/// - prefix_dir: Some(path) if -P specified, None otherwise
fn parse_wget_args(args: &[String]) -> Result<(Option<String>, Option<String>, Vec<String>)> {
    let mut output_file: Option<String> = None;
    let mut prefix_dir: Option<String> = None;
    let mut urls: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-O" => {
                if i + 1 >= args.len() {
                    return Err(eyre::eyre!("wget: option '-O' requires a file argument"));
                }
                output_file = Some(args[i + 1].clone());
                i += 2;
            }
            "-P" => {
                if i + 1 >= args.len() {
                    return Err(eyre::eyre!("wget: option '-P' requires a directory argument"));
                }
                prefix_dir = Some(args[i + 1].clone());
                i += 2;
            }
            other => {
                // Treat any non-option argument as a URL
                urls.push(other.to_string());
                i += 1;
            }
        }
    }

    if urls.is_empty() {
        return Err(eyre::eyre!("wget: missing URL"));
    }

    // Validate: -O can only be used with single URL
    if output_file.is_some() && urls.len() > 1 {
        return Err(eyre::eyre!("wget: option '-O' can only be used with a single URL"));
    }

    // Validate: -O and -P cannot be used together
    if output_file.is_some() && prefix_dir.is_some() {
        return Err(eyre::eyre!("wget: options '-O' and '-P' cannot be used together"));
    }

    Ok((output_file, prefix_dir, urls))
}

/// Create a symlink from the output location to the cached file.
fn link_downloaded_file(final_cached_path: &Path, output: &str) -> Result<()> {
    let out_path = Path::new(output);

    // Create parent directory if needed
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "download: failed to create parent directory for '{}'",
                    out_path.display()
                )
            })?;
        }
    }

    // Create symlink from output to cached file
    utils::force_symlink(final_cached_path, out_path)
        .with_context(|| {
            format!(
                "download: failed to create symlink from '{}' to '{}'",
                out_path.display(),
                final_cached_path.display()
            )
        })?;

    Ok(())
}

/// Prepare and validate the output directory.
/// Creates parent directory if output_file is specified, or validates/creates prefix_dir.
/// Returns the output directory path.
fn prepare_output_directory(output_file: &Option<String>, prefix_dir: &Option<String>) -> Result<PathBuf> {
    // If output_file is specified, create its parent directory
    if let Some(ref output) = output_file {
        let output_path = Path::new(output);
        if let Some(parent) = output_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("wget: failed to create parent directory for '{}'", output))?;
            }
        }
        // Return parent directory or current directory
        return Ok(output_path.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".")));
    }

    // If prefix_dir is specified, validate and create it
    if let Some(ref prefix) = prefix_dir {
        let dir = Path::new(prefix);
        if !dir.exists() {
            fs::create_dir_all(dir)
                .with_context(|| format!("wget: failed to create directory '{}'", prefix))?;
        }
        if !dir.is_dir() {
            return Err(eyre::eyre!("wget: '-P' must specify a directory, got: '{}'", prefix));
        }
        return Ok(dir.to_path_buf());
    }

    // Default to current directory
    Ok(PathBuf::from("."))
}

/// Determine the output path for a downloaded file.
fn determine_output_path(
    cached_path: &Path,
    output_file: &Option<String>,
    output_dir: &Path,
) -> PathBuf {
    if let Some(ref output) = output_file {
        // -O specified: use that file (only for single URL, validated in parse_wget_args)
        PathBuf::from(output)
    } else {
        // Use filename from cached path
        let filename = cached_path.file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "index.html".to_string());
        output_dir.join(&filename)
    }
}

/// Process all downloaded files: validate cache files and move them to output locations.
/// Continues processing even on errors (like make --keep-going) and reports all errors at the end.
fn process_downloaded_files(
    download_results: &[Result<String>],
    urls: &[String],
    output_file: &Option<String>,
    output_dir: &Path,
) -> Result<()> {
    let mut errors = Vec::new();

    for (i, result) in download_results.iter().enumerate() {
        let final_cached_path = match result {
            Ok(path) => std::path::PathBuf::from(path),
            Err(e) => {
                let url = urls.get(i).map(|s| s.as_str()).unwrap_or("unknown url");
                errors.push(format!("wget: download failed for {}: {}", url, e));
                continue;
            }
        };

        // Ensure the cache file exists
        if !final_cached_path.exists() {
            errors.push(format!(
                "wget: internal error, expected downloaded file not found at '{}'",
                final_cached_path.display()
            ));
            continue;
        }

        // Determine output path
        let output_path = determine_output_path(&final_cached_path, output_file, output_dir);

        // Try to move the file, but continue on error
        if let Err(e) = link_downloaded_file(&final_cached_path, &output_path.to_string_lossy()) {
            errors.push(format!(
                "wget: failed to move file from '{}' to '{}': {}",
                final_cached_path.display(),
                output_path.display(),
                e
            ));
            continue;
        }
    }

    // If there were any errors, return them all
    if !errors.is_empty() {
        let error_count = errors.len();
        let error_details = errors.join("\n");
        return Err(eyre::eyre!(
            "{} error(s) occurred:\n{}",
            error_count,
            error_details
        ));
    }

    Ok(())
}

/// Execute the `wget` built-in command.
///
/// Syntax:
///   wget [-O FILE] URL
///   wget [-P DIR] URL...
fn exec_builtin_wget(args: &[String]) -> Result<()> {
    let (output_file, prefix_dir, urls) = parse_wget_args(args)?;

    // Download all URLs in parallel
    use crate::download::download_urls;
    let download_results = download_urls(urls.to_vec());

    // Prepare output directory
    let output_dir = prepare_output_directory(&output_file, &prefix_dir)?;

    // Process downloaded files
    process_downloaded_files(&download_results, &urls, &output_file, &output_dir)?;

    Ok(())
}

/// Execute the `sleep` built-in command.
fn exec_builtin_sleep(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err(eyre::eyre!("sleep: missing operand"));
    }
    let duration = args[0].parse::<u64>()
        .map_err(|e| eyre::eyre!("sleep: invalid time interval '{}': {}", args[0], e))?;
    std::thread::sleep(Duration::from_secs(duration));
    Ok(())
}

/// Execute the `true` built-in command.
fn exec_builtin_true(_args: &[String]) -> Result<()> {
    Ok(())
}

/// Execute the `false` built-in command.
fn exec_builtin_false(_args: &[String]) -> Result<()> {
    std::process::exit(1);
}

/// Execute the `echo` built-in command.
fn exec_builtin_echo(args: &[String]) -> Result<()> {
    println!("{}", args.join(" "));
    Ok(())
}

/// Execute the `cat` built-in command.
fn exec_builtin_cat(args: &[String]) -> Result<()> {
    if args.is_empty() {
        // Read from stdin
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)
            .map_err(|e| eyre::eyre!("cat: failed to read from stdin: {}", e))?;
        print!("{}", buffer);
    } else {
        // Read from files
        for file_path in args {
            let content = fs::read_to_string(file_path)
                .map_err(|e| eyre::eyre!("cat: {}: {}", file_path, e))?;
            print!("{}", content);
        }
    }
    Ok(())
}

/// Execute the `ls` built-in command.
fn exec_builtin_ls(args: &[String]) -> Result<()> {
    let path = if args.is_empty() {
        Path::new(".")
    } else {
        Path::new(&args[0])
    };

    let entries = fs::read_dir(path)
        .map_err(|e| eyre::eyre!("ls: {}: {}", path.display(), e))?;

    let mut names: Vec<String> = entries
        .filter_map(|entry| {
            entry.ok().map(|e| {
                e.file_name().to_string_lossy().to_string()
            })
        })
        .collect();

    names.sort();
    let output = names.join("\n");
    println!("{}", output);
    Ok(())
}

/// Execute a built-in command
/// Returns an error if the command is not a built-in command
pub fn exec_builtin_command(cmd_name: &str, args: &[String]) -> Result<()> {
    match cmd_name {
        "wget" => exec_builtin_wget(args),
        "sleep" => exec_builtin_sleep(args),
        "true" => exec_builtin_true(args),
        "false" => exec_builtin_false(args),
        "echo" => exec_builtin_echo(args),
        "cat" => exec_builtin_cat(args),
        "ls" => exec_builtin_ls(args),
        _ => {
            Err(eyre::eyre!("Cannot run: {} {:?}", cmd_name, args))
        }
    }
}
