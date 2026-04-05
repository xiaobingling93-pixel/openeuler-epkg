//! VM start command implementation.

use std::path::Path;
use color_eyre::{Result, eyre};
use clap::ArgMatches;

use super::session::{VmConfig, discover_vm_session};
use super::keeper::run_vm_keeper;

/// Parse key=value arguments into VmConfig.
fn parse_kv_args(args: Option<clap::parser::ValuesRef<String>>) -> VmConfig {
    let mut config = VmConfig::default();

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

/// Write config to temp file for child process to read.
fn write_temp_config(config: &VmConfig) -> Result<std::path::PathBuf> {
    let temp_dir = std::env::temp_dir();
    let config_file = temp_dir.join(format!("epkg-vm-config-{}.json", std::process::id()));
    let content = serde_json::to_string_pretty(config)?;
    std::fs::write(&config_file, content)?;
    Ok(config_file)
}

/// Read config from temp file.
fn read_temp_config() -> Result<VmConfig> {
    let temp_dir = std::env::temp_dir();
    let config_file = temp_dir.join(format!("epkg-vm-config-{}.json", std::process::id()));
    let content = std::fs::read_to_string(&config_file)?;
    let config: VmConfig = serde_json::from_str(&content)?;
    // Clean up temp file
    let _ = std::fs::remove_file(&config_file);
    Ok(config)
}

/// Wait for VM session to be ready.
fn wait_for_session_ready(env_root: &Path, timeout_secs: u32) -> Result<()> {
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(timeout_secs as u64);

    while start.elapsed() < timeout {
        if discover_vm_session(env_root)?.is_some() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    Err(eyre::eyre!("Timeout waiting for VM session to be ready"))
}

/// Start VM keeper on Unix using fork().
#[cfg(unix)]
fn vm_start_unix(env_root: &Path, env_name: &str, config: VmConfig) -> Result<()> {
    // Write config for child process (child will clean it up)
    let _config_file = write_temp_config(&config)?;

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
            wait_for_session_ready(env_root, 30)?;

            // Clean up temp config file
            let temp_dir = std::env::temp_dir();
            let config_file = temp_dir.join(format!("epkg-vm-config-{}.json", std::process::id()));
            let _ = std::fs::remove_file(&config_file);
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
    use std::os::windows::process::CreationFlags;

    // Write config for child process
    let _config_file = write_temp_config(&config)?;

    let exe = std::env::current_exe()?;
    let env_root_str = env_root.display().to_string();

    // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    const DETACHED_FLAGS: u32 = 0x00000008 | 0x00000200;

    std::process::Command::new(&exe)
        .args([
            "vm", "start",
            &env_root_str,
            "--internal-keeper",
        ])
        .creation_flags(DETACHED_FLAGS)
        .spawn()?;

    // Parent: wait for session ready
    wait_for_session_ready(env_root, 30)?;

    // Clean up temp config file
    let temp_dir = std::env::temp_dir();
    let config_file = temp_dir.join(format!("epkg-vm-config-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&config_file);

    Ok(())
}

/// Entry point for `epkg vm start` command.
pub fn cmd_vm_start(args: &ArgMatches) -> Result<()> {
    // Check if this is the keeper process (internal)
    if args.get_flag("internal-keeper") {
        let config = read_temp_config()?;
        let env_root: std::path::PathBuf = args.get_one::<String>("env")
            .expect("env is required")
            .into();
        let env_name = env_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        return run_vm_keeper(&env_root, &env_name, config);
    }

    // Normal mode: parse arguments and spawn keeper
    let env_root: std::path::PathBuf = args.get_one::<String>("env")
        .expect("env is required")
        .into();

    // Get env_name from path
    let env_name = env_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Check if VM already running
    if discover_vm_session(&env_root)?.is_some() {
        return Err(eyre::eyre!("VM already running for {}", env_name));
    }

    // Parse key=value config
    let config = parse_kv_args(args.get_many::<String>("set"));

    // Start keeper process
    #[cfg(unix)]
    vm_start_unix(&env_root, &env_name, config.clone())?;

    #[cfg(windows)]
    vm_start_windows(&env_root, &env_name, config.clone())?;

    println!("VM started for {} (timeout={}s, extend={}s)",
             env_name, config.timeout, config.extend);

    Ok(())
}