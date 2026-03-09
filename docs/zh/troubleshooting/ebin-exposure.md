# Ebin 暴露问题排查指南

## 快速诊断

### 症状：命令找不到
```bash
# 错误信息
~/.epkg/envs/dev-alpine/ebin/gcc: No such file or directory

# 快速检查
ls -la ~/.epkg/envs/<env>/ebin/<tool>

# 如果不存在，继续以下排查
```

## 排查流程

### 步骤 1：确认包安装状态

```bash
# 检查包是否已安装
epkg -e <env> list | grep <pkgname>

# 检查 store 中是否有包
ls ~/.epkg/store/ | grep <pkgname>
```

### 步骤 2：启用调试日志

```bash
# 调试级别
RUST_LOG=debug epkg -e <env> install <pkg> 2>&1 | tee /tmp/epkg-debug.log

# 追踪级别（最详细）
RUST_LOG=trace epkg -e <env> install <pkg> 2>&1 | tee /tmp/epkg-trace.log
```

### 步骤 3：搜索关键日志

```bash
# 检查 ebin_exposure 设置
grep "ebin_exposure" /tmp/epkg-debug.log

# 检查 skipped_reinstalls 处理
grep -E "skipped_reinstall|No changes planned" /tmp/epkg-debug.log

# 检查暴露执行
grep -E "Exposing package" /tmp/epkg-debug.log
```

## 常见问题

### 问题 1：ebin 包装器未创建

**症状**：
```
~/.epkg/envs/dev-alpine/ebin/gcc: No such file or directory
```

**可能原因**：
1. 包作为依赖安装，`ebin_exposure=false`
2. 重新请求已安装包时，暴露逻辑未执行

**排查**：
```bash
# 查看 ebin_exposure 设置
RUST_LOG=debug epkg -e dev-alpine install gcc 2>&1 | grep "ebin_exposure=true"

# 应看到类似输出：
# Setting ebin_exposure=true for user-requested package: gcc__15.2.0-r2__x86_64 (gcc)
```

**解决**：
```bash
# 重新请求包以触发暴露
epkg -e dev-alpine install gcc

# 验证
ls -la ~/.epkg/envs/dev-alpine/ebin/gcc
```

### 问题 2：pkgline 为空

**症状**：
```
Error: Failed to expose package gcc__15.2.0-r2__x86_64
       Package filesystem directory does not exist: /home/wfg/.epkg/store/fs
```

**原因**：skipped_reinstall 的 pkgline 未填充，路径变成 `store_root.join("").join("fs")`

**排查**：
```bash
# 查看 pkgline 填充
RUST_LOG=trace epkg -e dev-alpine install gcc 2>&1 | grep "fill_pkglines"

# 应看到：
# fill_pkglines_in_plan: matched skipped reinstall gcc__... -> pkgline <hash>__gcc__...
```

### 问题 3：无操作时未暴露

**症状**：
```
No changes planned based on the current request.
# 但 ebin 包装器仍未创建
```

**排查**：
```bash
# 查看 skipped_reinstalls 检查
RUST_LOG=debug epkg -e dev-alpine install gcc 2>&1 | grep -E "No operations planned|skipped reinstalls need exposure"

# 应看到：
# No operations planned, but 1 skipped reinstalls need exposure
# Exposing package: gcc__...
```

## 调试命令

### 查看 ebin 包装器详情

```bash
# 文件类型
file ~/.epkg/envs/<env>/ebin/<tool>

# 硬链接计数（与 elf-loader 共享）
ls -l ~/.epkg/envs/<env>/ebin/<tool>

# 实际目标
readlink -f ~/.epkg/envs/<env>/ebin/<tool>

# 脚本内容（如果是脚本包装器）
cat ~/.epkg/envs/<env>/ebin/<tool>
```

### 查看包信息

```bash
# 包详情
epkg show <pkgkey>

# 包在 store 中的位置
ls ~/.epkg/store/<pkgkey>/fs/

# 包提供的可执行文件
find ~/.epkg/store/<pkgkey>/fs -type f -executable
```

### 验证修复

```bash
# 1. 删除 ebin 包装器
rm ~/.epkg/envs/<env>/ebin/<tool>

# 2. 重新触发暴露
epkg -e <env> install <pkg>

# 3. 验证包装器重建
ls -la ~/.epkg/envs/<env>/ebin/<tool>
```

## 日志模式参考

### 成功的 ebin 暴露

```
[DEBUG] Setting ebin_exposure=true for user-requested package: gcc__15.2.0-r2__x86_64 (gcc)
[DEBUG] fill_pkglines_in_plan: processing skipped reinstall gcc__15.2.0-r2__x86_64
[DEBUG] fill_pkglines_in_plan: matched skipped reinstall gcc__15.2.0-r2__x86_64 -> pkgline abc123__gcc__15.2.0-r2__x86_64
[INFO] No operations planned, but 1 skipped reinstalls need exposure
[INFO] Exposing package: gcc__15.2.0-r2__x86_64
[DEBUG] Exposing package: gcc__15.2.0-r2__x86_64 (store_fs_dir: /home/wfg/.epkg/store/abc123__gcc__15.2.0-r2__x86_64/fs)
```

### 失败的 ebin 暴露（缺少 pkgline）

```
[DEBUG] Setting ebin_exposure=true for user-requested package: gcc__15.2.0-r2__x86_64 (gcc)
[INFO] No operations planned, but 1 skipped reinstalls need exposure
[INFO] Exposing package: gcc__15.2.0-r2__x86_64
[ERROR] Failed to expose package: Package filesystem directory does not exist: /home/wfg/.epkg/store/fs
```

### 失败的 ebin 暴露（未触发暴露）

```
[DEBUG] Setting ebin_exposure=true for user-requested package: gcc__15.2.0-r2__x86_64 (gcc)
No changes planned based on the current request.
# 缺少后续日志，说明 expose_packages 未被调用
```

## 工具脚本

### 检查 ebin 包装器完整性

```bash
#!/bin/bash
# 检查环境中所有 ebin 包装器

ENV_ROOT="$HOME/.epkg/envs/$1/ebin"

for wrapper in "$ENV_ROOT"/*; do
    if [ ! -f "$wrapper" ]; then
        echo "BROKEN: $wrapper"
        continue
    fi

    target=$(readlink -f "$wrapper" 2>/dev/null)
    if [ ! -x "$target" ]; then
        echo "BROKEN: $wrapper -> $target (not executable)"
    else
        echo "OK: $wrapper -> $target"
    fi
done
```

### 重建 ebin 包装器

```bash
#!/bin/bash
# 重建指定包的 ebin 包装器

ENV="$1"
PKG="$2"

# 删除现有包装器
rm -f ~/.epkg/envs/$ENV/ebin/$PKG

# 重新触发安装（会重建包装器）
epkg -e $ENV install $PKG
```
