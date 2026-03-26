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

### 3. virtiofs 错误 (-120 EREMOTEIO)

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