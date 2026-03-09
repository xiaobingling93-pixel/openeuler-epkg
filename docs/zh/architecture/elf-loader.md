# ELF Loader 架构设计

## 设计目标

1. **透明性**：用户可以直接从宿主机运行环境中的二进制文件
2. **效率**：通过硬链接共享 elf-loader，减少磁盘占用
3. **兼容性**：支持跨设备部署，自动降级为复制
4. **安全性**：原子写入避免损坏共享文件

## 组件关系

```
┌─────────────────────────────────────────────────────────┐
│                    宿主机 (Host)                        │
│                                                         │
│  ~/.epkg/envs/dev-alpine/ebin/                          │
│  ├── gcc ──────────→ elf-loader (硬链接)                │
│  ├── g++ ──────────→ elf-loader (硬链接)                │
│  ├── npm ──────────→ 脚本包装器                         │
│  └── .gcc.target ──→ store 中的实际二进制               │
│                                                         │
└─────────────────────────────────────────────────────────┘
                          ↓ 运行时解析
┌─────────────────────────────────────────────────────────┐
│                    环境 (Environment)                   │
│                                                         │
│  ~/.epkg/envs/dev-alpine/usr/bin/                       │
│  ├── gcc ──────────→ ../lib/gcc/bin/gcc (symlink)       │
│  └── npm ──────────→ ../share/nodejs/npm/bin/npm-cli.js │
│                                                         │
└─────────────────────────────────────────────────────────┘
                          ↓ store 路径
┌─────────────────────────────────────────────────────────┐
│                    Store                                │
│                                                         │
│  ~/.epkg/store/<hash>__gcc__14.2.0__x86_64/fs/          │
│  └── usr/lib/gcc/bin/gcc ──────────── (实际 ELF 文件)   │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

## 数据类型

### FileType 枚举

```rust
pub enum FileType {
    Elf,           // ELF 二进制文件
    ShellScript,   // Shell 脚本
    NodeScript,    // Node.js 脚本
    PythonScript,  // Python 脚本
    PerlScript,    // Perl 脚本
    RubyScript,    // Ruby 脚本
    Text,          // 文本文件
    Unknown,       // 未知类型
}
```

## 关键函数

### 1. handle_elf

**位置**：`src/expose.rs:136`

**作用**：为 ELF 二进制创建 elf-loader 硬链接

```rust
fn handle_elf(
    target_path: &Path,    // ebin/gcc
    env_root: &Path,
    fs_file: &Path,        // store 中的实际二进制
) -> Result<()> {
    // 1. 定位 elf-loader
    let elf_loader_path = self_env_root.join("usr/bin/elf-loader");

    // 2. 移除已存在的目标文件
    if lfs::exists_in_env(target_path) {
        lfs::remove_file(target_path)?;
    }

    // 3. 创建父目录
    if let Some(parent) = target_path.parent() {
        lfs::create_dir_all(parent)?;
    }

    // 4. 创建硬链接（跨设备时复制）
    hard_link_or_copy(&elf_loader_path, target_path, true)?;

    // 5. 创建隐藏 symlink2（如果 bin/<program> 不存在）
    let has_bin_file = bin_file_exists(target_path, fs_file)?;
    if !has_bin_file {
        create_symlink2(target_path, fs_file)?;
    }

    Ok(())
}
```

### 2. create_script_wrapper

**位置**：`src/expose.rs:357`

**作用**：原子地创建脚本包装器

```rust
fn create_script_wrapper(
    env_root: &Path,
    fs_file: &Path,
    ebin_path: &Path,
    file_type: FileType,
    first_line: &str,
) -> Result<()> {
    // 1. 创建 shebang 行
    let env_shell_bang_line = create_shebang_line(env_root, first_line, fs_file)?;

    // 2. 生成执行命令
    let exec_cmd = get_exec_command(&file_type, fs_file, Some(env_root));

    // 3. 原子写入流程
    let temp_path = ebin_dir.join(format!(".tmp-{}", filename));

    // 3a. 写入临时文件
    let mut wrapper = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temp_path)?;

    wrapper.write_all(env_shell_bang_line.as_bytes())?;
    wrapper.write_all(exec_cmd.as_bytes())?;
    drop(wrapper);  // 关闭文件

    // 3b. 设置权限
    set_wrapper_permissions(&temp_path)?;

    // 3c. 原子重命名
    fs::rename(&temp_path, ebin_path)?;

    Ok(())
}
```

### 3. hard_link_or_copy

**位置**：`src/link.rs`

**作用**：跨设备兼容的硬链接/复制

```rust
fn hard_link_or_copy(src: &Path, dst: &Path, preserve_perms: bool) -> Result<()> {
    match std::fs::hard_link(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::CrossDevice => {
            std::fs::copy(src, dst)?;
            if preserve_perms {
                let perms = std::fs::metadata(src)?.permissions();
                std::fs::set_permissions(dst, perms)?;
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}
```

## 调用流程

```
expose_package()
    ↓
expose_package_ebin()
    ↓
list_package_files_with_info()
    ↓
create_ebin_wrappers()
    ↓
for each file in bin/, sbin/, libexec/:
    ↓
    ├─→ FileType::Elf
    │   └─→ handle_elf()
    │       ├─→ hard_link_or_copy(elf-loader, ebin/tool)
    │       └─→ create_symlink2(ebin/tool, store_file)
    │
    └─→ FileType::Script
        └─→ create_script_wrapper()
            ├─→ create_shebang_line()
            ├─→ get_exec_command()
            ├─→ 写入临时文件
            └─→ fs::rename()
```

## 不变量

1. **elf-loader 必须是 ELF 二进制**
   - 通过原子写入保证不被脚本覆盖

2. **脚本包装器必须原子写入**
   - 始终使用临时文件 + rename
   - 避免直接 truncate 目标文件

3. **跨设备兼容性**
   - 硬链接失败时自动降级为复制
   - 复制时保留原始权限

## 模块职责

| 模块 | 职责 |
|------|------|
| `expose.rs` | ebin 包装器创建（handle_elf, create_script_wrapper） |
| `link.rs` | hard_link_or_copy, create_symlink2 |
| `utils.rs` | 文件类型检测 (get_file_type) |
| `shebang.rs` | shebang 解析和解释器查找 |
| `lfs.rs` | 文件系统操作（带日志） |

## 修复历史

### 2026-03-09: elf-loader 损坏修复

**Commits**：
```
df72660 expose: use atomic write for script wrapper creation
e17b854 expose: fix script wrapper creation to avoid overwriting hard links
```

**问题**：`create_script_wrapper()` 使用 `truncate(true)` 直接打开目标文件，当目标是 elf-loader 的硬链接时，会损坏所有共享该 inode 的文件。

**修复**：
1. 使用临时文件写入内容
2. 关闭文件句柄
3. 设置权限
4. `fs::rename()` 原子替换目标

**代码对比**：

```rust
// 修复前（错误）
let mut wrapper = fs::OpenOptions::new()
    .write(true)
    .truncate(true)  // ❌ 直接截断
    .open(ebin_path)?;

// 修复后（正确）
let temp_path = ebin_dir.join(".tmp-{}", filename);
let mut wrapper = fs::OpenOptions::new()
    .write(true)
    .create(true)
    .truncate(true)  // ✓ 只截断临时文件
    .open(&temp_path)?;
// ... 写入 ...
fs::rename(&temp_path, ebin_path)?;  // ✓ 原子替换
```

### 2026-03-09: 破损 symlink 处理

**Commit**：
```
00a80b7 expose: fix ebin wrapper creation for broken symlinks in store
```

**问题**：包内 symlink 指向其他包（跨包 symlink），在 store 中是破损的，但在 env 中是有效的。

**修复**：对于 symlink，在 env 上下文中检查目标文件类型。

```rust
// 修复后
let (file_type, first_line) = if lfs::is_symlink(fs_file_absolute) {
    // 在 env 上下文中检查 symlink 目标
    utils::get_file_type(&resolved_env_path)
} else {
    // 检查 store 文件
    utils::get_file_type(fs_file_absolute)
};
```
