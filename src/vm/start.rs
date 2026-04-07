//! VM start command implementation.

use std::path::Path;
use color_eyre::{Result, eyre};
use clap::ArgMatches;

use super::session::{VmConfig, discover_vm_session};
use super::keeper::run_vm_keeper;

/// Parse key=value arguments into VmConfig.
fn parse_kv_args(args: Option<clap::parser::ValuesRef<String>>, vmm: Option<&str>) -> VmConfig {
    let mut config = VmConfig {
        backend: vmm.unwrap_or("libkrun").to_string(),
        ..Default::default()
    };

    if let Some(values) = args {
        for kv in values {
            let parts: Vec<&str> = kv.splitn(2, '=').collect();
            if parts.len() != 2 {
                log::warn!("Invalid key=value format: {}", kv);
                continue;
            }
            let key = parts[0].trim();
            let value = parts[1].trim();

            match key {
                "timeout" => {
                    if let Ok(v) = value.parse() {
                        config.timeout = v;
                    }
                }
                "extend" => {
                    if let Ok(v) = value.parse() {
                        config.extend = v;
                    }
                }
                "cpus" => {
                    if let Ok(v) = value.parse() {
                        config.cpus = v;
                    }
                }
                "memory" => {
                    if let Ok(v) = value.parse() {
                        config.memory_mib = v;
                    }
                }
                _ => {
                    log::warn!("Unknown config key: {}", key);
                }
            }
        }
    }

    config
}

/// Get the pending creation file path for an env_name.
/// This is used to coordinate between parent and child processes on Windows.
fn pending_file_path(env_name: &str) -> std::path::PathBuf {
    crate::models::dirs().epkg_run.join(format!("vm-pending-{}.json", env_name))
}

/// Write pending creation file with config.
#[cfg(windows)]
fn write_pending_config(env_name: &str, config: &VmConfig) -> Result<()> {
    let pending_file = pending_file_path(env_name);
    let content = serde_json::to_string_pretty(config)?;
    std::fs::write(&pending_file, content)?;
    Ok(())
}

/// Read and delete pending creation file.
fn read_and_delete_pending_config(env_name: &str) -> Result<Option<VmConfig>> {
    let pending_file = pending_file_path(env_name);
    if !pending_file.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&pending_file)?;
    let config: VmConfig = serde_json::from_str(&content)?;
    let _ = std::fs::remove_file(&pending_file);
    Ok(Some(config))
}

/// Wait for VM session to be ready.
fn wait_for_session_ready(_env_root: &Path, env_name: &str, timeout_secs: u32) -> Result<()> {
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(timeout_secs as u64);

    while start.elapsed() < timeout {
        if discover_vm_session(env_name)?.is_some() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Cleanup pending file on timeout
    let _ = std::fs::remove_file(&pending_file_path(env_name));
    Err(eyre::eyre!("Timeout waiting for VM session to be ready"))
}

/// Start VM keeper on Unix using fork().
#[cfg(unix)]
fn vm_start_unix(env_root: &Path, env_name: &str, config: VmConfig) -> Result<()> {
    match unsafe { libc::fork() } {
        0 => {
            // Child process: become session leader to detach from parent
            let _ = nix::unistd::setsid();

            // Run keeper logic
            if let Err(e) = run_vm_keeper(env_root, env_name, config) {
                log::error!("VM keeper failed: {}", e);
            }

            // Exit child process
            std::process::exit(0);
        }
        pid if pid > 0 => {
            // Parent process: wait for session ready
            wait_for_session_ready(env_root, env_name, 30)?;
        }
        _ => {
            return Err(eyre::eyre!("fork() failed"));
        }
    }

    Ok(())
}

/// Start VM keeper on Windows using spawn.
#[cfg(windows)]
fn vm_start_windows(env_root: &Path, env_name: &str, config: VmConfig) -> Result<()> {
    use std::os::windows::process::CommandExt;

    // Write pending file for child process to detect
    write_pending_config(env_name, &config)?;

    let exe = std::env::current_exe()?;
    let env_root_str = env_root.display().to_string();

    // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    const DETACHED_FLAGS: u32 = 0x00000008 | 0x00000200;

    std::process::Command::new(&exe)
        .args(["vm", "start", &env_root_str])
        .creation_flags(DETACHED_FLAGS)
        .spawn()?;

    // Parent: wait for session ready
    wait_for_session_ready(env_root, env_name, 30)?;

    Ok(())
}

/// Entry point for `epkg vm start` command.
pub fn cmd_vm_start(args: &ArgMatches) -> Result<()> {
    let cfg = crate::models::config();
    let env_name = cfg.common.env_name.clone();
    let env_root = if cfg.common.env_root.is_empty() {
        crate::dirs::get_env_root(env_name.clone())?
    } else {
        std::path::PathBuf::from(&cfg.common.env_root)
    };

    // Check for pending creation file (Windows child process path)
    // This indicates we're the spawned keeper process
    if let Some(config) = read_and_delete_pending_config(&env_name)? {
        return run_vm_keeper(&env_root, &env_name, config);
    }

    // Normal mode: check if VM already running
    if discover_vm_session(&env_name)?.is_some() {
        return Err(eyre::eyre!("VM already running for {}", env_name));
    }

    // Parse key=value config with optional --vmm backend override
    let vmm = args.get_one::<String>("vmm").map(|s| s.as_str());
    let config = parse_kv_args(args.get_many::<String>("set"), vmm);

    // Start keeper process
    #[cfg(unix)]
    vm_start_unix(&env_root, &env_name, config.clone())?;

    #[cfg(windows)]
    vm_start_windows(&env_root, &env_name, config.clone())?;

    let timeout_desc = if config.timeout == 0 {
        "never".to_string()
    } else {
        format!("{}s", config.timeout)
    };
    println!("VM started for {} (timeout={}, extend={}s)",
             env_name, timeout_desc, config.extend);

    Ok(())
}