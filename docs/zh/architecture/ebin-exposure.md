# Ebin 暴露架构设计

## 设计目标

1. **用户意图优先**：用户显式请求的包必须创建 ebin 包装器
2. **避免冗余**：依赖安装的包不创建 ebin 包装器
3. **动态更新**：当依赖包后来被用户直接请求时，应创建 ebin 包装器
4. **元包处理**：元包的依赖也应创建 ebin 包装器

## Ebin 包装器类型

根据宿主平台和包类型，ebin/ 目录中创建的包装器类型不同：

### Linux 平台

- **ELF 二进制文件**: 使用 `elf-loader` 硬链接
  - `elf-loader` 是 epkg 内置组件，处理解释器路径和库搜索路径
  - 通过硬链接 elf-loader 到 ebin/<程序>，配合隐藏符号链接实现透明加载
  - 这是唯一支持直接从 ebin/ 运行二进制文件的平台

- **脚本文件**: 创建 shell 包装器脚本
  - 支持 shebang 解析和解释器链创建
  - 处理 Python、Ruby、Node.js、Perl、Lua 等脚本

### macOS 平台

**对于原生 macOS 发行版 (homebrew):**
- **Mach-O 二进制文件**: 创建 shell 脚本包装器 (`#!/bin/sh\nexec <binary> "$@"`)
  - 可以直接在 macOS 主机上运行
  - ebin/ 包装器可以直接执行

- **脚本文件**: 创建 shell 包装器脚本

**对于 Linux 发行版 (在 macOS 上通过 libkrun VM 运行):**
- **ELF 二进制文件**: 不创建 ebin/ 包装器（跳过）
  - ELF 二进制文件需要 VM 执行，创建包装器过于复杂且脚本解释器本身也是 ELF
  - 用户需要通过 `epkg run` 执行命令

- **脚本文件**: 不创建 ebin/ 包装器（跳过）
  - 脚本解释器（如 /bin/sh, python3）是 ELF 二进制文件
  - 在 ebin/ 中创建的脚本包装器本身也需要在 VM 中运行
  - 统一要求使用 `epkg run` 执行

### Windows 平台

**对于原生 Windows 发行版 (msys2/conda):**
- **PE 二进制文件**: 创建符号链接或 shell 脚本包装器
  - 可以直接在 Windows 主机上运行（通过 MSYS2/Conda 环境）
  - ebin/ 包装器可以直接执行

- **脚本文件**: 创建 shell 包装器脚本

**对于 Linux 发行版 (在 Windows 上通过 VM 运行):**
- **ELF 二进制文件**: 不创建 ebin/ 包装器（跳过）
  - ELF 二进制文件需要 VM 执行
  - 用户需要通过 `epkg run` 执行命令

- **脚本文件**: 不创建 ebin/ 包装器（跳过）
  - 脚本解释器是 ELF 二进制文件
  - 统一要求使用 `epkg run` 执行

### 测试注意事项

在 dev-projects 测试中，`run_ebin()` 函数的行为：

- **Linux 主机**: 直接执行 `$ENV_ROOT/ebin/$bin`
- **原生 Windows/macOS 发行版 (msys2/conda/brew)**: 直接执行 `$ENV_ROOT/ebin/$bin`
- **Linux 发行版在非 Linux 主机上**: 使用 `epkg -e $ENV_NAME run -- $bin` 执行

这是因为：
1. Linux 发行版在 macOS/Windows 上需要 VM 执行
2. 对于 Linux 发行版，ebin/ 目录为空（没有包装器被创建）
3. 原生 Windows/macOS 发行版不需要 VM，可以像 Linux 主机一样直接执行
4. 使用 `epkg run` 确保在需要 VM 时有正确的环境设置

## 核心数据结构

### InstalledPackageInfo

```rust
pub struct InstalledPackageInfo {
    pub pkgline: String,           // store 中的路径，如 "abc123__gcc__14.2.0__x86_64"
    pub depend_depth: u16,         // 依赖深度 (0=用户直接请求)
    pub ebin_exposure: bool,       // 是否需要创建 ebin 包装器
    pub depends: BTreeSet<String>, // 正向依赖
    pub rdepends: BTreeSet<String>,// 反向依赖
}
```

### InstallationPlan

```rust
pub struct InstallationPlan {
    // 新包（fresh installs + upgrades）
    pub new_pkgs: InstalledPackagesMap,

    // 已安装但本次请求涉及的包
    pub skipped_reinstalls: InstalledPackagesMap,

    // 操作序列
    pub ordered_operations: Vec<PackageOperation>,
    // ...
}
```

## 关键函数

### 1. update_ebin_exposure_for_user_requested

**位置**：`src/depends.rs`

**作用**：为用户直接请求的包设置 `ebin_exposure=true`

### 2. extend_ebin_by_source

**位置**：`src/depends.rs`

**作用**：将与用户请求包同源的包的 `ebin_exposure` 设置为 `true`

### 3. fill_pkglines_in_plan

**位置**：`src/store.rs`

**作用**：填充包 store 路径，包括 skipped reinstalls

### 4. expose_packages

**位置**：`src/install.rs`

**作用**：创建 ebin 包装器

```rust
fn expose_packages(plan: &mut InstallationPlan) -> Result<()> {
    let mut pkgkeys_to_expose = Vec::new();

    // 从 ordered_operations 收集 (SHOULD_EXPOSE 标志)
    pkgkeys_to_expose.extend(
        plan.ordered_operations.iter()
            .filter(|op| op.should_expose())
            .filter_map(|op| op.new_pkgkey.clone())
    );

    // 从 skipped_reinstalls 收集
    for (pkgkey, info) in plan.skipped_reinstalls.iter() {
        if info.ebin_exposure {
            pkgkeys_to_expose.push(pkgkey.clone());
        }
    }

    // 收集元包的依赖 (新增)
    let meta_exposures = crate::depends::get_meta_package_exposures(&plan.new_pkgs)?;
    pkgkeys_to_expose.extend(meta_exposures);

    // 为每个包创建 ebin 包装器
    for pkgkey in pkgkeys_to_expose {
        let store_fs_dir = get_store_fs_dir(plan, pkgkey)?;
        crate::expose::expose_package(plan, &store_fs_dir, &pkgkey)?;
    }
}
```

### 5. get_meta_package_exposures（元包处理）

**位置**：`src/depends.rs`

**作用**：返回元包的所有依赖 pkgkeys，用于暴露

**原理**：元包（如 `default-jdk`）没有自己的可执行文件，依赖其他包提供实际功能。
只对元包传播 exposure，避免普通包（如 `gcc`）的依赖被不必要地暴露。

**判断元包**：
1. `0 < installed_size < 200KB`
2. no bin/* in filelist

`installed_size` 单位是 bytes。各发行版的源单位：
- Debian: `Installed-Size` 是 KB，解析时追加 "000" 转换为 bytes
- RPM: `RPMTAG_SIZE` 已经是 bytes
- Arch Linux: `%ISIZE%` 已经是 bytes
- Conda: size 已经是 bytes

**注意**：小包如 `rubypick` 可能有很小的 installed_size 但仍提供二进制文件，
所以需要 filelist 判断。

**调用时机**：在 `expose_packages` 中调用，此时 `link_packages` 已完成，
filelist 可用于更精确的元包检测。

## 调用流程

```
┌─────────────────────────────────────────────────┐
│  resolve_and_install_packages                   │
│  (src/depends.rs)                               │
│                                                 │
│  1. resolve_dependencies_adding_makepkg_deps    │
│  2. update_ebin_exposure_for_user_requested     │
│  3. extend_ebin_by_source                       │
│  4. prepare_installation_plan                   │
└─────────────────────────────────────────────────┘
                                                   │
┌──────────────────────────────────────────────────┘
│
│  ┌─────────────────────────────────────────────┐
│  │  execute_installation_plan                  │
│  │  (src/install.rs)                           │
│  │                                             │
│  │  1. download_and_unpack_packages            │
│  │  2. link_packages                           │
│  │  3. run_transaction_batch                   │
│  │     - 使用 SHOULD_EXPOSE 标志暴露包         │
│  │  4. expose_packages()                       │
│  │     - get_meta_package_exposures()          │
│  │     - 暴露元包的依赖                        │
│  └─────────────────────────────────────────────┘
```

## 不变量

1. **用户请求包必须有 ebin_exposure=true**
   - 通过 `update_ebin_exposure_for_user_requested()` 保证

2. **元包依赖必须暴露**
   - 通过 `get_meta_package_exposures()` 在 `expose_packages()` 中处理

3. **skipped_reinstalls 的 pkgline 必须填充**
   - 通过 `fill_pkglines_in_plan()` 处理 skipped_reinstalls 保证

## 模块职责

| 模块 | 职责 |
|------|------|
| `depends.rs` | 计算 ebin_exposure、元包检测 |
| `plan.rs` | 分类包到 skipped_reinstalls |
| `store.rs` | 填充 skipped_reinstalls 的 pkgline |
| `install.rs` | 执行暴露（包括元包依赖） |
| `expose.rs` | 创建 ebin 包装器文件 |
