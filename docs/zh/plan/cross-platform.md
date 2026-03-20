# epkg 跨平台支持计划 (Windows/macOS)

## 一、目标概述

### 1.1 核心目标

| 平台 | 支持范围 | 架构 |
|------|---------|------|
| **Linux** | 全功能（现有） | 单二进制 |
| **macOS** | Conda/Homebrew + Linux 发行版（VM） | 双二进制 |
| **Windows** | Conda/msys2 仅限 | 单二进制 |

### 1.2 各平台目标详解

#### Windows（计划缩减）
- **仅支持**：原生安装/运行 Conda 包（未来支持 msys2/pacman）
- **不支持**：双二进制架构、各种 Linux 发行版
- **原因**：
  - WSL1/WSL2 已存在，用户可在 WSL 下使用 Linux 版 epkg
  - libkrun 目前只支持 macOS，不支持 Windows
  - Conda/msys2 包都是原生 Windows .exe，无需 Linux 环境

#### macOS（双二进制架构）
- **原生支持**：Conda/Homebrew（无需 VM）
- **VM 支持**：Linux 发行版包（通过 libkrun）
- **Linux ELF epkg 用途**：
  1. (必需) 作为 Linux VM rootfs 的 `/usr/bin/init`（symlink 指向）
  2. (可选) 用户在 VM sandbox 内管理 Linux 包

#### Conda/Homebrew/msys2 共同特点

**Scriptlets 支持情况**：
| 格式 | Scriptlet 类型 | 执行环境 |
|------|---------------|---------|
| **Conda** | pre-link, post-link, pre-unlink | 原生脚本（bash/bat） |
| **Homebrew** | post_install method | 原生 Ruby 脚本 |
| **msys2** | archlinux 风格 scripts/hooks | 原生 bash 脚本 |

**关键点**：
- 所有 scriptlets 都是**原生可执行格式**（bash 脚本或 .bat/.exe）
- **不依赖 Linux 内核特性**（namespace/bind mount 等）
- 可在原生环境中直接执行，无需 VM sandbox

**无需 Linux 特性的原因**：
- Conda: 使用 `.sh` (Unix) 或 `.bat` (Windows) 脚本
- Homebrew: Ruby 脚本在原生 Ruby 环境中运行
- msys2: bash 脚本在 msys2 bash 环境中运行
- `epkg run conda-pkg-exe` 直接作为原生进程执行

### 1.3 macOS VM Sandbox 工作流程

```
'epkg install -e linux-env --isolate=vm PKGS'

[macOS epkg]                      [VM sandbox]
     │                                  │
     │ 1. resolve/download/file ops     │
     │    (native, 全程运行)            │
     │                                  │
     │ 2. fork_and_execute()            │
     │    with IsolateMode::Vm          │
     ├─────────────────────────────────>│
     │                                  │ 3. 执行 postinst
     │                                  │    scriptlet/hook
     │                                  │
     │ 4. 返回结果                      │
     │<─────────────────────────────────┤
     │                                  │
     │ 5. VM 保持运行                   │
     │    (后续复用)                    │
     │                                  │
```

**优化点**：
- native resolve/download/file ops（高效）
- VM sandbox 复用，服务后续 `fork_and_execute()` 调用
- 无需新建通信协议，复用现有 `IsolateMode::Vm` 机制

---

## 二、当前状态分析

### 2.1 构建系统（已就绪）

| 平台 | Rust Target | 工具链 | 状态 |
|-----|-------------|-------|------|
| macOS aarch64 | `aarch64-apple-darwin` | osxcross | ✅ 已实现 |
| macOS x86_64 | `x86_64-apple-darwin` | osxcross | ⚠️ 暂不支持 |
| Windows x86_64 | `x86_64-pc-windows-gnu` | mingw-w64 | ✅ 已实现 |

> **注意**：x86_64 macOS 即将淘汰，除非实现成本极低，否则不主动支持。

### 2.2 模块平台依赖分类

| 类别 | 模块 | Linux | macOS | Windows |
|-----|------|-------|-------|---------|
| **跨平台** | `dirs`, `models`, `io`, `download`, `depends`, `resolve`, `store`, `plan`, `utils`, `mtree`, `repo`, `mirror`, `location`, `package`, `conda_*`, `arch_repo`, `arch_pkg`, `shebang`, `version_*`, `parse_*`, `scriptlets`, `info`, `list`, `search` | ✅ | ✅ | ✅ |
| **Unix 专用** | `posix`, `hash`, `ipc`, `environment`, `init`, `deinit`, `run`, `namespace`, `idmap`, `mount`, `qemu`, `vm_client`, `hooks`, `userdb`, `transaction`, `gc`, `service`, `tool_wrapper`, `apparmor`, `history`, `xdesktop`, `aur`, `deb_*`, `rpm_*`, `apk_*` | ✅ | ✅ | ❌ |
| **Linux 专用** | `busybox/init.rs`, `busybox/vm_daemon.rs`, `busybox/modprobe.rs`, `libkrun` | ✅ | ❌ | ❌ |

---

## 三、Windows 跨平台支持约束

### 3.1 Windows 平台限制

Windows 用户默认没有创建符号链接（symlink）的权限，需要管理员权限或开发者模式。

### 3.2 可用的链接类型

| 类型 | 适用对象 | 权限要求 | 备注 |
|------|----------|----------|------|
| **Hard Link** | 文件 | 无特殊权限 | 两个文件指向同一数据，删除源文件不影响目标 |
| **Junction** | 目录 | 无特殊权限 | 只能使用绝对路径，类似目录符号链接 |
| **Symlink** | 文件/目录 | 管理员权限或开发者模式 | 正常用户不可用 |

### 3.3 实现策略

在 `lfs.rs` 底层实现中：

1. **`symlink()` 函数**：
   - 文件：创建 Hard Link
   - 目录：创建 Junction（需要绝对路径）
   - 失败时返回错误，调用者应优雅处理

2. **`force_symlink()` 函数**（在 `utils.rs`）：
   - "force" 含义是"覆盖已存在的链接"，不是"强制创建符号链接"
   - 先删除目标，再创建链接
   - Windows 上底层使用 Hard Link/Junction

### 3.4 代码原则（重要！）

**第一原则：最大化代码复用！禁止偷懒写Windows特定代码！**

Windows 移植的目标是支持原生 Conda/msys2 包，这意味着大量代码应该在 Windows 上直接工作。

**关键约束**：

1. **最大化代码复用（禁止复制业务流程）**：
   - 不要因为“Windows 可能不同”而把一套下载/校验/安装流程复制成新的 Windows 专用实现
   - 先抽出/复用共享 helper：Windows `#[cfg(windows)]` 代码块应尽量只做“参数映射”（asset 名称、安装目标路径等），把真正流程复用到公共函数里

2. **cfg  hygiene：cfg 不是业务边界**
   - `#[cfg(unix/windows/linux)]` 早期可能只是为了“先编译通过”而临时加的；不要把它当作长期业务逻辑的分界线
   - 长期目标：把“业务流程”提升到公共层，把平台差异通过参数/返回值（例如 `Option`）注入，而不是复制整段流程
   - `cfg` 只应保留在“无法编译/无法运行”的最低层（例如依赖不存在、系统调用语义完全不同），尽量别用它来分裂可共享的逻辑

3. **平台特定代码限制在底层或局部修改**：
   - 底层：`lfs.rs`（symlink/hardlink/junction 语义）、`utils.rs`（通用工具）
   - 局部：函数内部的条件编译，而非独立函数
   - **优先级**：底层修改 > 局部条件编译 > 独立函数

4. **自举（self install/upgrade）业务逻辑复用**：
   - 像“下载 epkg 二进制 + 下载对应 `.sha256` + 校验 + 原子复制/安装”这类业务流程，优先复用同一段代码
   - 平台差异通过参数注入，例如：`asset_name`、`target_epkg` 路径等

5. **symlink 透明化**：
   - `lfs::symlink()` 和 `utils::force_symlink()` 在 Windows 上应该能正常工作
   - 调用者不需要知道底层是 symlink、hardlink 还是 Junction
   - 只有极少数场景需要真正符号链接时才特殊处理

6. **不要假设问题**：
   - 遇到不确定的情况，**明确提问**
   - 不要基于假设创建 Windows 特定逻辑
   - 不要"以防万一"创建备用代码路径

7. **最小化平台特定代码**：
   - 优先在底层（`lfs.rs`, `utils.rs`）处理平台差异
   - 上层代码尽量复用，不创建重复逻辑

8. **透明化**：
   - 大部分情况下，`force_symlink()` 调用者在 Windows 上可以正常工作
   - 只有少数需要真正符号链接的场景需要特殊处理

9. **错误处理**：
   - `symlink()` 在 Windows 上可能失败
   - 调用者应该优雅处理失败，而不是假设成功

**反面案例**：
```rust
// 错误！创建了独立的 Windows 代码路径，偷懒不复用！
#[cfg(windows)]
fn setup_self_binaries() -> Result<()> {
    // 50+ 行重复逻辑...
}

// 正确！复用现有代码
fn setup_common_binaries(env_root: &Path, init_plan: &InitPlan) -> Result<()> {
    // 跨平台逻辑
    #[cfg(target_os = "linux")]
    { /* Linux 特有的 elf-loader */ }
}

// 错误！把通用数据结构标记为 Unix 特有！
#[cfg(unix)]
struct InitPlan { ... }

// 正确！数据结构是通用的，Linux 特有字段用条件编译
struct InitPlan {
    epkg_binary_path: PathBuf,
    #[cfg(target_os = "linux")]
    elf_loader_path: PathBuf,
}
```

### 3.5 已知问题

1. **Junction 需要绝对路径**：
   - 相对路径会在 `lfs::symlink()` 中被转换为绝对路径

2. **Hard Link 限制**：
   - 只能用于同一卷上的文件
   - 不能用于目录

### 3.6 测试要点

- [ ] 文件 Hard Link 创建
- [ ] 目录 Junction 创建
- [ ] `force_symlink()` 覆盖已存在的链接
- [ ] 相对路径转换为绝对路径

---

## 四、平台差异分析

### 4.1 macOS vs Linux

**难度**: ⭐⭐ (低)

| 差异点 | Linux | macOS | 解决方案 |
|-------|-------|-------|---------|
| errno 位置 | `__errno_location()` | `__error()` | 条件编译 |
| shebang 长度 | 127 | 512 | 运行时检查 |
| 文件系统 | 大小写敏感 | 默认不敏感 | 注意路径处理 |
| pivot_root | 支持 | 不支持 | 见下文 |

**IsolateMode 限制**：
- `IsolateMode::Fs`：需要 pivot_root，macOS 不支持，不实现
- `IsolateMode::Vm`：使用 libkrun，macOS 支持
- `IsolateMode::None`：Conda/Homebrew/msys2 使用
  - 对应 `run_options.skip_namespace_isolation` flag
  - 跳过 namespace 隔离，直接在主机环境执行

**macOS VM Sandbox 限制**：
- 无 namespace/bind mount 支持
- 无 subuid 映射（仅 1 个 host uid/gid）
- **接受限制**：VM 内不需要多用户支持（类似 Docker 容器，非完整 Linux 安装）

**未来改进方向**：
- `--mount`：libkrun 多 virtiofs 挂载
- 多用户：在磁盘镜像上创建 rootfs（ext4/btrfs），可自由使用 uid

### 4.2 Windows vs Linux

**难度**: ⭐⭐⭐ (中，因仅支持 Conda/msys2)

| 差异点 | Linux | Windows | 解决方案 |
|-------|-------|---------|---------|
| symlink | 标准 | 需管理员/开发者模式 | junction (目录) + symlink_file (文件) |
| 权限模型 | POSIX (rwx) | ACL | 忽略（Conda/msys2 包是原生 .exe） |
| 路径分隔符 | `/` | `\` | 使用 `Path`/`PathBuf` |
| 可执行权限 | `chmod +x` | `.exe` 扩展名 | 检查扩展名 |

**简化理由**：Windows 仅支持 Conda/msys2，包是原生 Windows .exe，无需 clone/unshare/namespace 或 rwx 权限位。

---

## 五、跨平台 Rust Crates 选择

### 5.1 已使用的跨平台 Crates

| Crate | 功能 | 状态 |
|-------|------|------|
| `dirs` | 跨平台用户目录 | ✅ 已使用 |
| `walkdir` | 跨平台目录遍历 | ✅ 已使用 |
| `tempfile` | 跨平台临时文件 | ✅ 已使用 |
| `pathdiff` | 跨平台相对路径 | ✅ 已使用 |
| `tar`, `flate2`, `zstd`, `liblzma` | 跨平台压缩 | ✅ 已使用 |
| `filetime` | 跨平台文件时间戳 | ✅ 已使用 |
| `sys-info` | 基础系统信息 | ✅ 已使用 |
| `ctrlc` | 跨平台 Ctrl-C | ✅ 已使用 |

### 5.2 推荐新增的 Crates

```toml
# Cargo.toml
[dependencies]
which = "7.0"              # 跨平台可执行文件查找

[target.'cfg(windows)'.dependencies]
junction = "1.2"           # Windows Junction Point（无需管理员权限）
remove_dir_all = "1.0"     # Windows 可靠删除目录
```

### 5.3 使用示例

#### `which` - 替换手动路径搜索

```rust
// 替换 utils.rs 中约 50 行的 find_command_in_paths()
pub fn find_command_in_paths(command_name: &str) -> Option<PathBuf> {
    which::which(command_name).ok()
}
```

#### `junction` - Windows 目录链接

```rust
#[cfg(windows)]
pub fn create_dir_link(original: &Path, link: &Path) -> Result<()> {
    // Junction: 不需要管理员权限或开发者模式
    junction::create(original, link)
        .map_err(|e| eyre!("Failed to create junction: {}", e))
}

#[cfg(unix)]
pub fn create_dir_link(original: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(original, link)
        .map_err(|e| eyre!("Failed to create symlink: {}", e))
}
```

---

## 六、代码组织策略

### 6.1 平台抽象方式选择

**问题**：是否需要 `trait Platform`？

**分析**：

| 方案 | 优点 | 缺点 |
|-----|------|------|
| **A. trait Platform** | 类型安全、IDE 支持好 | 过重、与现有 lfs.rs/dirs.rs 冲突 |
| **B. 独立函数** | 简单、渐进式、复用现有代码 | 无编译时检查缺失函数 |

**选定方案 B**：独立函数

理由：
1. 现有 `lfs.rs` 已封装大部分文件系统操作
2. 现有 `dirs.rs` 已处理大部分路径配置
3. 缺失函数会在编译时报错
4. 渐进式修改，风险低

### 6.2 文件组织

```
src/
├── lfs.rs              # 已有：文件系统操作（添加 Windows 实现）
├── dirs.rs             # 已有：路径配置（添加 macOS/Windows 路径）
├── platform/           # 新增：平台特定独立函数
│   ├── mod.rs          #    导出平台特定函数
│   ├── unix.rs         #    Unix 共用函数
│   ├── macos.rs        #    macOS 特定函数（如有）
│   └── windows.rs      #    Windows 特定函数
```

### 6.3 平台特定函数示例

```rust
// src/platform/mod.rs

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

// 跨平台默认实现
pub fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata().map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(windows)]
    {
        path.extension().map(|e| e == "exe" || e == "bat" || e == "cmd").unwrap_or(false)
    }
}
```

```rust
// src/platform/windows.rs

pub fn create_dir_link(original: &Path, link: &Path) -> Result<()> {
    junction::create(original, link)
        .map_err(|e| eyre!("Failed to create junction: {}", e))
}

pub fn create_file_link(original: &Path, link: &Path) -> Result<()> {
    use std::os::windows::fs::symlink_file;
    symlink_file(original, link)
        .wrap_err_with(|| format!("Failed to create file symlink"))
}

pub fn is_running_as_root() -> bool {
    // Windows: 检查管理员权限
    // 简化实现：Conda/msys2 不需要此功能
    false
}
```

```rust
// src/platform/unix.rs

pub fn create_dir_link(original: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(original, link)
        .map_err(|e| eyre!("Failed to create symlink: {}", e))
}

pub fn is_running_as_root() -> bool {
    nix::unistd::geteuid().is_root()
}
```

---

## 七、跨平台路径映射

### 7.1 Conda 包安装位置参考

| 平台 | 用户安装（默认） | 系统安装 |
|------|-----------------|----------|
| Linux | `~/anaconda3`, `~/miniconda3` | `/opt/conda` |
| macOS | `~/anaconda3`, `~/miniconda3` | `/opt/anaconda3` |
| Windows | `%USERPROFILE%\anaconda3` | `C:\ProgramData\anaconda3` |

### 7.2 epkg 路径映射方案

| 路径用途 | Linux | macOS | Windows |
|---------|-------|-------|---------|
| **全局安装目录** | `/opt/epkg` | `/opt/epkg` | `C:\Program Files\epkg` |
| **用户安装目录** | `~/.epkg` | `~/.epkg` | `%LOCALAPPDATA%\epkg` |
| **store (包存储)** | `~/.epkg/store` (private) 或 `/opt/epkg/store` (shared) | 同 Linux | `%LOCALAPPDATA%\epkg\store` |
| **cache (下载缓存)** | `~/.cache/epkg` | `~/Library/Caches/epkg` | `%LOCALAPPDATA%\epkg\cache` |
| **envs (环境目录)** | `~/.epkg/envs` (private) 或 `/opt/epkg/envs/$USER` (shared) | 同 Linux | `%LOCALAPPDATA%\epkg\envs` |
| **epkg 二进制** | `~/.epkg/envs/self/usr/bin/epkg` | `~/.epkg/envs/self/usr/bin/epkg` | `%LOCALAPPDATA%\epkg\envs\self\usr\bin\epkg.exe` |
| **elf-loader** | `~/.epkg/envs/self/usr/bin/elf-loader` | - | - |
| **Linux ELF epkg** | - | `~/.epkg/envs/self/usr/bin/epkg-linux-<arch>`（例如 x86_64/aarch64） | - |

**说明**：
- epkg 二进制统一放在 `~/.epkg/envs/self/usr/bin/` 目录下
- macOS 的 Linux ELF epkg 也放在同一目录，便于管理
- Windows 不需要 elf-loader 和 Linux ELF epkg

### 7.3 dirs.rs 修改要点

```rust
// src/dirs.rs

pub fn get_home() -> Result<String> {
    dirs::home_dir()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .ok_or_else(|| eyre!("Cannot determine home directory"))
}

pub fn get_cache_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".cache"))
            .join("epkg")
    }
    #[cfg(target_os = "linux")]
    {
        PathBuf::from(get_home().unwrap_or_default())
            .join(".cache/epkg")
    }
    #[cfg(windows)]
    {
        dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".cache"))
            .join("epkg")
    }
}

pub fn get_epkg_store(shared: bool) -> PathBuf {
    if shared {
        #[cfg(unix)]
        { PathBuf::from("/opt/epkg/store") }
        #[cfg(windows)]
        { PathBuf::from(r"C:\Program Files\epkg\store") }
    } else {
        #[cfg(unix)]
        { PathBuf::from(get_home().unwrap_or_default()).join(".epkg/store") }
        #[cfg(windows)]
        { dirs::data_local_dir().unwrap().join("epkg/store") }
    }
}
```

### 7.4 Conda 包目录布局（跨平台一致）

```
prefix/
├── bin/              # Linux/macOS 可执行文件
├── Scripts/          # Windows 可执行文件（替代 bin）
├── lib/              # 库文件
├── Library/          # Windows 特有
│   ├── bin/
│   ├── mingw-w64/bin/
│   └── usr/bin/
├── include/          # 头文件
├── conda-meta/       # 包元数据
│   ├── history
│   └── *.json
└── etc/              # 配置文件
```

---

## 八、实施计划

### Phase 1: macOS 基础支持（工作量：小）

**目标**：macOS 原生编译通过，支持 Conda 包管理

**任务清单**：

1. **修复编译错误**
   - `lposix.rs`：errno 处理（`__error()` vs `__errno_location()`）
   - `posix.rs`：macOS API 差异
   - `Cargo.toml`：确保 nix crate macOS features 正确

2. **添加跨平台依赖**
   ```toml
   [dependencies]
   which = "7.0"
   ```

3. **验证与测试**
   - `make cross-macos aarch64`
   - `epkg install python`（conda 包）

**修改文件**：
- `src/lposix.rs`, `src/posix.rs` - errno 处理
- `Cargo.toml` - 添加 which 依赖

### Phase 2: Windows 基础支持（工作量：中）

**目标**：Windows 编译通过，支持 Conda 包管理

**任务清单**：

1. **添加依赖**
   ```toml
   [target.'cfg(windows)'.dependencies]
   junction = "1.2"
   remove_dir_all = "1.0"
   ```

2. **修改 lfs.rs**
   - 实现 Windows symlink（junction 目录 + symlink_file 文件）
   - 使用 `remove_dir_all` crate

3. **修改 dirs.rs**
   - 添加 Windows 路径映射

4. **创建 platform/windows.rs**
   - 独立平台函数

5. **修改 utils.rs**
   - 使用 `which` crate
   - Windows 权限函数存根

**修改文件**：
- `Cargo.toml`, `src/lfs.rs`, `src/dirs.rs`, `src/utils.rs`
- 新增 `src/platform/mod.rs`, `src/platform/windows.rs`

### Phase 3: Conda 包完整支持（工作量：中）

**目标**：Conda 包在 Windows/macOS 完整可用

**任务清单**：

1. **conda_link.rs 跨平台验证**
   - Windows 目录链接使用 junction
   - 验证符号链接创建逻辑

2. **conda_pkg.rs 验证**
   - Conda post-link/pre-link/pre-unlink 脚本是原生格式
   - 无需 VM sandbox，直接在主机环境执行
   - 验证包解析和 scriptlet 执行逻辑

3. **shebang.rs 适配**
   - Windows：生成 `.bat` 包装脚本或使用 shebang 转换

4. **激活脚本生成**
   - Windows：`.bat`/`.ps1`
   - macOS：`.sh`

### Phase 4: macOS VM Sandbox 集成（工作量：大）

**目标**：macOS 支持 Linux 发行版包（scriptlets 在 VM 执行）

**前提条件**：
- Phase 1-3 完成（macOS 原生功能就绪）
- libkrun 集成（现有代码）

**任务清单**：

1. **双二进制安装**
   - `epkg self install` 安装两个二进制
   - macOS 原生 epkg: `~/.epkg/envs/self/usr/bin/epkg`
   - Linux ELF epkg: `~/.epkg/envs/self/usr/bin/epkg-linux`

2. **复用现有 IsolateMode::Vm 机制**
   - 无需新建通信协议
   - `fork_and_execute()` 设置 `IsolateMode::Vm` 选项
   - VM sandbox 复用优化

---

## 九、build.rs busybox 平台检测

### 方案选择

**推荐**：在 `build.rs` 中维护 central list

```rust
// build.rs

/// Linux 专用 applets
const LINUX_ONLY: &[&str] = &[
    "init",
    "vm_daemon",
    "modprobe",
];

/// Unix 专用 applets（不支持 Windows）
const UNIX_ONLY: &[&str] = &[
    "stat",
    "chroot",
    // ...
];

fn generate_busybox_modules(applets: &[(&str, &str)]) -> String {
    let mut code = String::new();

    for (module, cmd_name) in applets {
        let is_linux_only = LINUX_ONLY.contains(module);
        let is_unix_only = UNIX_ONLY.contains(module);

        if is_linux_only {
            code.push_str(&format!("#[cfg(target_os = \"linux\")]\n"));
        } else if is_unix_only {
            code.push_str(&format!("#[cfg(unix)]\n"));
        }

        code.push_str(&format!("mod {};\n", module));
        // ... 注册代码
    }

    code
}
```

**优点**：
- 集中维护，一目了然
- 无需解析源文件
- 易于审查和修改

---

## 十、测试策略

### 10.1 功能测试矩阵

| 功能 | Linux | macOS | Windows |
|-----|-------|-------|---------|
| Conda 包下载 | ✅ | ✅ | ✅ |
| Conda 包解析 | ✅ | ✅ | ✅ |
| Conda 包链接 | ✅ | ✅ | ✅ (junction) |
| Conda 包运行 | ✅ | ✅ | ✅ |
| Homebrew 包 | ❌ | ✅ (未来) | ❌ |
| msys2 包 | ❌ | ❌ | ✅ (未来) |
| Debian/RPM/APK | ✅ | ✅ (VM) | ❌ |
| Arch 包 | ✅ | ✅ (VM) | ❌ |

### 10.2 编译测试

```yaml
# CI matrix
strategy:
  matrix:
    include:
      - os: ubuntu-latest
        target: x86_64-unknown-linux-musl
      - os: macos-latest
        target: aarch64-apple-darwin
      - os: windows-latest
        target: x86_64-pc-windows-gnu
```

---

## 十一、风险与缓解

| 风险 | 缓解措施 |
|-----|---------|
| Windows symlink 权限 | 使用 junction（无需权限）+ hardlink/copy 降级 |
| macOS 代码签名 | 提供 Homebrew formula |
| VM sandbox 启动延迟 | VM 复用机制 |

---

## 附录 A：关键文件修改清单

### 高优先级
| 文件 | 修改内容 |
|-----|---------|
| `Cargo.toml` | 添加 `which`, `junction`, `remove_dir_all` |
| `src/lfs.rs` | Windows symlink 实现 |
| `src/dirs.rs` | macOS/Windows 路径映射 |
| `src/platform/mod.rs` | 新建：平台函数导出 |
| `src/platform/windows.rs` | 新建：Windows 特定函数 |

### 中优先级
| 文件 | 修改内容 |
|-----|---------|
| `src/utils.rs` | 使用 which crate |
| `src/main.rs` | 模块条件编译调整 |
| `build.rs` | busybox 平台检测 |
| `src/lposix.rs` | macOS errno |

### 低优先级
| 文件 | 修改内容 |
|-----|---------|
| `src/shebang.rs` | Windows 脚本处理 |
| `src/conda_link.rs` | junction 支持 |
