use std::path::Path;
use crate::models::InstalledPackageInfo;

/// Set up environment variables for RPM package scriptlets
/// Sets RPM_INSTALL_PREFIX and RPM_INSTALL_PREFIX_N for relocatable packages
pub fn setup_rpm_env_vars(
    env_vars: &mut std::collections::HashMap<String, String>,
    _pkgkey: &str,
    package_info: &InstalledPackageInfo,
    store_root: &Path,
) {
    // Try to read install prefixes from package metadata
    // RPM stores install prefixes in RPMTAG_INSTPREFIXES
    // For now, we'll try to read from a metadata file if it exists
    let install_dir = store_root.join(&package_info.pkgline).join("info/install");
    let prefixes_file = install_dir.join("install_prefixes.txt");

    if let Ok(prefixes_content) = std::fs::read_to_string(&prefixes_file) {
        let prefixes: Vec<&str> = prefixes_content.lines().filter(|s| !s.trim().is_empty()).collect();
        for (index, prefix) in prefixes.iter().enumerate() {
            let var_name = if index == 0 {
                "RPM_INSTALL_PREFIX".to_string()
            } else {
                format!("RPM_INSTALL_PREFIX_{}", index)
            };
            env_vars.insert(var_name, prefix.to_string());
        }
        // Also set RPM_INSTALL_PREFIX0 for compatibility
        if !prefixes.is_empty() {
            env_vars.insert("RPM_INSTALL_PREFIX0".to_string(), prefixes[0].to_string());
        }
    }
    // If no prefixes file exists, that's okay - package is not relocatable
}
