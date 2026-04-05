//! libkrun VM backend for macOS/Windows.
//!
//! This module provides the libkrun-based VM backend for running Linux environments
//! on non-Linux platforms (macOS/Windows). It includes:
//!
//! - `core`: VM creation, configuration, and execution logic
//! - `bridge`: vsock bridge for host↔guest communication (Unix sockets / Windows named pipes)
//! - `stream`: Command streaming protocol for interactive I/O

#[cfg(feature = "libkrun")]
pub mod core;

#[cfg(feature = "libkrun")]
pub mod bridge;

#[cfg(feature = "libkrun")]
pub mod stream;

// Re-export public interface functions from core
#[cfg(all(feature = "libkrun", not(target_os = "linux")))]
pub use core::is_vm_reuse_active_for_env;

#[cfg(all(feature = "libkrun", not(target_os = "linux")))]
pub use core::execute_via_existing_vm;

#[cfg(all(feature = "libkrun", not(target_os = "linux")))]
pub use core::run_command_in_krun;

#[cfg(all(feature = "libkrun", not(target_os = "linux")))]
pub use core::run_vm_daemon_mode;

#[cfg(all(feature = "libkrun", not(target_os = "linux")))]
pub use core::shutdown_vm_reuse_session_if_active;

#[cfg(all(feature = "libkrun", target_os = "linux"))]
pub use core::is_vm_reuse_active_for_env;