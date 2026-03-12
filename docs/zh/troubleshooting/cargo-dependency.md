# Cargo 依赖优化指南

本文档总结了优化 Cargo 依赖、消除重复版本的经验和最佳实践。

## 优化范围与目标

### 核心目标
- **消除重复版本**：减少同一依赖的多个版本共存
- **避免 edition 2024**：在支持前避免使用需要 Rust 2024 edition 的 crate
- **最小化依赖树**：减少不必要的传递依赖

### 优化对象
- **主项目依赖**：`/c/epkg/Cargo.toml` 中定义的直接依赖
- **子仓库依赖**：`/c/epkg/git/**/Cargo.toml` 中的依赖
- **传递依赖**：通过依赖分析工具识别并优化

### 优化原则
1. **优先直接修改**：直接升级/降级依赖版本
2. **次优间接修改**：修改引入重复版本的父依赖
3. **谨慎使用 patch**：仅在必要时使用 `[patch.crates-io]`
4. **避免 git 依赖**：不使用未经发布的 git 版本（过于激进）

## 发现问题的方法

### 1. 查看重复版本（核心命令）

```bash
# 查看所有重复版本的依赖
wfg /c/epkg% grep '".* .*"' Cargo.lock | sort | uniq -c
      1  "bitflags 1.3.2",
     10  "bitflags 2.11.0",
      1  "foldhash 0.1.5",
      1  "foldhash 0.2.0",
      2  "getrandom 0.2.17",
      6  "getrandom 0.3.4",
      1  "hashbrown 0.15.5",
      3  "hashbrown 0.16.1",
      ...
```

**输出解读：**
- 第一列数字表示该版本被引用的次数
- 如果同一依赖有多个不同版本，就是重复版本
- 示例中 `bitflags` 有 `1.3.2` 和 `2.11.0` 两个版本

### 2. 查看特定包的所有版本

```bash
# 查看 hashbrown 的所有版本
wfg /c/epkg% grep '"hashbrown ' Cargo.lock | sort | uniq -c
      1  "hashbrown 0.15.5",
      3  "hashbrown 0.16.1",
```

### 3. 分析依赖链

```bash
# 查看哪个包依赖了特定版本
cargo tree -i getrandom@0.2.17
getrandom v0.2.17
└── ring v0.17.14
    ├── rustls v0.23.37
    │   └── ureq v3.2.0
    │       └── epkg v0.2.4
    └── rustls-webpki v0.103.9
        └── rustls v0.23.37 (*)

# 查看包的完整依赖树
cargo tree -p ring@0.17.14

# 查看包的简洁信息
cargo tree -p versions@6.3.2
```

### 4. 检查 crate 依赖详情

```bash
# 查看 crate 的依赖版本要求（通过 crates.io API）
curl -s "https://crates.io/api/v1/crates/ctrlc/3.5.2/dependencies" | jq '.[] | select(.crate_id == "nix")'
{
  "crate_id": "nix",
  "req": "^0.31",      # 版本要求
  "optional": false
}

# 查看最新版本
cargo search ctrlc --limit 1
ctrlc = "3.5.2"
```

## 典型场景与解决方案

### 场景 1：直接依赖版本冲突

**问题**：`crate A` 需要 `dep ^1.0`，`crate B` 需要 `dep ^2.0`

**解决方案**：
- 如果 API 兼容，尝试统一到一个中间版本
- 否则选择主要依赖的版本，接受次要依赖的重复

### 场景 2：传递依赖版本冲突

**问题**：`A → B → C ^1.0` 和 `D → C ^2.0`

**解决方案**：
1. 升级/降级 `B` 或 `D` 以使用兼容的 `C` 版本
2. 使用 `cargo update --precise` 强制统一版本

### 场景 3：edition 2024 问题

**问题**：新版本 crate 使用 edition 2024

**解决方案**：
```bash
# 检查 registry 中的 edition
grep edition.*2024 /home/wfg/.cargo/registry/src/*/*/Cargo.toml

# Pin 旧版本
versions = "=6.3"  # 而不是 "7.0"
```

### 场景 4：上游限制无法升级

**问题**：`linux-loader` 限制 `vm-memory <=0.17.1`

**解决方案**：
- 在所有地方统一使用受限版本
- 使用 `=0.17.1` 精确锁定版本

## 优化方法论

### 方法 1：修改主项目依赖（推荐）

**适用场景**：直接依赖版本问题

**操作位置**：`/c/epkg/Cargo.toml`

```toml
# 升级到新版本
versions = "7.0"

# 降级避免 edition 2024
versions = "=6.3"

# 降级以统一依赖版本
ctrlc = "=3.5.1"  # 使用 nix 0.30 而不是 0.31
```

### 方法 2：修改子仓库依赖

**适用场景**：子仓库内部依赖需要调整

**操作位置**：`/c/epkg/git/**/Cargo.toml`

```bash
# 示例：修改 libkrun 中的依赖
cd /c/epkg/git/libkrun
# 编辑 src/devices/Cargo.toml 等

# 提交到子仓库
git add src/*/Cargo.toml
git commit -m "devices: upgrade bitflags from 1.x to 2.x"
```

**注意事项**：
- 子仓库修改需要单独提交
- 主项目构建时使用主项目的 `Cargo.lock`
- 子仓库的 `Cargo.lock` 通常不影响主项目

### 方法 3：间接修改（改变父依赖）

**适用场景**：传递依赖版本冲突

**策略**：修改引入旧版本依赖的父 crate 版本

```bash
# 示例：nix 0.31.2 来自 ctrlc 3.5.2
# 降级 ctrlc 到 3.5.1（使用 nix 0.30）
```

### 方法 4：强制统一版本

**命令**：`cargo update --precise`

```bash
# 将 vm-memory 从 0.18.0 降级到 0.17.1
cargo update -p vm-memory@0.18.0 --precise 0.17.1
```

**适用场景**：
- SemVer 允许降级
- 需要快速统一版本而不修改 `Cargo.toml`

### 方法 5：移除不必要的 derive 宏

**适用场景**：derive 宏引入旧版本依赖（如 `syn 1.x`）

```rust
// 移除前（使用 syn 1.x）
#[derive(enum_display_derive::Display)]
pub enum MyEnum { ... }

// 移除后（手动实现，无额外依赖）
impl std::fmt::Display for MyEnum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
```

### 方法 6：使用 `[patch.crates-io]`（谨慎）

**适用场景**：需要修复上游 bug 或统一版本

```toml
[patch.crates-io]
# 示例：统一使用特定版本的 crate
hashbrown = { version = "=0.16.1" }
```

**注意事项**：
- 仅当 API 兼容时使用
- 避免使用 git 依赖（过于激进，不稳定）
- 优先等待上游发布新版本

## 具体操作案例

### 案例 1：消除 nix 重复版本

**问题**：`nix 0.30.1` 和 `nix 0.31.2` 共存

**分析**：
```bash
# 查找 nix 0.31.2 的来源
cargo tree -i nix@0.31.2
nix v0.31.2
└── ctrlc v3.5.2
    └── epkg v0.2.4

# 检查 ctrlc 的历史版本
curl -s "https://crates.io/api/v1/crates/ctrlc/3.5.1/dependencies" | \
  jq '.[] | select(.crate_id == "nix") | .req'
"^0.30"
```

**解决**：降级 ctrlc 到 3.5.1
```toml
# Cargo.toml
ctrlc = "=3.5.1"
```

### 案例 2：消除 syn 1.x

**问题**：`syn 1.0.109` 存在

**分析**：
```bash
# 查找 syn 1.x 的来源
grep -B5 '"syn 1.0.109"' Cargo.lock
# 发现来自 enum-display-derive
```

**解决**：移除 derive 宏，手动实现 trait

### 案例 3：统一 vm-memory 版本

**问题**：`vm-memory 0.17.1` 和 `0.18.0` 共存

**分析**：
```bash
# 发现 linux-loader 限制 <=0.17.1
cargo tree -i vm-memory@0.18.0
# 来源：imago
```

**解决**：
1. 所有地方统一使用 `=0.17.1`
2. 使用 `cargo update --precise 0.17.1`

## 检查清单

优化完成后验证：

```bash
# 1. 检查重复版本
grep '"."' Cargo.lock | sort | uniq -c | awk '$1 > 1'

# 2. 编译成功
make

# 3. 检查警告
cargo build 2>&1 | grep -i warning

# 4. 功能测试
./target/debug/epkg --version
```

## 常见限制

以下情况通常**无法**在项目中直接消除：

| 类型 | 示例 | 原因 |
|------|------|------|
| 外部依赖未升级 | `vmm-sys-util` → bitflags 1.x | 上游仍用旧版本 |
| SemVer 不兼容 | petgraph → hashbrown 0.15 | API 版本限制 |
| 平台特定 | redox_syscall 多版本 | 多来源依赖 |
| TLS 依赖链 | ring → windows-sys | rustls 底层依赖 |

## 相关文件

- `/c/epkg/Cargo.toml` - 主项目依赖配置
- `/c/epkg/Cargo.lock` - 锁定依赖版本
- `/c/epkg/git/**/Cargo.toml` - 子仓库依赖配置
- `/c/epkg/Makefile` - 构建脚本