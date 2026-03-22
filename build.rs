use std::process::Command;
use std::fs;
use std::path::Path;

// Applet gating (see `generate_busybox_modules()`):
//   • neither list     → built on all targets (incl. Windows)
//   • UNIX_ONLY        → `#[cfg(unix)]` (macOS + Linux, not Windows)
//   • LINUX_ONLY       → `#[cfg(target_os = "linux")]` (checked first; do not duplicate names in UNIX_ONLY)
//
// LINUX_ONLY = Linux kernel ioctl/syscall ABI, mount(8) Linux flags, Debian/RPM integration
//              here. Account applets that edit passwd/group via userdb live in UNIX_ONLY (macOS too).

/// Applets wired to Linux-only behavior or ship only in Linux package workflows.
const LINUX_ONLY: &[&str] = &[
    // init / VM guest plumbing (`#![cfg(target_os = "linux")]` in sources)
    "init",
    "vm_daemon",

    // Kernel modules: finit_module / init_module syscalls, /lib/modules, modules.dep
    "insmod",
    "modprobe",

    // mount(2) + linux/mount.h MsFlags; umount2; mountpoint uses rdev major/minor Linux-style
    "mount",
    "mountpoint",
    "umount",

    // IPv4 ifreq / rtentry ioctl layout — Linux ABI, not BSD/macOS-compatible as written
    "ifconfig",
    "route",

    // Debian: dpkg, systemd helpers, update-alternatives tree
    "deb_systemd_helper",
    "dpkg",
    "dpkg_divert",
    "dpkg_maintscript_helper",
    "dpkg_query",
    "dpkg_realpath",
    "dpkg_statoverride",
    "update_alternatives",

    // RPM query/install path and Lua scriptlets (epkg Linux RPM story)
    "rpm",
    "rpmlua",

    "systemd_sysusers",
    "systemd_tmpfiles",
];

/// Applets that use POSIX / Unix APIs not available on Windows (no stable substitute in-tree).
const UNIX_ONLY: &[&str] = &[
    // Ownership and mode bits
    "chgrp",
    "chmod",
    "chown",

    // Privileged root relocation
    "chroot",

    // Passwd/group via userdb: Debian-style adduser/addgroup/del*, groupadd and shadow-ish user*
    "addgroup",
    "adduser",
    "delgroup",
    "deluser",
    "groupadd",
    "groupdel",
    "useradd",
    "userdel",
    "usermod",

    // Signals, priorities, tty session
    "kill",
    "killall",
    "nice",
    "nohup",
    "pidof",
    "pkill",

    // Mount tables, metadata, device nodes, archives; install uses chmod/chown semantics.
    // sync: POSIX sync(2)/fsync; --file-system uses syncfs(2) on Linux only (see sync.rs).
    // df / ls: not listed here — built on all targets (Windows: partial; see sources).
    "install",
    "mkfifo",
    "stat",
    "sync",
    "tar",

    "tty",

    // dpkg-trigger: updates var/lib/dpkg/triggers (same layout on macOS-hosted Linux envs)
    "dpkg_trigger",
];

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

    println!("cargo::rustc-check-cfg=cfg(epkg_ntfs_ea)");
    // `ntfs_ea.rs` uses `crate::lfs::sanitize_path_for_windows` when built inside epkg (see cfg above).
    println!("cargo:rustc-cfg=epkg_ntfs_ea");
    println!(
        "cargo:rerun-if-changed=git/libkrun/src/devices/src/virtio/fs/windows/win32_pua_paths.rs"
    );

    // Link Hypervisor framework on macOS when libkrun feature is enabled
    // Check target platform via CARGO_CFG_TARGET_OS (set by Cargo for the target)
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "macos" && std::env::var("CARGO_FEATURE_LIBKRUN").is_ok() {
        println!("cargo:rustc-link-lib=framework=Hypervisor");
    }

    // Generate busybox module declarations
    generate_busybox_modules();
}

fn generate_busybox_modules() {
    let busybox_dir = Path::new("src/busybox");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let registrations_path = Path::new(&out_dir).join("busybox_modules.rs");

    let mut modules = Vec::new();
    let mut registrations = Vec::new();

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

                        let is_linux_only = LINUX_ONLY.contains(&module_name);
                        let is_unix_only = UNIX_ONLY.contains(&module_name);

                        modules.push((module_name.to_string(), is_linux_only, is_unix_only));
                        registrations.push((module_name.to_string(), cmd_name.to_string(), is_linux_only, is_unix_only));
                    }
                }
            }
        }
    }

    // Sort for consistent output
    modules.sort();
    registrations.sort_by(|a, b| a.1.cmp(&b.1));

    // Generate module declarations as a string for mod.rs
    let mut decl_code = String::new();
    decl_code.push_str("// Auto-generated module declarations - do not edit manually\n");
    for (module, is_linux_only, is_unix_only) in &modules {
        if *is_linux_only {
            decl_code.push_str(&format!("#[cfg(target_os = \"linux\")]\n"));
        } else if *is_unix_only {
            decl_code.push_str(&format!("#[cfg(unix)]\n"));
        }
        decl_code.push_str(&format!("pub mod {};\n", module));
    }

    // Generate registrations file
    let mut reg_code = String::new();
    reg_code.push_str("// Auto-generated by build.rs - do not edit manually\n");
    reg_code.push_str("// Auto-register all applets found in src/busybox/\n");
    reg_code.push_str("register_busybox_applets! {\n");
    for (module, cmd_name, is_linux_only, is_unix_only) in &registrations {
        if *is_linux_only {
            reg_code.push_str(&format!("#[cfg(target_os = \"linux\")]\n"));
        } else if *is_unix_only {
            reg_code.push_str(&format!("#[cfg(unix)]\n"));
        }
        reg_code.push_str(&format!("    ({}, \"{}\"),\n", module, cmd_name));
    }
    reg_code.push_str("}\n");

    // Write the generated files
    fs::write(&registrations_path, reg_code).expect("Failed to write generated busybox_modules.rs");

    // Write module declarations to src/busybox/
    let decl_file = busybox_dir.join("_modules_gen.rs");
    let should_write = match fs::read_to_string(&decl_file) {
        Ok(existing) => existing != decl_code,
        Err(_) => true,
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
