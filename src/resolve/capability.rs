//! Package capability checking and normalization
//!
//! This module handles checking whether packages provide specific capabilities
//! and normalizing capability names. Key features:
//! - Capability matching against package provides (direct and bundled)
//! - Support for Fedora Bundled Software Policy (bundled() capabilities)
//! - Architecture-aware capability normalization (RPM vs Debian formats)
//! - File-based capability checking for absolute paths

use crate::parse_provides::parse_provides;
use crate::models::PackageFormat;
use crate::resolve::provider::GenericDependencyProvider;
use crate::package::pkgkey2pkgname;

impl GenericDependencyProvider {

    /// Check if a package provides a given capability
    ///
    /// IMPORTANT: The capability parameter should be cap_with_arch (e.g., "libfoo(x86-64)")
    /// if it includes architecture information. The function strips version constraints
    /// but preserves cap_with_arch, which is then matched against provide names that
    /// are also cap_with_arch (preserved by parse_provides).
    ///
    /// ## Bundled Software Support (Fedora Policy)
    ///
    /// This function also checks for `bundled()` variants. According to Fedora's
    /// Bundled Software Policy (https://docs.fedoraproject.org/en-US/fesco/Bundled_Software_policy/),
    /// packages that bundle libraries must include `Provides: bundled(library) = version`.
    ///
    /// When checking if a package provides a capability, this function will match both:
    /// - Direct provides: if package provides "php-composer(doctrine/cache)"
    /// - Bundled provides: if package provides "bundled(php-composer(doctrine/cache))"
    ///
    /// This ensures that packages providing bundled libraries can satisfy dependencies
    /// that require the unbundled capability name.
    pub fn package_provides_capability(&self, pkgkey: &str, capability: &str) -> bool {
        // Get package name from pkgkey
        let pkgname = match pkgkey2pkgname(pkgkey) {
            Ok(name) => name,
            Err(_) => {
                log::trace!(
                    "[RESOLVO] package_provides_capability: {} failed to parse pkgname, skipping",
                    pkgkey
                );
                return false;
            }
        };

        // Skip package if its name is in no_install
        if self.no_install.contains(&pkgname) {
            log::trace!(
                "[RESOLVO] package_provides_capability: {} ({}) skipped - in no_install",
                pkgkey,
                pkgname
            );
            return false;
        }

        // Try to load package info
        let package = match self.load_package_for_solvable(pkgkey) {
            Ok(pkg) => pkg,
            Err(_) => return false,
        };

        // Strip version constraints from capability (if any)
        // This preserves cap_with_arch (e.g., "libfoo(x86-64)=2.0" -> "libfoo(x86-64)")
        let (cap_without_version, _) =
            crate::parse_requires::parse_package_spec_with_version(capability, self.format);

        // Check if any provide matches the capability
        // provide_map from parse_provides contains cap_with_arch keys (atomic, never split)
        // Also check for bundled() variants: if looking for "cap", also check "bundled(cap)"
        let bundled_variant = format!("bundled({})", cap_without_version);
        for provide_str in &package.provides {
            let provide_map = parse_provides(provide_str, self.format);
            for (provide_name, _version) in provide_map {
                // Both provide_name and cap_without_version are cap_with_arch (atomic)
                // Check direct match
                if provide_name == cap_without_version {
                    log::debug!(
                        "[RESOLVO] package_provides_capability: {} provides '{}' via provide '{}'",
                        pkgkey,
                        cap_without_version,
                        provide_str
                    );
                    return true;
                }
                // Check bundled variant: if package provides "bundled(cap)", it also provides "cap"
                if provide_name == bundled_variant {
                    log::debug!(
                        "[RESOLVO] package_provides_capability: {} provides '{}' via bundled provide '{}'",
                        pkgkey,
                        cap_without_version,
                        provide_str
                    );
                    return true;
                }
            }
        }

        // If capability looks like a file path (starts with '/'), also check the files field
        if cap_without_version.starts_with('/') {
            for file_path in &package.files {
                // Check for exact match
                if file_path == &cap_without_version {
                    log::debug!(
                        "[RESOLVO] package_provides_capability: {} provides '{}' via file '{}'",
                        pkgkey,
                        cap_without_version,
                        file_path
                    );
                    return true;
                }
            }
        }

        log::trace!(
            "[RESOLVO] package_provides_capability: {} does NOT provide '{}'",
            pkgkey,
            cap_without_version
        );
        false
    }

    /// Normalize capability name by stripping architecture suffixes like `:any`
    /// Returns the base capability name used for package/provide lookup
    ///
    /// IMPORTANT: This function handles `:any` style arch suffixes (Debian format),
    /// but for RPM-style `(arch)` suffixes, the capability is cap_with_arch which
    /// should NOT be split. When capability contains `(arch)`, we preserve it as-is
    /// for provide lookups, as provide2pkgnames is keyed by cap_with_arch.
    pub fn normalize_capability_name(&self, capability: &str) -> String {
        let (base_capability, arch_spec) =
            crate::package::parse_capability_architecture(capability, self.format);

        // If arch_spec is Some, it means we successfully parsed an arch suffix
        // For RPM format with (arch), the capability is cap_with_arch and should
        // be preserved as-is for provide lookups. Only strip :any style suffixes.
        if base_capability.is_empty() {
            capability.to_string()
        } else if self.format == PackageFormat::Rpm && arch_spec.is_some() {
            // RPM format: capability is cap_with_arch (e.g., "libfoo(x86-64)")
            // Preserve it as-is for provide lookups, as provide2pkgnames is keyed by cap_with_arch
            capability.to_string()
        } else {
            // Debian format with :any or other formats: use base_capability
            // Note: For provides stored as cap_with_arch, this might not match,
            // but :any dependencies are handled differently in Debian
            base_capability
        }
    }

}
