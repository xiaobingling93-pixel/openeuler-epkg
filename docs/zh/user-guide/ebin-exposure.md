# Ebin 包装器暴露机制

## 核心概念

### 什么是 ebin 包装器

ebin 包装器是位于 `~/.epkg/envs/<env>/ebin/` 目录下的可执行文件包装器，用于直接暴露用户**显式请求**的包中的可执行文件。

```bash
# ebin 包装器示例
~/.epkg/envs/dev-alpine/ebin/gcc  → 指向 store 中的 gcc 包
~/.epkg/envs/dev-alpine/ebin/g++  → 指向 store 中的 g++ 包
```

### ebin vs bin 包装器

| 特性 | ebin 包装器 | bin 包装器 (tool_wrapper) |
|------|-----------|--------------------------|
| 位置 | `env/ebin/` | `env/bin/` |
| 创建时机 | 用户显式请求包时 | 包安装时自动创建 |
| 目标 | 直接指向 store 中的包 | 经过加速层的包装器 |
| 用途 | 暴露用户请求的工具 | 提供统一的命令入口 |

## 使用场景

### 场景 1：直接安装工具
```bash
# 用户直接请求 gcc，会创建 ebin 包装器
epkg -e dev-alpine install gcc

# 验证 ebin 包装器
ls -la ~/.epkg/envs/dev-alpine/ebin/gcc
~/.epkg/envs/dev-alpine/ebin/gcc --version
```

### 场景 2：包作为依赖安装
```bash
# build-base 依赖 gcc，gcc 作为依赖安装，不创建 ebin 包装器
epkg -e dev-alpine install build-base

# ebin/gcc 不存在，但 bin/gcc 存在（通过 tool_wrapper 创建）
ls ~/.epkg/envs/dev-alpine/ebin/gcc  # 不存在
ls ~/.epkg/envs/dev-alpine/bin/gcc   # 存在
```

### 场景 3：重新请求已安装的依赖
```bash
# 之前 gcc 作为依赖安装，没有 ebin 包装器
# 现在直接请求 gcc，会创建 ebin 包装器
epkg -e dev-alpine install gcc

# ebin/gcc 现在存在
ls -la ~/.epkg/envs/dev-alpine/ebin/gcc
```

## 工作机制

### ebin_exposure 标志

每个包在安装时都有 `ebin_exposure` 标志：

| 情况 | ebin_exposure | 是否创建 ebin 包装器 |
|------|---------------|---------------------|
| 用户直接请求的包 | true | 是 |
| 依赖安装的包 | false | 否 |
| 与用户请求包同源的包 | true | 是 |
| 重新请求的已安装包 | true | 是 |

### 暴露流程

```
用户请求 install gcc
    ↓
resolve_and_install_packages
    ↓
update_ebin_exposure_for_user_requested  ← 设置 gcc 的 ebin_exposure=true
    ↓
extend_ebin_by_source  ← 扩展到同源包（如 gcc-libs）
    ↓
prepare_installation_plan
    ↓
classify_packages  ← 如果 gcc 已安装，放入 skipped_reinstalls
    ↓
fill_pkglines_in_plan  ← 填充 pkgline（包括 skipped_reinstalls）
    ↓
execute_installation_plan
    ↓
如果 !go_on（无变化）:
    expose_packages(skipped_reinstalls)  ← 创建 ebin 包装器
```

## 故障排查

### 问题：ebin 包装器不存在

```bash
# 错误信息
~/.epkg/envs/dev-alpine/ebin/gcc: No such file or directory

# 排查步骤
# 1. 确认包是否已安装
epkg -e dev-alpine list | grep gcc

# 2. 启用调试日志
RUST_LOG=debug epkg -e dev-alpine install gcc 2>&1 | grep -E "ebin_exposure|Exposing"

# 3. 检查输出
# 应看到:
# - "Setting ebin_exposure=true for user-requested package: gcc..."
# - "Exposing package: gcc..."
```

### 问题：pkgline 为空

```bash
# 错误信息
Failed to expose package: Package filesystem directory does not exist: /home/wfg/.epkg/store/fs

# 原因：skipped_reinstall 的 pkgline 未填充

# 排查
RUST_LOG=trace epkg -e dev-alpine install gcc 2>&1 | grep -E "fill_pkglines|pkgline"
```

## 相关配置

无特殊配置，ebin 暴露由系统自动管理。

## 历史修复

### 2026-03-09: 修复 skipped reinstall 的 ebin 暴露

**问题**：当用户请求已安装的包时，ebin 包装器未被创建。

**修复**：
1. 添加 `update_ebin_exposure_for_user_requested()` 设置用户请求包的 `ebin_exposure=true`
2. 修改 `fill_pkglines_in_plan()` 处理 `skipped_reinstalls`，填充 `pkgline`
3. 在 `execute_installation_plan()` 中，即使无操作也暴露 `skipped_reinstalls`
