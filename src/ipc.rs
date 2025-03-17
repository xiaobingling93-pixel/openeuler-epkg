use std::fs;
use std::io;
use std::io::BufRead; // for .read_line()
use std::io::Write;
use std::os::unix::fs::chown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use nix::{self};
use nix::unistd::{fork, setuid, Uid, ForkResult};
use rand::Rng;
use users::{get_current_uid, get_effective_uid};
use anyhow::Result;
use serde_json::{json, Value};
use crate::models::*;

// ======================================================================================
// Design Document: SUID Worker/Master Architecture for `epkg`
// ======================================================================================
//
// OVERVIEW:
//   The `epkg` package manager supports SUID installation, where it forks a worker process
//   to manage privileged operations (e.g., unpacking packages to `/opt/epkg/store/`).
//   The master process drops privileges and communicates with the worker process via IPC.
//
//   Key Features:
//   - Fork a worker process if `epkg` is SUID.
//   - Drop privileges in the master process.
//   - Communicate via Unix domain sockets.
//   - Provide transparent wrapper functions for privileged operations.
//
// ARCHITECTURE:
//
//   +---------------------+       +-----------------------+
//   | Master Process      |       | Worker Process        |
//   |---------------------|       |-----------------------|
//   | - Drops privileges  |       | - Runs as root        |
//   | - Handles user I/O  |       | - Manages store       |
//   | - Calls wrappers    |<----->| - Exposes 2 APIs:     |
//   |                     |  IPC  |   1) unpack_packages  |
//   |                     |       |   2) garbage_collect  |
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
// 1. `PackageManager`:
//    - Manages the state of the package manager and IPC communication.
//
//    Fields:
//    - `has_worker_process: bool`:
//      - Indicates whether a worker process has been forked.
//      - Set to `true` if `epkg` is SUID and a worker process is running.
//      - Used to determine whether to use IPC or direct function calls.
//
//    - `ipc_socket: String`:
//      - Path to the Unix domain socket used for IPC communication.
//      - Created by the master process and passed to the worker process.
//      - Used by the master process to connect to the worker.
//
//    - `ipc_stream: Option<UnixStream>`:
//      - The Unix stream for IPC communication.
//      - Initialized when the master process connects to the worker process.
//      - Used to send commands and receive responses.
//
//    - `ipc_connected: bool`:
//      - Indicates whether the master process is connected to the worker process.
//      - Set to `true` after successfully connecting to the worker.
//      - Used to avoid reconnecting unnecessarily.
//
// 2. `WorkerCommand`:
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
//    - Checks if `epkg` is SUID.
//    - If SUID:
//      a. Forks a worker process.
//      b. Drops privileges.
//      c. Connects to the worker via a Unix socket.
//    - If not SUID:
//      a. Directly calls privileged functions.
//
// 2. Worker Process:
//    - Binds to a Unix socket.
//    - Listens for commands from the master process.
//    - Executes privileged operations:
//      a. `unpack_packages(files)`: Unpack `.epkg` files to `/opt/epkg/store/`.
//      b. `garbage_collect()`: Clean up unused dirs in the store.
//
// 3. IPC Communication:
//    - Master sends JSON commands to the worker.
//    - Worker responds with JSON status messages.
//
// USAGE:
//
// 1. Transparent Wrapper Functions:
//    - `unpack_packages(files)`: Unpacks `.epkg` files.
//    - `garbage_collect()`: Performs garbage collection.
//
//    Those functions automatically handle SUID/non-SUID cases:
//    - If SUID, they send commands to the worker process via IPC.
//    - If not SUID, they directly call the underlying functions.
//
// 2. Example Usage in `PackageManager`:
//    - `install_packages()`: Downloads packages and calls `unpack_packages()`.
//    - `store_gc()`: Calls `garbage_collect()`.
//
// 3. Example Usage in `main()`:
//    - Call `fork_on_suid()` before performing privileged operations.
//    - Call `privdrop_on_suid()` to drop privileges if necessary.
//
// IMPLEMENTATION DETAILS:
//
// 1. `store.rs`:
//    - Contains the privileged functions:
//      a. `unpack_packages(files)`: Unpacks `.epkg` files to `/opt/epkg/store/`.
//      b. `garbage_collect()`: Cleans up unused files in the store.
//
// 2. `privdrop_on_suid()`:
//    - Drops privileges if `epkg` is SUID.
//
// 3. `fork_on_suid()`:
//    - Forks a worker process if `epkg` is SUID.
//    - Sets up IPC communication.
//
// 4. IPC Communication:
//    - Uses Unix domain sockets.
//    - JSON format for messages:
//      {
//        "command": "unpack" | "gc",
//        "params": { "files": ["file1.epkg", "file2.epkg"] }
//      }
//
// 5. Transparent Client Wrappers in master process and PackageManager:
//    - `unpack_packages(files)`:
//      - If SUID, sends IPC command to worker.
//      - If not SUID, directly calls `crate::store::unpack_packages()`.
//    - `garbage_collect()`:
//      - If SUID, sends IPC command to worker.
//      - If not SUID, directly calls `crate::store::garbage_collect()`.
//
// EXAMPLE CALLERS:
//
// 1. `main()`:
//    ```
//    if let Some(matches) = matches.subcommand_matches("remove") {
//        if let Some(package_specs) = matches.get_many::<String>("package-spec") {
// =>         package_manager.fork_on_suid()?;
//            package_manager.remove_packages(package_specs)?;
//        }
//    }
//    ```
//
// 2. `PackageManager::install_packages()`:
//    ```
//    let files = self.download_packages(&packages_to_install)?;
// => self.unpack_packages(files)?;
//    self.installed_packages.extend(packages_to_install);
//    self.save_installed_packages()?;
//    ```
//
// 3. `PackageManager::store_gc()`:
//    ```
// => self.garbage_collect()?;
//    ```
//
// ======================================================================================

#[derive(Debug)]
enum WorkerCommand {
    Download(Vec<String>, String, usize, usize, Option<String>),
    CreateDir(PathBuf),
    UntarZst(String, String, bool),
    Unzst(String, String),
    PermAndOwner(String),
}

pub fn privdrop_on_suid() {
    if is_suid() {
        setuid(Uid::from_raw(get_current_uid())).expect("Failed to drop privileges");
    }
}

impl PackageManager {
    pub fn fork_on_suid(&mut self) -> Result<()> {
        if is_suid() {
            let socket_path = create_random_socket_path();

            match unsafe { fork() } {
                Ok(ForkResult::Parent { child, .. }) => {
                    setuid(Uid::from_raw(get_current_uid())).expect("Failed to drop privileges");
                    self.has_worker_process = true;
                    self.child_pid = Some(child); 
                }
                Ok(ForkResult::Child) => {
                    privilege_worker_main(&socket_path)?;
                }
                Err(e) => panic!("Fork failed: {}", e),
            }

            self.ipc_socket = socket_path.to_string_lossy().into_owned();
        }
        Ok(())
    }
}

impl Drop for PackageManager {
    fn drop(&mut self) {
        // remove socket file 
        if !self.ipc_socket.is_empty() {
            let _ = std::fs::remove_file(&self.ipc_socket);
        }
        // kill work process
        if let Some(pid) = self.child_pid {
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
        }
    }
}

fn is_suid() -> bool {
    get_current_uid() != get_effective_uid()
}

fn create_random_socket_path() -> PathBuf {
    let mut rng = rand::thread_rng();
    PathBuf::from(format!(
        "/tmp/epkg-{}-{:x}.sock",
        get_effective_uid(),
        rng.gen::<u64>()
    ))
}

fn privilege_worker_main(socket_path: &Path) -> Result<()> {
    let listener = UnixListener::bind(socket_path)?;

    // Set permissions for master process
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o700))?;
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
    let command = read_command(stream)?;
    match command {
        WorkerCommand::Download(urls, output_dir, nr_parallel, max_retries, proxy) => {
            let res = crate::download::download_urls(urls, &output_dir, nr_parallel, max_retries, proxy.as_deref())
                .and_then(|_| send_response(
                        stream,
                        json!({"status": "success", "message": "Downloaded files"})
                ))
                .or_else(|e| send_response(
                        stream,
                        json!({"status": "error", "message": e.to_string()})
                ));
            res?;
        }
        WorkerCommand::CreateDir(path) => {
            let res = fs::create_dir_all(&path)
                .map_err(anyhow::Error::from)
                .and_then(|_| send_response(
                        stream,
                        json!({"status": "success", "message": "Created directory"})
                ))
                .or_else(|e| send_response(
                        stream,
                        json!({"status": "error", "message": e.to_string()})
                ));
            res?;
        }
        WorkerCommand::UntarZst(file_path, output_dir, package_flag) => {
            let res = crate::store::untar_zst(&file_path, &output_dir, package_flag)
                .and_then(|_| send_response(
                        stream,
                        json!({"status": "success", "message": "Unpacked tar zst file"})
                ))
                .or_else(|e| send_response(
                        stream,
                        json!({"status": "error", "message": e.to_string()})
                ));
            res?;
        }
        WorkerCommand::Unzst(input_path, output_path) => {
            let res = crate::store::unzst(&input_path, &output_path)
                .and_then(|_| send_response(
                        stream,
                        json!({"status": "success", "message": "Unpacked zst file"})
                ))
                .or_else(|e| send_response(
                        stream,
                        json!({"status": "error", "message": e.to_string()})
                ));
            res?;
        }
        WorkerCommand::PermAndOwner(dir_str) => {
            let res = crate::store::set_perm_and_owner(&dir_str)
                .and_then(|_| send_response(
                        stream,
                        json!({"status": "success", "message": "Set permission and owner"})
                ))
                .or_else(|e| send_response(
                        stream,
                        json!({"status": "error", "message": e.to_string()})
                ));
            res?;
        }
    }
    Ok(())
}

fn read_command(stream: &mut UnixStream) -> Result<WorkerCommand> {
    let mut buf = String::new();
    io::BufReader::new(stream).read_line(&mut buf)?;

    let value: Value = serde_json::from_str(&buf)?;
    match value["command"].as_str() {
        Some("download") => Ok(WorkerCommand::Download(
            value["params"]["urls"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            value["params"]["output_dir"].as_str().unwrap().to_string(),
            value["params"]["nr_parallel"].as_u64().unwrap() as usize,
            value["params"]["max_retries"].as_u64().unwrap() as usize,
            value["params"]["proxy"].as_str().map(String::from),
        )),
        Some("create_dir") => Ok(WorkerCommand::CreateDir(
            PathBuf::from(value["params"]["path"].as_str().unwrap()),
        )),
        Some("untar_zst") => Ok(WorkerCommand::UntarZst(
            value["params"]["file_path"].as_str().unwrap().to_string(),
            value["params"]["output_dir"].as_str().unwrap().to_string(),
            value["params"]["package_flag"].as_bool().unwrap(),
        )),
        Some("unzst") => Ok(WorkerCommand::Unzst(
            value["params"]["input_path"].as_str().unwrap().to_string(),
            value["params"]["output_path"].as_str().unwrap().to_string(),
        )),
        Some("set_perm_and_owner") => Ok(WorkerCommand::PermAndOwner(
            value["params"]["dir_str"].as_str().unwrap().to_string()
        )),
        _ => Err(anyhow::Error::new(io::Error::new(io::ErrorKind::InvalidInput, "Invalid command"))),
    }
}

fn send_response(stream: &mut UnixStream, response: Value) -> Result<()> {
    let mut writer = io::BufWriter::new(stream);
    serde_json::to_writer(&mut writer, &response)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn receive_response(stream: &UnixStream, command: &str) -> Result<()> {
    let mut reader = io::BufReader::new(stream);
    let mut response_line = String::new();
    reader.read_line(&mut response_line).unwrap();
    let response: serde_json::Value = serde_json::from_str(&response_line).unwrap();
    if response["status"] == "error" {
        return Err(anyhow::anyhow!("{} command error: {}", command, response["message"].as_str().unwrap_or("Unknown error")));
    } 
    // else {
    //     println!("ipc {} command completed successfully", command);
    // }
    Ok(())
}

// send side
impl PackageManager {
    fn send_command(&mut self, command: serde_json::Value) -> Result<()> {
        // Get ipc stream
        self.ipc_stream = Some(UnixStream::connect(&self.ipc_socket)?);
        let stream = self.ipc_stream
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("IPC stream not initialized"))?;

        // Send command
        let mut writer = io::BufWriter::new(stream);
        serde_json::to_writer(&mut writer, &command)?;
        writer.write_all(b"\n")?;
        writer.flush()?;

        // Receive response
        receive_response(stream, command["command"].as_str().unwrap())?;

        Ok(())
    }

    // wrapper functions
    pub fn download_urls(&mut self, urls: Vec<String>, output_dir: &str, nr_parallel: usize, max_retries: usize, proxy: Option<&str>) -> Result<()> {
        if !self.has_worker_process {
            crate::download::download_urls(urls, output_dir, nr_parallel, max_retries, proxy)?;
        } else {
            self.send_command(
                json!({
                    "command": "download",
                    "params": {
                        "urls": urls,
                        "output_dir": output_dir,
                        "nr_parallel": nr_parallel,
                        "max_retries": max_retries,
                        "proxy": proxy
                    }
                }),
            )?;
        }
        Ok(())
    }

    pub fn create_dir(&mut self, path: &Path) -> Result<()> {
        if !self.has_worker_process {
            fs::create_dir_all(path)?;
        } else {
            self.send_command(
                json!({
                    "command": "create_dir", 
                    "params": {
                        "path": path
                    }
                })
            )?;
        }
        Ok(())
    }
    
    pub fn untar_zst(&mut self, file_path: &str, output_dir: &str, package_flag: bool) -> Result<()> {
        if !self.has_worker_process {
            crate::store::untar_zst(file_path, output_dir, package_flag)?;
        } else {
            self.send_command(
                json!({
                    "command": "untar_zst",
                    "params": {
                        "file_path": file_path,
                        "output_dir": output_dir,
                        "package_flag": package_flag
                    }
                })
            )?;
        }
        Ok(())
    }

    pub fn unzst(&mut self, input_path: &str, output_path: &str) -> Result<()> {
        if !self.has_worker_process {
            crate::store::unzst(input_path, output_path)?;
        } else {
            self.send_command(
                json!({
                    "command": "unzst",
                    "params": {
                        "input_path": input_path,
                        "output_path": output_path
                    }
                })
            )?;
        }
        Ok(())
    }

    pub fn set_perm_and_owner(&mut self, dir_str: &str) -> Result<()> {
        if !self.has_worker_process {
            crate::store::set_perm_and_owner(dir_str)?;
        } else {
            self.send_command(
                json!({
                    "command": "set_perm_and_owner",
                    "params": {
                        "dir_str": dir_str
                    }
                })
            )?;
        }
        Ok(())
    }
}
