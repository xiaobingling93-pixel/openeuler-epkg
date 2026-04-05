//! VM lifecycle management.
//!
//! Provides `epkg vm` subcommands for managing VM sessions:
//! - `vm start` - Start a VM for an environment (non-Linux only)
//! - `vm stop` - Stop a running VM (non-Linux only)
//! - `vm list` - List running VMs (all platforms)
//! - `vm status` - Show VM status (YAML) (all platforms)
//!
//! Session management (session.rs) is available on all platforms for cross-process
//! VM discovery. The start/stop/keeper modules are only for non-Linux (libkrun backend).
//! On Linux, QEMU VMs are managed through vm_client directly.

pub mod session;

#[cfg(not(target_os = "linux"))]
mod start;
#[cfg(not(target_os = "linux"))]
mod stop;
#[cfg(not(target_os = "linux"))]
mod keeper;

mod list;
mod status;

pub use session::{VmConfig, discover_vm_session, register_vm_session, register_vm_session_simple, unregister_vm_session, is_vm_session_active, vm_socket_path_for_env};

#[cfg(not(target_os = "linux"))]
pub use start::cmd_vm_start;
#[cfg(not(target_os = "linux"))]
pub use stop::cmd_vm_stop;

pub use list::cmd_vm_list;
pub use status::cmd_vm_status;