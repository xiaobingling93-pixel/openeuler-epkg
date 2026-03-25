# dpkg 数据库兼容架构

## 背景

Debian/Ubuntu 的维护者脚本（postinst、prerm 等）经常调用 `dpkg --status` 或 `dpkg-query`
来检查包的安装状态。这些命令读取 `/var/lib/dpkg/status` 文件，而不是 epkg 的
`installed-packages.json`。

## 问题

当维护者脚本调用真实的 `dpkg` 命令时：
```bash
# 维护者脚本中的常见模式
if dpkg --status emacs > /dev/null 2>&1; then
    # 为 emacs 安装插件
fi
```

由于 epkg 不维护 `/var/lib/dpkg/status`，这些检查会失败，导致脚本行为异常。

## 解决方案

epkg 的 `dpkg_db` 模块生成 dpkg 兼容的元数据：

1. **`/var/lib/dpkg/status`**：列出所有已安装的包及其状态
2. **`/var/lib/dpkg/info/{pkg}.*`**：符号链接到 store 中的包信息文件

## 文件布局

### /var/lib/dpkg/status 格式

```text
Package: gcc
Status: install ok installed
Priority: optional
Section: devel
Installed-Size: 36
Maintainer: Debian GCC Maintainers <debian-gcc@lists.debian.org>
Architecture: amd64
Source: gcc-defaults (1.220)
Version: 4:14.2.0-1
Provides: c-compiler
Depends: cpp (= 4:14.2.0-1), gcc-14 (>= 14.2.0-6~)
Description: GNU C compiler
 This is the GNU C compiler, a fairly portable optimizing compiler for C.

Package: g++
Status: install ok installed
...
```

### /var/lib/dpkg/info/ 目录结构

```text
/var/lib/dpkg/info/
├── gcc.conffiles      # 符号链接 -> $HOME/.epkg/store/.../info/deb/conffiles
├── gcc.md5sums        # 符号链接 -> $HOME/.epkg/store/.../info/deb/md5sums
├── gcc.postinst       # 符号链接 -> $HOME/.epkg/store/.../info/deb/postinst
├── gcc.postrm         # 符号链接 -> $HOME/.epkg/store/.../info/deb/postrm
├── gcc.prerm          # 符号链接 -> $HOME/.epkg/store/.../info/deb/prerm
└── ...
```

**注意**：使用符号链接而不是复制文件，因为：
- 节省磁盘空间（无重复）
- 文件本身是只读的
- 当包被删除时，只需删除符号链接

## 安装流程

```
┌─────────────────────────────────────────────────────────────┐
│  安装 Debian/Ubuntu 包                                        │
│                                                              │
│  1. 解析包元数据                                              │
│  2. 下载并解包到 store                                        │
│  3. 链接到环境                                                │
│  4. 运行维护者脚本前：                                        │
│     - 生成 /var/lib/dpkg/status（已安装包）                   │
│     - 追加正在安装的包到 status                               │
│  5. 运行维护者脚本（可调用 dpkg --status）                     │
│  6. 安装完成后：                                              │
│     - 重新生成完整的 dpkg status                              │
│     - 创建 info 目录下的符号链接                              │
└─────────────────────────────────────────────────────────────┘
```

## 关键函数

### generate_dpkg_status()

从 `installed-packages.json` 生成 `/var/lib/dpkg/status`：

```rust
pub fn generate_dpkg_status() -> Result<()> {
    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();

    for (pkgkey, info) in installed.iter() {
        let pkgname = pkgkey2pkgname(pkgkey)?;
        let version = pkgkey2version(pkgkey)?;
        let control = read_package_control(&info.pkgline)?;

        // 从控制文件复制字段，设置 Status: install ok installed
        let entry = generate_status_entry(&pkgname, &version, &arch, info, control);
        entries.push((pkgname, entry));
    }

    // 按包名排序后写入
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    lfs::write(&status_path, content)?;
}
```

### append_pending_to_dpkg_status()

在运行维护者脚本前，将正在安装的包追加到 status：

```rust
pub fn append_pending_to_dpkg_status(pending: &InstalledPackagesMap) -> Result<()> {
    // 读取现有 status
    let mut content = std::fs::read_to_string(&status_path)?;

    // 追加尚未在 status 中的包
    for (pkgkey, info) in pending.iter() {
        if !existing_packages.contains(&pkgname) {
            let entry = generate_status_entry(...);
            content.push_str(&entry);
        }
    }

    lfs::write(&status_path, content)?;
}
```

### create_dpkg_info_symlinks()

创建 `/var/lib/dpkg/info/{pkg}.*` 符号链接：

```rust
pub fn create_dpkg_info_symlinks(pkgname: &str, pkgline: &str) -> Result<()> {
    let store_info_path = dirs().epkg_store.join(pkgline).join("info/deb");

    for entry in fs::read_dir(&store_info_path)? {
        let filename = entry.file_name();
        if filename == "control" { continue; }  // control 不需要链接

        let link_name = format!("{}.{}", pkgname, filename);
        let link_path = info_dir.join(&link_name);

        // 创建相对符号链接
        lfs::symlink_file_for_virtiofs(&entry.path(), &link_path)?;
    }
}
```

## 局限性

1. **文件列表**：epkg 不生成 `{pkg}.list` 文件，因为使用 store 的文件跟踪机制
2. **版本约束**：依赖字段中的版本约束目前简化为包名
3. **多架构**：暂不支持多架构场景

## 兼容性

此功能仅对 Debian/Ubuntu 格式的包启用：

```rust
if plan.package_format == PackageFormat::Deb {
    crate::dpkg_db::generate_dpkg_database()?;
}
```

其他格式（RPM、Pacman、Conda）不受影响。