# VM Session 文件架构：数据完整性与跨进程发现

## 概述

VM session 文件系统确保每个 env_root 只有一个活跃的 VM，防止并发 host/guest 文件操作导致数据损坏。

## 问题背景

### 数据完整性风险

当多个进程同时操作同一个环境时：
- Host 进程 A 执行 `epkg install vim`
- VM guest（属于进程 B）执行 `epkg remove vim`
- 两个操作同时修改同一文件系统 → 数据损坏

### 解决方案：ONE VM per env_root

通过文件-based session discovery：
- 任何进程都能检测已有 VM session
- `epkg install/upgrade/remove` 发现 VM 后，将命令路由到 VM guest 执行
- 防止并发 host/guest 操作

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

**Socket path**: 从 PID-based 改为 env_hash-based，使 socket path 可预测。

## Discovery 流程

```
epkg run/install/upgrade command
    │
    ├── 1. Check session file
    │       ├── File exists?
    │       │   ├── Yes → Parse JSON
    │       │   │   ├── Process alive? (signal 0 / OpenProcess)
    │       │   │   │   ├── Yes → Socket connectable?
    │       │   │   │   │   ├── Yes → REUSE SESSION ✓
    │       │   │   │   │   └── No → cleanup stale, create new
    │       │   │   │   └── No → cleanup stale, create new
    │       │   │   └── Parse error → cleanup, create new
    │       │   └── No → create new VM
    │
    └── 2. Execute command
            ├── Reuse: connect to existing socket (see limitations below)
            └── New: create VM, register session file
```

## 实现细节

### Session 注册时机

Session 在 Guest 连接成功后立即注册（在发送命令之前）：

```rust
// 在 accept_reverse_connection 成功后立即注册
if run_options.reuse_vm {
    let _ = register_vm_session(env_root, &vsock_sock_path);
}
```

这确保其他进程在第一个命令执行期间就能发现 session。

### Stale Session 清理

启动时自动清理 crashed processes 的 session 文件：
- 通过 `signal(0)` (Unix) 或 `OpenProcess` (Windows) 检查 PID 是否存活
- 清理 session file 和 socket file

### 关键代码位置

| 文件 | 功能 |
|------|------|
| `src/vm_session.rs` | Session 类型、发现、注册、清理 |
| `src/libkrun.rs` | Session 注册时机、reuse check |
| `src/main.rs` | 启动时清理、install/upgrade/remove 路由 |
| `src/run.rs` | cross-process VM detection |

## 当前限制：跨进程 VM Reuse

### Reverse Vsock 模式的架构限制

Reverse vsock 模式设计为 **in-process reuse**：
- Host 创建 UnixListener，等待 Guest 连接
- Guest 执行完命令后 reconnect 到同一个 listener
- 循环继续

当另一个 host 进程（VM2）尝试 reuse：
- VM2 连接到 VM1 的 UnixListener
- VM1 期望 Guest 连接，收到的是 host 进程
- **协议不匹配**：VM1 发送 READY signal，但 VM2 期望发送命令

### 可行的跨进程场景

1. **命令路由** (已实现): `epkg install` 发现 VM 后，将命令发送到 VM guest 执行
   - 这通过 VM daemon 的 handle_connection 处理
   - Guest 接收 host 发送的命令 JSON

2. **In-process reuse** (已实现): 同一进程的多次 `epkg run` 命令 reuse VM
   - 通过内存中的 `VM_REUSE_SESSION` 管理

3. **Future: VM Manager Daemon**: 独立进程管理所有 VM 连接
   - 所有 host 进程通过 daemon 路由命令
   - Daemon 序列化命令执行
   - 类似 Docker daemon 架构

## 测试验证

```bash
# 测试 session 发现
epkg -e test run --isolate=vm --vm-keep-timeout=30 -- bash &
sleep 5
cat ~/.epkg/run/vm-sessions/*.json

# 测试 in-process reuse (works)
epkg -e test run --isolate=vm --reuse -- bash

# 测试跨进程发现 (works for detection)
epkg -e test run --isolate=vm --reuse -- bash  # detects session
```

## 安全保证

- **ONE VM per env_root**: session file + socket path collision prevention
- **Stale cleanup**: PID liveness check prevents zombie sessions
- **Socket lock**: connection success indicates VM is truly alive

## 相关文件

- `docs/zh/architecture/vm-virtiofs-mount.md`: VM virtiofs mount architecture
- `docs/zh/architecture/virtiofs-rootfs.md`: VM rootfs architecture