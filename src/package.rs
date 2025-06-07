use color_eyre::eyre::{bail, eyre, Result};

// parsed from pkgline
#[derive(Debug, Clone)]
pub struct PackageLine {
    pub ca_hash: String,
    pub pkgname: String,
    pub version: String,
}


/// Formats a package line string from its components.
/// pkgline format: {ca_hash}__{pkgname}__{version}
pub fn format_pkgline(ca_hash: &str, pkgname: &str, version: &str) -> String {
    format!("{}__{}__{}", ca_hash, pkgname, version)
}

// Function to parse a pkgline into a PackageLine
pub fn parse_pkgline(pkgline: &str) -> Result<PackageLine> {
    let parts: Vec<&str> = pkgline.split("__").collect();
    if parts.len() != 3 {
        bail!("Invalid package line format: {}", pkgline);
    }

    let spec = PackageLine {
        ca_hash: parts[0].to_string(),
        pkgname: parts[1].to_string(),
        version: parts[2].to_string(),
    };
    Ok(spec)
}

// Note: pkgkey cannot include the user friendly "version" due to Dependency
// only contains package "pkgname" and "ca_hash"
pub fn format_pkgkey(pkgname: &str, pkgid: &str) -> String {
    format!("{}__{:.8}", pkgname, pkgid)
}

pub fn pkgkey2pkgname(pkgkey: &str) -> Result<String> {
    match pkgkey.split_once("__") {
        Some((pkgname, _)) if !pkgname.is_empty() => Ok(pkgname.to_string()),
        _ => Err(eyre!("Invalid pkgkey format: {}", pkgkey)),
    }
}

