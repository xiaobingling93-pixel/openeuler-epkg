use std::path::Path;
use std::process::Command;
use color_eyre::Result;

pub fn install_apparmor_profile() -> Result<()> {
    let host_apparmor_dir = Path::new("/etc/apparmor.d");
    if !host_apparmor_dir.exists() {
        return Ok(());
    }

    let profile_src = crate::dirs::path_join(
        crate::dirs::get_epkg_src_path().as_path(),
        &["assets", "etc", "apparmor.d", "epkg"],
    );
    let profile_dst = host_apparmor_dir.join("epkg");

    if profile_src.exists() {
        if let Err(e) = std::fs::copy(&profile_src, &profile_dst) {
            if e.to_string().contains("Permission denied") {
                eprintln!("Installing AppArmor profile /etc/apparmor.d/epkg requires sudo privileges");
                let output = Command::new("sudo")
                    .args(&["sh", "-c", &format!("cp {} {} && apparmor_parser -rv {}",
                        profile_src.to_string_lossy(),
                        profile_dst.to_string_lossy(),
                        profile_dst.to_string_lossy())])
                    .output()?;
                if !output.status.success() {
                    log::warn!("Failed to install AppArmor profile: {}",
                        String::from_utf8_lossy(&output.stderr));
                    return Ok(());
                }
            } else {
                log::warn!("Failed to copy AppArmor profile: {}", e);
                return Ok(());
            }
        }
        println!("Installed epkg AppArmor profile");
    }

    Ok(())
}

pub fn remove_apparmor_profile() -> Result<()> {
    let profile_path = Path::new("/etc/apparmor.d/epkg");
    if !profile_path.exists() {
        return Ok(());
    }

    match std::fs::remove_file(&profile_path) {
        Ok(()) => {
            println!("Removed epkg AppArmor profile");
            Ok(())
        }
        Err(e) => {
            if e.to_string().contains("Permission denied") {
                eprintln!("Removing AppArmor profile /etc/apparmor.d/epkg requires sudo privileges");
                let output = Command::new("sudo")
                    .args(&["rm", &profile_path.to_string_lossy()])
                    .output()?;
                if !output.status.success() {
                    log::warn!("Failed to remove AppArmor profile: {}",
                        String::from_utf8_lossy(&output.stderr));
                } else {
                    println!("Removed epkg AppArmor profile");
                }
            }
            Ok(())
        }
    }
}