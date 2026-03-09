# ELF Loader 用户指南

## 什么是 ELF Loader

ELF Loader 是 epkg 的核心组件，它让你可以直接从宿主机运行环境中的 ELF 二进制文件，无需进入环境。

```bash
# 无需进入环境，直接运行
~/.epkg/envs/dev-alpine/ebin/gcc --version

# 而不是
epkg -e dev-alpine run -- gcc --version
```

## 你看到的 ebin 包装器

### ELF 二进制包装器

```bash
# 查看 ebin 包装器
ls -la ~/.epkg/envs/dev-alpine/ebin/

# 输出示例
-rwxr-xr-x  282 user user  18K  gcc      # 硬链接计数=282
-rwxr-xr-x  282 user user  18K  g++      # 共享同一个 inode
-rwxr-xr-x    1 user user  120  npm      # 脚本包装器
```

**硬链接计数说明**：
- 计数 > 1 表示是硬链接（共享 inode）
- 计数 = 1 表示是独立文件或脚本

### 脚本包装器

```bash
# 查看脚本包装器内容
cat ~/.epkg/envs/dev-alpine/ebin/npm

# 输出示例
#!/bin/sh
exec node "/home/wfg/.epkg/envs/dev-alpine/usr/share/nodejs/npm/bin/npm-cli.js" "$@"
```

## 使用场景

### 场景 1：安装编译工具

```bash
# 安装 gcc/g++
epkg -e dev-alpine install build-base

# 直接使用 ebin 包装器
~/.epkg/envs/dev-alpine/ebin/gcc --version
~/.epkg/envs/dev-alpine/ebin/g++ --version

# 或添加到 PATH
export PATH="$HOME/.epkg/envs/dev-alpine/ebin:$PATH"
gcc --version
```

### 场景 2：安装语言运行时

```bash
# 安装 Node.js
epkg -e dev-alpine install nodejs npm

# 使用 ebin 包装器
~/.epkg/envs/dev-alpine/ebin/node --version
~/.epkg/envs/dev-alpine/ebin/npm --version

# 安装包
~/.epkg/envs/dev-alpine/ebin/npm install -g typescript
```

### 场景 3：安装 Python 工具

```bash
# 安装 Python
epkg -e dev-alpine install python3 py3-pip

# 使用 ebin 包装器
~/.epkg/envs/dev-alpine/ebin/python3 --version
~/.epkg/envs/dev-alpine/ebin/pip3 --version

# 安装包
~/.epkg/envs/dev-alpine/ebin/pip3 install --user requests
```

## 验证 ELF Loader 状态

### 检查 elf-loader 是否正常

```bash
# 检查文件类型（应为 ELF 二进制）
file ~/.epkg/envs/self/usr/bin/elf-loader

# 正确输出：
# elf-loader: ELF 64-bit LSB pie executable, ...

# 错误输出（已损坏）：
# elf-loader: ASCII text  ← 变成脚本了！
```

### 检查 ebin 包装器

```bash
# 检查包装器类型
file ~/.epkg/envs/dev-alpine/ebin/*

# 检查硬链接
ls -li ~/.epkg/envs/dev-alpine/ebin/gcc ~/.epkg/envs/dev-alpine/ebin/g++
# inode 相同表示共享硬链接

# 检查包装器是否可执行
~/.epkg/envs/dev-alpine/ebin/gcc --version
```

## 常见问题

### 问题 1: Exec format error

```bash
$ ~/.epkg/envs/dev-alpine/ebin/gcc --version
bash: ~/.epkg/envs/dev-alpine/ebin/gcc: Exec format error
```

**原因**：elf-loader 损坏（变成脚本而非 ELF 二进制）。

**解决**：见 [故障排查](./troubleshooting/elf-loader.md)

### 问题 2: No such file or directory

```bash
$ ~/.epkg/envs/dev-alpine/ebin/gcc: No such file or directory
```

**原因**：包作为依赖安装，未创建 ebin 包装器。

**解决**：直接请求包以触发暴露
```bash
epkg -e dev-alpine install gcc
```

### 问题 3: 跨设备错误

```bash
Error: Failed to create hard link: Invalid cross-device link
```

**原因**：`~/.epkg` 和根目录在不同文件系统。

**解决**：系统会自动降级为复制，不影响使用。

## 最佳实践

1. **使用 ebin 包装器而非 run 命令**
   ```bash
   # 推荐
   ~/.epkg/envs/dev/ebin/gcc main.c -o main

   # 不推荐（每次都要输入 epkg -e ... run --）
   epkg -e dev run -- gcc main.c -o main
   ```

2. **添加 ebin 到 PATH**
   ```bash
   export PATH="$HOME/.epkg/envs/dev-alpine/ebin:$PATH"
   ```

3. **不要手动修改 ebin 文件**
   - 需要更新时重新安装包
   - 手动修改会破坏硬链接

4. **定期验证 elf-loader**
   ```bash
   file ~/.epkg/envs/self/usr/bin/elf-loader
   ```
