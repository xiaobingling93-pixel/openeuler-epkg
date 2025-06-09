use color_eyre::eyre::{bail, eyre, Result};

// parsed from pkgline
#[derive(Debug, Clone)]
pub struct PackageLine {
    pub ca_hash: String,
    pub pkgname: String,
    pub version: String,
    pub arch: String,
}


/// Formats a package line string from its components.
/// pkgline format: {ca_hash}__{pkgname}__{version}__{arch}
pub fn format_pkgline(ca_hash: &str, pkgname: &str, version: &str, arch: &str) -> String {
    format!("{}__{}__{}__{}", ca_hash, pkgname, version, arch)
}

// Function to parse a pkgline into a PackageLine
pub fn parse_pkgline(pkgline: &str) -> Result<PackageLine> {
    let parts: Vec<&str> = pkgline.split("__").collect();
    if parts.len() < 4 {
        bail!("Invalid package line format: {}", pkgline);
    }

    let spec = PackageLine {
        ca_hash: parts[0].to_string(),
        pkgname: parts[1].to_string(),
        version: parts[2].to_string(),
        arch:    parts[3].to_string(),
    };
    Ok(spec)
}

// pkgkey format: {pkgname}__{version}__{arch}
pub fn format_pkgkey(pkgname: &str, version: &str, arch: &str) -> String {
    format!("{}__{}__{}", pkgname, version, arch)
}

pub fn pkgkey2pkgname(pkgkey: &str) -> Result<String> {
    match pkgkey.split_once("__") {
        Some((pkgname, _)) if !pkgname.is_empty() => Ok(pkgname.to_string()),
        _ => Err(eyre!("Invalid pkgkey format: {}", pkgkey)),
    }
}

pub fn pkgkey2version(pkgkey: &str) -> Result<String> {
    let parts: Vec<&str> = pkgkey.split("__").collect();
    if parts.len() != 3 {
        return Err(eyre!("Invalid pkgkey format, expected 3 parts: {}", pkgkey));
    }
    Ok(parts[1].to_string())
}

// Extract a package key from a pkgline
pub fn pkgline2pkgkey(pkgline: &str) -> Result<String> {
    let parts: Vec<&str> = pkgline.split("__").collect();
    if parts.len() < 4 {
        return Err(eyre!("Invalid pkgline format, expected at least 4 parts: {}", pkgline));
    }
    // Format as pkgname__version__arch
    Ok(format!("{}__{}__{}", parts[1], parts[2], parts[3]))
}

pub fn pkgkey2arch(pkgkey: &str) -> Result<String> {
    let parts: Vec<&str> = pkgkey.split("__").collect();
    if parts.len() != 3 {
        return Err(eyre!("Invalid pkgkey format, expected 3 parts: {}", pkgkey));
    }
    Ok(parts[2].to_string())
}

pub fn parse_pkgkey(pkgkey: &str) -> Result<(String, String, String)> {
    let parts: Vec<&str> = pkgkey.split("__").collect();
    if parts.len() != 3 {
        return Err(eyre!("Invalid pkgkey format, expected 3 parts: {}", pkgkey));
    }
    Ok((parts[0].to_string(), parts[1].to_string(), parts[2].to_string()))
}

