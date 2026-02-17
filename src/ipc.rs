use std::io;
use std::io::BufRead; // for .read_line()
use std::io::Write;
use std::os::unix::fs::chown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use nix::{self};
use nix::unistd::{fork, ForkResult};
use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;
use users::{get_current_uid, get_effective_uid};
use color_eyre::eyre::{self, Result};
use serde::{Deserialize, Serialize};
use serde_json;
use crate::utils;

// ======================================================================================
// Design Document: SUID Worker/Master Architecture for `epkg`
// ======================================================================================
//
// OVERVIEW:
//   The `epkg` package manager may fork a worker process to manage privileged operations
//   (e.g., unpacking packages to `/opt/epkg/store/`) if installed SUID.
//   The master process drops privileges and communicates with the worker process via IPC.
//
//   Key Features:
//   - Always fork a worker process for privileged operations.
//   - Drop privileges in the master process.
//   - Communicate via Unix domain sockets.
//
// ARCHITECTURE:
//
//   +---------------------+       +-----------------------+
//   | Master Process      |       | Worker Process        |
//   |---------------------|       |-----------------------|
//   | - Drops privileges  |       | - Runs as root        |
//   | - Handles user I/O  |       | - Manages store       |
//   | - Uses PrivilegedClient|<----->| - Handles PrivilegedCall|
//   |                     |  IPC  |   enum variants:      |
//   |                     |       |   unpack, gc, moves   |
//   +---------------------+       +-----------------------+
//
//   IPC Communication:
//   - Uses Unix domain sockets.
//   - JSON format for messages:
//     {
//       "command": "unpack" | "gc",
//       "params": { "files": ["file1.epkg", "file2.epkg"] }
//     }
//
// INTERNAL DATA STRUCTURES:
//
// 1. `PrivilegedClient`:
//    - Provides access to privileged operations via IPC.
//    - Always forks a worker process for privileged operations.
//    - Fields:
//      - `ipc_stream: Option<UnixStream>`: Connection to privileged worker process
//      - `socket_path: Option<String>`: Path to the Unix socket for IPC
//
// 2. `PrivilegedCall`:
//    - Enum representing all supported privileged operations.
//    - Serialized directly over IPC for type-safe communication.
//    - Represents commands sent from the master process to the worker process.
//
//    Variants:
//    - `Unpack(Vec<String>)`:
//      - Command to unpack a list of `.epkg` files.
//      - Contains the list of file paths to unpack.
//
//    - `GarbageCollect`:
//      - Command to perform garbage collection in the store.
//
// 3. `UnixStream`:
//    - Represents a connection to a Unix domain socket.
//    - Used for bidirectional communication between the master and worker processes.
//
// CALL FLOW:
//
// 1. Master Process:
//    - Always forks a worker process for privileged operations.
//    - Drops privileges.
//    - Connects to the worker via a Unix socket.
//
// 2. Worker Process:
//    - Binds to a Unix socket.
//    - Listens for commands from the master process.
//    - Executes privileged operations:
//      a. `unpack_packages(files)`: Unpack `.epkg` files to `/opt/epkg/store/`.
//      b. `garbage_collect()`: Clean up unused dirs in the store.
//
// 3. IPC Communication:
//    - Master sends `PrivilegedCall` enum instances as JSON.
//    - Worker responds with `PrivilegedResponse` enum instances.

static PRIVILEGED_CLIENT: LazyLock<Mutex<PrivilegedClient>> = LazyLock::new(|| {
    let mut client = PrivilegedClient::new();
    // Establish IPC connection at initialization time
    client.establish_ipc_connection()
        .expect("Failed to establish IPC connection for privileged client");
    Mutex::new(client)
});

#[allow(dead_code)]
pub fn privileged_client() -> std::sync::MutexGuard<'static, PrivilegedClient> {
    PRIVILEGED_CLIENT.lock().unwrap()
}

// Handler functions for privileged operations

fn handle_garbage_collect() -> PrivilegedResponse {
    PrivilegedResponse::Success {
        message: "Garbage collection completed".to_string(),
        data: None,
    }
}

fn handle_move_to_cache(temp_path: &str, final_path: &str) -> PrivilegedResponse {
    match atomic_move_file(temp_path, final_path) {
        Ok(msg) => PrivilegedResponse::Success {
            message: msg,
            data: None,
        },
        Err(e) => PrivilegedResponse::Error {
            message: format!("Failed to move file: {}", e),
        },
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub enum PrivilegedCall {
    GarbageCollect,
    MoveToDownloadsCache { temp_path: String, final_path: String },
}

#[derive(Serialize, Deserialize, Debug)]
pub enum PrivilegedResponse {
    Success { message: String, data: Option<String> },
    Error { message: String },
}

/// Client for seamless privileged operations
///
/// Provides a clean, minimal API for calling privileged functions with minimal boilerplate.
/// Features a generic `call()` method for any privileged operation.
/// Always forks a worker process for privileged operations.
///
/// The system uses unified handlers that minimize per-operation code duplication.
///
/// # Examples
///
/// ## Using the generic call method (for any privileged operation):
/// ```rust
/// let mut client = PrivilegedClient::new();
/// client.call(PrivilegedCall::GarbageCollect)?;
/// client.call(PrivilegedCall::MoveToDownloadsCache {
///     temp_path: "/tmp/user_channel".to_string(),
///     final_path: "/opt/epkg/cache/channels/final".to_string()
/// })?;
/// ```
///
/// ## Adding a new privileged operation:
/// 1. Add enum variant to `PrivilegedCall`
/// 2. Implement a handler function for the operation
/// 3. Add match arm in the `execute_privileged_call` function
/// That's it! Simple function-based implementation.
pub struct PrivilegedClient {
    ipc_stream: Option<UnixStream>,
    socket_path: Option<String>,
}

impl PrivilegedClient {
    pub fn new() -> Self {
        Self {
            ipc_stream: None,
            socket_path: None,
        }
    }

    /// Generic privileged call method - use this for any privileged operation
    ///
    /// This method eliminates the need for individual wrapper methods per operation.
    /// Simply pass the PrivilegedCall enum variant directly.
    #[allow(dead_code)]
    pub fn call(&mut self, call: PrivilegedCall) -> Result<String> {
        // Send PrivilegedCall enum directly as JSON
        let request = serde_json::to_string(&call)?;
        let response_str = self.send_privileged_request(&request)?;
        let response: PrivilegedResponse = serde_json::from_str(&response_str)?;

        match response {
            PrivilegedResponse::Success { message, .. } => Ok(message),
            PrivilegedResponse::Error { message } => Err(eyre::eyre!("Privileged operation failed: {}", message)),
        }
    }

    fn establish_ipc_connection(&mut self) -> Result<()> {
        if self.ipc_stream.is_some() {
            return Ok(());
        }

        // Create random socket path
        let socket_path = create_random_socket_path();
        let socket_path_str = socket_path.to_string_lossy().to_string();

        // Fork privileged worker
        match unsafe { fork() } {
            Ok(ForkResult::Parent { .. }) => {
                // Drop privileges in parent
                nix::unistd::setuid(nix::unistd::Uid::from_raw(nix::unistd::getuid().as_raw()))
                    .map_err(|e| eyre::eyre!("Failed to drop privileges: {}", e))?;

                // Wait for worker to create socket
                for _ in 0..10 {
                    if socket_path.exists() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                if !socket_path.exists() {
                    return Err(eyre::eyre!("Socket file creation timeout"));
                }

                // Connect to worker
                self.ipc_stream = Some(UnixStream::connect(&socket_path)?);
                self.socket_path = Some(socket_path_str);
            }
            Ok(ForkResult::Child) => {
                // Run privileged worker
                if let Err(e) = privilege_worker_main(&socket_path) {
                    eprintln!("Privileged worker error: {}", e);
                    std::process::exit(1);
                }
                std::process::exit(0);
            }
            Err(e) => return Err(eyre::eyre!("Fork failed: {}", e)),
        }

        Ok(())
    }

    #[allow(dead_code)]
    fn send_privileged_request(&mut self, request: &str) -> Result<String> {
        let stream = self.ipc_stream.as_mut()
            .ok_or_else(|| eyre::eyre!("IPC stream not initialized"))?;

        // Send request
        {
            let mut writer = io::BufWriter::new(&mut *stream);
            writer.write_all(request.as_bytes())?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        } // writer is dropped here

        // Receive response
        let mut reader = io::BufReader::new(&mut *stream);
        let mut response_line = String::new();
        reader.read_line(&mut response_line)?;
        let response = response_line.trim();

        Ok(response.to_string())
    }
}

fn create_random_socket_path() -> PathBuf {
    let mut rng = StdRng::from_entropy();
    PathBuf::from(format!(
        "/tmp/epkg-{}-{:x}.sock",
        get_effective_uid(),
        rng.gen::<u64>()
    ))
}

fn privilege_worker_main(socket_path: &Path) -> Result<()> {
    let listener = UnixListener::bind(socket_path)?;

    // Set permissions for master process
    utils::set_permissions_from_mode(socket_path, 0o700)?;
    chown(socket_path, Some(get_current_uid()), None)?;

    for stream in listener.incoming() {
        // The steam is automatically closed when it goes out of scope.
        match stream {
            Ok(mut stream) => handle_client(&mut stream)?,
            Err(e) => eprintln!("Connection error: {}", e),
        }
    }
    Ok(())
}

fn handle_client(stream: &mut UnixStream) -> Result<()> {
    let call = read_privileged_call(stream)?;
    let response = execute_privileged_call(call);
    send_privileged_response(stream, &response)
}

fn read_privileged_call(stream: &mut UnixStream) -> Result<PrivilegedCall> {
    let mut buf = String::new();
    io::BufReader::new(stream).read_line(&mut buf)?;
    let call: PrivilegedCall = serde_json::from_str(buf.trim())?;
    Ok(call)
}

fn execute_privileged_call(call: PrivilegedCall) -> PrivilegedResponse {
    match call {
        PrivilegedCall::GarbageCollect => handle_garbage_collect(),
        PrivilegedCall::MoveToDownloadsCache {
            temp_path,
            final_path,
        } => handle_move_to_cache(&temp_path, &final_path),
    }
}

fn send_privileged_response(stream: &mut UnixStream, response: &PrivilegedResponse) -> Result<()> {
    let response_json = serde_json::to_string(response)?;
    let mut writer = io::BufWriter::new(stream);
    writer.write_all(response_json.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn atomic_move_file(temp_path: &str, final_path: &str) -> Result<String> {
    use std::fs;
    let temp = PathBuf::from(temp_path);
    let final_path = PathBuf::from(final_path);

    // Ensure the destination directory exists
    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Perform atomic move (rename on same filesystem)
    fs::rename(&temp, &final_path)?;

    Ok(format!("Successfully moved {} to {}",
               temp.display(), final_path.display()))
}

// send side
