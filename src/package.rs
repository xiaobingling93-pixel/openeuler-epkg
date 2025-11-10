use color_eyre::eyre::{bail, eyre, Result};

// parsed from pkgline
#[derive(Debug, Clone)]
pub struct PackageLine {
    #[allow(dead_code)]
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

// Helper function to parse pkgkey, handling pkgnames that start with "__"
pub fn parse_pkgkey_parts(pkgkey: &str) -> Result<(&str, &str, &str)> {
    // If pkgkey starts with "__", treat it as part of pkgname
    // Split from the right to get the last 2 parts (version and arch)
    let starts_with_underscores = pkgkey.starts_with("__");
    let parts: Vec<&str> = if starts_with_underscores {
        pkgkey.rsplitn(3, "__").collect()
    } else {
        pkgkey.split("__").collect()
    };

    if parts.len() != 3 {
        return Err(eyre!("Invalid pkgkey format, expected 3 parts: {}", pkgkey));
    }

    // If split from right, parts are in reverse order: [arch, version, pkgname]
    if starts_with_underscores {
        Ok((parts[2], parts[1], parts[0]))
    } else {
        Ok((parts[0], parts[1], parts[2]))
    }
}

pub fn parse_pkgkey(pkgkey: &str) -> Result<(String, String, String)> {
    parse_pkgkey_parts(pkgkey).map(|(pkgname, version, arch)| {
        (pkgname.to_string(), version.to_string(), arch.to_string())
    })
}

pub fn pkgkey2pkgname(pkgkey: &str) -> Result<String> {
    parse_pkgkey_parts(pkgkey).map(|(pkgname, _, _)| pkgname.to_string())
}

pub fn pkgkey2version(pkgkey: &str) -> Result<String> {
    parse_pkgkey_parts(pkgkey).map(|(_, version, _)| version.to_string())
}

pub fn pkgkey2arch(pkgkey: &str) -> Result<String> {
    parse_pkgkey_parts(pkgkey).map(|(_, _, arch)| arch.to_string())
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

