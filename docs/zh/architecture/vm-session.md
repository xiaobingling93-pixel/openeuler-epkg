# VM Session 架构

## 概述

VM session 文件系统确保每个 env_root 只有一个活跃的 VM，支持跨进程 VM reuse，防止并发 host/guest 文件操作导致数据损坏。

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

## 平台支持

| 平台 | 默认后端 | --vmm 选项 |
|------|---------|-----------|
| macOS | libkrun | libkrun |
| Windows | libkrun | libkrun |
| Linux | - | qemu |

## 架构设计

### Forward Mode

所有平台统一使用 forward mode + ready port：
- **Forward mode**: Guest 监听 vsock port 10000，Host 连接
- **Ready port 10001**: Guest bind/listen 后连接到 Host，通知已准备好

这解决了 Windows/WHPX 的时序问题，并支持跨进程 reuse。

### 跨进程 VM Reuse 流程

```
Process 1: epkg vm start fuzz-alpine timeout=120
    │
    ├── 1. Check session file → not found
    ├── 2. fork()/spawn keeper subprocess
    ├── 3. Wait for session ready
    └── 4. Exit (keeper keeps running)

Keeper Process:
    │
    ├── 1. Create VM (forward mode)
    ├── 2. Wait for Guest ready
    ├── 3. Register session file
    ├── 4. krun_start_enter() blocks
    └── 5. On idle timeout: cleanup, exit

Process 2: epkg run --isolate=vm -- ls
    │
    ├── 1. Auto-detect session file → found!
    ├── 2. Verify daemon_pid alive
    ├── 3. Verify socket connectable
    ├── 4. Connect to Guest's vsock port
    └── 5. Send command (reused VM)
```

## 平台差异

| 平台 | vsock 实现 | Keeper 启动方式 |
|------|-----------|-----------------|
| macOS | Unix socket 桥接 | fork() + setsid() |
| Windows | Named Pipe 桥接 | CreateProcess(DETACHED_PROCESS) |
| Linux (QEMU) | 原生 AF_VSOCK | N/A (QEMU 是独立进程) |

## 安全保证

- **ONE VM per env_root**: session file + socket path collision prevention
- **Stale cleanup**: PID liveness check prevents zombie sessions
- **Socket lock**: connection success indicates VM is truly alive
- **Cross-process safe**: any host process can connect to Guest

## Timeout 语义

- **timeout=0（默认）**: 永不自动超时，VM 会一直运行直到手动停止
- **timeout=N**: 空闲 N 秒后自动关闭（从命令执行**完成**开始计时，不是从开始运行）
- **自动延长**：每次 `epkg run` 完成后延长 `extend` 秒

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

- `src/vm/mod.rs` - vm 模块入口
- `src/vm/session.rs` - Session 文件管理
- `src/vm/start.rs` - vm start 实现
- `src/vm/stop.rs` - vm stop 实现
- `src/vm/keeper.rs` - VM keeper 进程逻辑
- `src/libkrun.rs` - libkrun backend
- `src/busybox/vm_daemon.rs` - Guest 端 vm_daemon