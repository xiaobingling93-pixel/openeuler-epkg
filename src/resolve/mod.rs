//! Generic resolvo-based dependency solver for all package formats
//!
//! This module implements a dependency resolver using the resolvo SAT solver
//! that works across all supported package formats (RPM, DEB, APK, Conda, ArchLinux/Pacman).
//!
//! Key features:
//! - Lazy/on-demand package loading (only loads packages accessed during solving)
//! - Uses pkgkey as unique package identifier
//! - Supports multiple dependency fields (Requires, BuildRequires, Recommends, Suggests)
//! - Format-aware version comparison and constraint checking

pub mod types;
pub mod candidate;
pub mod capability;
pub mod constraint;
pub mod provider;
pub mod requirement;
