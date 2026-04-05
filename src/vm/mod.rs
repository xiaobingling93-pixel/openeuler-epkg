//! VM lifecycle management.
//!
//! Provides `epkg vm` subcommands for managing VM sessions:
//! - `vm start` - Start a VM for an environment (non-Linux only)
//! - `vm stop` - Stop a running VM (non-Linux only)
//! - `vm list` - List running VMs (all platforms)
//! - `vm status` - Show VM status (YAML) (all platforms)
//!
//! Session management (session.rs) is used by libkrun backend on non-Linux for
//! cross-process VM discovery. On Linux, QEMU VMs are managed through vm::client directly.
//!
//! Guest daemon (guest_daemon.rs) runs inside the VM to handle commands from host.

#[cfg(not(target_os = "linux"))]
pub mod session;

#[cfg(not(target_os = "linux"))]
mod start;
#[cfg(not(target_os = "linux"))]
mod stop;
#[cfg(not(target_os = "linux"))]
mod keeper;
#[cfg(not(target_os = "linux"))]
mod list;
#[cfg(not(target_os = "linux"))]
mod status;

#[cfg(target_os = "linux")]
pub mod guest_daemon;

#[cfg(target_os = "linux")]
pub mod client;

#[cfg(not(target_os = "linux"))]
pub use session::{VmConfig, discover_vm_session, register_vm_session, register_vm_session_simple, unregister_vm_session, is_vm_session_active, vm_socket_path_for_env};

#[cfg(not(target_os = "linux"))]
pub use start::cmd_vm_start;
#[cfg(not(target_os = "linux"))]
pub use stop::cmd_vm_stop;

#[cfg(not(target_os = "linux"))]
pub use list::cmd_vm_list;
#[cfg(not(target_os = "linux"))]
pub use status::cmd_vm_status;