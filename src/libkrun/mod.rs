//! libkrun VM backend for sandbox isolation across all platforms.
//!
//! ## Architecture Overview
//!
//! libkrun provides a lightweight VM-based sandbox for running isolated Linux environments.
//! It uses KVM/HVF/WHPX hypervisors for hardware-accelerated virtualization.
//!
//! **Platform Support:**
//! - Linux: Uses KVM - sandbox isolation (alternative to qemu/namespaces)
//! - macOS: Uses HVF (Hypervisor.framework) - runs Linux binaries in VM
//! - Windows: Uses WHPX (Windows Hypervisor Platform) - runs Linux binaries in VM
//!
//! ## Communication Flow (Host ↔ Guest)
//!
//! ```text
//! Host Process                          Guest VM (Linux)
//!    │                                      │
//!    │  ┌──────────────────┐                │  ┌──────────────────┐
//!    │  │    bridge.rs     │                │  │   vsock daemon   │
//!    │  │  (vsock setup)   │◄───vsock──────►│  │   (port 10000)   │
//!    │  │  Unix/Win pipe   │                │  │                  │
//!    │  └──────────────────┘                │  └──────────────────┘
//!    │                                      │
//!    │  ┌──────────────────┐                │  ┌──────────────────┐
//!    │  │    stream.rs     │                │  │   guest init     │
//!    │  │  (cmd + I/O)     │◄───vsock──────►│  │   (exec cmd)     │
//!    │  │  stdin/stdout    │                │  │   exit code      │
//!    │  └──────────────────┘                │  └──────────────────┘
//! ```
//!
//! ## Module Responsibilities
//!
//! - `core`: VM creation, configuration, lifecycle management, command execution
//! - `bridge`: vsock bridge setup (Unix sockets on Unix, named pipes on Windows)
//! - `stream`: Command streaming protocol for interactive I/O (stdin/stdout/stderr/exit)
//!
//! ## Transport Layer
//!
//! | Platform | Transport        | Notes                              |
//! |----------|------------------|------------------------------------|
//! | Linux    | Unix socket      | KVM backend                        |
//! | macOS    | Unix socket      | HVF backend                        |
//! | Windows  | Named pipe       | WHPX requires named pipe for vsock |
//!
//! ## Public API (All Platforms)
//!
//! All APIs are available on all platforms for consistency and code reuse:
//!
//! **Command Execution:**
//! - `run_command_in_krun`: Execute command in libkrun VM sandbox
//! - `execute_via_existing_vm`: Execute via existing VM session (VM reuse)
//!
//! **VM Session Management:**
//! - `is_vm_reuse_active_for_env`: Check for active VM reuse session
//! - `run_vm_daemon_mode`: Start VM in daemon mode (`epkg vm start`)
//! - `shutdown_vm_reuse_session_if_active`: End VM reuse session
//!
//! ## Why VM Reuse on All Platforms
//!
//! VM reuse provides critical benefits across all platforms:
//!
//! 1. **Performance**: Avoid VM boot overhead (~2-5 seconds) for repeated commands
//! 2. **Data Safety**: Preserve in-memory state across operations (caches, databases)
//! 3. **Stateful Operations**: Scriptlets/hooks can share VM session
//! 4. **Resource Efficiency**: One VM serves multiple commands instead of spawning new VMs
//!
//! On Linux, libkrun VM serves as a sandbox backend similar to qemu, and VM reuse
//! provides the same performance and safety benefits as on macOS/Windows.

#[cfg(feature = "libkrun")]
pub mod core;

#[cfg(feature = "libkrun")]
pub mod bridge;

#[cfg(feature = "libkrun")]
pub mod stream;

// ============================================================================
// Public API (All Platforms)
// ============================================================================

/// Check if there's an active VM reuse session for a specific env_root.
/// Returns true if there's an active VM session for the same environment.
#[cfg(feature = "libkrun")]
pub use core::is_vm_reuse_active_for_env;

/// Execute a command in a libkrun VM.
/// Creates a new VM or reuses an existing one based on run_options.
#[cfg(feature = "libkrun")]
pub use core::run_command_in_krun;

/// Execute a command via an existing VM session (VM reuse).
/// Returns None if no existing session exists.
#[cfg(feature = "libkrun")]
pub use core::execute_via_existing_vm;

/// Run VM in daemon mode for `epkg vm start` command.
/// Creates VM, registers session, and blocks until VM shuts down.
#[cfg(feature = "libkrun")]
pub use core::run_vm_daemon_mode;

/// End a VM reuse session after install/upgrade completes.
/// Cleans up VM resources and unregisters the session.
#[cfg(feature = "libkrun")]
pub use core::shutdown_vm_reuse_session_if_active;
