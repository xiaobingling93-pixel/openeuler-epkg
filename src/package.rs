use color_eyre::eyre::{bail, eyre, Result};
use crate::models::{Package, PackageFormat};

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

#[cfg(target_os = "linux")]
pub fn pkgkey2arch(pkgkey: &str) -> Result<String> {
    parse_pkgkey_parts(pkgkey).map(|(_, _, arch)| arch.to_string())
}

/// Name, version, and architecture from a package spec (e.g. name or name:arch).
/// Used by dpkg-query and rpm-style query applets.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
pub struct PackageNVRA {
    pub name: String,
    pub version: Option<String>,
    pub arch: Option<String>,
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

/// Parse capability name and extract architecture specification
/// Returns (base_capability, architecture_spec) where architecture_spec is:
/// - Some("any") for `:any` suffix
/// - Some(arch) for specific architecture like `:amd64` or `(x86-32)`
/// - None for no architecture specification
pub fn parse_capability_architecture(capability: &str, format: PackageFormat) -> (String, Option<String>) {
    // Handle based on package format
    if format == PackageFormat::Deb {
            if let Some(colon_pos) = capability.rfind(':') {
                let base_capability = capability[..colon_pos].to_string();
                let arch_spec = capability[colon_pos + 1..].to_string();

                // Only treat as architecture spec if it's "any" or a valid architecture name
                // Valid architecture names are alphanumeric with possible hyphens/underscores
                // Common Debian architectures: any, amd64, arm64, armel, armhf, i386, etc.
                if arch_spec == "any" || is_valid_architecture_name(&arch_spec) {
                    return (base_capability, Some(arch_spec))
                }
                // Otherwise, treat the colon as part of the package name (e.g., "lib:unknown")
            }
    } else if format == PackageFormat::Rpm {
        // RPM uses parentheses for architecture specifications: capability(arch)
        // Examples: wine-cms(x86-32), wine-cms(x86-64)
        // IMPORTANT: Only treat as arch spec if it's a recognized architecture.
        // Other things in parentheses (like :lang=en) are part of the capability name.
        if let Some(open_paren) = capability.rfind('(') {
            if capability.ends_with(')') {
                let arch_spec = capability[open_paren + 1..capability.len() - 1].to_string();

                // Map RPM architecture names to package architecture names
                // Only if it's a recognized architecture, extract it as arch_spec
                if let Some(mapped_arch) = map_rpm_arch_to_package_arch(&arch_spec) {
                    let base_capability = capability[..open_paren].to_string();
                    return (base_capability, Some(mapped_arch));
                }
                // If it's not a recognized architecture, treat parentheses as part of capability name
            }
        }
    }
    // Other distros do not encode arch in require name.
    // Alpine uses prefixes like: so:, cmd:, pc:, py3.XX:, ocaml4-intf:, dbus:, etc.
    // which are not related to arch.
    (capability.to_string(), None)
}

/// Map RPM architecture specification names to package architecture names
/// RPM uses names like "x86-32", "x86-64" in capability specifications,
/// but packages use standard architecture names like "i686", "x86_64"
/// Also handles "64bit" and "32bit" specifications used in library capabilities
/// like "libavahi-client.so.3()(64bit)"
pub fn map_rpm_arch_to_package_arch(rpm_arch: &str) -> Option<String> {
    match rpm_arch {
        "x86-32" => Some("i686".to_string()),
        "x86-64" => Some("x86_64".to_string()),
        "64bit" => Some("x86_64".to_string()),
        "32bit" => Some("i686".to_string()),
        // Add other mappings as needed
        _ => None,
    }
}

/// Check if a string is a valid architecture name
/// Architecture names are typically lowercase, alphanumeric with possible hyphens/underscores
/// Common Debian architectures: amd64, arm64, armel, armhf, i386, i486, i586, i686,
/// powerpc, ppc64el, mips, mipsel, etc.
fn is_valid_architecture_name(s: &str) -> bool {
    // Known Debian architecture names (non-exhaustive but covers common cases)
    // This is a whitelist approach to avoid false positives like "unknown", "test", etc.
    const KNOWN_ARCHITECTURES: &[&str] = &[
        "amd64", "x86_64",
        "arm64", "aarch64",
        "armel", "armhf", "arm",
        "i386", "i486", "i586", "i686",
        "powerpc", "ppc", "ppc64", "ppc64el",
        "mips", "mipsel", "mips64el",
        "riscv64",
        "loongarch64",
        "s390x",
        "sparc", "sparc64",
        "alpha",
        "hppa",
        "ia64",
        "m68k",
        "sh4",
    ];

    // Check against known architectures
    KNOWN_ARCHITECTURES.contains(&s)
        // Also accept patterns that look like architecture names:
        // - Short (2-10 chars), lowercase, alphanumeric with hyphens/underscores
        // - Starts with letter, ends with alphanumeric
        || (s.len() >= 2
            && s.len() <= 10
            && s.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            && s.chars().next().map_or(false, |c| c.is_alphabetic())
            && s.chars().last().map_or(false, |c| c.is_alphanumeric())
            && s == s.to_lowercase()
            // Exclude common non-architecture words
            && !matches!(s, "unknown" | "test" | "none" | "all" | "any"))
}

/// Filter packages based on architecture specification
/// If arch_spec is "any", only allow packages with Multi-Arch: allowed/foreign (per Debian rules)
/// If arch_spec is specific architecture, filter by that architecture
/// If arch_spec is None, use default architecture filtering
pub fn filter_packages_by_arch_spec(packages: Vec<Package>, arch_spec: Option<&str>, format: PackageFormat) -> Vec<Package> {
    match arch_spec {
        Some("any") => {
            // For :any dependencies, ONLY allow packages that support Multi-Arch: allowed or foreign
            // This is a strict requirement per Debian Multi-Arch specification
            let multiarch_packages: Vec<Package> = packages.iter()
                .filter(|pkg| {
                    match &pkg.multi_arch {
                        Some(multi_arch) => {
                            let multi_arch_lower = multi_arch.to_lowercase();
                            // Only "allowed" and "foreign" can satisfy :any dependencies
                            // "same" and "no" cannot satisfy :any dependencies
                            multi_arch_lower == "allowed" || multi_arch_lower == "foreign"
                        }
                        None => {
                            // No Multi-Arch field means Multi-Arch: no (traditional behavior)
                            // Cannot satisfy :any dependency per Debian rules
                            false
                        }
                    }
                })
                .cloned()
                .collect();

            if !multiarch_packages.is_empty() {
                log::trace!(
                    "Filtered packages for :any specification: {} out of {} packages support Multi-Arch (allowed/foreign)",
                    multiarch_packages.len(),
                    packages.len()
                );
                multiarch_packages
            } else {
                log::warn!(
                    "No packages found with Multi-Arch: allowed/foreign for :any dependency. This violates Debian Multi-Arch rules. Falling back to same-architecture packages as last resort."
                );
                // Fallback to default architecture filtering as last resort
                // This is non-standard but provides graceful degradation
                filter_packages_by_arch(packages, format)
            }
        }
        Some(specific_arch) => {
            // Filter by specific architecture (e.g., :amd64, :arm64)
            // This works regardless of Multi-Arch field value
            let arch_packages: Vec<Package> = packages.iter()
                .filter(|pkg| !pkg.arch.is_empty() && pkg.arch == specific_arch)
                .cloned()
                .collect();

            log::trace!(
                "Filtered packages by specific architecture '{}': {} out of {} packages matched",
                specific_arch,
                arch_packages.len(),
                packages.len()
            );

            if !arch_packages.is_empty() {
                arch_packages
            } else {
                // If no packages match the specific architecture, return empty
                // (don't fall back to other architectures for explicit arch requests)
                packages
            }
        }
        None => {
            // No architecture suffix - use traditional same-architecture matching
            // This respects Multi-Arch: same behavior and traditional dependencies
            filter_packages_by_arch(packages, format)
        }
    }
}

// Filter packages based on architecture that matches config().common.arch
// This is to handle situation when both x86_64 and i686 packages are available with same
// pkgname and version, e.g. fedora fcitx5-qt 5.1.9-3.fc42 has 2 packages for x86_64/i686.
pub fn filter_packages_by_arch(packages: Vec<Package>, format: PackageFormat) -> Vec<Package> {
    let config = crate::models::config();
    let target_arch = config.common.arch.as_str();

    // For RPM format, noarch packages should be included regardless of target arch
    // This is standard RPM behavior - noarch packages are architecture-independent
    let is_rpm_format = format == PackageFormat::Rpm;
    // For Conda format, "all" arch packages (from noarch) should be included regardless of target arch
    // This is standard Conda behavior - noarch packages are architecture-independent
    let is_conda_format = format == PackageFormat::Conda;

    // If there are no packages with matching architecture, return all packages
    let arch_packages: Vec<Package> = packages.iter()
        .filter(|pkg| {
            // For RPM format, noarch packages should be included regardless of target arch
            // This is standard RPM behavior - noarch packages are architecture-independent
            if is_rpm_format && pkg.arch == "noarch" {
                return true;
            }
            // For Conda format, "all" arch packages (from noarch) should be included regardless of target arch
            // This is standard Conda behavior - noarch packages are architecture-independent
            if is_conda_format && pkg.arch == "all" {
                return true;
            }
            !pkg.arch.is_empty() && pkg.arch == target_arch
        })
        .cloned()
        .collect();

    log::debug!(
        "filter_packages_by_arch: target_arch='{}', format={:?}, is_rpm={}, input={} packages, output={} packages",
        target_arch,
        format,
        is_rpm_format,
        packages.len(),
        arch_packages.len()
    );

    if !arch_packages.is_empty() {
        arch_packages
    } else {
        packages
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_capability_architecture() {
        // Test Debian-style architecture specifications
        let (base, arch_spec) = parse_capability_architecture("perl:any", PackageFormat::Deb);
        assert_eq!(base, "perl");
        assert_eq!(arch_spec, Some("any".to_string()));

        let (base, arch_spec) = parse_capability_architecture("python3:amd64", PackageFormat::Deb);
        assert_eq!(base, "python3");
        assert_eq!(arch_spec, Some("amd64".to_string()));

        // Test no architecture specification
        let (base, arch_spec) = parse_capability_architecture("gcc", PackageFormat::Deb);
        assert_eq!(base, "gcc");
        assert_eq!(arch_spec, None);

        // Test Alpine shared object capabilities (should NOT be parsed as arch specs)
        let (base, arch_spec) = parse_capability_architecture("so:libc.musl-x86_64.so.1", PackageFormat::Apk);
        assert_eq!(base, "so:libc.musl-x86_64.so.1");
        assert_eq!(arch_spec, None);

        let (base, arch_spec) = parse_capability_architecture("so:libzstd.so.1", PackageFormat::Apk);
        assert_eq!(base, "so:libzstd.so.1");
        assert_eq!(arch_spec, None);

        // Test Alpine command capabilities
        let (base, arch_spec) = parse_capability_architecture("cmd:zstd", PackageFormat::Apk);
        assert_eq!(base, "cmd:zstd");
        assert_eq!(arch_spec, None);

        // Test Alpine pkg-config capabilities
        let (base, arch_spec) = parse_capability_architecture("pc:libzstd", PackageFormat::Apk);
        assert_eq!(base, "pc:libzstd");
        assert_eq!(arch_spec, None);

        // Test Alpine Python module capabilities
        let (base, arch_spec) = parse_capability_architecture("py3.12:setuptools", PackageFormat::Apk);
        assert_eq!(base, "py3.12:setuptools");
        assert_eq!(arch_spec, None);

        // Test Alpine ocaml capabilities
        let (base, arch_spec) = parse_capability_architecture("ocaml4-intf:Csexp", PackageFormat::Apk);
        assert_eq!(base, "ocaml4-intf:Csexp");
        assert_eq!(arch_spec, None);

        // Test unknown colon usage in Debian should not be treated as arch spec
        let (base, arch_spec) = parse_capability_architecture("lib:unknown", PackageFormat::Deb);
        assert_eq!(base, "lib:unknown");
        assert_eq!(arch_spec, None);

        // Test Debian package with multiple colons (should take the last one if it's a known arch)
        let (base, arch_spec) = parse_capability_architecture("lib:test:any", PackageFormat::Deb);
        assert_eq!(base, "lib:test");
        assert_eq!(arch_spec, Some("any".to_string()));

        // Test Alpine package with multiple colons (should never split)
        let (base, arch_spec) = parse_capability_architecture("so:lib:test.so.1", PackageFormat::Apk);
        assert_eq!(base, "so:lib:test.so.1");
        assert_eq!(arch_spec, None);

        // Test RPM architecture specifications
        let (base, arch_spec) = parse_capability_architecture("wine-cms(x86-32)", PackageFormat::Rpm);
        assert_eq!(base, "wine-cms");
        assert_eq!(arch_spec, Some("i686".to_string()));

        let (base, arch_spec) = parse_capability_architecture("wine-cms(x86-64)", PackageFormat::Rpm);
        assert_eq!(base, "wine-cms");
        assert_eq!(arch_spec, Some("x86_64".to_string()));

        let (base, arch_spec) = parse_capability_architecture("wine-core(x86-32)", PackageFormat::Rpm);
        assert_eq!(base, "wine-core");
        assert_eq!(arch_spec, Some("i686".to_string()));

        let (base, arch_spec) = parse_capability_architecture("wine-ldap(x86-64)", PackageFormat::Rpm);
        assert_eq!(base, "wine-ldap");
        assert_eq!(arch_spec, Some("x86_64".to_string()));

        // Test RPM library capabilities with 64bit/32bit architecture specifications
        let (base, arch_spec) = parse_capability_architecture("libavahi-client.so.3()(64bit)", PackageFormat::Rpm);
        assert_eq!(base, "libavahi-client.so.3()");
        assert_eq!(arch_spec, Some("x86_64".to_string()));

        let (base, arch_spec) = parse_capability_architecture("libavahi-common.so.3()(64bit)", PackageFormat::Rpm);
        assert_eq!(base, "libavahi-common.so.3()");
        assert_eq!(arch_spec, Some("x86_64".to_string()));

        let (base, arch_spec) = parse_capability_architecture("libfoo.so.1()(32bit)", PackageFormat::Rpm);
        assert_eq!(base, "libfoo.so.1()");
        assert_eq!(arch_spec, Some("i686".to_string()));

        // Test RPM capabilities without architecture specification
        let (base, arch_spec) = parse_capability_architecture("wine", PackageFormat::Rpm);
        assert_eq!(base, "wine");
        assert_eq!(arch_spec, None);

        // Test RPM capabilities with invalid architecture (should not parse)
        let (base, arch_spec) = parse_capability_architecture("wine-cms(invalid-arch)", PackageFormat::Rpm);
        assert_eq!(base, "wine-cms(invalid-arch)");
        assert_eq!(arch_spec, None);

        // Test RPM capabilities with parentheses but not at the end (should not parse as arch spec)
        let (base, arch_spec) = parse_capability_architecture("wine-cms(x86-32)-extra", PackageFormat::Rpm);
        assert_eq!(base, "wine-cms(x86-32)-extra");
        assert_eq!(arch_spec, None);

        // Test RPM capabilities with unmatched parentheses (should not parse)
        let (base, arch_spec) = parse_capability_architecture("wine-cms(x86-32", PackageFormat::Rpm);
        assert_eq!(base, "wine-cms(x86-32");
        assert_eq!(arch_spec, None);

        // Test that Debian format doesn't parse RPM-style parentheses
        let (base, arch_spec) = parse_capability_architecture("wine-cms(x86-32)", PackageFormat::Deb);
        assert_eq!(base, "wine-cms(x86-32)");
        assert_eq!(arch_spec, None);
    }

    #[test]
    fn test_map_rpm_arch_to_package_arch() {
        // Test valid RPM architecture mappings
        assert_eq!(map_rpm_arch_to_package_arch("x86-32"), Some("i686".to_string()));
        assert_eq!(map_rpm_arch_to_package_arch("x86-64"), Some("x86_64".to_string()));

        // Test invalid/unmapped architectures
        assert_eq!(map_rpm_arch_to_package_arch("invalid"), None);
        assert_eq!(map_rpm_arch_to_package_arch("amd64"), None);
        assert_eq!(map_rpm_arch_to_package_arch("i686"), None);
    }

    #[test]
    fn test_filter_packages_by_arch_spec_multiarch() {

        // Create test packages covering all Multi-Arch scenarios
        let mut pkg_multiarch_allowed = Package {
            pkgname: "perl".to_string(),
            version: "5.32.0".to_string(),
            arch: "amd64".to_string(),
            multi_arch: Some("allowed".to_string()),  // CAN satisfy :any
            ..Default::default()
        };
        pkg_multiarch_allowed.pkgkey = "perl__5.32.0__amd64".to_string();

        let mut pkg_multiarch_foreign = Package {
            pkgname: "python3".to_string(),
            version: "3.9.0".to_string(),
            arch: "amd64".to_string(),
            multi_arch: Some("foreign".to_string()),  // CAN satisfy :any
            ..Default::default()
        };
        pkg_multiarch_foreign.pkgkey = "python3__3.9.0__amd64".to_string();

        let mut pkg_multiarch_same = Package {
            pkgname: "libc6".to_string(),
            version: "2.31".to_string(),
            arch: "amd64".to_string(),
            multi_arch: Some("same".to_string()),     // CANNOT satisfy :any
            ..Default::default()
        };
        pkg_multiarch_same.pkgkey = "libc6__2.31__amd64".to_string();

        let mut pkg_multiarch_no = Package {
            pkgname: "some-tool".to_string(),
            version: "1.0".to_string(),
            arch: "amd64".to_string(),
            multi_arch: Some("no".to_string()),       // CANNOT satisfy :any (explicit no)
            ..Default::default()
        };
        pkg_multiarch_no.pkgkey = "some-tool__1.0__amd64".to_string();

        let mut pkg_no_multiarch = Package {
            pkgname: "gcc".to_string(),
            version: "10.0.0".to_string(),
            arch: "amd64".to_string(),
            multi_arch: None,                         // CANNOT satisfy :any (implicit no)
            ..Default::default()
        };
        pkg_no_multiarch.pkgkey = "gcc__10.0.0__amd64".to_string();

        // Test case insensitivity - Multi-Arch fields can be in different cases
        let mut pkg_multiarch_allowed_uppercase = Package {
            pkgname: "python3-pip".to_string(),
            version: "20.0".to_string(),
            arch: "amd64".to_string(),
            multi_arch: Some("ALLOWED".to_string()),  // CAN satisfy :any (case insensitive)
            ..Default::default()
        };
        pkg_multiarch_allowed_uppercase.pkgkey = "python3-pip__20.0__amd64".to_string();

        let packages = vec![
            pkg_multiarch_allowed.clone(),
            pkg_multiarch_foreign.clone(),
            pkg_multiarch_same.clone(),
            pkg_multiarch_no.clone(),
            pkg_no_multiarch.clone(),
            pkg_multiarch_allowed_uppercase.clone(),
        ];

        // Test :any filtering - should only return packages with Multi-Arch: allowed or foreign
        // Excludes: same, no, None (missing field)
        let filtered = filter_packages_by_arch_spec(packages.clone(), Some("any"), PackageFormat::Deb);
        assert_eq!(filtered.len(), 3, "Only packages with Multi-Arch: allowed/foreign should satisfy :any");

        // Should include Multi-Arch: allowed
        assert!(filtered.iter().any(|p| p.pkgkey == pkg_multiarch_allowed.pkgkey),
                "Multi-Arch: allowed should satisfy :any");

        // Should include Multi-Arch: foreign
        assert!(filtered.iter().any(|p| p.pkgkey == pkg_multiarch_foreign.pkgkey),
                "Multi-Arch: foreign should satisfy :any");

        // Should include Multi-Arch: ALLOWED (case insensitive)
        assert!(filtered.iter().any(|p| p.pkgkey == pkg_multiarch_allowed_uppercase.pkgkey),
                "Multi-Arch: ALLOWED (uppercase) should satisfy :any");

        // Should NOT include Multi-Arch: same
        assert!(!filtered.iter().any(|p| p.pkgkey == pkg_multiarch_same.pkgkey),
                "Multi-Arch: same should NOT satisfy :any");

        // Should NOT include Multi-Arch: no
        assert!(!filtered.iter().any(|p| p.pkgkey == pkg_multiarch_no.pkgkey),
                "Multi-Arch: no should NOT satisfy :any");

        // Should NOT include packages with no Multi-Arch field
        assert!(!filtered.iter().any(|p| p.pkgkey == pkg_no_multiarch.pkgkey),
                "Packages without Multi-Arch field should NOT satisfy :any");

        // Test specific architecture filtering - works regardless of Multi-Arch
        let filtered = filter_packages_by_arch_spec(packages.clone(), Some("amd64"), PackageFormat::Deb);
        assert_eq!(filtered.len(), 6, "All packages should match amd64 architecture");

        // Test no architecture specification (should use default filtering)
        let filtered = filter_packages_by_arch_spec(packages.clone(), None, PackageFormat::Deb);
        assert_eq!(filtered.len(), 6, "All packages should match default arch filtering");
    }

    #[test]
    fn test_filter_packages_by_arch_conda_all() {

        // Create test Conda packages
        let mut pkg_all = Package {
            pkgname: "glibc-amzn2-aarch64".to_string(),
            version: "2.26-5".to_string(),
            arch: "all".to_string(),  // Conda noarch packages use "all"
            ..Default::default()
        };
        pkg_all.pkgkey = "glibc-amzn2-aarch64__2.26-5__all".to_string();

        let mut pkg_x86_64 = Package {
            pkgname: "glibc-amzn2".to_string(),
            version: "2.26-5".to_string(),
            arch: "x86_64".to_string(),
            ..Default::default()
        };
        pkg_x86_64.pkgkey = "glibc-amzn2__2.26-5__x86_64".to_string();

        let mut pkg_aarch64 = Package {
            pkgname: "glibc-amzn2".to_string(),
            version: "2.26-5".to_string(),
            arch: "aarch64".to_string(),
            ..Default::default()
        };
        pkg_aarch64.pkgkey = "glibc-amzn2__2.26-5__aarch64".to_string();

        let packages = vec![pkg_all.clone(), pkg_x86_64.clone(), pkg_aarch64.clone()];

        // Test filtering: packages with arch="all" should be included regardless of target arch
        // This is similar to RPM's "noarch" behavior
        let filtered = filter_packages_by_arch(packages.clone(), PackageFormat::Conda);

        // The package with arch="all" should always be included
        assert!(filtered.iter().any(|p| p.pkgkey == pkg_all.pkgkey),
                "Conda package with arch='all' should be included regardless of target architecture");

        // Packages matching target arch should be included
        let config = crate::models::config();
        let target_arch = config.common.arch.as_str();
        if target_arch == "x86_64" {
            assert!(filtered.iter().any(|p| p.pkgkey == pkg_x86_64.pkgkey),
                    "Package with arch='x86_64' should be included when target is x86_64");
            assert!(!filtered.iter().any(|p| p.pkgkey == pkg_aarch64.pkgkey),
                    "Package with arch='aarch64' should NOT be included when target is x86_64");
        } else if target_arch == "aarch64" {
            assert!(filtered.iter().any(|p| p.pkgkey == pkg_aarch64.pkgkey),
                    "Package with arch='aarch64' should be included when target is aarch64");
            assert!(!filtered.iter().any(|p| p.pkgkey == pkg_x86_64.pkgkey),
                    "Package with arch='x86_64' should NOT be included when target is aarch64");
        }
    }

}

