# Cargo 依赖优化指南

本文档总结了优化 Cargo 依赖、消除重复版本的经验和最佳实践。

## 分析工具

### 查看重复版本

```bash
# 查看所有重复版本的依赖
grep '".* .*"' Cargo.lock | sort | uniq -c | awk '$1 > 1'

# 查看特定包的所有版本
grep '"package_name ' Cargo.lock | sort | uniq -c
```

### 分析依赖链

```bash
# 查看哪个包依赖了特定版本
cargo tree -i package@version

# 查看包的依赖树
cargo tree -p package@version

# 查看包的详细信息
cargo tree -p package
```

## 消除重复版本的方法

### 1. 升级/降级直接依赖

最直接的方法是调整 `Cargo.toml` 中的版本约束：

```toml
# 升级到使用新依赖版本的版本
versions = "7.0"  # 从 6.3 升级，使用 itertools 0.14 和 nom 8.0

# 降级到与其他依赖兼容的版本
vm-memory = { version = "=0.17.1" }  # 因为 linux-loader 限制 <=0.17.1
```

### 2. 使用 cargo update --precise

强制将某个包降级到特定版本：

```bash
# 将 vm-memory 0.18.0 降级到 0.17.1
cargo update -p vm-memory@0.18.0 --precise 0.17.1
```

**适用场景：** 当传递依赖需要不同版本，但 SemVer 允许使用旧版本时。

### 3. 移除不必要的 derive 宏

某些 derive 宏会引入旧版本依赖。例如 `enum-display-derive` 使用 `syn 1.x`：

```rust
// 移除前
#[derive(enum_display_derive::Display)]
pub enum MyEnum { ... }

// 移除后 - 手动实现
impl std::fmt::Display for MyEnum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
```

### 4. 使用 [patch.crates-io]

当上游尚未发布修复时，可以 patch：

```toml
[patch.crates-io]
petgraph = { git = "https://github.com/petgraph/petgraph", rev = "xxx" }
```

**注意：** 使用 git 依赖较为激进，应谨慎使用。

## 常见问题案例

### bitflags 1.x vs 2.x

**问题：** `vmm-sys-util` 等包仍使用 bitflags 1.x

**解决方案：** 升级使用 bitflags 的包到 2.x，并添加必要的 derive 属性：

```rust
// bitflags 2.x 需要显式 derive
bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct MyFlags: u32 {
        const FLAG_A = 1;
    }
}
```

### vm-memory 版本冲突

**问题：** `linux-loader` 限制 `vm-memory <=0.17.1`，但 `imago` 可以使用 0.18

**解决方案：** 在所有使用 vm-memory 的地方 pin 到 0.17.1：

```toml
vm-memory = { version = "=0.17.1", features = ["backend-mmap"] }
```

并使用 `cargo update --precise` 确保统一版本。

### hashbrown/foldhash 重复

**问题：** `petgraph` 依赖 `hashbrown 0.15`，而其他包使用 `hashbrown 0.16`

**解决方案：** 等待 petgraph 0.9 发布（已提交 hashbrown 0.16 升级），或暂时接受重复版本。

## 无法消除的重复版本

以下类型的重复版本通常无法在项目中直接消除：

| 类型 | 示例 | 原因 |
|------|------|------|
| 外部依赖限制 | `vmm-sys-util` → bitflags 1.x | 上游未升级 |
| TLS 库依赖 | `ring` → windows-sys 0.52 | ureq → rustls 依赖链 |
| SemVer 不兼容 | petgraph → hashbrown 0.15 | 版本号不兼容 |
| 平台特定依赖 | redox_syscall | 多个来源 |

## edition 2024 注意事项

某些 crate 升级后需要 Rust 2024 edition。需要 pin 的版本示例：

```toml
# Pin 这些版本以避免 edition 2024
time = { version = "=0.3.44" }
rand = { version = "=0.9" }
mlua = { version = "=0.11.5" }
deranged = "=0.5.7"
```

**注意：** 检查 crate 是否真的需要 edition 2024，某些升级可能不需要。

## 多仓库协作优化

当项目包含多个 Rust 子仓库（如 `git/` 目录下）时：

1. **统一版本策略**：在所有仓库中使用相同的依赖版本
2. **Cargo.lock 同步**：修改子仓库后，更新主项目 Cargo.lock
3. **分别提交**：每个仓库独立提交，保持提交历史清晰

## 检查清单

优化完成后，检查以下内容：

- [ ] 编译成功：`make` 或 `cargo build`
- [ ] 重复版本减少：`grep '".* .*"' Cargo.lock | sort | uniq -c | awk '$1 > 1'`
- [ ] 无警告：检查编译警告
- [ ] 功能正常：运行测试或手动验证

## 相关文件

- 主项目 `Cargo.toml`：定义直接依赖
- `Cargo.lock`：锁定所有依赖版本
- 子仓库 `Cargo.toml`：如 `git/libkrun/*/Cargo.toml`