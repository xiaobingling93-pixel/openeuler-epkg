# Ebin 暴露架构设计

## 设计目标

1. **用户意图优先**：用户显式请求的包必须创建 ebin 包装器
2. **避免冗余**：依赖安装的包不创建 ebin 包装器
3. **动态更新**：当依赖包后来被用户直接请求时，应创建 ebin 包装器

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

```rust
fn update_ebin_exposure_for_user_requested(
    packages: &mut InstalledPackagesMap,
    user_request_world: Option<&HashMap<String, String>>,
) -> Result<()> {
    let Some(user_request_world) = user_request_world else {
        return Ok(());
    };

    for requested_name in user_request_world.keys() {
        for (pkgkey, info_arc) in packages.iter_mut() {
            if let Ok(pkgname) = crate::package::pkgkey2pkgname(pkgkey) {
                if &pkgname == requested_name {
                    Arc::make_mut(info_arc).ebin_exposure = true;
                }
            }
        }
    }
    Ok(())
}
```

### 2. extend_ebin_by_source

**位置**：`src/depends.rs`

**作用**：将与用户请求包同源的包的 `ebin_exposure` 设置为 `true`

```rust
fn extend_ebin_by_source(packages: &mut InstalledPackagesMap) -> Result<InstalledPackagesMap> {
    // 1. 收集用户请求包的源包名
    for (pkgkey, info) in packages.iter() {
        if info.ebin_exposure == true {
            let source = load_package_info(pkgkey)?.source;
            user_requested_sources.insert(source);
        }
    }

    // 2. 为同源包设置 ebin_exposure=true
    for (pkgkey, info) in packages.iter() {
        if info.ebin_exposure == false {
            let source = load_package_info(pkgkey)?.source;
            if user_requested_sources.contains(source) {
                Arc::make_mut(info).ebin_exposure = true;
            }
        }
    }
}
```

### 3. fill_pkglines_in_plan

**位置**：`src/store.rs`

**作用**：填充包 store 路径，包括 skipped reinstalls

```rust
pub fn fill_pkglines_in_plan(plan: &mut InstallationPlan) -> Result<usize> {
    // 处理新包
    for op in &mut plan.ordered_operations {
        if let Some(pkgkey) = &op.new_pkgkey {
            try_match_and_fill_pkgline(pkgkey, ...)?;
        }
    }

    // 处理 skipped reinstalls（新增）
    for (pkgkey, info_arc) in plan.skipped_reinstalls.iter_mut() {
        try_match_and_fill_pkgline(pkgkey, info_arc, ...)?;
    }
}
```

### 4. expose_packages

**位置**：`src/install.rs`

**作用**：创建 ebin 包装器

```rust
fn expose_packages(plan: &mut InstallationPlan) -> Result<()> {
    let mut pkgkeys_to_expose = Vec::new();

    // 从 ordered_operations 收集
    pkgkeys_to_expose.extend(
        plan.ordered_operations.iter()
            .filter(|op| op.should_expose())
            .filter_map(|op| op.new_pkgkey.clone())
    );

    // 从 skipped_reinstalls 收集（新增）
    for (pkgkey, info) in plan.skipped_reinstalls.iter() {
        if info.ebin_exposure {
            pkgkeys_to_expose.push(pkgkey.clone());
        }
    }

    // 为每个包创建 ebin 包装器
    for pkgkey in pkgkeys_to_expose {
        let store_fs_dir = get_store_fs_dir(plan, pkgkey)?;
        crate::expose::expose_package(plan, &store_fs_dir, &pkgkey)?;
    }
}
```

### 5. extend_ebin_to_dependencies（元包处理）

**位置**：`src/depends.rs`

**作用**：为元包（meta-package）的依赖设置 `ebin_exposure=true`

元包是指没有自己可执行文件的包，它们依赖其他包来提供实际功能。
例如 `default-jdk` 是一个元包，它依赖 `openjdk-21-jdk` 来提供 `javac` 等工具。

```rust
fn extend_ebin_to_dependencies(packages: &mut InstalledPackagesMap) -> Result<()> {
    // 收集用户请求的包
    let user_requested_pkgkeys: Vec<String> = packages.iter()
        .filter(|(_, info)| info.ebin_exposure)
        .map(|(pkgkey, _)| pkgkey.clone())
        .collect();

    // 使用工作列表算法传播 ebin_exposure
    // 但仅对元包（没有自己可执行文件的包）传播
    let mut worklist: VecDeque<String> = VecDeque::new();

    for pkgkey in &user_requested_pkgkeys {
        if let Some(info) = packages.get(pkgkey) {
            // 仅对元包传播
            if !package_has_binaries(&store_root, &info.pkgline) {
                worklist.push_back(pkgkey.clone());
            }
        }
    }
    // ... 传播逻辑
}
```

**判断元包的方法**：

```rust
fn package_has_binaries(store_root: &Path, pkgline: &str) -> bool {
    let filelist = map_pkgline2filelist(store_root, pkgline)?;

    for file in &filelist {
        let file_lower = file.to_lowercase();
        // 检查是否有 bin/ 或 sbin/ 目录下的文件
        if file_lower.starts_with("bin/") ||
           file_lower.starts_with("sbin/") ||
           file_lower.contains("/bin/") ||
           file_lower.contains("/sbin/") {
            return true;
        }
    }
    false
}
```

**为什么只对元包传播**：
- 用户安装 `gcc` 时，只需要 `gcc` 本身被暴露，不需要所有依赖都被暴露
- 用户安装 `default-jdk` 时，由于它是元包，需要传播到实际提供 `javac` 的 `openjdk-21-jdk`

## 调用流程

```
┌─────────────────────────────────────────────────┐
│  resolve_and_install_packages                   │
│  (src/depends.rs)                               │
│                                                  │
│  1. resolve_dependencies_adding_makepkg_deps    │
│  2. update_ebin_exposure_for_user_requested  ←─┐│
│  3. extend_ebin_by_source                     ←┤│
│  4. prepare_installation_plan                   ││
└─────────────────────────────────────────────────┼│
                                                   │
┌──────────────────────────────────────────────────┘│
│                                                   │
│  ┌─────────────────────────────────────────────┐ │
│  │  prepare_installation_plan                  │ │
│  │  (src/plan.rs)                              │ │
│  │                                             │ │
│  │  1. classify_packages                       │ │
│  │     - new_pkgs                              │ │
│  │     - skipped_reinstalls                    │ │
│  │  2. fill_pkglines_in_plan                ←──┤ │
│  │  3. build_ordered_operations                │ │
│  └─────────────────────────────────────────────┘ │
│                                                   │
┌──────────────────────────────────────────────────┘
│
│  ┌─────────────────────────────────────────────┐
│  │  execute_installation_plan                  │
│  │  (src/install.rs)                           │
│  │                                             │
│  │  1. prompt_and_confirm_install_plan         │
│  │  2. if !go_on:                           ←──┤
│  │     expose_packages(skipped_reinstalls)     │
│  │  3. execute_installations                   │
│  │     if ordered_operations.is_empty():       │
│  │       expose_packages(skipped_reinstalls)   │
│  └─────────────────────────────────────────────┘
```

## 不变量

1. **用户请求包必须有 ebin_exposure=true**
   - 通过 `update_ebin_exposure_for_user_requested()` 保证

2. **skipped_reinstalls 的 pkgline 必须填充**
   - 通过 `fill_pkglines_in_plan()` 处理 skipped_reinstalls 保证

3. **无操作时也要暴露 skipped_reinstalls**
   - 通过 `execute_installation_plan()` 和 `execute_installations()` 中的检查保证

## 模块职责

| 模块 | 职责 |
|------|------|
| `depends.rs` | 计算 ebin_exposure（用户请求 + 同源扩展） |
| `plan.rs` | 分类包到 skipped_reinstalls |
| `store.rs` | 填充 skipped_reinstalls 的 pkgline |
| `install.rs` | 执行暴露（包括 skipped_reinstalls） |
| `expose.rs` | 创建 ebin 包装器文件 |
