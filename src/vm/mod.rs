//! VM lifecycle management.
//!
//! Provides `epkg vm` subcommands for managing VM sessions:
//! - `vm start` - Start a VM for an environment
//! - `vm stop` - Stop a running VM
//! - `vm list` - List running VMs
//! - `vm status` - Show VM status (YAML)

pub mod session;
mod start;
mod stop;
mod list;
mod status;
mod keeper;

pub use session::{VmConfig, discover_vm_session, register_vm_session, register_vm_session_simple, unregister_vm_session, is_vm_session_active, vm_socket_path_for_env};
pub use start::cmd_vm_start;
pub use stop::cmd_vm_stop;
pub use list::cmd_vm_list;
pub use status::cmd_vm_status;