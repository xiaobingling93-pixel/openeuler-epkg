# Ebin 暴露架构设计

## 设计目标

1. **用户意图优先**：用户显式请求的包必须创建 ebin 包装器
2. **避免冗余**：依赖安装的包不创建 ebin 包装器
3. **动态更新**：当依赖包后来被用户直接请求时，应创建 ebin 包装器
4. **元包处理**：元包的依赖也应创建 ebin 包装器

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
