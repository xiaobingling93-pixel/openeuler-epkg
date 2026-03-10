# VM 内核故障排除

## 常见问题

### 1. 内核文件不存在

**错误信息**：
```
No kernel image for VM. Use '--kernel /path/to/kernel',
run 'epkg self install', or ensure a kernel exists in /boot.
```

**原因**：`~/.epkg/envs/self/boot/kernel` 文件不存在

**解决方案**：

```bash
# 方案 1: 重新安装
epkg self install

# 方案 2: 本地构建
cd git/libkrunfw
./build.sh

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
curl -I https://gitee.com/api/v5/repos/wu_fengguang/libkrunfw/releases/latest

# 2. 检查下载缓存
ls -la ~/.cache/epkg/

# 3. 手动下载测试
curl -L -o /tmp/vmlinux.zst "https://gitee.com/wu_fengguang/libkrunfw/releases/download/<tag>/vmlinux-x86_64-<version>.zst"

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
strings ~/.epkg/envs/self/boot/kernel | grep "Linux version"

# 检查内核文件
file ~/.epkg/envs/self/boot/kernel
```

### 5. 符号链接损坏

**症状**：`kernel` 符号链接指向不存在的文件

**检查并修复**：

```bash
# 检查符号链接
ls -la ~/.epkg/envs/self/boot/

# 修复符号链接
cd ~/.epkg/envs/self/boot/
ln -sf kernel-6.19.6 kernel  # 使用实际存在的版本
```

## 调试命令

### 检查内核状态

```bash
# 列出所有内核文件
ls -la ~/.epkg/envs/self/boot/

# 查看内核详细信息
file ~/.epkg/envs/self/boot/kernel
strings ~/.epkg/envs/self/boot/kernel | head -50

# 检查内核架构
readelf -h ~/.epkg/envs/self/boot/kernel | grep Machine
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

# 检查 libkrunfw.so (旧版本)
ldd ~/.epkg/envs/self/usr/lib/libkrunfw.so 2>/dev/null
```

### 构建调试

```bash
# 查看构建脚本帮助
cd git/libkrunfw
./build.sh --help 2>&1 || head -30 build.sh

# 构建时显示详细输出
make V=1 -C git/linux vmlinux

# 检查内核配置
diff git/libkrunfw/config-libkrunfw_x86_64 git/linux/.config
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
  Decompressing vmlinux-6.19.6...
  Installed kernel: /home/user/.epkg/envs/self/boot/kernel-6.19.6 (22000000 bytes)
```

**内核已存在**：
```
# build.sh 输出
kernel -> kernel-6.19.6
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
du -h ~/.epkg/envs/self/boot/kernel*

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
ls -la ~/.epkg/envs/self/boot/kernel-*

# 删除旧版本（保留当前使用的）
rm ~/.epkg/envs/self/boot/kernel-6.12.68
```

## 相关文件路径

| 路径 | 说明 |
|------|------|
| `~/.epkg/envs/self/boot/kernel` | 默认内核（符号链接） |
| `~/.epkg/envs/self/boot/kernel-$ver` | 具体版本内核 |
| `~/.cache/epkg/epkg/vmlinux-*.zst` | 下载的压缩内核 |
| `git/libkrunfw/config-libkrunfw_$arch` | 内核配置文件 |
| `git/linux/vmlinux` | 本地构建的内核 |
