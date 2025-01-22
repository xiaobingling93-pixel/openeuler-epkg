use std::fs;
use std::io;
use std::io::BufRead; // for .read_line()
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use anyhow::Result;
use nix::unistd::{fork, setuid, Uid, ForkResult};
use rand::Rng;
use serde_json::{json, Value};
use users::{get_current_uid, get_effective_uid};
use crate::models::*;

// static has_worker_process: bool = false;
// static ipc_connected: bool = false;
// static ipc_stream: UnixStream;

#[derive(Debug)]
enum WorkerCommand {
    Unpack(Vec<String>),
    GarbageCollect,
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
                Ok(ForkResult::Parent { child: _, .. }) => {
                    setuid(Uid::from_raw(get_current_uid())).expect("Failed to drop privileges");
                    self.has_worker_process = true;
                }
                Ok(ForkResult::Child) => {
                    privilege_worker_main(&socket_path)?;
                    std::process::exit(0);
                }
                Err(e) => panic!("Fork failed: {}", e),
            }

            self.ipc_socket = socket_path.to_string_lossy().into_owned();
        }
        Ok(())
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
    nix::unistd::chown(socket_path, Some(Uid::from_raw(get_current_uid())), None)?;

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => handle_client(&mut stream)?,
            Err(e) => eprintln!("Connection error: {}", e),
        }
    }

    fs::remove_file(socket_path)?;
    Ok(())
}

fn handle_client(stream: &mut UnixStream) -> Result<()> {
    let command = read_command(stream)?;
    match command {
        WorkerCommand::Unpack(files) => {
            let res = crate::store::unpack_packages(files)
                .and_then(|_| send_response(
                        stream,
                        json!({"status": "success", "message": "Unpacked files"})
                ))
                .or_else(|e| send_response(
                        stream,
                        json!({"status": "error", "message": e.to_string()})
                ));

            res?;
        }
        WorkerCommand::GarbageCollect => {
            let res = crate::store::garbage_collect()
                .and_then(|_| send_response(
                        stream,
                        json!({"status": "success", "message": "GC completed"})
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
        Some("unpack") => Ok(WorkerCommand::Unpack(
            value["params"]["files"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
        )),
        Some("gc") => Ok(WorkerCommand::GarbageCollect),
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

// send side

impl PackageManager {
    fn send_command(&mut self, command: serde_json::Value) -> Result<()> {
        let stream: &UnixStream = self.get_ipc_stream()?;
        let mut writer = io::BufWriter::new(stream);
        serde_json::to_writer(&mut writer, &command)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        Ok(())
    }

    pub fn get_ipc_stream(&mut self) -> Result<&UnixStream> {
        if !self.ipc_connected {
            // Connect to the socket and store the stream in the Option
            self.ipc_stream = Some(UnixStream::connect(&self.ipc_socket)?);
            self.ipc_connected = true;
        }

        // Unwrap the Option to get the UnixStream
        self.ipc_stream
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("IPC stream not initialized"))
    }

    // wrapper functions

    pub fn unpack_packages(&mut self, files: Vec<String>) -> Result<()> {
        if !self.has_worker_process {
            crate::store::unpack_packages(files)?;
        } else {
            self.send_command(
                json!({
                    "command": "unpack",
                    "params": {
                        "files": ["package1.epkg", "package2.epkg"]
                    }
                }),
            )?;
        }
        Ok(())
    }

    pub fn garbage_collect(&mut self) -> Result<()> {
        if !self.has_worker_process {
            crate::store::garbage_collect()?;
        } else {
            self.send_command(json!({"command": "gc"}))?;
        }
        Ok(())
    }
}
