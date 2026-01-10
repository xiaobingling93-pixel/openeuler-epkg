//! Shared shebang handling utilities
//!
//! This module provides common functionality for parsing and manipulating shebang lines
//! used across different parts of epkg (conda linking, package exposure, etc.)

use std::path::Path;
use std::borrow::Cow;
use regex::Regex;
use lazy_static::lazy_static;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use log;

/// Maximum shebang length on Linux (127 characters)
pub const MAX_SHEBANG_LENGTH_LINUX: usize = 127;

/// Information extracted from a shebang line for creating wrappers
#[derive(Debug, PartialEq)]
pub struct ShebangInfo {
    pub interpreter_path: String,      // Path to interpreter for wrapper creation (e.g., "/usr/bin/python")
    pub interpreter_basename: String,  // Basename for wrapper lookup (e.g., "python")
    pub remaining_params: String,      // Additional parameters to pass (e.g., "-u -O")
}

// Regex to match shebang lines
// ^(#!      pretty much the whole match string
// (?:[ ]*)  allow spaces between #! and beginning of the executable path
// (/(?:\\ |[^ \n\r\t])*)  the executable is the next text block without an
//                         escaped space or non-space whitespace character
// (.*))$    the rest of the line can contain option flags and end whole_shebang group
lazy_static! {
    pub static ref SHEBANG_REGEX: Regex = Regex::new(r"^(#!(?:[ ]*)(/(?:\\ |[^ \n\r\t])*)(.*))$").unwrap();
    // Match string starting with `python`, and optional version number
    // python matches the string `python`
    // (?:\d+(?:\.\d+)*)? matches an optional version number
    pub static ref PYTHON_REGEX: Regex = Regex::new(r"^python(?:\d+(?:\.\d+)?)?$").unwrap();
}

/// Check if shebang length is valid for the current platform
pub fn is_valid_shebang_length(shebang: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        shebang.len() <= MAX_SHEBANG_LENGTH_LINUX
    }
    #[cfg(target_os = "macos")]
    {
        shebang.len() <= 512
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        // For other platforms, assume it's valid
        true
    }
}

/// Convert a shebang to use `/usr/bin/env` to find the executable.
/// This is useful for long shebangs or shebangs with spaces.
///
/// For Python interpreters, uses a special exec wrapper format.
pub fn convert_shebang_to_env(shebang: Cow<'_, str>) -> Cow<'_, str> {
    if let Some(captures) = SHEBANG_REGEX.captures(&shebang) {
        let path = captures.get(2).map(|m| m.as_str()).unwrap_or("");
        let exe_name = path.rsplit_once('/').map_or(path, |(_, f)| f);
        let rest = captures.get(3).map(|m| m.as_str()).unwrap_or("");

        if PYTHON_REGEX.is_match(exe_name) {
            Cow::Owned(format!(
                "#!/bin/sh\n'''exec' \"{}\"{}\" \"$0\" \"$@\" #'''",
                path, rest
            ))
        } else {
            Cow::Owned(format!("#!/usr/bin/env {}{}", exe_name, rest))
        }
    } else {
        shebang
    }
}


/// Parse a shebang line into interpreter path and parameters
fn parse_shebang_line(first_line: &str) -> Result<(String, String)> {
    if !first_line.starts_with("#!") {
        return Err(eyre::eyre!("No shebang line found"));
    }

    let interpreter_with_params = first_line[2..].trim().replace("\t", " ");
    // Example: interpreter_with_params = "/bin/sh"
    let (interpreter_path, params) = match interpreter_with_params.split_once(' ') {
        Some((path, params)) => (path.to_string(), params.to_string()),  // Example: path="/usr/bin/env", params="python3"
        None => (interpreter_with_params.to_string(), String::new()),    // Example: path="/bin/sh", params=""
    };
    log::debug!("interpreter_path: '{}', params: '{}'", interpreter_path, params);

    Ok((interpreter_path, params))
}

/// Parse a shebang line and extract information needed for wrapper creation
/// This function handles env-based shebangs specially by resolving the actual interpreter
///
/// # Examples
///
/// ```
/// # use epkg::install::parse_shebang_for_wrapper;
/// let info = parse_shebang_for_wrapper("#!/usr/bin/env python").unwrap();
/// assert_eq!(info.interpreter_path, "/usr/bin/python");
/// assert_eq!(info.interpreter_basename, "python");
/// assert_eq!(info.remaining_params, "");
///
/// let info = parse_shebang_for_wrapper("#!/usr/bin/env python3 -u").unwrap();
/// assert_eq!(info.interpreter_path, "/usr/bin/python3");
/// assert_eq!(info.interpreter_basename, "python3");
/// assert_eq!(info.remaining_params, "-u");
///
/// let info = parse_shebang_for_wrapper("#!/bin/bash").unwrap();
/// assert_eq!(info.interpreter_path, "/bin/bash");
/// assert_eq!(info.interpreter_basename, "bash");
/// assert_eq!(info.remaining_params, "");
/// ```
pub fn parse_shebang_for_wrapper(first_line: &str) -> Result<ShebangInfo> {
    let (interpreter_path, params) = parse_shebang_line(first_line)
        .with_context(|| format!("Failed to parse shebang line: '{}'", first_line))?;

    // Special handling for env-based shebangs like "#!/usr/bin/env python"
    if interpreter_path == "/usr/bin/env" {
        // Check for case where line has trailing space after env but empty params
        // This catches "#!/usr/bin/env " with trailing space (but not tabs)
        if params.is_empty() {
            return Err(eyre::eyre!("env requires an interpreter to be specified"));
        }

        if !params.trim().is_empty() {
            let mut param_parts: Vec<&str> = params.split_whitespace().collect();

            // Handle env -S flag which allows env to split arguments on whitespace
            // Example: "#!/usr/bin/env -S awk -f" should be treated as "awk -f"
            if param_parts.len() >= 2 && param_parts[0] == "-S" {
                // Remove the -S flag and process the rest
                param_parts.remove(0);
            }

            if param_parts.is_empty() {
                return Err(eyre::eyre!("env -S requires an interpreter to be specified"));
            }

            // For env-based shebangs, the actual interpreter is in the first remaining parameter
            let actual_interpreter = param_parts[0];
            let remaining_params = param_parts[1..].join(" ");

            return Ok(ShebangInfo {
                interpreter_path: format!("/usr/bin/{}", actual_interpreter),
                interpreter_basename: actual_interpreter.to_string(),
                remaining_params,
            });
        }
    }

    // Original logic for non-env shebangs OR env without parameters
    // Handle edge case where interpreter_path is empty (e.g., just "#!")
    if interpreter_path.is_empty() {
        return Ok(ShebangInfo {
            interpreter_path: String::new(),
            interpreter_basename: String::new(),
            remaining_params: params,
        });
    }

    let interpreter_basename = Path::new(&interpreter_path).file_name()
        .ok_or_else(|| eyre::eyre!("Failed to get interpreter basename from: {}", interpreter_path))?
        .to_string_lossy()
        .to_string();

    Ok(ShebangInfo {
        interpreter_path,
        interpreter_basename,
        remaining_params: params,
    })
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_based_shebangs() {
        // Basic env python
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python");
        assert_eq!(info.interpreter_basename, "python");
        assert_eq!(info.remaining_params, "");

        // Python3 variant
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python3 ").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "");

        // Python with version
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python3.11").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3.11");
        assert_eq!(info.interpreter_basename, "python3.11");
        assert_eq!(info.remaining_params, "");

        // Python with options
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python -u").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python");
        assert_eq!(info.interpreter_basename, "python");
        assert_eq!(info.remaining_params, "-u");

        // Python with multiple options
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python3 -u -O ").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "-u -O");

        // Node.js
        let info = parse_shebang_for_wrapper("#!/usr/bin/env node").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/node");
        assert_eq!(info.interpreter_basename, "node");
        assert_eq!(info.remaining_params, "");

        // Node.js with options
        let info = parse_shebang_for_wrapper("#!/usr/bin/env node --experimental-modules").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/node");
        assert_eq!(info.interpreter_basename, "node");
        assert_eq!(info.remaining_params, "--experimental-modules");

        // Ruby
        let info = parse_shebang_for_wrapper("#!/usr/bin/env ruby").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/ruby");
        assert_eq!(info.interpreter_basename, "ruby");
        assert_eq!(info.remaining_params, "");

        // Perl
        let info = parse_shebang_for_wrapper("#!/usr/bin/env perl").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/perl");
        assert_eq!(info.interpreter_basename, "perl");
        assert_eq!(info.remaining_params, "");

        // PHP
        let info = parse_shebang_for_wrapper("#!/usr/bin/env php").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/php");
        assert_eq!(info.interpreter_basename, "php");
        assert_eq!(info.remaining_params, "");

        // Bash via env
        let info = parse_shebang_for_wrapper("#!/usr/bin/env bash").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "");

        // Zsh via env
        let info = parse_shebang_for_wrapper("#!/usr/bin/env zsh").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/zsh");
        assert_eq!(info.interpreter_basename, "zsh");
        assert_eq!(info.remaining_params, "");
    }

    #[test]
    fn test_direct_interpreter_shebangs() {
        // Standard shell
        let info = parse_shebang_for_wrapper("#! /bin/sh").unwrap();
        assert_eq!(info.interpreter_path, "/bin/sh");
        assert_eq!(info.interpreter_basename, "sh");
        assert_eq!(info.remaining_params, "");

        // Bash
        let info = parse_shebang_for_wrapper("#!/bin/bash ").unwrap();
        assert_eq!(info.interpreter_path, "/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "");

        // Bash with options
        let info = parse_shebang_for_wrapper("#!/bin/bash -e ").unwrap();
        assert_eq!(info.interpreter_path, "/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "-e");

        // Bash with multiple options
        let info = parse_shebang_for_wrapper("#!/bin/bash -eu -o pipefail").unwrap();
        assert_eq!(info.interpreter_path, "/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "-eu -o pipefail");

        // Python direct
        let info = parse_shebang_for_wrapper("#!/usr/bin/python3").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "");

        // Python with version and options
        let info = parse_shebang_for_wrapper("#!/usr/bin/python3.11 -u").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3.11");
        assert_eq!(info.interpreter_basename, "python3.11");
        assert_eq!(info.remaining_params, "-u");

        // Perl direct
        let info = parse_shebang_for_wrapper("#!/usr/bin/perl").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/perl");
        assert_eq!(info.interpreter_basename, "perl");
        assert_eq!(info.remaining_params, "");

        // Ruby direct
        let info = parse_shebang_for_wrapper("#!/usr/bin/ruby").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/ruby");
        assert_eq!(info.interpreter_basename, "ruby");
        assert_eq!(info.remaining_params, "");

        // Node.js direct
        let info = parse_shebang_for_wrapper("#!/usr/bin/node").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/node");
        assert_eq!(info.interpreter_basename, "node");
        assert_eq!(info.remaining_params, "");

        // Lua
        let info = parse_shebang_for_wrapper("#!/usr/bin/lua").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/lua");
        assert_eq!(info.interpreter_basename, "lua");
        assert_eq!(info.remaining_params, "");

        // AWK
        let info = parse_shebang_for_wrapper("#!/usr/bin/awk -f").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/awk");
        assert_eq!(info.interpreter_basename, "awk");
        assert_eq!(info.remaining_params, "-f");

        // GNU AWK
        let info = parse_shebang_for_wrapper("#!/usr/bin/gawk -f").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/gawk");
        assert_eq!(info.interpreter_basename, "gawk");
        assert_eq!(info.remaining_params, "-f");

        // Tcl/Tk
        let info = parse_shebang_for_wrapper("#!/usr/bin/tclsh").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/tclsh");
        assert_eq!(info.interpreter_basename, "tclsh");
        assert_eq!(info.remaining_params, "");

        // Fish shell
        let info = parse_shebang_for_wrapper("#!/usr/bin/fish").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/fish");
        assert_eq!(info.interpreter_basename, "fish");
        assert_eq!(info.remaining_params, "");
    }

    #[test]
    fn test_exotic_shebangs() {
        // Different env paths
        let info = parse_shebang_for_wrapper("#!/bin/env python").unwrap();
        assert_eq!(info.interpreter_path, "/bin/env");
        assert_eq!(info.interpreter_basename, "env");
        assert_eq!(info.remaining_params, "python");

        // Executable in non-standard location
        let info = parse_shebang_for_wrapper("#!/opt/python/bin/python").unwrap();
        assert_eq!(info.interpreter_path, "/opt/python/bin/python");
        assert_eq!(info.interpreter_basename, "python");
        assert_eq!(info.remaining_params, "");

        // Local installation
        let info = parse_shebang_for_wrapper("#!/usr/local/bin/python3").unwrap();
        assert_eq!(info.interpreter_path, "/usr/local/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "");

        // Complex paths
        let info = parse_shebang_for_wrapper("#!/home/user/.local/bin/custom-script").unwrap();
        assert_eq!(info.interpreter_path, "/home/user/.local/bin/custom-script");
        assert_eq!(info.interpreter_basename, "custom-script");
        assert_eq!(info.remaining_params, "");

        // Hyphenated interpreter names
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python-config").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python-config");
        assert_eq!(info.interpreter_basename, "python-config");
        assert_eq!(info.remaining_params, "");

        // Dotted interpreter names
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python3.11-config").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3.11-config");
        assert_eq!(info.interpreter_basename, "python3.11-config");
        assert_eq!(info.remaining_params, "");
    }

    #[test]
    fn test_edge_cases() {
        // Empty env params should fail
        let result = parse_shebang_for_wrapper("#!/usr/bin/env ");
        assert!(result.is_err());

        // No shebang
        let result = parse_shebang_for_wrapper("#!/usr/bin/env");
        assert!(result.is_err());

        // Multiple spaces
        let info = parse_shebang_for_wrapper("#! /usr/bin/env   python3   -u   -O").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "-u -O");

        // Tabs instead of spaces
        let info = parse_shebang_for_wrapper("#!/usr/bin/env\tpython3\t-u").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "-u");

        // Space after #! (common in real world)
        let info = parse_shebang_for_wrapper("#! /bin/bash").unwrap();
        assert_eq!(info.interpreter_path, "/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "");

        // Space after #! with parameters
        let info = parse_shebang_for_wrapper("#! /bin/bash -e").unwrap();
        assert_eq!(info.interpreter_path, "/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "-e");

        // Space after #! with env
        let info = parse_shebang_for_wrapper("#! /usr/bin/env python3").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "");

        // Space after #! with env and options
        let info = parse_shebang_for_wrapper("#! /usr/bin/env python3 -u").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "-u");

        // Multiple spaces after #!
        let info = parse_shebang_for_wrapper("#!   /bin/bash").unwrap();
        assert_eq!(info.interpreter_path, "/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "");

        // Tab after #!
        let info = parse_shebang_for_wrapper("#!\t/bin/bash").unwrap();
        assert_eq!(info.interpreter_path, "/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "");
    }

    #[test]
    fn test_invalid_shebangs() {
        // No shebang prefix
        let result = parse_shebang_for_wrapper("python script");
        assert!(result.is_err());

        // Just hash
        let result = parse_shebang_for_wrapper("#python");
        assert!(result.is_err());

        // Empty string
        let result = parse_shebang_for_wrapper("");
        assert!(result.is_err());

        // Only shebang
        let result = parse_shebang_for_wrapper("#!");
        assert!(result.is_ok()); // This actually parses as empty interpreter path
    }

    #[test]
    fn test_real_world_examples() {
        // From Django management commands
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python").unwrap();
        assert_eq!(info.interpreter_basename, "python");

        // From Node.js scripts
        let info = parse_shebang_for_wrapper("#!/usr/bin/env node").unwrap();
        assert_eq!(info.interpreter_basename, "node");

        // From system scripts
        let info = parse_shebang_for_wrapper("#!/bin/bash").unwrap();
        assert_eq!(info.interpreter_basename, "bash");

        // From build scripts
        let info = parse_shebang_for_wrapper("#!/bin/sh").unwrap();
        assert_eq!(info.interpreter_basename, "sh");

        // From Python virtual environments
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python3").unwrap();
        assert_eq!(info.interpreter_basename, "python3");

        // From Ruby gems
        let info = parse_shebang_for_wrapper("#!/usr/bin/env ruby").unwrap();
        assert_eq!(info.interpreter_basename, "ruby");

        // From Perl scripts
        let info = parse_shebang_for_wrapper("#!/usr/bin/perl -w").unwrap();
        assert_eq!(info.interpreter_basename, "perl");
        assert_eq!(info.remaining_params, "-w");

        // From AWK scripts
        let info = parse_shebang_for_wrapper("#!/usr/bin/awk -f").unwrap();
        assert_eq!(info.interpreter_basename, "awk");
        assert_eq!(info.remaining_params, "-f");
    }

    #[test]
    fn test_user_provided_real_world_cases() {
        // Based on actual usage data from the user

        // #!/usr/bin/env ruby (192 occurrences)
        let info = parse_shebang_for_wrapper("#!/usr/bin/env ruby").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/ruby");
        assert_eq!(info.interpreter_basename, "ruby");
        assert_eq!(info.remaining_params, "");

        // #!/bin/sh (13 occurrences)
        let info = parse_shebang_for_wrapper("#!/bin/sh").unwrap();
        assert_eq!(info.interpreter_path, "/bin/sh");
        assert_eq!(info.interpreter_basename, "sh");
        assert_eq!(info.remaining_params, "");

        // #!/usr/bin/awk -f (9 occurrences)
        let info = parse_shebang_for_wrapper("#!/usr/bin/awk -f").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/awk");
        assert_eq!(info.interpreter_basename, "awk");
        assert_eq!(info.remaining_params, "-f");

        // #!/usr/bin/env -S awk -f (4 occurrences) - env with -S flag
        let info = parse_shebang_for_wrapper("#!/usr/bin/env -S awk -f").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/awk");
        assert_eq!(info.interpreter_basename, "awk");
        assert_eq!(info.remaining_params, "-f");

        // #!/usr/bin/env bash (2 occurrences)
        let info = parse_shebang_for_wrapper("#!/usr/bin/env bash").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "");

        // #!/bin/bash (2 occurrences)
        let info = parse_shebang_for_wrapper("#!/bin/bash").unwrap();
        assert_eq!(info.interpreter_path, "/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "");
    }

    #[test]
    fn test_env_s_flag_variations() {
        // Basic env -S usage
        let info = parse_shebang_for_wrapper("#!/usr/bin/env -S awk -f").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/awk");
        assert_eq!(info.interpreter_basename, "awk");
        assert_eq!(info.remaining_params, "-f");

        // env -S with multiple arguments
        let info = parse_shebang_for_wrapper("#!/usr/bin/env -S python3 -u -O").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "-u -O");

        // env -S with just interpreter, no additional args
        let info = parse_shebang_for_wrapper("#!/usr/bin/env -S python3").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "");

        // Space after #! with env -S
        let info = parse_shebang_for_wrapper("#! /usr/bin/env -S awk -f").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/awk");
        assert_eq!(info.interpreter_basename, "awk");
        assert_eq!(info.remaining_params, "-f");

        // Multiple spaces with env -S
        let info = parse_shebang_for_wrapper("#!/usr/bin/env   -S   python3   -u").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "-u");

        // env -S with complex interpreter names
        let info = parse_shebang_for_wrapper("#!/usr/bin/env -S python3.11 -u").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3.11");
        assert_eq!(info.interpreter_basename, "python3.11");
        assert_eq!(info.remaining_params, "-u");

        // env -S with hyphenated interpreter
        let info = parse_shebang_for_wrapper("#!/usr/bin/env -S python-config --version").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python-config");
        assert_eq!(info.interpreter_basename, "python-config");
        assert_eq!(info.remaining_params, "--version");
    }

    #[test]
    fn test_env_s_flag_edge_cases() {
        // Test that regular env (non -S) still works
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python3").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "");
    }
}
