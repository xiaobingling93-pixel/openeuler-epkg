# ELF Loader 故障排查

## 快速诊断命令

```bash
# 1. 检查 elf-loader 文件类型
file ~/.epkg/envs/self/usr/bin/elf-loader

# 2. 检查 ebin 包装器
ls -la ~/.epkg/envs/<env>/ebin/

# 3. 检查硬链接 inode
ls -li ~/.epkg/envs/<env>/ebin/{gcc,g++,python}

# 4. 启用调试日志
RUST_LOG=debug epkg -e <env> install <pkg> 2>&1 | grep -E "elf|handle_elf|script_wrapper"
```

## ELF Loader 调试技巧

### 编译和测试 elf-loader

```bash
# 1. 进入 elf-loader 源码目录
cd /c/epkg/git/elf-loader/src

# 2. 安装开发依赖
make dev-depends

# 3. 编译（可选 DEBUG=1 用于调试）
make          # 普通编译
make DEBUG=1  # 调试版本

# 4. 复制到 self 环境（供未来新环境使用）
cp loader ~/.epkg/envs/self/usr/bin/elf-loader

# 5. 复制到具体环境的 ebin 进行调试
cp loader ~/.epkg/envs/dev-alpine/ebin/go

# 6. 测试
~/.epkg/envs/dev-alpine/ebin/go version
```

### 快速验证脚本

```bash
# 运行验证脚本快速测试修复
/c/epkg/git/elf-loader/src/verify.sh
```

### 完整调试示例

```bash
# 调试版本编译和测试
cd /c/epkg/git/elf-loader/src
make DEBUG=1
cp loader ~/.epkg/envs/dev-alpine/ebin/go
~/.epkg/envs/dev-alpine/ebin/go version

# 运行验证脚本
./verify.sh
```

## 常见问题

### 问题 1: elf-loader 损坏

**症状**：
```bash
$ file ~/.epkg/envs/self/usr/bin/elf-loader
elf-loader: ASCII text, not ELF

$ ~/.epkg/envs/dev-alpine/ebin/gcc --version
bash: gcc: Exec format error
```

**原因**：
- `create_script_wrapper()` 曾直接使用 `truncate()` 写入目标文件
- 当目标是 elf-loader 的硬链接时，所有共享该 inode 的文件都被覆盖

**诊断**：
```bash
# 查看 elf-loader 内容（应显示 ELF，而非脚本）
head -5 ~/.epkg/envs/self/usr/bin/elf-loader

# 检查硬链接计数（异常时应为 1）
ls -l ~/.epkg/envs/self/usr/bin/elf-loader
```

**修复**：
```bash
# 1. 从源码恢复 elf-loader
cp /c/epkg/git/elf-loader/src/loader ~/.epkg/envs/self/usr/bin/elf-loader

# 2. 或者重新安装 epkg
cd /c/epkg && make install

# 3. 重建 ebin 包装器
epkg -e dev-alpine install --force build-base
```

**验证**：
```bash
file ~/.epkg/envs/self/usr/bin/elf-loader
# 应输出：ELF 64-bit LSB pie executable, ...
```

### 问题 2: ebin 包装器未创建

**症状**：
```bash
$ ls ~/.epkg/envs/dev-alpine/ebin/gcc
ls: cannot access 'gcc': No such file or directory
```

**原因**：
- 包作为依赖安装，`ebin_exposure=false`
- 重新请求已安装包时未触发暴露

**诊断**：
```bash
# 检查包是否已安装
epkg -e dev-alpine list | grep gcc

# 查看调试日志
RUST_LOG=debug epkg -e dev-alpine install gcc 2>&1 | grep -E "ebin_exposure|Exposing"
```

**修复**：
```bash
# 直接请求包以触发暴露
epkg -e dev-alpine install gcc

# 验证
ls -la ~/.epkg/envs/dev-alpine/ebin/gcc
```

### 问题 3: 跨设备硬链接失败

**症状**：
```
Error: Failed to create hardlink or copy elf-loader:
       Invalid cross-device link (os error 18)
```

**原因**：`~/.epkg` 和根目录在不同文件系统。

**诊断**：
```bash
# 检查文件系统
df ~/.epkg /
```

**修复**：
```bash
# 系统应自动降级为复制，不影响使用
# 如果仍失败，检查权限
ls -la ~/.epkg/envs/self/usr/bin/elf-loader
```

### 问题 4: 隐藏 symlink 冗余

**症状**：
```bash
$ ls -la ~/.epkg/envs/dev-alpine/ebin/
.e gcc.target → ...
.egcc.target → ...   # 多余的 symlink
```

**原因**：重复安装或异常退出导致。

**诊断**：
```bash
# 查找所有隐藏 symlink
ls -la ~/.epkg/envs/dev-alpine/ebin/.*target
```

**修复**：
```bash
# 清理多余 symlink（保留单个 .xxx.target）
rm ~/.epkg/envs/dev-alpine/ebin/.e*

# 或者重新安装包
epkg -e dev-alpine install --force gcc
```

### 问题 5: 脚本包装器执行失败

**症状**：
```bash
$ ~/.epkg/envs/dev-alpine/ebin/npm --version
/bin/sh: node: command not found
```

**原因**：包装器中的解释器路径不正确。

**诊断**：
```bash
# 查看包装器内容
cat ~/.epkg/envs/dev-alpine/ebin/npm

# 检查解释器是否存在
ls -la ~/.epkg/envs/dev-alpine/ebin/node
```

**修复**：
```bash
# 重新安装包以重建包装器
epkg -e dev-alpine install --force nodejs npm
```

## 调试日志模式

### 成功的 ELF 处理日志

```
[INFO] handle_elf: target_path=/home/wfg/.epkg/envs/dev-alpine/ebin/gcc, fs_file=/home/wfg/.epkg/store/.../fs/usr/bin/gcc
[INFO]   elf_loader_path=/home/wfg/.epkg/envs/self/usr/bin/elf-loader
[INFO]   Target exists, removing...
[INFO]   Removed existing target
[DEBUG] handle_elf_with_loader target_path=/home/wfg/.epkg/envs/dev-alpine/ebin/gcc, ...
```

### 成功的脚本包装器日志

```
[DEBUG] Created script wrapper: ebin_path=/home/wfg/.epkg/envs/dev-alpine/ebin/npm,
        fs_file=/home/wfg/.epkg/store/.../fs/usr/bin/npm,
        file_type=NodeScript, first_line="#!/usr/bin/env node"
```

### elf-loader 损坏时的日志

```
[INFO] handle_elf: target_path=/home/wfg/.epkg/envs/dev-alpine/ebin/gcc
[INFO]   elf_loader_path=/home/wfg/.epkg/envs/self/usr/bin/elf-loader
Error: Failed to create hardlink or copy elf-loader:
       ... (可能 elf-loader 已损坏)
```

## 工具脚本

### 检查 elf-loader 完整性

```bash
#!/bin/bash
# check-elf-loader.sh

ELF_LOADER="$HOME/.epkg/envs/self/usr/bin/elf-loader"

if [ ! -f "$ELF_LOADER" ]; then
    echo "ERROR: elf-loader not found"
    exit 1
fi

FILE_TYPE=$(file -b "$ELF_LOADER")
if [[ "$FILE_TYPE" != ELF* ]]; then
    echo "ERROR: elf-loader is not an ELF binary"
    echo "  Type: $FILE_TYPE"
    echo ""
    echo "To fix:"
    echo "  cp /c/epkg/git/elf-loader/src/loader $ELF_LOADER"
    exit 1
fi

echo "OK: elf-loader is a valid ELF binary"
echo "  Type: $FILE_TYPE"
exit 0
```

### 检查 ebin 包装器

```bash
#!/bin/bash
# check-ebin-wrappers.sh

ENV_ROOT="$HOME/.epkg/envs/$1/ebin"

if [ ! -d "$ENV_ROOT" ]; then
    echo "ERROR: ebin directory not found: $ENV_ROOT"
    exit 1
fi

echo "Checking ebin wrappers in $ENV_ROOT"
echo ""

BROKEN=0
for wrapper in "$ENV_ROOT"/*; do
    if [ ! -f "$wrapper" ]; then
        echo "BROKEN: $wrapper (not a file)"
        BROKEN=1
        continue
    fi

    if [ ! -x "$wrapper" ]; then
        echo "BROKEN: $wrapper (not executable)"
        BROKEN=1
        continue
    fi

    # Check if it's a script wrapper
    if file "$wrapper" | grep -q "ASCII text"; then
        echo "SCRIPT: $wrapper"
    else
        # ELF binary (hard link to elf-loader)
        INODE=$(stat -c %i "$wrapper" 2>/dev/null)
        echo "ELF:    $wrapper (inode: $INODE)"
    fi
done

if [ $BROKEN -eq 1 ]; then
    echo ""
    echo "Some wrappers are broken."
    exit 1
fi

echo ""
echo "All wrappers OK."
exit 0
```

## 恢复流程

### 完整恢复 ebin 包装器

```bash
# 1. 验证 elf-loader
file ~/.epkg/envs/self/usr/bin/elf-loader

# 2. 如有问题，恢复 elf-loader
cp /c/epkg/git/elf-loader/src/loader ~/.epkg/envs/self/usr/bin/elf-loader

# 3. 清理 ebin 目录
rm -rf ~/.epkg/envs/<env>/ebin/*

# 4. 重新安装所有包
for pkg in $(epkg -e <env> list); do
    epkg -e <env> install --force $pkg
done
```

### 恢复单个包的 ebin 包装器

```bash
# 1. 删除现有包装器
rm ~/.epkg/envs/<env>/ebin/<tool>
rm ~/.epkg/envs/<env>/ebin/.<tool>.target 2>/dev/null

# 2. 重新安装包
epkg -e <env> install --force <pkg>

# 3. 验证
ls -la ~/.epkg/envs/<env>/ebin/<tool>
```

## 架构限制：Conda 环境

### 症状

```bash
$ ~/.epkg/envs/dev-conda/ebin/gcc --version
error: can't open /lib64/ld-linux-x86-64.so.2
```

### 原因

**Conda 的应用程序设计为直接在 host OS 上运行，无需 ebin 包装器。**

Conda 环境与传统发行版的差异：
- 库路径：`lib/` 而非 `usr/lib/`
- 无 `/lib64/ld-linux-x86-64.so.2`
- 自包含设计，不依赖系统库

### 解决方案

使用 `epkg run`：

```bash
epkg -e dev-conda run -- gcc --version
```

这是设计差异，而非 bug。
