# libkrun/VM 故障排查指南

## 环境变量

调试 VM 问题时，以下环境变量非常有用：

```bash
# 启用 Hyper-V enlightenments（推荐开启）
LIBKRUN_WINDOWS_HYPERV_ENLIGHTENMENTS=1

# 启用详细 WHPX 调试日志
LIBKRUN_WINDOWS_VERBOSE_DEBUG=1

# 启用 Rust 日志
RUST_LOG=debug

# 完整调试命令示例
LIBKRUN_WINDOWS_HYPERV_ENLIGHTENMENTS=1 \
LIBKRUN_WINDOWS_VERBOSE_DEBUG=1 \
RUST_LOG=debug \
cargo run --bin epkg --features libkrun -- run --isolate=vm --timeout 60 -- /usr/bin/true
```

## 日志文件位置

| 日志类型 | 位置 | 说明 |
|---------|------|------|
| WHPX I/O 日志 | `tmp_whpx_io.log` | vCPU 退出、中断、MMIO/PIO 访问 |
| 控制台日志 | `~/.epkg/cache/vmm-logs/libkrun-console-*.log` | 内核串口输出 |
| epkg 日志 | stderr (RUST_LOG 控制) | Rust 级别日志 |

## VM 启动流程与关键检查点

```
1. 内核加载
   - load_payload: entry_addr=0x2162bc0
   - 检查点: 内核入口地址是否正确

2. 实模式初始化
   - CPUID exits (多个)
   - 检查点: CPUID 返回值是否合理

3. 保护模式/长模式切换
   - 页表设置
   - 检查点: 是否有页面错误

4. APIC/中断初始化
   - ..TIMER: vector=0x30
   - 检查点: 定时器中断是否正常工作

5. BogoMIPS 校准
   - Calibrating delay loop... 5989.76 BogoMIPS
   - 检查点: 校准是否完成

6. 设备初始化
   - virtio 设备探测
   - 检查点: MMIO 访问是否正常

7. 根文件系统挂载
   - virtiofs 挂载
   - 检查点: FUSE 请求是否成功

8. init 执行
   - Run /usr/bin/init as init process
   - 检查点: 是否能执行 init
```

## 常见问题诊断

### 1. VM 卡在启动阶段

**症状**: VM 无输出或卡住

**诊断步骤**:
1. 检查控制台日志是否有输出
2. 检查 WHPX 日志中 vCPU 是否还在运行
3. 确认定时器中断是否触发 (搜索 `[IRQ0]`)

**可能原因**:
- 定时器未启动 (检查 `vector=0x30`)
- APIC 配置问题
- HLT 指令阻塞

### 2. STATUS_ACCESS_VIOLATION (0xc0000005)

**症状**: 主机进程崩溃

**诊断步骤**:
1. 查看 WHPX 日志最后几行，确定崩溃位置
2. 检查是否在特定操作后崩溃 (如 CPUID、MSR 写入)
3. 检查 guest 内存访问是否越界

**常见原因**:
- guest 物理地址无效
- WHPX API 调用参数错误
- ~~并发访问共享数据结构~~ (已修复)

**已修复的 race condition**:

在 vCPU 运行时，timer 线程调用 `WHvCancelRunVirtualProcessor()` 来强制 vCPU 退出以便注入中断。但 WHPX API 要求此函数只能在 vCPU 实际在 `WHvRunVirtualProcessor` 内部时调用。如果 vCPU 正在处理 MSR 或其他退出时被取消，会导致 undefined behavior。

**解决方案**: 使用 `Arc<Vec<AtomicBool>>` 跟踪每个 vCPU 是否在 `WHvRunVirtualProcessor` 内部，只在此时调用 cancel。

### 3. 过多调试日志导致崩溃

**症状**: VM 在早期启动阶段崩溃 (STATUS_ACCESS_VIOLATION)

**诊断步骤**:
1. 检查是否启用了 RUST_LOG=debug 或 LIBKRUN_WINDOWS_VERBOSE_DEBUG=1
2. 观察崩溃是否发生在 CPUID 或中断注入后

**根本原因**:
`inject_pending_interrupt()` 函数在每次调用时都生成调试日志。由于该函数在 vCPU 运行循环中被高频调用（每次 VM 退出后），过多的字符串格式化和 I/O 操作可能导致性能问题甚至内存问题。

**解决方案**:
对调试日志进行速率限制，每 100 次调用才记录一次。

### 4. virtiofs 错误 (-120 EREMOTEIO)

**症状**: FUSE 请求返回远程 I/O 错误

**诊断步骤**:
1. 检查 FUSE 服务器日志
2. 检查 Windows 文件系统权限
3. 验证路径映射是否正确

### 4. APIC LINT 寄存器访问错误

**症状**: 日志显示 `APIC LINT register access is unavailable`

**说明**: 这是 Windows Hyper-V 的限制，不影响 VM 运行。WHPX 不允许直接访问某些 APIC 寄存器。

## 调试技巧

### 查看串口输出

```bash
# 串口输出以单个字符形式记录，需要提取
grep -o "data: 0x[0-9a-f]* ('.') " tmp_whpx_io.log | \
  sed "s/.*('\(.\)').*/\1/" | tr -d '\n'
```

### 追踪特定 GPA 的 MMIO 访问

在 `should_log_mmio_gpa()` 函数中添加需要监控的 GPA 地址。

### 追踪中断注入

搜索以下关键字：
- `[IRQ] inject` - 中断注入
- `[IRQ] request_ok` - 中断请求成功
- `[IRQ0] ioapic` - IRQ0 定时器中断

## Hyper-V Enlightenments

启用 `LIBKRUN_WINDOWS_HYPERV_ENLIGHTENMENTS=1` 可以：
- 提供稳定的时钟源 (hyperv_clocksource)
- 加速某些虚拟化操作
- 避免内核在 PIT 校准上卡住

**注意**: 不启用时，内核可能在 "Calibrating delay loop" 阶段卡住，因为没有可靠的时钟源。

## 内核命令行参数

推荐参数：
```
console=ttyS0          # 串口控制台
earlyprintk=serial     # 早期打印
loglevel=8             # 最高日志级别
debug                  # 启用调试
panic=-1               # 禁止 panic（调试用）
```

## 已知限制

1. **APIC LINT 寄存器**: WHPX 不允许访问，需要软件模拟
2. **TSC 频率**: 需要通过 MSR 模拟返回
3. **VP Assist Page**: 需要软件清零 guest 物理页面

## 重要注意事项

### 在 MSYS2 环境中调试

**关键点**: 我们在 MSYS2 环境中开发，不是在原生 Windows 或 guest OS 中！

**这意味着**:
1. `ls -la` 显示的文件类型可能不准确（MSYS2 对 Windows 文件系统的解释有限制）
2. 必须通过 **NTFS EA 属性** 来判断文件类型，而不是 MSYS2 的 ls 输出
3. virtiofs 返回给 guest 的文件类型由 NTFS EA 属性决定，而不是 Windows 文件系统 API 的直接返回值

### virtiofs 调试日志

**启用 VIRTIOFS 调试日志**:
```bash
LIBKRUN_WINDOWS_VERBOSE_DEBUG=1
```

**日志文件位置**:
- `~/.epkg/cache/vmm-logs/virtiofs-debug.log` - passthrough 文件系统日志
- `~/.epkg/cache/vmm-logs/virtiofs-worker.log` - virtio 队列处理日志
- `~/.epkg/cache/vmm-logs/virtiofs-server.log` - FUSE 服务器日志
- `~/.epkg/cache/vmm-logs/fuse-ops.log` - FUSE 操作日志
- `~/.epkg/cache/vmm-logs/init-trace.log` - init 执行跟踪日志

### 成功案例参考

嵌入式 `init.krun` 在 Windows WHPX 上可以正常工作：
- 构建命令: `cargo build --example boot_wsl2_kernel -p a3s-libkrun-sys`
- 运行命令: `./target/debug/examples/boot_wsl2_kernel.exe <kernel> <root>`
- 日志文件: `/tmp/boot_wsl2_kernel.log`

**成功启动的特征**:
```
[VIRTIOFS-WORKER] FsWorker::work hpq_fd=... req_fd=... stop_fd=...
[VIRTIOFS-SERVER] FsServer::handle_message opcode=26 unique=2 nodeid=0 len=104  # INIT
[VIRTIOFS-SERVER] FsServer::handle_message opcode=3 ...                         # GETATTR
[VIRTIOFS-SERVER] FsServer::handle_message opcode=1 ...                         # LOOKUP
[VIRTIOFS-SERVER] FsServer::handle_message opcode=14 ...                        # OPEN
[VIRTIOFS-SERVER] FsServer::handle_message opcode=15 ...                        # READ
```

### 符号链接创建注意事项

**在 Windows 上创建 Linux 格式的符号链接**:
1. 必须使用 `symlink_file_for_virtiofs()` 函数
2. 该函数会设置正确的 NTFS EA 属性（LXSS 格式）
3. 对于 Linux 格式的包（Apk/Deb/Rpm），不要添加 `.exe` 后缀
4. virtiofs 会读取 NTFS EA 属性来返回正确的文件类型给 guest

**常见错误**:
- 在 MSYS2 中 `ls -la` 显示文件为目录，但实际上 NTFS EA 属性正确
- 需要通过 guest 内核的行为来验证，而不是 MSYS2 的文件系统视图

### init 二进制文件架构问题

**症状**: `Kernel panic - not syncing: Requested init /usr/bin/init failed (error -13)` 或 `(error -5)`

**诊断步骤**:
1. 检查 init 二进制文件的架构: `file /path/to/init`
2. 确认是否为 Linux ELF 格式，而不是 Windows PE 格式

**根本原因**:
内核命令行指定 `init=/usr/bin/init`，该文件必须是 Linux ELF 可执行文件。
在 Windows/macOS 主机上，不能直接复制 Windows 可执行文件作为 init。

**解决方案**:
1. `epkg-linux-$arch` 是预编译的 Linux 静态二进制文件，用于 VM 内部执行
2. 在创建 Linux 格式包的环境时，需要创建 `init -> epkg-linux-$arch` 符号链接
3. 该符号链接由 `environment.rs::create_epkg_symlink()` 自动创建

**验证方法**:
```bash
# 检查 init 文件是否为 Linux ELF
file ~/.epkg/envs/alpine/usr/bin/init
# 应显示: ELF 64-bit LSB pie executable, x86-64, ..., static-pie linked

# 检查文件大小是否正确 (Linux 二进制约 13MB，Windows 二进制约 220MB)
ls -la ~/.epkg/envs/alpine/usr/bin/init
```

### init 二进制权限问题

**症状**: `Kernel panic - not syncing: Requested init /usr/bin/init failed (error -13)` (EACCES)

**诊断步骤**:
1. 检查控制台日志中的错误码
2. Error -13 = EACCES (权限被拒绝)
3. 检查文件是否具有可执行权限

**可能原因**:
1. 文件没有可执行权限 (`chmod +x`)
2. NTFS EA 属性中的权限位不正确
3. 文件是 Windows 可执行文件 (错误 -13 后可能变为 -5)

**解决方案**:
```bash
# 方法1: 设置可执行权限
chmod +x ~/.epkg/envs/alpine/usr/bin/init

# 方法2: 使用 epkg 的 symlink_file_for_native() 创建符号链接
# 这会自动设置正确的 NTFS EA 权限
```

**注意**: 在 Windows 上，virtiofs 通过 NTFS EA 属性读取文件权限，
而不是 Windows 文件系统权限。需要确保 EA 属性正确设置。

### NTFS EA 文件类型位问题

**症状**: `Kernel panic - not syncing: Requested init /usr/bin/init failed (error -5)` (EIO)

**根本原因**: 设置 NTFS EA 权限时，MODE 值必须包含文件类型位（S_IFREG）。
- 错误: `MODE_755 = 0o755` (仅权限位)
- 正确: `MODE_755 = 0o100755` (包含 S_IFREG = 0o100000)

virtiofs 的 `metadata_to_stat()` 函数直接使用 EA 值作为 `st_mode`，
如果缺少文件类型位，内核会无法识别文件类型，导致 EIO 错误。

**解决方案**:
```rust
// 正确的 MODE 设置
const S_IFREG: u32 = 0o100000;  // Regular file type bit
const MODE_755: u32 = S_IFREG | 0o755;  // 0o100755
```

### 在 WSL2 中调试 Windows 可执行文件

**WSLENV 环境变量传递**:

在 WSL2 中运行 Windows 可执行文件时，需要使用 `WSLENV` 来传递环境变量：

```bash
# 设置 WSLENV 以传递环境变量到 Windows 进程
export WSLENV=RUST_LOG:1:LIBKRUN_WINDOWS_VERBOSE_DEBUG:1
export RUST_LOG=debug
export LIBKRUN_WINDOWS_VERBOSE_DEBUG=1

# 运行 Windows 可执行文件
/home/wfg/epkg/dist/epkg-windows-x86_64.exe run -e alpine --isolate=vm ls /
```

**WSLENV 格式说明**:
- `VAR:1` - 传递变量 VAR 到 Windows 进程
- `VAR:0` 或 `VAR` - 传递变量 VAR，但转换 Windows 路径格式
- 多个变量用冒号分隔

**常用调试命令**:
```bash
# 完整调试输出
export WSLENV=RUST_LOG:1:LIBKRUN_WINDOWS_VERBOSE_DEBUG:1
export RUST_LOG=debug
export LIBKRUN_WINDOWS_VERBOSE_DEBUG=1

# 带 timeout 运行
/home/wfg/epkg/dist/epkg-windows-x86_64.exe run -e alpine --isolate=vm --timeout 30 ls /
```

**日志文件位置**:
- `~/.epkg/cache/vmm-logs/virtiofs-device.log` - VIRTIOFS 设备日志
- `~/.epkg/cache/vmm-logs/virtiofs-debug.log` - passthrough 文件系统日志
- `~/.epkg/cache/vmm-logs/init-trace.log` - init 执行跟踪日志
- `~/.epkg/cache/vmm-logs/libkrun-console-*.log` - 内核控制台输出

**WSL2 特定注意事项**:
1. WSL2 中 `ls -la` 显示的文件权限可能不准确（显示 `----------`）
2. 实际权限由 NTFS EA 属性决定，virtiofs 会正确读取
3. 可以通过 virtiofs-debug.log 验证 EA 是否正确读取
4. 日志时间戳可能不更新（WSL2 文件系统缓存问题）

**构建注意事项**:
```bash
# 构建 Windows 版本（带 libkrun 特性）
make cross-windows

# 或显式启用 libkrun 特性
FEATURES=libkrun make cross-windows

# 检查构建的版本
./dist/epkg-windows-x86_64.exe --version
# 应显示正确的 git commit hash
```