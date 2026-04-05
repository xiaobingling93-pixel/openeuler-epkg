# VM Session 架构

## 问题背景

pir.py 连续调用多个 epkg run 命令，每次都有 ~2-3s VM 启动开销（macOS libkrun backend）。

**根本原因**：libkrun VM 是 host 进程内的线程，进程退出时 VM 立即死亡。

**解决方案**：新增 `epkg vm` 子命令，VM keeper 进程独立运行，支持跨进程 VM 复用。

### Windows/WHPX 时序问题

epkg 在 Windows/WHPX 环境下遇到 vsock 握手时序问题：VM 在启动后约 4.4 秒时关闭。根本原因是 Windows/WHPX 的 vsock 握手是异步的，Host 在 Guest 还未完成初始化时就尝试连接。

**解决方案**：统一在所有平台使用 forward mode + ready port 架构。

## 概述

VM session 文件系统确保每个 env_root 只有一个活跃的 VM，支持跨进程 VM reuse，防止并发 host/guest 文件操作导致数据损坏。

## 架构设计

### Forward Mode + Ready Port

所有平台统一使用 forward mode + ready port：

- **Forward mode (port 10000)**: Guest 监听 vsock port 10000，Host 连接
- **Ready port (port 10001)**: Guest bind/listen 后连接到 Host，通知已准备好

**为什么不用 Reverse Mode**：

Reverse mode（Guest 连接到 Host）不支持跨进程 reuse：
- Host 的 listener 期望 Guest 连接
- 另一个 host 进程连接会导致协议冲突
- 无法实现"ONE VM per env_root"

**Forward Mode 流程**：

```
1. Host 创建 ready listener (port 10001)
2. Guest 启动，bind/listen port 10000
3. Guest 连接到 Host 的 ready port 通知就绪
4. Host 收到通知，连接到 Guest port 10000
5. 任何 host 进程都可以连接（跨进程 reuse）
```

### 架构图

```
┌─────────────────────────────────────────────────────────────┐
│                              Host                            │
│  ┌─────────────────┐      ┌─────────────────────────────┐  │
│  │   epkg (main)   │      │        libkrun/QEMU          │  │
│  │                 │      │                             │  │
│  │  setup_ready    │◄────►│  1. Create listen socket    │  │
│  │  _listener()    │      │     (Unix socket /          │  │
│  │                 │      │      Named Pipe)            │  │
│  │  wait_guest     │◄────►│  2. Wait for Guest connect  │  │
│  │  _ready()       │      │     on ready port 10001     │  │
│  └─────────────────┘      └─────────────────────────────┘  │
│           │                                               │
│           │ vsock 桥接 (Unix socket / Named Pipe)         │
│           ▼                                               │
│  ┌─────────────────────────────────────────────────────┐  │
│  │              Guest (Linux VM)                        │  │
│  │  ┌─────────────┐    ┌─────────────────────────────┐ │  │
│  │  │    init     │───►│  3. Start vm_daemon          │ │  │
│  │  └─────────────┘    └─────────────────────────────┘ │  │
│  │         │                                           │  │
│  │         ▼                                           │  │
│  │  ┌─────────────┐    ┌─────────────────────────────┐ │  │
│  │  │  vm-daemon  │───►│  4. bind/listen port 10000   │ │  │
│  │  │             │    │     (forward mode server)    │ │  │
│  │  └─────────────┘    └─────────────────────────────┘ │  │
│  │         │                                           │  │
│  │         ▼                                           │  │
│  │  ┌─────────────┐    ┌─────────────────────────────┐ │  │
│  │  │  ready notif│───►│  5. Connect to ready socket │ │  │
│  │  │ (port 10001)│    │     (Unix socket / pipe)    │ │  │
│  │  └─────────────┘    └─────────────────────────────┘ │  │
│  └─────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

### 平台实现差异

| 方面 | libkrun (macOS/Windows) | QEMU (Linux) |
|------|-------------------------|--------------|
| vsock 实现 | Unix socket / Named Pipe 桥接 | 原生 AF_VSOCK |
| socket 文件 | 有 | 无 |
| ready socket | Unix socket / Named Pipe 文件路径 | AF_VSOCK port |
| Keeper 启动 | fork() + setsid() / CreateProcess(DETACHED_PROCESS) | N/A (QEMU 是独立进程) |

**libkrun 模式**：

```
krun_add_vsock_port2(port, unix_socket_path, listen)
→ 创建 Unix socket 文件作为 vsock 桥接
```

**QEMU 模式**：

```
Host: socket(AF_VSOCK) + bind(port 10001)
Guest: connect to vsock port 10001
```

## epkg vm 命令

```bash
epkg vm start <env> [key=value ...]   # 启动 VM
epkg vm stop <env>                    # 停止 VM
epkg vm list                          # 列出 VM
epkg vm status <env>                  # YAML dump
```

### 启动选项

**--vmm** 指定 VMM 后端：
- `libkrun` (默认，macOS/Windows)
- `qemu` (Linux)

### key=value 参数

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `timeout` | 空闲超时（秒）- 从命令执行完成开始计时，0 = 永不超时 | 0 |
| `extend` | 每次 run 完成后延长的时间（秒） | 10 |
| `cpus` | VM CPU 数 | 2 |
| `memory` | VM 内存（MiB） | 1024 |

### 使用示例

```bash
# 启动 VM（后台运行，永不超时）
epkg vm start fuzz-alpine cpus=4 memory=2048

# 启动 VM（带超时）
epkg vm start fuzz-alpine timeout=120 cpus=4 memory=2048

# 在 Linux 上使用 QEMU 后端
epkg vm start fuzz-alpine --vmm qemu cpus=4

# 查看运行中的 VM
epkg vm list

# 查看 VM 详情（YAML）
epkg vm status fuzz-alpine

# 停止 VM
epkg vm stop fuzz-alpine
```

## Session 文件格式

**位置**: `{epkg_run}/vm-sessions/{env_name}.json`

```json
{
  "version": 2,
  "env_name": "fuzz-alpine",
  "env_root": "/Users/aa/.epkg/envs/fuzz-alpine",
  "daemon_pid": 12345,
  "socket_path": "/Users/aa/.epkg/run/vsock-fuzz-alpine.sock",
  "backend": "libkrun",
  "config": {
    "timeout": 0,
    "extend": 10,
    "cpus": 4,
    "memory_mib": 2048,
    "backend": "libkrun"
  },
  "created_at": 1712345678,
  "last_activity": 1712345678
}
```

## 流程图

### vm start 流程

```
┌─────────────────────────────────────────────────────────────────────────┐
│  epkg vm start fuzz-alpine timeout=120                                  │
│                                                                         │
│  主进程:                                                                 │
│  1. 检查 session → 不存在                                                │
│  2. Unix: fork() / Windows: spawn DETACHED_PROCESS                      │
│  3. 子进程执行 keeper 逻辑                                               │
│  4. 主进程等待 session ready (最多 30s)                                  │
│  5. 主进程退出                                                           │
│                                                                         │
│  子进程 (keeper):                                                        │
│  1. 创建 VM (forward mode)                                               │
│  2. 等待 Guest ready                                                     │
│  3. 注册 session file                                                    │
│  4. krun_start_enter() 阻塞                                              │
│  5. 空闲 120s 后 VM 关闭（或 timeout=0 永不关闭）                         │
│  6. 清理 session file，退出                                              │
└─────────────────────────────────────────────────────────────────────────┘
```

### epkg run 自动复用流程

```
┌─────────────────────────────────────────────────────────────────────────┐
│  epkg run --isolate=vm -- ls                                            │
│                                                                         │
│  1. VM 模式 → 自动检测 session                                           │
│  2. 发现 session → 自动设置 reuse_vm=true                                │
│  3. 验证 daemon_pid alive                                                │
│  4. 验证 socket connectable                                              │
│  5. 连接 vsock socket                                                    │
│  6. 发送命令                                                             │
│  7. Guest 执行完成后，空闲计时开始                                        │
└─────────────────────────────────────────────────────────────────────────┘
```

## 安全保证

- **ONE VM per env_root**: session file + socket path collision prevention
- **Stale cleanup**: PID liveness check prevents zombie sessions
- **Socket lock**: connection success indicates VM is truly alive
- **Cross-process safe**: any host process can connect to Guest

## Timeout 语义

- **timeout=0（默认）**: 永不自动超时，VM 会一直运行直到手动停止
- **timeout=N**: 空闲 N 秒后自动关闭（从命令执行**完成**开始计时，不是从开始运行）
- **自动延长**：每次 `epkg run` 完成后延长 `extend` 秒

## 自动复用机制

VM 模式自动检测现有 session：

```rust
// src/run.rs
#[cfg(not(target_os = "linux"))]
let has_active_vm_session = is_vm_reuse_active_for_env(env_root) ||
    crate::vm::is_vm_session_active(env_root);

if has_active_vm_session {
    run_options.reuse_vm = true;
    run_options.effective_sandbox.isolate_mode = Some(IsolateMode::Vm);
}
```

- `is_vm_reuse_active_for_env()` - 检查当前进程内的 VM session（快速路径）
- `crate::vm::is_vm_session_active()` - 检查跨进程的 VM session（磁盘文件）

无需手动指定 `--reuse`，只要检测到活跃 session 就自动复用。

## 与 pir.py 集成

```python
def run_fuzz_iteration(os_name, env_name, ...):
    # 启动 VM（永不超时）
    run_epkg(['vm', 'start', env_name], 'self')

    # 测试 executables（自动复用 VM）
    for exe in executables:
        # 无需 --reuse，自动检测 VM session
        run_epkg(['run', '--isolate=vm', '--', exe, '--help'], env_name)

    # 停止 VM
    run_epkg(['vm', 'stop', env_name], 'self')
```

## 相关文件

| 文件 | 功能 |
|------|------|
| `src/libkrun/mod.rs` | libkrun 模块入口 |
| `src/libkrun/core.rs` | libkrun VM 核心逻辑（run_vm_daemon_mode） |
| `src/libkrun/bridge.rs` | vsock 桥接（ready listener、connect、reverse mode） |
| `src/libkrun/stream.rs` | 命令流处理（send_command_via_vsock、streaming I/O） |
| `src/vm/mod.rs` | vm 模块入口 |
| `src/vm/session.rs` | Session 文件管理 |
| `src/vm/start.rs` | vm start 实现 |
| `src/vm/stop.rs` | vm stop 实现 |
| `src/vm/keeper.rs` | VM keeper 进程逻辑 |
| `src/vm/list.rs` | vm list 实现 |
| `src/vm/status.rs` | vm status 实现 |
| `src/vm/guest_daemon.rs` | Guest 端 vm_daemon |
| `src/vm/client.rs` | QEMU TCP/vsock client |
| `src/qemu.rs` | QEMU backend |
| `src/run.rs` | 自动复用机制 |

## 验证步骤

```bash
# 启动 VM
epkg vm start fuzz-alpine timeout=120

# 查看运行中的 VM
epkg vm list

# 查看 VM 详情（YAML）
epkg vm status fuzz-alpine

# 自动复用 VM（无需 --reuse）
epkg run --isolate=vm -- ls

# 停止 VM
epkg vm stop fuzz-alpine
```