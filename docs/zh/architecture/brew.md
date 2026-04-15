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

## 隔离模式与目录布局矩阵

### Linux (linuxbrew) 场景矩阵

| 隔离模式 | env_root | .LB 符号链接 | ld.so 路径 | RPATH | 硬编码引用 |
|----------|----------|--------------|------------|-------|------------|
| skip_namespace | env_base | 主机创建 | `/home/linuxbrew/.LB/lib/ld.so` | `.LB/...` | 通过 `.LB->.linuxbrew` 解析 |
| Env (bind mount) | env_base | 主机+环境创建 | `/home/linuxbrew/.LB/lib/ld.so` | `.LB/...` | bind mount + `.LB` symlink |
| Fs (pivot_root) | env_base | 环境创建 | `/home/linuxbrew/.LB/lib/ld.so` | `.LB/...` | `.LB->.linuxbrew->../../../..` |
| VM (libkrun) | env_base | 环境创建 | `/home/linuxbrew/.LB/lib/ld.so` | `.LB/...` | VM 内符号链接链 |

**详细说明**：

#### skip_namespace_isolation

```
主机文件系统：
/home/linuxbrew/
├── .LB -> .linuxbrew           (epkg 创建)
└── .linuxbrew/
    └── lib/
        ├── ld.so -> /lib64/ld-linux-x86-64.so.2  (主机 brew 安装)
        └── ld-linux-x86-64.so.2 -> ../Cellar/glibc/...

env_root = /home/wfg/.epkg/envs/test-brew (bind mount 到 .linuxbrew)

运行时：
- Interpreter: /home/linuxbrew/.LB/lib/ld.so -> .linuxbrew/lib/ld.so -> Cellar/glibc
- RPATH: /home/linuxbrew/.LB/Cellar/... -> .linuxbrew/Cellar/... -> env_root/Cellar/...
- 硬编码: /home/linuxbrew/.linuxbrew/... 通过 bind mount 直接访问 env_root
```

#### Env 模式 (bind mount)

```
namespace 内：
/home/linuxbrew/.linuxbrew = env_root (bind mount)
/home/linuxbrew/.LB = .linuxbrew (主机符号链接，在 namespace 内可见)

运行时：
- Interpreter: /home/linuxbrew/.LB/lib/ld.so -> .linuxbrew/lib/ld.so -> Cellar/glibc
- RPATH: .LB 路径通过符号链解析到 bind mount 点
- 硬编码: /home/linuxbrew/.linuxbrew/... 直接指向 bind mount 的 env_root
```

#### Fs 模式 (pivot_root)

```
pivot_root 后的目录结构：
/ (原 env_root)
├── home/linuxbrew/
│   ├── .linuxbrew -> ../../     (指向新根目录 /)
│   └── .LB -> ../../            (epkg 创建，减少一次查找)
├── lib/
│   ├── ld.so -> ld-linux-x86-64.so.2
│   └── ld-linux-x86-64.so.2 -> ../Cellar/glibc/...
└── Cellar/
    └── glibc/2.39/lib/ld-linux-x86-64.so.2

运行时：
- Interpreter: /home/linuxbrew/.LB/lib/ld.so -> ../../lib/ld.so
- RPATH: .LB -> ../../ (根目录)
- 硬编码: /home/linuxbrew/.linuxbrew/... -> ../../... (根目录)
```

**符号链接相对路径计算**：
```
env_root/home/linuxbrew/.linuxbrew -> ../../

解析过程（从符号链接的父目录 linuxbrew/ 向上）:
  linuxbrew -> home       (../ 第1层)
  home      -> env_root   (../ 第2层)

注意：符号链接从其父目录解析，而非从自身位置解析
```

#### VM 模式 (libkrun)

```
VM 内目录结构与 Fs 模式相同：
- pivot_root 到 env_root
- 符号链接在 VM 内创建
- .LB 和 .linuxbrew 都指向根目录
```

### macOS (homebrew) 场景矩阵

| 隔离模式 | env_root | 前缀 | dylib 路径 | 硬编码引用 |
|----------|----------|------|------------|------------|
| 无隔离 | HOMEBREW_PREFIX | `/opt/homebrew` (ARM) 或 `/usr/local` (Intel) | `@rpath/...` | 直接访问 |

**macOS 特点**：
- 不支持 Linux namespace/bind mount
- 必须使用实际 HOMEBREW_PREFIX 作为 env_root
- dylib 使用 `install_name_tool` 重写，而非 ELF 重写
- 无 `.LB` 短前缀需求（macOS 使用 `@rpath` 相对路径机制）

### 硬编码引用处理

**问题**：部分 brew bottles 在编译时已硬编码 `/home/linuxbrew/.linuxbrew/...` 路径，无占位符。

**解决方案**：
1. **skip_namespace**：bind mount 使硬编码路径指向 env_root
2. **Env/Fs/VM**：`.linuxbrew` 符号链指向根目录或 bind mount 点
3. **所有模式**：`.LB -> .linuxbrew` 使短前缀路径也能解析

**关键符号链接**：
```bash
# 主机（所有 Linux brew 环境需要）
/home/linuxbrew/.LB -> .linuxbrew

# 环境（Fs/VM 模式）
env_root/home/linuxbrew/.LB -> ../../          (pivot_root 后指向 /)
env_root/home/linuxbrew/.linuxbrew -> ../../   (pivot_root 后指向 /)
```

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

**注意**：以下 Env/Fs 隔离模式仅适用于 Linux（linuxbrew），因为只有 Linux 支持普通用户使用 namespace + bind mount。macOS 无此功能，只能直接使用 HOMEBREW_PREFIX 作为 env_root。

### Env 模式（--isolate=env）

**触发条件**：Linux 系统，HOMEBREW_PREFIX 存在且可访问，env_root != HOMEBREW_PREFIX

**挂载策略**：
```
$env_root -> /home/linuxbrew/.linuxbrew  (绑定挂载)
```

**特点**：
- 轻量级隔离
- 不挂载主机的 /usr, /etc（linuxbrew 自包含 glibc 和依赖，不需要主机系统目录）
- 仅重定向 brew 包路径到 HOMEBREW_PREFIX

### Fs 模式（--isolate=fs）

**触发条件**：Linux 系统，HOMEBREW_PREFIX 不存在，或显式指定

**目录结构**：
```
$env_root/                    (pivot_root 后成为新的 /)
├── home/
│   └── linuxbrew/
│       └── .linuxbrew -> ../../../../  (相对符号链接指向新的根目录)
├── usr/                      (仅包含 epkg 工具链，linuxbrew 包不使用 /usr/)
│   └── bin/     (epkg, init 等工具)
├── bin/         (brew 包二进制文件)
├── lib/         (brew 包库文件，包括 glibc)
└── ...
```

**挂载策略**：
```
1. 不挂载主机 /home 目录
   - 环境自带 home/linuxbrew/ 结构
   - 可能挂载当前用户的 $HOME/.epkg 等，但不会覆盖 /home/linuxbrew/

2. 挂载 /opt/epkg（只读，用于包操作）

3. 不挂载主机库目录（/lib64, /lib/x86_64-linux-gnu）
   - linuxbrew 已安装自己的 glibc 作为 essential 包
   - 库和依赖完全自包含，无需使用主机系统库

4. pivot_root 到 $env_root，使环境成为新的根文件系统
```

**符号链接工作原理**：
- 符号链接 `home/linuxbrew/.linuxbrew -> ../../../../`
- 从 `.linuxbrew` 向上 4 层到达 `env_root`
- pivot_root 后，env_root 成为新的根目录 `/`
- 因此链接指向 `/`，即新的根目录
- 所有 brew 包安装在 `bin/`, `lib/` 下，通过该链接可访问

## ELF 文件处理

### 短前缀设计 (.LB)

**问题**：Homebrew bottles 使用 `@@HOMEBREW_PREFIX@@` 占位符（22 字符），替换为完整路径 `/home/linuxbrew/.linuxbrew`（26 字符）会超出缓冲区长度，导致复杂包（如 Python）溢出。

**解决方案**：使用短前缀 `/home/linuxbrew/.LB`（18 字符），比占位符更短，永不溢出。

**设计要点**：

| 组件 | 占位符 | 替换后 | 长度对比 |
|------|--------|--------|----------|
| RPATH | `@@HOMEBREW_PREFIX@@` (22) | `/home/linuxbrew/.LB` (18) | 更短，永不过溢出 |
| Interpreter | `@@HOMEBREW_PREFIX@@/lib/ld.so` (32) | `/home/linuxbrew/.LB/lib/ld.so` (28) | 更短，适配缓冲区 |

**符号链接链**：

```
/home/linuxbrew/.LB -> .linuxbrew            (主机和环境中创建)

在环境中：
lib/ld.so -> ld-linux-x86-64.so.2             (为 interpreter 创建)
lib/ld-linux-x86-64.so.2 -> ../Cellar/glibc/2.39/lib/ld-linux-x86-64.so.2
```

**优势**：
1. **永不溢出**：替换路径比占位符更短，适配任何 bottle
2. **跨隔离模式兼容**：`.LB` 符号链接在主机和环境中都创建，所有模式都能工作
3. **简洁设计**：单一策略，无需 fallback

### RPATH 重写策略

**目标**：使二进制文件在两种场景下都能工作
1. **直接运行**（无隔离）：RPATH 指向 $env_root 下的实际路径
2. **沙箱运行**（Fs/Env/VM 模式）：通过 `.LB` 符号链接使路径有效

**实现**：`src/brew_pkg.rs`

**占位符替换**：
- `@@HOMEBREW_PREFIX@@` -> `/home/linuxbrew/.LB`（短前缀）
- `@@HOMEBREW_CELLAR@@` -> `/home/linuxbrew/.LB/Cellar`（短前缀）

**技术细节**：
- 使用 `goblin` 库解析 ELF
- 修改 PT_INTERP 段（动态链接器路径）为 `/home/linuxbrew/.LB/lib/ld.so`
- 修改 DT_RPATH/DT_RUNPATH 动态标签，使用 `.LB` 短前缀
- 原地修改，保持文件结构
- 短前缀保证永不超过原始缓冲区长度

**路径设计示例**：
```
原始 bottle 中的 RPATH: @@HOMEBREW_PREFIX@@/Cellar/jq/1.8.1/lib
重写后: /home/linuxbrew/.LB/Cellar/jq/1.8.1/lib

符号链接解析：
.LB -> .linuxbrew -> (env_root 或 bind mount)
最终指向: env_root/Cellar/jq/1.8.1/lib
```

### Interpreter 重写策略

**问题**：brew bottles 使用 `@@HOMEBREW_PREFIX@@/lib/ld.so` 作为 interpreter，长度受限。

**解决方案**：
1. 重写为 `/home/linuxbrew/.LB/lib/ld.so`（28 字符）
2. 创建 `lib/ld.so -> ld-linux-x86-64.so.2` 符号链接
3. `lib/ld-linux-x86-64.so.2` 已指向 Cellar/glibc 的 ld.so

**glibc 二进制特殊处理**：
- glibc 包的二进制 interpreter 已经是完整路径（非占位符）
- 例如 `/home/linuxbrew/.linuxbrew/Cellar/glibc/2.39/lib/ld-linux-x86-64.so.2`
- 这些路径无需重写，在 `.LB` 符号链接环境下也能正常解析

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

## 目录布局：Cellar 结构

epkg 的 brew 包采用与 vanilla Homebrew 相同的 Cellar 目录布局：

### 布局结构

```
$env_root/                    (HOMEBREW_PREFIX)
├── Cellar/                   (所有包的实际文件)
│   └── jq/                   (包名)
│       └── 1.7.1/            (版本)
│           ├── bin/
│           │   └── jq        (实际二进制文件)
│           ├── lib/
│           ├── share/
│           ├── .brew/        (formula 文件)
│           └── INSTALL_RECEIPT.json
│   └── git/
│       └── 2.53.0/
│           └── ...
├── bin/                      (符号链接指向 Cellar)
│   ├── jq -> ../Cellar/jq/1.7.1/bin/jq
│   └── git -> ../Cellar/git/2.53.0/bin/git
├── lib/                      (符号链接指向 Cellar)
│   ├── libjq.1.dylib -> ../Cellar/jq/1.7.1/lib/libjq.1.dylib
│   └── libgit2.so -> ../Cellar/git/2.53.0/lib/libgit2.so
├── share/                    (目录级符号链接指向 Cellar)
│   ├── git-core -> ../Cellar/git/2.53.0/share/git-core
│   └── doc/                  (共享目录，非符号链接)
│   └── info/                 (共享目录，非符号链接)
├── opt/                      (包名符号链接指向 Cellar)
│   ├── jq -> ../Cellar/jq/1.7.1
│   └── git -> ../Cellar/git/2.53.0
├── Frameworks/               (macOS Framework)
│   └── Python.framework -> ../Cellar/python@3.12/3.12.0/Frameworks/Python.framework
└── libexec/                  (特殊执行目录)
    └── go -> ../Cellar/go/1.22.0/libexec/go
```

### 设计原因

1. **与 vanilla Homebrew 一致**：
   - 便于理解和调试
   - 与 Homebrew 工具链兼容
   - 文档和经验可直接参考

2. **bin/ 和 lib/ 为真实目录**：
   - 非 usr-merge 符号链接
   - 文件级符号链接便于管理
   - 升级/删除时精确控制

3. **opt/ 符号链接**：
   - 用于包自引用路径
   - 例如 Python 查找自身路径

4. **share/doc/, share/info/ 等**：
   - 共享目录，多包文件共存
   - 非 Cellar 符号链接

### 实现要点

**brew_path_policy()**：
- tar 包路径 `jq/1.7.1/bin/jq`
- 提取到 `Cellar/jq/1.7.1/bin/jq`
- 保留包名/版本层级结构

**create_cellar_symlinks()**：
- 扫描 Cellar/ 目录
- 创建 `bin/`、`lib/` 的文件级符号链接
- 创建 `share/`、`libexec/` 的目录级符号链接
- 创建 `opt/` 的包名符号链接
- 移除 usr-merge 符号链接（brew 环境需要真实 bin/、lib/ 目录）

## 未来发展方向

1. **多架构支持**
   - 支持 ARM64 Linux brew bottles
   - 通过 binfmt_misc 或 QEMU 实现跨架构运行

2. **依赖解析增强**
   - 实现完整的 Homebrew 依赖解析器
   - 支持 `depends_on` 和 `resource` 块
   - 自动安装依赖链

## post_install 执行

### 背景

Homebrew formula 通常定义 `post_install` 方法，在包安装后执行设置任务：
- 创建目录（如 `var/archiva/logs`）
- 运行设置命令（如 `system bin/"abricate", "--setupdb"`）
- 更新缓存（如 `glib-compile-schemas`, `gtk-update-icon-cache`）

### 实现策略

**方案选择**：最小化 Ruby stub（约60行）

**理由**：
- 完整 Homebrew Library 太重（formula.rb 4884行）
- post_install 使用的 API 很集中（90%+只需少数方法）
- Ruby 标准库 Pathname 已提供大部分路径操作

### API 使用统计

基于 homebrew-core 所有 formula 分析：

| API | 使用次数 | 说明 |
|-----|---------|------|
| `system` | 161 | 执行外部命令 |
| `HOMEBREW_PREFIX` | 117 | 全局常量 |
| `var/` | 91 | Pathname 方法 |
| `Formula["pkgname"]` | 86 | 获取其他 formula |
| `bin/` | 81 | Pathname 方法 |
| `.mkpath` | 80 | 创建目录 |
| `prefix` | 76 | 当前 formula prefix |
| `opt_bin` | 74 | 其他 formula opt 路径 |

### 支持的 API

**全局常量**：
- `HOMEBREW_PREFIX` - env_root
- `HOMEBREW_CELLAR` - env_root/Cellar

**Formula 类方法**：
- `Formula[name]` - 返回 Formula 对象

**Formula 实例方法**：
- `prefix`, `bin`, `lib`, `share`, `include`, `libexec`
- `var`, `etc`, `pkgshare`, `pkgetc`
- `opt_prefix`, `opt_bin`, `opt_lib`, `opt_share`

**Pathname 方法**（Ruby 标准库提供）：
- `.mkpath`, `.exist?`, `.install`, `.join`, `/` 操作符

**辅助方法**：
- `system` - 执行命令
- `ohai`, `opoo` - 输出信息
- FileUtils（cp, cp_r, rm, mv 等）

### 执行流程

```
1. 安装包后检测 post_install（文本扫描 formula.rb）
2. 如果存在：
   a. 复制 Ruby stub 到 env_root/Homebrew/Library/Homebrew/
   b. 设置环境变量（HOMEBREW_PREFIX, PATH, TMPDIR 等）
   c. 用 portable-ruby 执行
3. 错误不中断安装（记录警告）
```

### 关键文件

- `src/brew_postinstall.rs` - Rust 模块
- `assets/homebrew/formula_stub.rb` - Ruby stub（约150行，覆盖99% API）

### uses_from_macos 依赖处理

### 背景

Homebrew formula 可以定义 `uses_from_macos` 依赖，表示在 macOS 上使用系统提供的库，但在 Linux 上需要作为真实依赖安装。

**示例**：
```ruby
# curl formula
uses_from_macos "krb5"
uses_from_macos "openldap"
```

### 依赖类型

`uses_from_macos` 支持多种格式：

| 格式 | 示例 | 含义 |
|------|------|------|
| 简单字符串 | `"krb5"` | 运行时依赖 |
| 带类型 | `{"bison": "build"}` | 构建依赖 |
| 多类型 | `{"python": ["build", "test"]}` | 多种依赖类型 |

### 版本约束

部分依赖有版本约束（`uses_from_macos_bounds`）：
```json
"uses_from_macos_bounds": [{"since": "sequoia"}]
```
表示该系统库仅在 macOS Sequoia (15) 或更高版本可用。

### epkg 处理策略

**Linux bottles**：所有 `uses_from_macos` 条目添加为 `recommends`（推荐依赖），而非 `requires`。

**原因**：
1. 这些依赖对功能完整性有影响，但不影响基本运行
2. 用户可选择安装以获得完整功能
3. curl 示例：基本 HTTP 不需要 krb5/openldap，但 Kerberos/LDAP 功能需要

**实现位置**：`src/brew_repo.rs` - `UsesFromMacosEntry` enum 和 `to_package()` 方法

## 命令查找与路径解析

### 绝对路径解析

**问题**：当用户执行 `epkg run -- /bin/sh -c 'echo hello'` 时，需要正确解析 `/bin/sh` 到 env 内的路径。

**解决方案**：`resolve_command_path()` 函数将绝对路径转换为 env_root 内的路径：

```
输入: /bin/sh
转换: env_root/bin/sh
```

**实现位置**：`src/run.rs`

**关键逻辑**：
1. 检查路径是否已处于 env_root 下 → 直接返回
2. 对于 Unix 绝对路径：转换为 `env_root/{relative_path}`
3. 验证文件在 env 中存在

**验证**：
```bash
$ epkg -e dev-brew run -- /bin/sh -c 'echo hello'
hello  # 正确执行 env 内的 bash（通过 bin/sh -> bash symlink）
```

## 环境封闭性与二阶调用

### 模式对比

| 模式 | 平台 | 一阶调用 | 二阶调用 | 说明 |
|------|------|---------|---------|------|
| Env (bind mount) | Linux | env_root/bin/xxx | host 执行 | `/usr/bin` 未挂载 |
| Fs (pivot_root) | Linux | env_root/bin/xxx | env 内执行 | 完全封闭 |
| Vm (libkrun) | Linux | env_root/bin/xxx | VM 内执行 | 完全封闭 |
| 无隔离 | macOS | HOMEBREW_PREFIX/bin/xxx | host 执行 | 无 namespace 支持 |

### macOS 特殊情况

**约束**：macOS 不支持 Linux 的用户命名空间机制：
- 无法使用 `unshare(CLONE_NEWNS)` 创建 mount namespace
- 无法实现 bind mount（非 root 用户）
- 无法实现 pivot_root

**结果**：
- env_root 必须等于 HOMEBREW_PREFIX（`/opt/homebrew` 或 `/usr/local`）
- 所有调用都在 host 上执行，无隔离
- 二阶调用直接访问 host 系统，不存在"跑到 host"的问题（因为本来就在 host 上）

**布局特点**：
- macOS 使用 vanilla Homebrew 目录布局
- `/usr/local/bin` 或 `/opt/homebrew/bin` 是标准路径
- 系统已适配这些路径，不存在路径冲突问题

### Env 模式的二阶调用问题（Linux）

**场景**：
```
一阶调用: epkg run -- /bin/bash -c 'script.sh'
二阶调用: script.sh 中执行 /usr/bin/env 或 /bin/sh
```

**问题**：
- Env 模式只 bind mount `$env_root -> /home/linuxbrew/.linuxbrew`
- `/usr`、`/bin` 等目录未挂载
- 二阶调用会访问到 host 的 `/usr/bin/yyy`

**现有挂载**（Env 模式）：
```
$env_root -> /home/linuxbrew/.linuxbrew  (brew 前缀)
@/usr://usr                              (shebang 支持)
@/etc://etc, @/tmp://tmp, @/var://var    (基本功能)
```

### usr-merge 的复杂性

**主机布局**（主流 Linux）：
```
/bin -> usr/bin
/sbin -> usr/sbin
/lib -> usr/lib
```

**brew env_root 布局**：
```
bin/      (真实目录，非 symlink)
usr/bin/  (真实目录，非 symlink)
```

两者布局不同，无法简单地创建 `/bin -> usr/bin` symlink。

### 解决方案

**推荐**：需要二阶调用封闭性时，使用 Fs 或 VM 模式。

**Fs 模式**：pivot_root 到 env_root，所有路径都在 env 内解析。

**Env 模式**：
- 适合轻量级场景
- 一阶调用已正确解析（通过 `resolve_command_path()`）
- 二阶调用可能访问 host，但这不一定是坏事（如访问 host 的网络配置）

## portable-ruby

Homebrew 依赖 Ruby 运行 post_install 脚本。epkg 安装 `portable-ruby` 作为 essential brew 包：

**目录结构**：
```
env_root/Homebrew/Library/Homebrew/vendor/portable-ruby/
├── 4.0.2_1 -> ../../../../../Cellar/portable-ruby/4.0.2_1
└── current -> 4.0.2_1
```

**好处**：
- 确保 Ruby 版本兼容（Homebrew 要求 Ruby >= 3.4）
- 环境自包含，不依赖主机 Ruby
- 与 vanilla Homebrew 结构一致
