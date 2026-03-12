# VM 内核故障排除

## 常见问题

### 1. 内核文件不存在

**错误信息**：
```
No kernel image for VM. Use '--kernel /path/to/kernel',
run 'epkg self install', or ensure a kernel exists in /boot.
```

**原因**：`~/.epkg/envs/self/boot/vmlinux` 文件不存在

**解决方案**：

```bash
# 方案 1: 重新安装
epkg self install

# 方案 2: 本地构建
cd git/sandbox-kernel
./scripts/build.sh

# 方案 3: 手动指定内核
epkg run --kernel /boot/vmlinuz-$(uname -r) <package>
```

### 2. 内核下载失败

**错误信息**：
```
Failed to download vmlinux from https://gitee.com/...
```

**排查步骤**：

```bash
# 1. 检查网络连接
curl -I https://gitee.com/api/v5/repos/wu_fengguang/sandbox-kernel/releases/latest

# 2. 检查下载缓存
ls -la ~/.cache/epkg/

# 3. 手动下载测试
curl -L -o /tmp/vmlinux.zst "https://gitee.com/wu_fengguang/sandbox-kernel/releases/download/<tag>/vmlinux-<version>-x86_64.zst"

# 4. 验证文件完整性
zstd -t /tmp/vmlinux.zst
```

### 3. zstd 解压失败

**错误信息**：
```
Failed to decompress zst file
```

**排查步骤**：

```bash
# 1. 检查文件大小
ls -la ~/.cache/epkg/epkg/vmlinux-*.zst

# 2. 验证 zstd 格式
file ~/.cache/epkg/epkg/vmlinux-*.zst

# 3. 手动解压测试
zstd -d ~/.cache/epkg/epkg/vmlinux-*.zst -o /tmp/vmlinux

# 4. 验证解压后的内核
file /tmp/vmlinux
# 应显示: ELF 64-bit LSB executable, x86-64...
```

### 4. 内核版本不匹配

**症状**：VM 启动失败或行为异常

**检查当前内核**：

```bash
# 查看内核版本
strings ~/.epkg/envs/self/boot/vmlinux | grep "Linux version"

# 检查内核文件
file ~/.epkg/envs/self/boot/vmlinux
```

### 5. 符号链接损坏

**症状**：`vmlinux` 符号链接指向不存在的文件

**检查并修复**：

```bash
# 检查符号链接
ls -la ~/.epkg/envs/self/boot/

# 修复符号链接
cd ~/.epkg/envs/self/boot/
ln -sf vmlinux-6.19.6-x86_64 vmlinux  # 使用实际存在的版本
```

## 调试命令

### 检查内核状态

```bash
# 列出所有内核文件
ls -la ~/.epkg/envs/self/boot/

# 查看内核详细信息
file ~/.epkg/envs/self/boot/vmlinux
strings ~/.epkg/envs/self/boot/vmlinux | head -50

# 检查内核架构
readelf -h ~/.epkg/envs/self/boot/vmlinux | grep Machine
```

### 检查下载缓存

```bash
# 缓存位置
ls -la ~/.cache/epkg/epkg/

# 查看下载的 vmlinux 文件
ls -la ~/.cache/epkg/epkg/vmlinux-*

# 验证 sha256
cd ~/.cache/epkg/epkg/
sha256sum -c vmlinux-*.zst.sha256
```

### 检查 libkrun 状态

```bash
# 检查 libkrun 是否找到内核
RUST_LOG=debug epkg run <package> 2>&1 | grep -i kernel
```

### 构建调试

```bash
# 查看构建脚本帮助
cd git/sandbox-kernel
./scripts/build.sh --help

# 构建时显示详细输出
make V=1 -C git/sandbox-kernel/linux-stable vmlinux

# 检查内核配置
diff <(cat git/sandbox-kernel/kconfig/common git/sandbox-kernel/kconfig/arch/x86_64) git/sandbox-kernel/linux-stable/.config
```

## 日志分析

### 启用调试日志

```bash
# 启用 libkrun 调试日志
RUST_LOG=debug epkg run <package>

# 仅启用 init 模块日志
RUST_LOG=epkg::init=debug epkg self install
```

### 常见日志信息

**正常下载**：
```
Downloading vmlinux from https://gitee.com/...
  Decompressing vmlinux-6.19.6-x86_64...
  Installed kernel: /home/user/.epkg/envs/self/boot/vmlinux-6.19.6-x86_64 (22000000 bytes)
```

**内核已存在**：
```
# build.sh 输出
vmlinux -> vmlinux-6.19.6-x86_64
```

**架构不支持**：
```
vmlinux not available for loongarch64, VM feature won't be usable
```

## 性能问题

### 内核文件过大

**症状**：下载或解压缓慢

**检查**：

```bash
# 查看内核大小
du -h ~/.epkg/envs/self/boot/vmlinux*

# 查看压缩比
ls -la ~/.cache/epkg/epkg/vmlinux-*.zst
```

**说明**：
- vmlinux 原始大小约 20-25MB
- zstd 压缩后约 5-6MB
- 压缩/解压时间通常 < 5 秒

### 清理旧内核

```bash
# 列出所有内核
ls -la ~/.epkg/envs/self/boot/vmlinux-*

# 删除旧版本（保留当前使用的）
rm ~/.epkg/envs/self/boot/vmlinux-6.12.68-x86_64
```

## 相关文件路径

| 路径 | 说明 |
|------|------|
| `~/.epkg/envs/self/boot/vmlinux` | 默认内核（符号链接） |
| `~/.epkg/envs/self/boot/vmlinux-$ver-$arch` | 具体版本内核 |
| `~/.epkg/envs/self/boot/config-$ver-$arch` | 对应配置文件 |
| `~/.cache/epkg/epkg/vmlinux-*.zst` | 下载的压缩内核 |
| `git/sandbox-kernel/kconfig/common` | 共享内核配置 |
| `git/sandbox-kernel/kconfig/arch/$arch` | 架构特定配置 |
| `git/sandbox-kernel/linux-stable/vmlinux` | 本地构建的内核 |

## 启动时间排查方法论

### 问题现象

VM 启动时间从 ~0.3s 增加到 ~0.6s 或更长。

### 科学排查方法：initcall_debug

使用 QEMU + `initcall_debug` 内核参数可以精确测量每个初始化函数的耗时。

#### 1. 收集启动日志

```bash
# 使用 QEMU 模式（日志更完整）
epkg run --sandbox=vm --vmm=qemu \
  --kernel=/c/epkg/git/sandbox-kernel/linux-stable/vmlinux \
  --kernel-args='initcall_debug=1 ignore_loglevel printk.time=1' \
  ls /

# 日志位置
grep initcall ~/.cache/epkg/vmm-logs/latest-qemu.log
```

#### 2. 分析 initcall 耗时

```bash
# 提取最耗时的初始化函数
grep "initcall.*returned" ~/.cache/epkg/vmm-logs/latest-qemu.log | \
  awk '{for(i=1;i<=NF;i++) if($i ~ /after/) print $(i+1), $0}' | \
  sort -rn | head -20
```

输出示例：
```
276000 [    0.844511] initcall raid6_select_algo+0x0/0x140 returned 0 after 276000 usecs
 92000 [    0.533745] initcall acpi_init+0x0/0x110 returned 0 after 92000 usecs
 15117 [    0.920059] initcall virtio_pci_driver_init+0x0/0x20 returned 0 after 15117 usecs
```

#### 3. 常见性能瓶颈

| 初始化函数 | 典型耗时 | 原因 | 解决方案 |
|-----------|---------|------|----------|
| `raid6_select_algo` | 200-300ms | `CONFIG_RAID6_PQ_BENCHMARK=y` | 禁用 `CONFIG_RAID6_PQ_BENCHMARK` |
| `acpi_init` | 50-100ms | ACPI 表解析 | 通常不可避免 |
| `jent_mod_init` | 10-15ms | 熵池初始化 | 可接受 |
| `inet_init` | 10-15ms | 网络协议栈 | 通常不可避免 |

#### 4. Top 20 耗时 initcall 对比

**优化前（CONFIG_RAID6_PQ_BENCHMARK=y）：**
```
排名 | 耗时   | initcall 函数              | CONFIG 选项
-----|--------|----------------------------|----------------------------------
  1  | 276ms  | raid6_select_algo          | CONFIG_RAID6_PQ_BENCHMARK  ⭐罪魁祸首
  2  | 84ms   | acpi_init                  | CONFIG_ACPI
  3  | 23ms   | virtio_pci_driver_init     | CONFIG_VIRTIO_PCI
  4  | 15ms   | jent_mod_init              | CONFIG_CRYPTO_JITTERENTROPY
  5  | 12ms   | inet_init                  | CONFIG_INET
  6  | 11ms   | virtio_fs_init             | CONFIG_VIRTIO_FS
  7  | 10ms   | serial8250_init            | CONFIG_SERIAL_8250
  8  | 8ms    | pci_apply_final_quirks     | CONFIG_PCI
  9  | 6ms    | acpi_processor_driver_init | CONFIG_ACPI_PROCESSOR
 10  | 6ms    | init_acpi_pm_clocksource   | CONFIG_ACPI
```

**优化后（CONFIG_RAID6_PQ_BENCHMARK disabled）：**
```
排名 | 耗时   | initcall 函数              | CONFIG 选项                      | 可优化?
-----|--------|----------------------------|----------------------------------|--------
  1  | 64ms   | acpi_init                  | CONFIG_ACPI                      | ❌ 必需
  2  | 16ms   | virtio_pci_driver_init     | CONFIG_VIRTIO_PCI                | ❌ 必需
  3  | 10ms   | inet_init                  | CONFIG_INET                      | ❌ 必需
  4  | 10ms   | serial8250_init            | CONFIG_SERIAL_8250               | ⚠️ 可选
  5  | 10ms   | jent_mod_init              | CONFIG_CRYPTO_JITTERENTROPY      | ⚠️ 可选
  6  | 5ms    | pci_apply_final_quirks     | CONFIG_PCI                       | ❌ 必需
  7  | 5ms    | loop_init                  | CONFIG_BLK_DEV_LOOP              | ✅ 可禁用
  8  | 4ms    | acpi_processor_driver_init | CONFIG_ACPI_PROCESSOR            | ⚠️ 可选
  9  | 4ms    | pcibios_assign_resources   | CONFIG_PCI                       | ❌ 必需
 10  | 3ms    | chr_dev_init               | CONFIG_UNIX98_PTYS               | ⚠️ 可选
```

**优化效果：**
- `raid6_select_algo` 从 **276ms** 降至 **0ms**（禁用 BENCHMARK）
- 启动时间从 ~0.61s 降至 ~0.35s（节省 260ms）
- 剩余可优化空间约 30-50ms（串口、loop、熵池等）

#### 5. 对比分析

```bash
# 统计 initcall 数量
grep -c 'initcall.*+0x' ~/.cache/epkg/vmm-logs/latest-qemu.log

# 对比两个内核的差异
diff <(grep 'initcall.*+0x' good.log | awk '{print $5}') \
     <(grep 'initcall.*+0x' bad.log | awk '{print $5}')
```

#### 6. 优化配置

根据分析结果修改配置：

```bash
# git/sandbox-kernel/kconfig/common

# 禁用 RAID6（节省 200-300ms）
# CONFIG_RAID6_PQ_BENCHMARK is not set
```

#### 7. 验证优化效果

```bash
# 重新构建内核
cd git/sandbox-kernel/linux-stable
cp .config-libkrunfw .config
cat ../kconfig/common ../kconfig/arch/x86_64 >> .config
make olddefconfig
make vmlinux

# 测试启动时间
time epkg run --sandbox=vm --vmm=qemu \
  --kernel=/c/epkg/git/sandbox-kernel/linux-stable/vmlinux \
  ls /
```
