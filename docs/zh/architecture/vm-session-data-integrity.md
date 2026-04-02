# VM Session 文件架构：数据完整性与跨进程发现

## 概述

VM session 文件系统确保每个 env_root 只有一个活跃的 VM，支持跨进程 VM reuse，防止并发 host/guest 文件操作导致数据损坏。

## 设计原则

### 统一使用 Forward Mode

所有平台统一使用 forward mode + ready port：
- **Forward mode**: Guest 监听 vsock port 10000，Host 连接
- **Ready port 10001**: Guest bind/listen 后连接到 Host，通知已准备好

这解决了 Windows/WHPX 的时序问题（VM 启动后约 4.4 秒关闭），并支持跨进程 reuse。

### 为什么不用 Reverse Mode

Reverse mode（Guest 连接到 Host）不支持跨进程 reuse：
- Host 的 listener 期望 Guest 连接
- 另一个 host 进程连接会导致协议冲突
- 无法实现"ONE VM per env_root"

### Forward Mode 优势

```
Forward mode 流程:
1. Host 创建 ready listener (port 10001)
2. Guest 启动，bind/listen port 10000
3. Guest 连接到 Host 的 ready port 通知就绪
4. Host 收到通知，连接到 Guest port 10000
5. 任何 host 进程都可以连接（跨进程 reuse）
```

## Session 文件格式

**位置**: `{epkg_run}/vm-sessions/{env_hash}.json`

**格式**:
```json
{
  "version": 1,
  "env_root": "/Users/aa/.epkg/envs/debian",
  "pid": 12345,
  "socket_path": "/Users/aa/.epkg/run/vsock-{env_hash}.sock",
  "created_at": 1712345678,
  "last_activity": 1712345678
}
```

**env_hash 计算**: `hash_env_root(env_root)` → 16字符十六进制字符串

**Socket path**: 使用 env_hash 命名，使 socket path 可预测，支持跨进程发现。

## 实现细节

### Session 注册时机

Session 在 Guest ready 后立即注册（在发送命令之前）：

```rust
// 在 wait_guest_ready_* 成功后立即注册
if run_options.reuse_vm {
    let _ = register_vm_session(env_root, &vsock_sock_path);
}
```

这确保其他进程在第一个命令执行期间就能发现 session。

### Ready Port 命名

Ready socket 使用 env_hash 命名：
- 路径: `{epkg_run}/ready-{env_hash}.sock`
- 与 vsock socket 命名一致
- 支持 libkrun vsock 桥接

### 关键代码位置

| 文件 | 功能 |
|------|------|
| `src/vm_session.rs` | Session 类型、发现、注册、清理 |
| `src/libkrun.rs` | Session 注册时机、forward mode 配置 |
| `src/libkrun_bridge.rs` | Ready listener 创建 |
| `src/main.rs` | 启动时清理、install/upgrade/remove 路由 |
| `src/busybox/vm_daemon.rs` | Guest 端 vm_daemon，forward mode server |

## 跨进程 VM Reuse 流程

```
Process 1: epkg -e test run --isolate=vm --reuse
    │
    ├── 1. Check session file → not found
    ├── 2. Create VM (forward mode)
    ├── 3. Wait for Guest ready
    ├── 4. Register session file
    ├── 5. Send command to Guest
    └── 6. Keep VM alive (reuse mode)

Process 2: epkg -e test run --isolate=vm --reuse
    │
    ├── 1. Check session file → found!
    ├── 2. Verify PID alive
    ├── 3. Verify socket connectable
    ├── 4. Connect to Guest's vsock port
    └── 5. Send command (reused VM)
```

## 平台支持

| 平台 | vsock 实现 | Ready Port |
|------|-----------|------------|
| macOS | Unix socket 桥接 | Unix socket |
| Windows | Named Pipe 桥接 | Named Pipe |
| Linux (QEMU) | 原生 AF_VSOCK | AF_VSOCK |

所有平台使用相同的 forward mode 架构。

## 安全保证

- **ONE VM per env_root**: session file + socket path collision prevention
- **Stale cleanup**: PID liveness check prevents zombie sessions
- **Socket lock**: connection success indicates VM is truly alive
- **Cross-process safe**: any host process can connect to Guest

## 测试验证

```bash
# Terminal 1: 启动 VM
epkg -e test run --isolate=vm --reuse -- bash

# Terminal 2: 复用 VM (works!)
epkg -e test run --isolate=vm --reuse -- whoami

# 检查 session 文件
cat ~/.epkg/run/vm-sessions/*.json
```

## 相关文件

- `docs/zh/architecture/dual-vsock.md`: Dual vsock 架构设计
- `docs/zh/architecture/vm-virtiofs-mount.md`: VM virtiofs mount architecture