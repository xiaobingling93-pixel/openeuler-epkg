//! Common types and data structures for dependency resolution
//!
//! This module defines the core data structures used throughout the dependency
//! resolution system, including:
//! - DependFieldFlags: Bitflags for selecting which dependency fields to consider
//! - SolverPackageRecord: Package representation for resolvo solver
//! - SolverMatchSpec: Version set representation for dependency matching
//! - NameType: Package name type for resolvo pool interning

use std::cmp::Ordering;
use std::fmt::{Display, Formatter};
use std::ops::{BitOr, BitOrAssign};

use resolvo::utils::VersionSet;
use crate::models::{Package, PackageFormat};
use crate::parse_requires::AndDepends;

/// Represents a set of dependency fields to consider during resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DependFieldFlags(u32);

impl DependFieldFlags {
    pub const NONE: Self = Self(0);
    pub const REQUIRES: Self = Self(1 << 0);
    pub const BUILD_REQUIRES: Self = Self(1 << 1);
    pub const CHECK_REQUIRES: Self = Self(1 << 2);
    pub const RECOMMENDS: Self = Self(1 << 3);
    pub const SUGGESTS: Self = Self(1 << 4);

    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }
}

impl Default for DependFieldFlags {
    fn default() -> Self {
        Self::NONE
    }
}

impl BitOr for DependFieldFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for DependFieldFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Package record for use in resolvo pool
/// Uses pkgkey as unique identifier: {pkgname}__{version}__{arch}
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SolverPackageRecord {
    pub pkgkey: String,
    pub pkgname: String,
    pub version: String,
    pub arch: String,
    pub format: PackageFormat,
}

impl SolverPackageRecord {
    pub fn from_package(package: &Package, format: PackageFormat) -> Self {
        Self {
            pkgkey: package.pkgkey.clone(),
            pkgname: package.pkgname.clone(),
            version: package.version.clone(),
            arch: package.arch.clone(),
            format,
        }
    }
}

impl Ord for SolverPackageRecord {
    fn cmp(&self, other: &Self) -> Ordering {
        self.pkgkey.cmp(&other.pkgkey)
    }
}

impl PartialOrd for SolverPackageRecord {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Display for SolverPackageRecord {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.pkgkey)
    }
}

/// Version set representation for resolvo
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum SolverMatchSpec {
    /// A parsed dependency specification
    MatchSpec(AndDepends),
}

impl VersionSet for SolverMatchSpec {
    type V = SolverPackageRecord;
}

impl Display for SolverMatchSpec {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            SolverMatchSpec::MatchSpec(and_depends) => {
                // Format the dependency specification with constraints
                let parts: Vec<String> = and_depends
                    .iter()
                    .map(|or_depends| {
                        or_depends
                            .iter()
                            .map(|pkg_dep| {
                                if pkg_dep.constraints.is_empty() {
                                    pkg_dep.capability.clone()
                                } else {
                                    let constraint_strs: Vec<String> = pkg_dep.constraints
                                        .iter()
                                        .map(|c| {
                                            let op_str = match c.operator {
                                                crate::parse_requires::Operator::VersionEqual => "=",
                                                crate::parse_requires::Operator::VersionNotEqual => "!=",
                                                crate::parse_requires::Operator::VersionGreaterThanEqual => ">=",
                                                crate::parse_requires::Operator::VersionGreaterThan => ">",
                                                crate::parse_requires::Operator::VersionLessThanEqual => "<=",
                                                crate::parse_requires::Operator::VersionLessThan => "<",
                                                crate::parse_requires::Operator::VersionCompatible => "~",
                                                crate::parse_requires::Operator::IfInstall => "if",
                                            };
                                            format!("{}{}", op_str, c.operand)
                                        })
                                        .collect();
                                    format!("{}({})", pkg_dep.capability, constraint_strs.join(","))
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(" | ")
                    })
                    .collect();
                write!(f, "{}", parts.join(", "))
            }
        }
    }
}

/// Package name type for resolvo pool
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct NameType(pub String);

impl Display for NameType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Ord for NameType {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl PartialOrd for NameType {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
