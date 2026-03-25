use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use std::path::{Path, PathBuf};
use crate::lfs;

use crate::utils;

pub struct WgetOptions {
    pub output_file: Option<String>,
    pub prefix_dir: Option<String>,
    pub urls: Vec<String>,
    #[allow(dead_code)] pub quiet: bool,
}

/// Normalize URL by prepending http:// if no scheme is present.
fn normalize_url(url: &str) -> String {
    if url.contains("://") {
        url.to_string()
    } else {
        eprintln!("Prepended http:// to '{}'", url);
        format!("http://{}", url)
    }
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<WgetOptions> {
    let output_file = matches.get_one::<String>("output").cloned();
    let prefix_dir = matches.get_one::<String>("directory-prefix").cloned();
    let quiet = matches.get_flag("quiet");
    let urls: Vec<String> = matches.get_many::<String>("urls")
        .map(|vals| vals.map(|u| normalize_url(u)).collect())
        .unwrap_or_default();

    // Validate: -O can only be used with single URL
    if output_file.is_some() && urls.len() > 1 {
        return Err(eyre::eyre!("wget: option '-O' can only be used with a single URL"));
    }


    Ok(WgetOptions {
        output_file,
        prefix_dir,
        urls,
        quiet,
    })
}

pub fn command() -> Command {
    Command::new("wget")
        .about("Download files from the web")
        .arg_required_else_help(true) // This will show help if no args are provided
        .arg(Arg::new("output")
            .short('O')
            .long("output-document")
            .value_name("FILE")
            .help("Write documents to FILE"))
        .arg(Arg::new("directory-prefix")
            .short('P')
            .long("directory-prefix")
            .value_name("DIR")
            .help("Save files to DIR"))
        .arg(Arg::new("quiet")
            .short('q')
            .long("quiet")
            .help("Quiet mode (no output)")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("urls")
            .required(true)
            .num_args(1..)
            .help("URLs to download"))
}

pub fn run(options: WgetOptions) -> Result<()> {
    // Download all URLs in parallel
    use crate::download::download_urls;
    let download_results = download_urls(options.urls.to_vec());

    // Prepare output directory
    let output_dir = prepare_output_directory(&options.output_file, &options.prefix_dir)?;

    // Process downloaded files
    process_downloaded_files(&download_results, &options.urls, &options.output_file, &output_dir)?;

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
                lfs::create_dir_all(parent)?;
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
            lfs::create_dir_all(dir)?;
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
        // -O specified: use that file (only for single URL, validated in run)
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

/// Create a symlink from the output location to the cached file.
fn link_downloaded_file(final_cached_path: &Path, output: &str) -> Result<()> {
    let out_path = Path::new(output);

    // Create parent directory if needed
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            lfs::create_dir_all(parent)?;
        }
    }

    // Create symlink from output to cached file
    utils::force_symlink_file_for_native(final_cached_path, out_path)
        .with_context(|| {
            format!(
                "download: failed to create symlink from '{}' to '{}'",
                out_path.display(),
                final_cached_path.display()
            )
        })?;

    Ok(())
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

