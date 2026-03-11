use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use nix::unistd::getpid;
use std::fs;
use std::path::Path;

pub struct PidofOptions {
    pub programs: Vec<String>,
    pub check_session: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<PidofOptions> {
    let programs = matches.get_many::<String>("programs")
        .map(|vals| vals.map(|s| s.clone()).collect::<Vec<_>>())
        .unwrap_or_default();

    if programs.is_empty() {
        return Err(eyre!("pidof: missing program name"));
    }

    let check_session = matches.get_flag("session");

    Ok(PidofOptions { programs, check_session })
}

pub fn command() -> Command {
    Command::new("pidof")
        .about("Find the process ID of a running program")
        .arg(Arg::new("session")
            .short('c')
            .help("Match only processes whose controlling terminal is the current one")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("programs")
            .help("Program names to find")
            .required(true)
            .num_args(1..))
}



fn get_process_tokens_with_indices(pid: u32) -> Vec<(usize, String)> {
    let cmdline_path = format!("/proc/{}/cmdline", pid);
    if let Ok(content) = fs::read_to_string(&cmdline_path) {
        content.split('\0')
            .enumerate()
            .filter(|(_, token)| !token.is_empty())
            .filter_map(|(idx, token)| {
                Path::new(token).file_name()
                    .and_then(|os_str| os_str.to_str())
                    .map(|s| (idx, s.to_string()))
            })
            .collect()
    } else {
        Vec::new()
    }
}

fn get_controlling_terminal(pid: u32) -> Option<u32> {
    let stat_path = format!("/proc/{}/stat", pid);
    if let Ok(content) = fs::read_to_string(&stat_path) {
        // Parse the stat file - tty_nr is field 7 (0-indexed as 6)
        let fields: Vec<&str> = content.split_whitespace().collect();
        if fields.len() > 6 {
            fields[6].parse::<u32>().ok()
        } else {
            None
        }
    } else {
        None
    }
}

fn get_current_controlling_terminal() -> Option<u32> {
    get_controlling_terminal(getpid().as_raw() as u32)
}

fn find_processes_by_names(target_names: &[String], check_session: bool) -> Result<Vec<u32>> {
    let self_pid = getpid().as_raw() as u32;
    let mut pids = Vec::new();
    let mut seen_pids = std::collections::HashSet::new();

    let current_tty = if check_session {
        get_current_controlling_terminal()
    } else {
        None
    };

    let proc_dir = Path::new("/proc");
    if !proc_dir.exists() {
        return Err(eyre!("pidof: /proc directory not found"));
    }

    let read_dir_result = fs::read_dir(proc_dir);
    for entry in read_dir_result
        .map_err(|e| eyre!("pidof: error reading /proc: {}", e))?
    {
        let entry = entry.map_err(|e| eyre!("pidof: error reading /proc entry: {}", e))?;
        let file_name = entry.file_name();
        let pid_str = file_name.to_str().unwrap_or("");

        if let Ok(pid) = pid_str.parse::<u32>() {
            let tokens = get_process_tokens_with_indices(pid);
            let basenames: Vec<String> = tokens.iter()
                .filter(|(idx, _)| pid != self_pid || *idx < 3) // keep first three tokens for self
                .map(|(_, name)| name.clone())
                .collect();
            // Check session constraint if requested
            let session_ok = if check_session {
                if let Some(current_tty) = current_tty {
                    get_controlling_terminal(pid) == Some(current_tty)
                } else {
                    false // No current tty, can't match
                }
            } else {
                true // No session check requested
            };

            if session_ok {
                for target_name in target_names {
                    if basenames.contains(target_name) && !seen_pids.contains(&pid) {
                        pids.push(pid);
                        seen_pids.insert(pid);
                        break; // Found a match, no need to check other names for this PID
                    }
                }
            }
        }
    }

    Ok(pids)
}

pub fn run(options: PidofOptions) -> Result<()> {
    let pids = find_processes_by_names(&options.programs, options.check_session)?;

    if pids.is_empty() {
        // pidof exits with non-zero status when no processes found
        std::process::exit(1);
    } else {
        let pid_strings: Vec<String> = pids.iter().map(|&pid| pid.to_string()).collect();
        println!("{}", pid_strings.join(" "));
    }

    Ok(())
}