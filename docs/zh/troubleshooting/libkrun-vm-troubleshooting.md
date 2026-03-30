# libkrun/VM 故障排查指南

## 当前状态更新 (2026-03-28)

### 最新发现

**VM 实际上已经可以正常启动！**

从控制台日志观察：
```
[    0.000000] Linux version 6.19.8-dirty ...
[    0.437374] Run /usr/bin/init as init process
[    0.438097]     /usr/bin/init
[    0.438397]     tsi_hijack
init: init_loggi[    1.257042] sysrq: Power Off
[    1.259110] reboot: Power down
[    1.259488] reboot: Power off not available: System halted instead
```

**证明：**
1. ✅ Linux 内核成功启动（6.19.8）
2. ✅ 2 个 vCPU 被正确识别和配置
3. ✅ virtiofs 成功挂载（/dev/root, .epkg_4831）
4. ✅ init 进程被执行（/usr/bin/init）
5. ✅ VM 正常关闭（sysrq power off）

**问题：**
- 退出代码仍然是 -1073741819 (0xC0000005 Access Violation)
- 可能是 vsock 通信或 Windows 端清理代码问题
- init 的输出可能没有正确传回主机

### 下一步调试方向

1. 检查 vsock 通信是否正常
2. 检查 init.krun 是否正确执行命令并返回输出
3. 检查 Windows 端代码在等待 VM 输出时的处理
4. 可能需要添加更多日志来跟踪 vsock 通信流程

---

### 当前 VM 启动状态

✅ **已修复/正常工作：**
1. init.krun 自动部署（937KB 嵌入二进制）
2. 内核文件加载（vmlinux 25.7MB ELF 格式）
3. VM 配置（2 vCPUs, 2048 MiB RAM）
4. virtiofs 挂载（/dev/root 和 .epkg_4831）
5. vCPU 配置（RIP=0x2162bc0, 页表映射）

🔄 **待解决问题：**
- VM 在 `krun_start_enter` 后崩溃（Exit code: -1073741819 = 0xC0000005 Access Violation）
- Console 日志为空（内核未及输出即崩溃）
- 需要进一步分析 WHPX 层问题

### 已知崩溃点

从日志观察，崩溃发生在：
```
[vmm/src/windows/vstate.rs:958] start_threaded called for vCPU 0
[vmm/src/windows/vstate.rs:644] Configuring vCPU 0 for x86_64 boot: RIP=0x2162bc0
[vmm/src/windows/vstate.rs:180] === HIGHER-HALF KERNEL MAPPING FIX ACTIVE ===
[vmm/src/windows/vstate.rs:231] Page tables configured: PML4=0x9000, PDPTE=0xa000, PDE=0xb000
...
[vmm/src/windows/vstate.rs:958] start_threaded called for vCPU 1
...  # <-- 崩溃发生在这里
```

**可能原因：**
1. vCPU 线程启动时的竞态条件
2. WHPX API 调用参数问题
3. 内存映射权限问题

### 多 vCPU 启动问题分析

**现象：**
- vCPU 0 配置成功
- vCPU 1 启动时崩溃 (Access Violation)
- 单 vCPU 测试 (`--cpus 1`) 同样崩溃

**代码分析：**

`start_threaded()` 函数流程 (vstate.rs:955-1394):
1. 创建 vCPU 线程 (std::thread::Builder::new().spawn(...))
2. 线程内调用 `configure_x86_64()` 配置寄存器
3. 等待 `VcpuEvent::Resume` 事件
4. 启动监控线程检测卡死
5. 进入主运行循环调用 `whpx_vcpu.run()`

vCPU 创建流程:
- 默认 vCPU 数量: 2 (run.rs:151 resolve_vm_cpus 默认返回 2)
- 可通过 `--cpus 1` 或 `EPKG_VM_CPUS=1` 设置单 vCPU
- 每个 vCPU 调用 `WhpxVcpu::new()` → `WHvCreateVirtualProcessor()`
- 然后在线程中配置寄存器 `WHvSetVirtualProcessorRegisters()`

**关键观察：**
- 崩溃发生在 `start_threaded` 调用后，实际执行前
- 单 vCPU 也崩溃，说明不是多 vCPU 竞态条件
- 可能与 `WHvCreateVirtualProcessor` 或寄存器配置有关
- 监控线程在 vCPU 启动后立即创建，可能干扰

### init 执行失败问题

**症状**: 内核 panic - `Requested init /usr/bin/init failed (error -5)` 或 `(error -13)`

**错误码含义：**
- error -5 = EIO (Input/output error) - 文件类型位问题（已修复）
- error -13 = EACCES (Permission denied) - 权限问题

**可能原因：**
1. virtiofs 文件系统竞态条件
2. NTFS EA 属性读取不稳定
3. init 文件权限在 WSL2/Windows 边界上不一致

**验证方法：**
```bash
# 检查 init 文件类型（应在 WSL2 中执行）
file /mnt/c/Users/epkg/.epkg/envs/alpine/usr/bin/init
# 应显示: ELF 64-bit LSB executable, x86-64, ...
```

---

## WSL2 中调试 Windows 可执行文件

### PowerShell 和 cmd.exe 路径

在 WSL2 中调用 Windows 可执行文件：

```bash
# PowerShell 路径
/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe

# cmd.exe 路径
/mnt/c/Windows/System32/cmd.exe

# 验证可用性
/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe -Command "Write-Host 'PowerShell works!'"
/mnt/c/Windows/System32/cmd.exe /c "echo cmd.exe works!"
```

### 环境变量传递

**WSLENV 格式：**
```bash
# 传递环境变量到 Windows 进程
export WSLENV=RUST_LOG:1:LIBKRUN_WINDOWS_VERBOSE_DEBUG:1

# :1 表示传递但不转换路径格式
# :0 或省略表示传递并转换 Windows 路径格式
```

**完整调试命令：**
```bash
# 设置调试环境
export WSLENV=RUST_LOG:1:LIBKRUN_WINDOWS_VERBOSE_DEBUG:1:EPKG_VM_DEBUG:1
export RUST_LOG=trace
export LIBKRUN_WINDOWS_VERBOSE_DEBUG=1
export EPKG_VM_DEBUG=1

# 运行测试
/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe -Command "
  \$env:RUST_LOG='trace'
  \$env:LIBKRUN_WINDOWS_VERBOSE_DEBUG='1'
  C:\Users\epkg\.epkg\envs\self\usr\bin\epkg.exe run -e alpine --isolate=vm --timeout 30 ls /
  Write-Host 'Exit code:' \$LASTEXITCODE
"
```

### 查看 Windows 日志

```bash
# 列出日志文件
/mnt/c/Windows/System32/cmd.exe /c 'dir /b C:\Users\epkg\.epkg\cache\vmm-logs\'

# 查看控制台日志
/mnt/c/Windows/System32/cmd.exe /c 'type C:\Users\epkg\.epkg\cache\vmm-logs\libkrun-console-XXXX.log'

# PowerShell 查看日志
/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe -Command "
  Get-Content 'C:\Users\epkg\.epkg\cache\vmm-logs\latest-console.log' -ErrorAction SilentlyContinue
"
```

### 调试技巧

**捕获完整输出：**
```bash
# 保存所有输出到文件
/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe -Command "
  \$env:RUST_LOG='trace'
  C:\Users\epkg\.epkg\envs\self\usr\bin\epkg.exe run -e alpine --isolate=vm --timeout 30 ls / 2>&1
" > vm_test.log 2>&1

# 查看最后 50 行
/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe -Command "
  Get-Content vm_test.log -Tail 50
"
```

**检查退出代码：**
```bash
/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe -Command "
  C:\Users\epkg\.epkg\envs\self\usr\bin\epkg.exe run -e alpine --isolate=vm ls /
  Write-Host 'Exit code:' \$LASTEXITCODE
"
```

**常用退出代码：**
- `0` - 成功
- `-1073741819` - Access Violation (0xC0000005)
- `-1` - 一般错误

---

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

# 通过 PowerShell 查看最新控制台日志
/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe -Command \
  "Get-Content 'C:\Users\epkg\.epkg\cache\vmm-logs\latest-console.log'"

# 查看所有控制台日志
/mnt/c/Windows/System32/cmd.exe /c \
  'dir C:\Users\epkg\.epkg\cache\vmm-logs\libkrun-console-*.log'
```

**关键观察点：**
- `Run /usr/bin/init as init process` - init 进程已启动
- `sysrq: Power Off` - VM 正常关闭
- `reboot: Power down` - 关机流程完成

### 追踪特定 GPA 的 MMIO 访问

在 `should_log_mmio_gpa()` 函数中添加需要监控的 GPA 地址。

### 追踪中断注入

搜索以下关键字：
- `[IRQ] inject` - 中断注入
- `[IRQ] request_ok` - 中断请求成功
- `[IRQ0] ioapic` - IRQ0 定时器中断

### 测试不同模式

**命令行模式 (不使用 vsock，通过内核参数传递命令):**
```bash
export EPKG_VM_NO_DAEMON=1
/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe -Command \
  "& { \$env:EPKG_VM_NO_DAEMON='1'; C:\Users\epkg\.epkg\envs\self\usr\bin\epkg.exe run -e alpine --isolate=vm ls / }"
```

**保持 VM 不超时（用于调试）:**
```bash
export EPKG_VM_KEEP_TIMEOUT=1
```

**单 vCPU 模式:**
```bash
export EPKG_VM_CPUS=1
```

---

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
# Windows 交叉编译需要两步构建:
# 1. 先构建 Linux 版本（生成 init/init 供嵌入）
make

# 2. 交叉编译 Windows 版本（x86_64 为默认架构）
make cross-windows

# 检查构建的版本
./dist/epkg-windows-x86_64.exe --version
# 应显示正确的 git commit hash
```

## 最新调试发现 (2026-03-28)

### 当前状态

**问题**: VM 仍然崩溃，退出代码 -1073741819 (0xC0000005 = STATUS_ACCESS_VIOLATION)

**Windows 事件日志分析**:
```
Faulting module: winhvplatform.dll (version 10.0.26100.7920)
Exception code: 0xc0000005 (Access Violation)
Fault offset: 0x0000000000013010
```

**已尝试的修复** (未解决问题):
1. ✅ 修复了 `WHvCancelRunVirtualProcessor` 竞态条件 (commit d962a75)
2. ✅ 添加了调试日志速率限制 (commit 5d8faf1)
3. ✅ 测试了单 vCPU 模式 (`--cpus 1`)
4. ✅ 测试了减少内存 (`--memory 512M`)
5. ✅ 测试了命令行模式 (`EPKG_VM_NO_DAEMON=1`)
6. ✅ 测试了禁用 PIT 定时器 (`LIBKRUN_WHPX_DISABLE_PIT_TIMER=1`)
7. ✅ 测试了禁用 vCPU cancel (`LIBKRUN_WHPX_DISABLE_VCPU_CANCEL=1`)

**观察结果**:
- 崩溃发生在 WHPX DLL 内部，不是 epkg/libkrun 代码
- 每次启动都崩溃，不是随机/竞态问题
- 内核配置成功，但在 vCPU 运行阶段崩溃
- 控制台日志为空，说明内核未及输出即崩溃

**可能原因**:
1. Windows 11 24H2/25H2 (build 26200) 的 WHPX 兼容性问题
2. 内存映射或页表配置与 WHPX 不兼容
3. vCPU 寄存器配置问题
4. 需要进一步分析 winhvplatform.dll 崩溃位置

**新增调试发现** (2026-03-28 14:00):

经过深入分析 WHPX 代码和崩溃日志，发现以下关键信息：

1. **崩溃位置**: winhvplatform.dll + 0x13010
   - 每次崩溃偏移量相同，说明是确定性的代码路径
   - 不是竞态条件或内存损坏

2. **vCPU 启动流程分析**:
   ```
   Vcpu::start_threaded() [vstate.rs:955]
     └─> thread spawn
         └─> configure_x86_64() [vstate.rs:971]
             └─> WHvSetVirtualProcessorRegisters() - 成功
         └─> wait for Resume event - 成功
         └─> monitor thread spawn [vstate.rs:1012]
         └─> loop [vstate.rs:1304]
             └─> Vcpu::run() [vstate.rs:1318]
                 └─> WhpxVcpu::run() [whpx_vcpu.rs:2688]
                     └─> WHvRunVirtualProcessor() [whpx_vcpu.rs:2723] ← 崩溃点
   ```

3. **可能原因**:
   - Windows 11 24H2/25H2 (build 26200) 的 WHPX 可能有 API 行为变化
   - `WHvRunVirtualProcessor` 可能在某些参数组合下崩溃
   - 可能是 APIC 模拟与 WHPX 的新版本不兼容

4. **需要进一步测试**:
   - 禁用 APIC 模拟 (`enable_apic=false`) 测试
   - 测试最简单的 HLT 程序（见 vstate.rs 中的 smoke tests）
   - 使用 WinDbg 附加分析崩溃堆栈

**下一步**:
1. 在 Windows 11 23H2 (build 22631) 上测试验证
2. 联系 Microsoft 确认 Windows 11 24H2 的 WHPX 已知问题
3. 考虑添加 Windows 版本检测和兼容性模式
4. 可能需要为 24H2+ 版本禁用某些 WHPX 特性