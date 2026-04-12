# Homebrew/Linuxbrew 包支持架构设计

## 概述

Homebrew 是 macOS 和 Linux 上流行的包管理器。epkg 通过支持 Homebrew 预编译包（bottles），让用户可以快速安装和使用这些软件包，同时保持与主机系统的隔离。

## 核心设计原则

1. **路径兼容性优先**：brew bottles 包含硬编码的 HOMEBREW_PREFIX 路径，必须确保这些路径在运行时可用
2. **渐进式隔离**：根据环境配置自动选择最合适的隔离级别
3. **主机最小依赖**：尽可能在环境内部解决依赖，减少对外部系统的依赖

## 平台差异与 HOMEBREW_PREFIX

不同平台使用不同的标准安装前缀：

| 平台 | HOMEBREW_PREFIX | 说明 |
|------|-----------------|------|
| Linux | `/home/linuxbrew/.linuxbrew` | 用户目录下可写，无需 root |
| macOS ARM | `/opt/homebrew` | Apple Silicon 标准路径 |
| macOS Intel | `/usr/local` | Intel Mac 传统路径 |

### 前缀选择原因

- **Linux 使用 `/home/linuxbrew/.linuxbrew`**：
  - 避免写入系统目录（/usr/local 通常需要 root）
  - 与官方 Homebrew on Linux 推荐路径一致
  - 用户 home 目录下可写，便于管理

- **macOS 使用标准路径**：
  - ARM 与 Intel 路径不同，便于区分架构
  - 与官方 Homebrew 安装指南一致

## 使用场景与完整流程

### 场景 1：Linux，HOMEBREW_PREFIX 不存在或不可写

**适用情况**：首次使用 brew 环境，或没有 sudo 权限

**流程**：
```
1. 创建环境时检测到 brew 格式
2. 尝试创建 HOMEBREW_PREFIX 目录
   a. 直接创建（如果有权限）
   b. 尝试 sudo 创建（如果用户同意）
3. 如果创建成功且目录为空 → 使用 HOMEBREW_PREFIX 作为 env_root
4. 如果创建失败 → 使用 ~/.epkg/envs/<name> 作为 env_root
5. 运行时使用 --isolate=fs 模式
   - 创建 home/linuxbrew/.linuxbrew -> ../../../../ 符号链接
   - pivot_root 后该链接指向新的根目录
```

**命令示例**：
```bash
epkg env init test-brew --channel brew-core
epkg run -e test-brew -- tree --version  # 自动使用 Fs 模式
```

### 场景 2：Linux，HOMEBREW_PREFIX 存在且为空

**适用情况**：管理员已预先创建 HOMEBREW_PREFIX 并授权给用户

**流程**：
```
1. 检测到 HOMEBREW_PREFIX 存在且为空
2. 直接使用 HOMEBREW_PREFIX 作为 env_root
3. 包直接安装到 HOMEBREW_PREFIX
4. 运行时跳过 namespace 隔离（env_root == HOMEBREW_PREFIX）
5. ELF 文件中的占位符指向实际路径（$HOMEBREW_PREFIX/...）
```

**优势**：无需 namespace 隔离，性能最好，可直接在宿主机运行二进制文件

**命令示例**：
```bash
# 管理员预先创建目录
sudo mkdir -p /home/linuxbrew/.linuxbrew
sudo chown $(id -u):$(id -g) /home/linuxbrew/.linuxbrew

# 用户创建环境
epkg env init system-brew --channel brew-core
epkg run -e system-brew -- tree --version  # 无隔离，原生性能
```

### 场景 3：Linux，HOMEBREW_PREFIX 存在且非空

**适用情况**：系统已安装其他 brew 包，或目录被占用

**流程**：
```
1. 检测到 HOMEBREW_PREFIX 存在且有内容
2. 使用 ~/.epkg/envs/<name> 作为 env_root
3. 运行时使用 namespace 隔离（Env 或 Fs 模式）
   - Env 模式（默认）：绑定挂载 $env_root -> /home/linuxbrew/.linuxbrew
   - Fs 模式（回退）：创建符号链接，pivot_root 进入沙箱
```

### 场景 4：macOS

**特殊约束**：macOS 无法使用用户命名空间进行绑定挂载

**流程**：
```
1. 创建环境时检查 HOMEBREW_PREFIX
2. 如果 HOMEBREW_PREFIX 不存在 → 尝试创建（需要 sudo）
3. 如果 HOMEBREW_PREFIX 存在且为空 → 使用作为 env_root
4. 如果 HOMEBREW_PREFIX 存在且非空 → 报错（无法使用其他路径）
```

**原因**：macOS 不支持 Linux 的用户命名空间机制，无法实现路径重映射

## 隔离模式详细说明

### 无隔离模式（env_root == HOMEBREW_PREFIX）

**触发条件**：环境根目录恰好等于 HOMEBREW_PREFIX

**行为**：
- 设置 `skip_namespace_isolation = true`
- 不创建 mount namespace
- 不执行 pivot_root
- 二进制文件直接运行，无需路径转换

**适用场景**：
- 个人开发环境
- 性能敏感场景
- 需要与系统其他工具集成的场景

### Env 模式（--isolate=env）

**触发条件**：HOMEBREW_PREFIX 存在且可访问，env_root != HOMEBREW_PREFIX

**挂载策略**：
```
$env_root -> /home/linuxbrew/.linuxbrew  (绑定挂载)
```

**特点**：
- 轻量级隔离
- 与主机共享 /usr, /etc 等目录
- 仅重定向 brew 包路径

### Fs 模式（--isolate=fs）

**触发条件**：HOMEBREW_PREFIX 不存在，或显式指定

**目录结构**：
```
$env_root/
├── home/
│   └── linuxbrew/
│       └── .linuxbrew -> ../../../../  (相对符号链接)
├── usr/
│   ├── bin/     (epkg 工具链)
│   ├── lib/     (主机库挂载点)
│   └── lib64/   (主机库挂载点)
├── bin/         (brew 包二进制文件)
├── lib/         (brew 包库文件)
└── ...
```

**挂载策略**：
```
1. 不挂载主机 /home（避免覆盖环境的 home 结构）
2. 挂载 /opt/epkg（只读，用于包操作）
3. 挂载主机库目录（用于动态链接）：
   - /lib64 -> $env_root/usr/lib64
   - /lib/x86_64-linux-gnu -> $env_root/usr/lib/x86_64-linux-gnu
4. pivot_root 到 $env_root
```

**符号链接工作原理**：
- 符号链接 `home/linuxbrew/.linuxbrew -> ../../../../`
- 从 `.linuxbrew` 向上 4 层到达 `env_root`
- pivot_root 后，env_root 成为新的根目录 `/`
- 因此链接指向 `/`，即新的根目录

## ELF 文件处理

### RPATH 重写策略

**目标**：使二进制文件在两种场景下都能工作
1. **直接运行**（无隔离）：RPATH 指向 $env_root 下的实际路径
2. **沙箱运行**（Fs 模式）：通过符号链接或绑定挂载使路径有效

**实现**：`src/brew_pkg.rs`

**占位符替换**：
- `@@HOMEBREW_PREFIX@@` -> `/home/linuxbrew/.linuxbrew`（或平台对应前缀）
- `@@HOMEBREW_CELLAR@@` -> `/home/linuxbrew/.linuxbrew/Cellar`

**技术细节**：
- 使用 `goblin` 库解析 ELF
- 修改 PT_INTERP 段（动态链接器路径）
- 修改 DT_RPATH/DT_RUNPATH 动态标签
- 原地修改，保持文件结构

**路径设计示例**：
```
原始 bottle 中的 RPATH: @@HOMEBREW_PREFIX@@/lib
重写后: /home/linuxbrew/.linuxbrew/lib

场景 A（env_root = HOMEBREW_PREFIX）：
- 路径直接指向实际位置
- 无隔离运行：正常工作

场景 B（env_root = ~/.epkg/envs/test-brew，Fs 模式）：
- 沙箱内 /home/linuxbrew/.linuxbrew 是符号链接
- 指向沙箱根目录，与 env_root 内容一致
- 因此 /home/linuxbrew/.linuxbrew/lib 有效
```

## 命令查找机制

### find_command_in_env_path() 逻辑

**对于 brew 环境**：
1. 检查 `$HOMEBREW_PREFIX/bin/`
2. 检查 `$HOMEBREW_PREFIX/libexec/bin/`
3. 解析符号链接，返回实际路径

**路径转换**：
- 如果 env_root == HOMEBREW_PREFIX：直接返回真实路径
- 如果 namespace 隔离：返回带 HOMEBREW_PREFIX 前缀的沙箱路径

## 依赖处理

### glibc 策略

**平台差异**：
- **Linux**：brew 提供 `glibc` 包，已设置为 essential，自动安装
  - 使环境自包含，减少主机依赖
  - 可通过 RPATH 指向环境内部的 glibc
- **macOS**：homebrew 不提供 glibc（使用系统 libc）
  - 依赖主机系统库
  - 这是设计选择，与 macOS 系统紧密集成

**当前实现**：
- Linux：安装 brew 的 glibc 作为 essential 包
- macOS：依赖主机系统库

**运行时链接**：
- 优先使用环境内部的库（如果有）
- 主机库作为后备

## 调试与故障排除

### 常见问题

**1. 命令找不到**

检查清单：
- 环境是否为 brew 格式：`cat ~/.epkg/envs/<name>/etc/epkg/channel.yaml`
- HOMEBREW_PREFIX 符号链接是否存在：`ls -la ~/.epkg/envs/<name>/home/linuxbrew/`
- 如果是 Fs 模式，检查 pivot_root 后路径：`epkg run -e <name> --isolate=fs -- ls -la /home/linuxbrew/.linuxbrew/`

**2. 动态链接失败**

检查清单：
- 查看 ELF interpreter：`readelf -l /path/to/binary | grep interpreter`
- 检查 RPATH：`readelf -d /path/to/binary | grep RPATH`
- 验证库文件存在：`ls /home/linuxbrew/.linuxbrew/lib/`

**3. 库版本不匹配**

检查清单：
- 确认主机库版本：`ldd --version`
- 检查 brew 包需要的版本：`strings /path/to/binary | grep GLIBC`
- 考虑升级主机 glibc 或安装 brew 的 glibc

### 调试命令

```bash
# 查看挂载情况
epkg run -e <env> --isolate=fs -- cat /proc/mounts | grep -E "home|lib"

# 检查符号链接
epkg run -e <env> --isolate=fs -- ls -la /home/linuxbrew/

# 验证 ELF 文件
readelf -l /home/linuxbrew/.linuxbrew/bin/tree | grep interpreter
readelf -d /home/linuxbrew/.linuxbrew/bin/tree | grep -E "RPATH|RUNPATH"

# 跟踪运行日志
RUST_LOG=trace epkg run -e <env> -- tree --version 2>&1 | head -50
```

## 平台特定实现

### Linux 实现

**核心文件**：
- `src/namespace.rs`：namespace 隔离逻辑
- `src/brew_pkg.rs`：bottle 提取和 ELF 重写
- `src/environment.rs`：环境创建和 HOMEBREW_PREFIX 处理

**关键系统调用**：
- `unshare(CLONE_NEWNS)`：创建 mount namespace
- `pivot_root()`：切换根文件系统（Fs 模式）
- `mount(MS_BIND)`：绑定挂载（Env 模式）

### macOS 实现

**约束**：
- 无用户命名空间支持
- 无法绑定挂载目录（非 root）
- 必须使用实际 HOMEBREW_PREFIX 路径

**实现差异**：
- 环境根目录必须等于 HOMEBREW_PREFIX
- 无 Fs/Env 模式区分
- 使用 `install_name_tool` 修改动态库路径（而非 ELF 重写）

## 未来发展方向

1. **glibc 捆绑**
   - 在 brew 环境中安装 glibc 作为 essential 包
   - 修改动态链接器路径指向环境内部
   - 完全消除主机库依赖

2. **多架构支持**
   - 支持 ARM64 Linux brew bottles
   - 通过 binfmt_misc 或 QEMU 实现跨架构运行

3. **依赖解析增强**
   - 实现完整的 Homebrew 依赖解析器
   - 支持 `depends_on` 和 `resource` 块
   - 自动安装依赖链

4. **性能优化**
   - 延迟挂载：仅在首次访问时挂载主机库
   - OverlayFS：支持可写的层叠文件系统
   - 二进制缓存：缓存重写后的 ELF 文件
