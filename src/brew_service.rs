//! Generate service files from Homebrew service definitions
//!
//! Homebrew formulas can define services using the `service` DSL.
//! This module converts those definitions to platform-specific service files:
//! - macOS: launchd plist files in ~/Library/LaunchAgents/
//! - Linux: systemd service files in etc/systemd/system/

use std::path::Path;
use color_eyre::Result;
use crate::lfs;
use crate::brew_repo::BrewService;

/// Generate a launchd plist file from a service definition
///
/// The plist file is written to `{env_root}/Library/LaunchAgents/homebrew.mxcl.{service_name}.plist`
#[cfg(target_os = "macos")]
pub fn generate_launchd_plist(
    env_root: &Path,
    service_name: &str,
    service: &BrewService,
) -> Result<std::path::PathBuf> {
    let plist_dir = crate::dirs::path_join(env_root, &["Library", "LaunchAgents"]);
    lfs::create_dir_all(&plist_dir)?;

    let plist_path = plist_dir.join(format!("homebrew.mxcl.{}.plist", service_name));

    // Build the run command
    let program_args = resolve_run_command(&service.run, env_root);

    // Build the plist content
    let mut plist = String::new();
    plist.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    plist.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    plist.push_str("<plist version=\"1.0\">\n");
    plist.push_str("<dict>\n");

    // Label
    plist.push_str(&format!("\t<key>Label</key>\n"));
    plist.push_str(&format!("\t<string>homebrew.mxcl.{}</string>\n", service_name));

    // ProgramArguments
    plist.push_str(&format!("\t<key>ProgramArguments</key>\n"));
    plist.push_str(&format!("\t<array>\n"));
    for arg in &program_args {
        plist.push_str(&format!("\t\t<string>{}</string>\n", escape_plist_string(arg)));
    }
    plist.push_str(&format!("\t</array>\n"));

    // RunAtLoad (for immediate run_type)
    if service.run_type.as_deref() == Some("immediate") {
        plist.push_str(&format!("\t<key>RunAtLoad</key>\n"));
        plist.push_str(&format!("\t<true/>\n"));
    }

    // KeepAlive
    if let Some(keep_alive) = &service.keep_alive {
        plist.push_str(&format!("\t<key>KeepAlive</key>\n"));
        if let Some(obj) = keep_alive.as_object() {
            if obj.contains_key("always") && obj["always"].as_bool() == Some(true) {
                plist.push_str(&format!("\t<true/>\n"));
            } else {
                // Complex keep_alive - just use true for simplicity
                plist.push_str(&format!("\t<true/>\n"));
            }
        } else if keep_alive.as_bool() == Some(true) {
            plist.push_str(&format!("\t<true/>\n"));
        }
    }

    // WorkingDirectory
    if let Some(working_dir) = &service.working_dir {
        let resolved = resolve_homebrew_prefix(working_dir, env_root);
        plist.push_str(&format!("\t<key>WorkingDirectory</key>\n"));
        plist.push_str(&format!("\t<string>{}</string>\n", resolved));
    }

    // StandardOutPath (log_path)
    if let Some(log_path) = &service.log_path {
        let resolved = resolve_homebrew_prefix(log_path, env_root);
        plist.push_str(&format!("\t<key>StandardOutPath</key>\n"));
        plist.push_str(&format!("\t<string>{}</string>\n", resolved));
    }

    // StandardErrorPath (error_log_path)
    if let Some(error_log_path) = &service.error_log_path {
        let resolved = resolve_homebrew_prefix(error_log_path, env_root);
        plist.push_str(&format!("\t<key>StandardErrorPath</key>\n"));
        plist.push_str(&format!("\t<string>{}</string>\n", resolved));
    }

    // EnvironmentVariables
    if let Some(env_vars) = &service.environment_variables {
        plist.push_str(&format!("\t<key>EnvironmentVariables</key>\n"));
        plist.push_str(&format!("\t<dict>\n"));
        for (key, value) in env_vars {
            plist.push_str(&format!("\t\t<key>{}</key>\n", key));
            plist.push_str(&format!("\t\t<string>{}</string>\n", escape_plist_string(value)));
        }
        plist.push_str(&format!("\t</dict>\n"));
    }

    plist.push_str("</dict>\n");
    plist.push_str("</plist>\n");

    lfs::write(&plist_path, plist.as_bytes())?;
    log::info!("Generated launchd plist: {}", plist_path.display());

    Ok(plist_path)
}

/// Generate a systemd service file from a service definition
///
/// The service file is written to `{env_root}/etc/systemd/system/{service_name}.service`
#[cfg(target_os = "linux")]
pub fn generate_systemd_service(
    env_root: &Path,
    service_name: &str,
    service: &BrewService,
) -> Result<std::path::PathBuf> {
    let service_dir = crate::dirs::path_join(env_root, &["etc", "systemd", "system"]);
    lfs::create_dir_all(&service_dir)?;

    let service_path = service_dir.join(format!("{}.service", service_name));

    // Build the run command
    let program_args = resolve_run_command(&service.run, env_root);
    let exec_start = program_args.join(" ");

    // Build the service file content
    let mut content = String::new();

    content.push_str("[Unit]\n");
    content.push_str(&format!("Description=Homebrew service: {}\n", service_name));
    content.push_str("After=network.target\n");
    content.push_str("\n");

    content.push_str("[Service]\n");
    content.push_str(&format!("ExecStart={}\n", exec_start));

    // Service type
    let service_type = service.run_type.as_deref().unwrap_or("simple");
    if service_type == "immediate" {
        content.push_str("Type=simple\n");
    } else {
        content.push_str(&format!("Type={}\n", service_type));
    }

    // KeepAlive -> Restart
    if let Some(keep_alive) = &service.keep_alive {
        if let Some(obj) = keep_alive.as_object() {
            if obj.contains_key("always") && obj["always"].as_bool() == Some(true) {
                content.push_str("Restart=always\n");
            }
        } else if keep_alive.as_bool() == Some(true) {
            content.push_str("Restart=always\n");
        }
    }

    // WorkingDirectory
    if let Some(working_dir) = &service.working_dir {
        let resolved = resolve_homebrew_prefix(working_dir, env_root);
        content.push_str(&format!("WorkingDirectory={}\n", resolved));
    }

    // Environment variables
    if let Some(env_vars) = &service.environment_variables {
        for (key, value) in env_vars {
            content.push_str(&format!("Environment=\"{}={}\"", key, value));
            if value.contains("\"") || value.contains("'") {
                content.push_str(&format!("Environment='{}={}'", key, value));
            } else {
                content.push_str(&format!("Environment=\"{}={}\"", key, value));
            }
            content.push('\n');
        }
    }

    // Standard output/error
    if let Some(log_path) = &service.log_path {
        let resolved = resolve_homebrew_prefix(log_path, env_root);
        content.push_str(&format!("StandardOutput=append:{}\n", resolved));
    }
    if let Some(error_log_path) = &service.error_log_path {
        let resolved = resolve_homebrew_prefix(error_log_path, env_root);
        content.push_str(&format!("StandardError=append:{}\n", resolved));
    }

    content.push_str("\n");
    content.push_str("[Install]\n");
    content.push_str("WantedBy=default.target\n");

    lfs::write(&service_path, content.as_bytes())?;
    log::info!("Generated systemd service: {}", service_path.display());

    Ok(service_path)
}

/// Resolve the run command from the service definition
///
/// The `run` field can be:
/// - A string: "/path/to/binary arg1 arg2"
/// - An array: ["/path/to/binary", "arg1", "arg2"]
/// - An object with macos/linux keys
fn resolve_run_command(run: &Option<serde_json::Value>, env_root: &Path) -> Vec<String> {
    let run = match run {
        Some(r) => r,
        None => return Vec::new(),
    };

    let args = match run {
        serde_json::Value::String(s) => {
            // Split by whitespace (simple split, doesn't handle quoted strings)
            s.split_whitespace().map(|s| s.to_string()).collect()
        }
        serde_json::Value::Array(arr) => {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        }
        serde_json::Value::Object(obj) => {
            // Platform-specific run command
            #[cfg(target_os = "macos")]
            {
                if let Some(macos) = obj.get("macos") {
                    return resolve_run_command(&Some(macos.clone()), env_root);
                }
            }
            #[cfg(target_os = "linux")]
            {
                if let Some(linux) = obj.get("linux") {
                    return resolve_run_command(&Some(linux.clone()), env_root);
                }
            }
            Vec::new()
        }
        _ => Vec::new(),
    };

    // Resolve $HOMEBREW_PREFIX in each argument
    args.into_iter()
        .map(|arg| resolve_homebrew_prefix(&arg, env_root))
        .collect()
}

/// Resolve $HOMEBREW_PREFIX placeholder in a path
///
/// Homebrew uses $HOMEBREW_PREFIX as a placeholder that needs to be
/// replaced with the actual environment root path.
fn resolve_homebrew_prefix(path: &str, env_root: &Path) -> String {
    // Replace $HOMEBREW_PREFIX and ${HOMEBREW_PREFIX}
    let env_root_str = env_root.to_string_lossy();
    path.replace("$HOMEBREW_PREFIX", &env_root_str)
        .replace("${HOMEBREW_PREFIX}", &env_root_str)
}

/// Escape special characters for plist string values
#[cfg(target_os = "macos")]
fn escape_plist_string(s: &str) -> String {
    s.replace('&', "&amp;")
     .replace('<', "&lt;")
     .replace('>', "&gt;")
}

/// Get service name from BrewService
///
/// The name can be platform-specific (macos/linux) or use the formula name.
pub fn get_service_name(service: &BrewService, default_name: &str) -> String {
    if let Some(name) = &service.name {
        #[cfg(target_os = "macos")]
        {
            if let Some(macos) = &name.macos {
                return macos.clone();
            }
        }
        #[cfg(target_os = "linux")]
        {
            if let Some(linux) = &name.linux {
                return linux.clone();
            }
        }
    }
    default_name.to_string()
}

/// Generate service files from BrewService definition
///
/// This is the main entry point for service file generation.
/// It creates the appropriate service file for the current platform.
pub fn generate_service_files(
    env_root: &Path,
    pkgname: &str,
    service: &BrewService,
) -> Result<std::path::PathBuf> {
    let service_name = get_service_name(service, pkgname);

    #[cfg(target_os = "macos")]
    {
        generate_launchd_plist(env_root, &service_name, service)
    }

    #[cfg(target_os = "linux")]
    {
        generate_systemd_service(env_root, &service_name, service)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err(color_eyre::eyre::eyre!("Service file generation not supported on this platform"))
    }
}