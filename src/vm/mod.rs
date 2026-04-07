//! VM lifecycle management.
//!
//! Provides `epkg vm` subcommands for managing VM sessions:
//! - `vm start` - Start a VM for an environment (all platforms)
//! - `vm stop` - Stop a running VM (all platforms)
//! - `vm list` - List running VMs (all platforms)
//! - `vm status` - Show VM status (YAML) (all platforms)
//!
//! Session management (session.rs) is used by VM backends (libkrun, qemu) for
//! cross-process VM discovery. This is needed on all platforms where VM backends run.
//!
//! Guest daemon (guest_daemon.rs) runs inside the VM to handle commands from host.

pub mod session;

mod start;
mod stop;
mod keeper;
mod list;
mod status;

#[cfg(target_os = "linux")]
pub mod guest_daemon;

#[cfg(target_os = "linux")]
pub mod client;

pub use session::{VmConfig, discover_vm_session, register_vm_session, register_vm_session_simple, unregister_vm_session, is_vm_session_active, vm_socket_path_for_env};

pub use start::cmd_vm_start;
pub use stop::cmd_vm_stop;

pub use list::cmd_vm_list;
pub use status::cmd_vm_status;