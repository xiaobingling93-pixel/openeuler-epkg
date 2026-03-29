# Dual Vsock 架构设计

## 背景

epkg 在 Windows/WHPX 环境下遇到 vsock 握手时序问题：VM 在启动后约 4.4 秒时关闭。根本原因是 Windows/WHPX 的 vsock 握手是异步的，Host 在 Guest 还未完成初始化时就尝试连接。

## 当前实现的差异

### libkrun 模式

libkrun 使用 Unix socket 作为 vsock 桥接：

```
krun_add_vsock_port2(port, unix_socket_path, listen)
→ 创建 Unix socket 文件作为 vsock 桥接
```

**Ready notification:**
- Host: `UnixListener::bind(ready_socket_path)`
- Guest: connect to vsock port 10001 → 桥接到 ready_socket_path

### QEMU 模式

QEMU 使用原生 AF_VSOCK，没有 Unix socket 文件：

**Ready notification:**
- Host: `socket(AF_VSOCK)` + `bind(port 10001)`
- Guest: connect to vsock port 10001

### 对比表

| 方面 | libkrun | QEMU |
|------|---------|------|
| vsock 实现 | Unix socket 桥接 | 原生 AF_VSOCK |
| socket 文件 | 有 | 无 |
| ready socket | Unix socket 文件路径 | AF_VSOCK port |

**根本原因**: libkrun 的 vsock 是通过 Unix socket 模拟的，而 QEMU 使用真正的 AF_VSOCK。

## 混合 Vsock 架构

### 设计思想

采用**反向模式 (Reverse Mode)** 解决时序问题：

- **首次启动** (`epkg run`): Guest 初始化完成后主动连接 Host
- **复用模式** (`epkg run --reuse`): 使用传统的正向模式，Host 连接 Guest

### 为什么反向模式能解决时序问题

```
正向模式 (Forward) - 有问题:
1. Host 创建 vsock port
2. Host 立即尝试连接 Guest
3. Guest 可能还未初始化完成 → 连接失败

反向模式 (Reverse) - 解决方案:
1. Host 创建监听 socket/pipe
2. Guest 完全初始化后连接 Host
3. Host accept 连接，通信建立
```

### 架构图

```
┌─────────────────────────────────────────────────────────────┐
│                         Host (Windows)                       │
│  ┌─────────────────┐      ┌─────────────────────────────┐  │
│  │   epkg (main)   │      │        libkrun (WHPX)        │  │
│  │                 │      │                             │  │
│  │  setup_reverse  │◄────►│  1. CreateNamedPipeW        │  │
│  │  _listener()    │      │     (\\.\pipe\vsock-N)      │  │
│  │                 │      │                             │  │
│  │  accept_reverse │◄────►│  2. Wait for Guest connect   │  │
│  │  _connection()  │      │     (blocking in thread)    │  │
│  │                 │      │                             │  │
│  └─────────────────┘      └─────────────────────────────┘  │
│           │                                               │
│           │ Named Pipe                                    │
│           ▼                                               │
│  ┌─────────────────────────────────────────────────────┐  │
│  │              Guest (Linux VM)                        │  │
│  │  ┌─────────────┐    ┌─────────────────────────────┐ │  │
│  │  │    init     │───►│  3. Read kernel cmdline      │ │  │
│  │  │             │    │     epkg.vsock_reverse=1     │ │  │
│  │  └─────────────┘    └─────────────────────────────┘ │  │
│  │         │                                           │  │
│  │         ▼                                           │  │
│  │  ┌─────────────┐    ┌─────────────────────────────┐ │  │
│  │  │  vm-daemon  │    │  4. Connect to vsock port    │ │  │
│  │  │             │───►│     (triggers vsock          │ │  │
│  │  │ run_reverse │    │      handshake)              │ │  │
│  │  │ _vsock_cli  │◄───┘                             │ │  │
│  │  │ ent()       │                                  │ │  │
│  │  └─────────────┘                                  │  │
│  │         │                                          │  │
│  │         ▼                                          │  │
│  │  ┌─────────────┐    ┌─────────────────────────────┐ │  │
│  │  │  ready notif│───►│  5. ConnectNamedPipe        │ │  │
│  │  │  (port 10001)    │     to Host's named pipe    │ │  │
│  │  └─────────────┘    └─────────────────────────────┘ │  │
│  └─────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

## 平台差异处理

### Unix (macOS/Linux)

```rust
// Unix: 使用 Unix domain socket
pub fn setup_reverse_listener(sock_path: &Path) -> Result<UnixListener>
pub fn accept_reverse_connection(listener: &UnixListener, ...) -> Result<TcpStream>
```

### Windows

```rust
// Windows: 使用 Named Pipe
pub fn setup_reverse_listener(sock_path: &Path) -> Result<WindowsReadyPipe>
pub fn accept_reverse_connection(pipe: &WindowsReadyPipe, ...) -> Result<File>
```

### 差异总结

| 特性 | Unix | Windows |
|------|------|---------|
| Socket 类型 | Unix domain socket | Named Pipe (\\.\pipe\*) |
| 监听 API | `UnixListener::bind()` | `CreateNamedPipeW()` |
| 接受连接 | `listener.accept()` | `ConnectNamedPipe()` |
| 非阻塞支持 | `set_nonblocking(true)` | 需要独立线程 |
| 超时处理 | `poll()` + `POLLIN` | `mpsc::channel` + 轮询 |

## 配置参数

### Host 端 (epkg)

```rust
// Windows 且首次运行 (非复用) 时启用反向模式
#[cfg(windows)]
let use_reverse_vsock = use_vsock && !run_options.reuse_vm;
#[cfg(not(windows))]
let use_reverse_vsock = false;  // Unix 没有时序问题
```

### Guest 端 (init/vm-daemon)

```rust
// 从 kernel cmdline 读取配置
let reverse_mode = get_cmdline_param("epkg.vsock_reverse")
    .map_or(false, |v| v == "1");
```

## 状态机

```
┌─────────────┐
│   Start     │
└──────┬──────┘
       │
       ▼
┌─────────────┐     reuse_vm=false      ┌─────────────────┐
│  Check reuse │ ───────────────────────►│  Reverse Mode   │
│   option    │                         │ (Guest→Host)    │
└──────┬──────┘                         └────────┬────────┘
       │ reuse_vm=true                         │
       ▼                                        ▼
┌─────────────┐                         ┌─────────────────┐
│ Forward Mode │                        │ Host: Create    │
│ (Host→Guest) │                        │ named pipe      │
└─────────────┘                         └────────┬────────┘
                                                 │
                                                 ▼
                                        ┌─────────────────┐
                                        │ Host: Accept    │
                                        │ connection      │
                                        └────────┬────────┘
                                                 │
                                                 ▼
                                        ┌─────────────────┐
                                        │ Guest: Connect  │
                                        │ to vsock port   │
                                        └────────┬────────┘
                                                 │
                                                 ▼
                                        ┌─────────────────┐
                                        │ Guest: Send     │
                                        │ READY signal    │
                                        └────────┬────────┘
                                                 │
                                                 ▼
                                        ┌─────────────────┐
                                        │ Bidirectional   │
                                        │ communication   │
                                        │ established     │
                                        └─────────────────┘
                                                 │
                              Shutdown or reuse requested
                                                 │
                                                 ▼
                                        ┌─────────────────┐
                              ┌────────►│ Forward Mode    │
                              │         │ (for reuse)     │
                              │         └─────────────────┘
                              │
                              └───────── Shutdown VM
```

## 关键代码路径

### Host 端

1. `build_libkrun_config()`: 决定是否使用反向模式
2. `setup_libkrun_vsock_host_sockets()`: 创建监听 socket/pipe
3. `run_reverse_vsock_mode()`: 等待 Guest 连接
4. `send_command_over_stream()`: 通过已建立的连接发送命令

### Guest 端

1. `init.rs`: 读取 kernel cmdline，决定启动模式
2. `vm_daemon.rs:run_reverse_vsock_client()`: Guest 主动连接 Host
3. `vm_daemon.rs:run_vsock_server()`: 复用时切换回正向模式

## 调试信息

在 Host 和 Guest 都添加了详细的内核日志 (kmsg) 记录：

```rust
// Guest
kmsg_write("<6>run_reverse_vsock_client: connecting to 127.0.0.1:10000\n");
kmsg_write("<6>run_reverse_vsock_client: connected to Host\n");
kmsg_write("<6>run_reverse_vsock_client: sent READY signal\n");

// Host
log::debug!("libkrun: reverse listener created on pipe {}", full);
log::debug!("libkrun: Guest connected to reverse pipe");
```

## 未来改进

1. **Unix 反向模式**: 当前 Unix 平台始终使用正向模式，未来可以统一代码路径
2. **超时调整**: 30 秒超时可以根据不同场景调整
3. **重试策略**: 连接失败时可以增加指数退避重试
4. **优雅降级**: 反向模式失败时自动回退到正向模式
