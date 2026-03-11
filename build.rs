use std::process::Command;
use std::fs;
use std::path::Path;

fn main() {
    // Get git commit hash
    let git_hash = get_git_hash();

    // Get build date and full build time using time crate
    let build_date = get_build_date();
    let build_time = get_build_time();

    let epkg_version_info = format!("version {} (build date {}, commit {})",
                                    env!("CARGO_PKG_VERSION"),
                                    build_date,
                                    git_hash);

    // Set environment variables for the build
    println!("cargo:rustc-env=GIT_HASH={}", git_hash);
    println!("cargo:rustc-env=BUILD_DATE={}", build_date);
    println!("cargo:rustc-env=BUILD_TIME={}", build_time);
    println!("cargo:rustc-env=EPKG_VERSION_TAG=v{}", env!("CARGO_PKG_VERSION"));
    println!("cargo:rustc-env=EPKG_VERSION_INFO={}", epkg_version_info);

    // Generate busybox module declarations
    generate_busybox_modules();
}

fn is_linux_only(path: &Path) -> bool {
    use std::fs::File;
    use std::io::BufRead;
    use std::io::BufReader;

    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };

    let reader = BufReader::new(file);
    for line in reader.lines().take(150) { // Check first 150 lines for crate attributes (allow extensive documentation)
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.contains("#![cfg(target_os = \"linux\")]") {
            return true;
        }
        // Stop checking after we're past crate attributes and non-comment code
        if !trimmed.starts_with('#') && !trimmed.is_empty() && !trimmed.starts_with("//") && !trimmed.starts_with("/*") {
            break;
        }
    }
    false
}

fn is_unix_only(path: &Path) -> bool {
    use std::fs::File;
    use std::io::BufRead;
    use std::io::BufReader;

    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };

    let reader = BufReader::new(file);
    for line in reader.lines().take(150) { // Check first 150 lines for crate attributes (allow extensive documentation)
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.contains("#![cfg(unix)]") {
            return true;
        }
        // Stop checking after we're past crate attributes and non-comment code
        if !trimmed.starts_with('#') && !trimmed.is_empty() && !trimmed.starts_with("//") && !trimmed.starts_with("/*") {
            break;
        }
    }
    false
}

fn generate_busybox_modules() {
    let busybox_dir = Path::new("src/busybox");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let registrations_path = Path::new(&out_dir).join("busybox_modules.rs");

    let mut modules = Vec::new();
    let mut registrations = Vec::new();
    let mut linux_only_modules = Vec::new();
    let mut unix_only_modules = Vec::new();

    if let Ok(entries) = fs::read_dir(busybox_dir) {
        for entry in entries {
            if let Ok(entry) = entry {
                let path = entry.path();
                if path.is_file() {
                    if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                        // Skip mod.rs, generated files (starting with _), and any non-Rust files
                        if filename == "mod.rs"
                            || filename.starts_with("_")
                            || !filename.ends_with(".rs") {
                            continue;
                        }

                        // Extract module name (remove .rs extension)
                        let module_name = filename.trim_end_matches(".rs");

                        // Map module name to command name
                        // Convention: if module ends with _cmd, remove it (e.g., true_cmd -> "true")
                        // Convert underscores to hyphens (e.g., dpkg_query -> "dpkg-query")
                        // Special case: bracket module maps to "[" command
                        let cmd_name = if module_name == "bracket" {
                            "[".to_string()
                        } else {
                            let cmd_name_str = if module_name.ends_with("_cmd") {
                                &module_name[..module_name.len() - 4]
                            } else {
                                module_name
                            };
                            // Convert underscores to hyphens for command names
                            cmd_name_str.replace('_', "-")
                        };

                        let linux_only = is_linux_only(&path);
                        let unix_only = is_unix_only(&path);

                        modules.push(module_name.to_string());
                        registrations.push((module_name.to_string(), cmd_name.to_string(), linux_only, unix_only));
                        if linux_only {
                            linux_only_modules.push(module_name.to_string());
                        }
                        if unix_only && !linux_only { // Don't add if already in linux_only
                            unix_only_modules.push(module_name.to_string());
                        }
                    }
                }
            }
        }
    }

    // Sort for consistent output
    modules.sort();
    registrations.sort_by(|a, b| a.1.cmp(&b.1));

    // Generate module declarations as a string for mod.rs
    // We'll write this to a file that can be included
    let mut decl_code = String::new();
    decl_code.push_str("// Auto-generated module declarations - do not edit manually\n");
    for module in &modules {
        // Check if this module is Linux-only or Unix-only
        let linux_only = linux_only_modules.contains(module);
        let unix_only = unix_only_modules.contains(module);
        if linux_only {
            decl_code.push_str(&format!("#[cfg(target_os = \"linux\")]\n"));
        } else if unix_only {
            decl_code.push_str(&format!("#[cfg(unix)]\n"));
        }
        decl_code.push_str(&format!("pub mod {};\n", module));
    }

    // Generate registrations file
    let mut reg_code = String::new();
    reg_code.push_str("// Auto-generated by build.rs - do not edit manually\n");
    reg_code.push_str("// Auto-register all applets found in src/busybox/\n");
    reg_code.push_str("register_busybox_applets! {\n");
    for (module, cmd_name, linux_only, unix_only) in &registrations {
        if *linux_only {
            reg_code.push_str(&format!("#[cfg(target_os = \"linux\")]\n"));
        } else if *unix_only {
            reg_code.push_str(&format!("#[cfg(unix)]\n"));
        }
        reg_code.push_str(&format!("    ({}, \"{}\"),\n", module, cmd_name));
    }
    reg_code.push_str("}\n");

    // Write the generated files
    fs::write(&registrations_path, reg_code).expect("Failed to write generated busybox_modules.rs");

    // Write module declarations to src/busybox/ - modules must be in the same directory
    // This file is auto-generated and should be in .gitignore
    // Only write if content has changed to avoid unnecessary rebuilds
    let decl_file = busybox_dir.join("_modules_gen.rs");
    let should_write = match fs::read_to_string(&decl_file) {
        Ok(existing) => existing != decl_code,
        Err(_) => true, // File doesn't exist, need to create it
    };
    if should_write {
        fs::write(&decl_file, decl_code).expect("Failed to write generated _modules_gen.rs");
    }

    // Tell Cargo to rerun if busybox directory changes
    println!("cargo:rerun-if-changed=src/busybox");
}

fn get_git_hash() -> String {
    Command::new("git")
        .args(&["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn get_build_date() -> String {
    use time::OffsetDateTime;

    OffsetDateTime::now_utc()
        .format(&time::format_description::parse("[year]-[month]-[day]").unwrap())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn get_build_time() -> String {
    use time::OffsetDateTime;

    let format = time::format_description::parse("[year]-[month]-[day] [hour]:[minute]:[second] [offset_hour sign:mandatory][offset_minute]")
        .unwrap();
    OffsetDateTime::now_local()
        .ok()
        .and_then(|t| t.format(&format).ok())
        .unwrap_or_else(|| "unknown".to_string())
}
