# epkg dual vsock 架构设计

## 背景

在 Windows/WHPX 环境下，epkg 使用 vsock 进行 Host-Guest 通信时遇到了时序问题：

- Host 在 Named Pipe 连接成功后立即发送数据
- 但此时 vsock 握手（VSOCK_OP_REQUEST → VSOCK_OP_RESPONSE）尚未完成
- Guest 的 `accept()` 还没返回，数据丢失

## 解决方案：混合反向/正向架构

### 核心思想

| 场景 | 连接方向 | 原因 |
|------|---------|------|
| 首次 `epkg run` | Guest → Host (反向) | Guest 完全初始化后才连接，无 vsock 时序问题 |
| `epkg run --reuse` | Host → Guest (正向) | VM 已运行，vsock 稳定，保持现有架构 |

### 架构图

#### 首次启动（反向连接）

```
┌─────────────────┐                         ┌─────────────────┐
│   Host (epkg)   │                         │   Guest (init)  │
│                 │                         │                 │
│  listen(10000)  │◄─────────────────────── │  connect(10000) │
│      ↓          │    Guest 启动完成       │      ↓          │
│  accept()       │◄─────────────────────── │  send("READY")  │
│      ↓          │                         │      ↓          │
│  send(command)  │────────────────────────►│  recv command   │
│      ↓          │                         │      ↓          │
│  recv(result)   │◄─────────────────────── │  execute()      │
│      ↓          │                         │      ↓          │
│  VM shutdown    │                         │  poweroff()     │
└─────────────────┘                         └─────────────────┘
```

#### Reuse 模式（正向连接）

```
┌─────────────────┐                         ┌─────────────────┐
│   Host (epkg)   │                         │   Guest (init)  │
│                 │                         │                 │
│                 │                         │  listen(10000)  │◄──保持监听
│                 │                         │      ↑          │
│  connect(10000) │────────────────────────►│  accept()       │
│      ↓          │                         │      ↓          │
│  send(command)  │────────────────────────►│  recv command   │
│      ↓          │                         │      ↓          │
│  recv(result)   │◄─────────────────────── │  execute()      │
│      ↓          │                         │      ↓          │
│  keepalive/idle │                         │  wait next cmd  │
└─────────────────┘                         └─────────────────┘
```

## 实现细节

### 1. libkrun 端口配置

```rust
// 首次启动：Host 监听，Guest 连接
#[cfg(windows)]
unsafe {
    // listen=false: Host 创建 Named Pipe 服务器
    krun_add_vsock_port2_windows(ctx_id, 10000, stem_c.as_ptr(), false)?;
}

// Reuse 模式：Guest 监听，Host 连接
#[cfg(windows)]
unsafe {
    // listen=true: Guest 创建 vsock 监听
    krun_add_vsock_port2_windows(ctx_id, 10000, stem_c.as_ptr(), true)?;
}
```

### 2. vm_daemon 逻辑

```rust
pub fn run(options: VmDaemonOptions) -> Result<()> {
    if options.reverse_mode {
        // 反向模式：Guest 主动连接 Host
        run_reverse_mode()
    } else {
        // 正向模式：Guest 监听，等待 Host 连接
        run_forward_mode()
    }
}

fn run_reverse_mode() -> Result<()> {
    // 等待 vsock 完全初始化
    setup_vsock()?;

    // 主动连接 Host
    let mut stream = connect_to_host(10000)?;

    // 发送 ready 信号
    stream.write_all(b"READY\n")?;

    // 直接在这个连接上处理命令
    handle_connection(stream)?;

    // 执行完后关机或进入 reuse 模式
    if reuse_enabled {
        // 切换到正向模式监听
        switch_to_forward_mode()?;
    } else {
        poweroff();
    }
}
```

### 3. Host 逻辑

```rust
pub fn run_command_in_krun(...) -> Result<i32> {
    if is_first_run() {
        // 首次：反向模式
        run_reverse_mode(cmd_parts, io_mode)
    } else {
        // Reuse：正向模式（现有代码）
        run_forward_mode(cmd_parts, io_mode)
    }
}

fn run_reverse_mode(cmd_parts: &[String], io_mode: IoMode) -> Result<i32> {
    // 1. 创建 VM
    let vm_ctx = create_and_configure_vm(...)?;

    // 2. 设置 vsock port 10000 为 listen=false (Host 监听)
    let ready_listener = setup_reverse_listener()?;

    // 3. 启动 VM
    let vm_thread = start_libkrun_vm(vm_ctx.ctx)?;

    // 4. 等待 Guest 连接（带超时）
    let (stream, addr) = ready_listener.accept()?;

    // 5. 读取 ready 信号
    let mut buf = [0u8; 6];
    stream.read_exact(&mut buf)?;
    assert_eq!(&buf, b"READY\n");

    // 6. 直接在这个连接上发送命令（无需额外延迟！）
    send_command(stream, cmd_parts, io_mode)?;

    // 7. 等待结果
    let exit_code = recv_result(stream)?;

    // 8. 如果 reuse，切换到正向模式
    if reuse_vm {
        // 通知 Guest 切换到监听模式
        stream.write_all(b"SWITCH_TO_FORWARD\n")?;
        // Host 关闭当前连接，下次使用正向连接
    }

    Ok(exit_code)
}
```

## 优势分析

### 1. 首次启动无 vsock 时序问题

```
Guest 视角：
1. vsock 初始化完成
2. connect() 到 Host
3. 自己的 vsock 状态是 ESTABLISHED
4. 收到数据时，连接已经准备好

Host 视角：
1. accept() 返回
2. Guest 已经准备好
3. 立即发送数据，不会丢失
```

### 2. Reuse 模式保持现有架构

- 不需要改变 reuse VM 的工作方式
- 向后兼容
- 正向连接在 VM 稳定后工作正常

### 3. 无需要修复 libkrun

- 完全在 epkg 层面解决
- 利用现有的 vsock 功能
- 不依赖 libkrun 的同步修复

## 挑战与解决方案

### 挑战 1：Guest 如何知道 Host 地址？

**方案 A：固定 CID + 端口**
```rust
// Guest 硬编码 Host CID = 2 (VMADDR_CID_HOST)
let host_addr = VsockAddr::new(VMADDR_CID_HOST, 10000);
```

**方案 B：通过内核参数传递**
```
kernel cmdline: epkg.host_port=10000
```

### 挑战 2：连接超时/重试

```rust
// Guest 侧指数退避重试
fn connect_to_host_with_retry(port: u32) -> Result<TcpStream> {
    let mut backoff = Duration::from_millis(100);
    for attempt in 0..30 {
        match VsockStream::connect(VMADDR_CID_HOST, port) {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(1));
            }
        }
    }
    Err(...)
}
```

### 挑战 3：双向切换复杂度

```rust
// 使用状态机管理
enum VsockMode {
    ReverseConnecting,  // 首次启动，Guest 连接 Host
    ReverseConnected,   // 已连接，处理命令中
    ForwardSwitching,   // 切换到正向模式
    ForwardListening,   // Guest 监听中
}

// 简化方案：VM 生命周期内只切换一次
// 首次反向 → 之后一直正向
```

### 挑战 4：Windows Named Pipe 实现

```rust
// Windows 反向模式：Guest 连接 Host 的 Named Pipe
#[cfg(windows)]
fn connect_to_host_pipe(pipe_name: &str) -> Result<File> {
    let full = format!("\\\\.\\pipe\\{}", pipe_name);

    // 等待 Host 创建 pipe
    unsafe {
        WaitNamedPipeA(...)?;
        CreateFileW(...)?;
    }
}
```

## 代码修改范围

### 需要修改的文件

1. **src/libkrun.rs**
   - 支持 `listen=false` 模式（Host 监听）
   - 反向模式连接逻辑

2. **src/libkrun_bridge.rs**
   - `setup_reverse_listener()` - Host 创建监听
   - `accept_reverse_connection()` - 等待 Guest 连接

3. **src/busybox/vm_daemon.rs**
   - `run_reverse_mode()` - Guest 主动连接
   - 模式切换逻辑

4. **src/libkrun_stream.rs**
   - 支持通过已有 stream 发送命令（而非重新连接）

## 与纯反向方案的对比

| 特性 | 纯反向 | 混合方案 (本设计) |
|------|-------------|------------------|
| 首次启动 | ✅ Guest → Host | ✅ Guest → Host |
| Reuse 模式 | ✅ Guest → Host | ✅ Host → Guest（现有）|
| 架构改变 | 大，全部反向 | 中等，首次反向 |
| 代码复杂度 | 低，统一方向 | 中等，两种模式 |
| Host 主动能力 | 弱（需 Guest 先连）| 强（reuse 模式）|
| 适用场景 | 服务端/长期运行 | CLI 工具/epkg |

## 结论

**混合方案非常适合 epkg**：

1. **首次启动**：反向连接从根本上解决 vsock 时序问题
2. **Reuse 模式**：保持现有正向架构，无需大规模重构
3. **渐进式迁移**：可以逐步实现，不影响现有功能

**实现优先级**：

1. P0：实现反向首次启动（解决当前问题）
2. P1：实现模式切换（reuse 支持）
3. P2：优化重试和错误处理
