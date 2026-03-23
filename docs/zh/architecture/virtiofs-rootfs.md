# virtiofs 作为 Linux rootfs：Windows 宿主机 + libkrun 场景

## 场景概述

在 **Windows 宿主机** 上通过 **libkrun**（或同类 VMM）运行 **Linux 虚拟机**，将宿主上的目录通过 **virtiofs** 共享进客体，并作为 **rootfs**（或与 initramfs 配合的根文件系统来源）。宿主侧目录通常位于 **NTFS** 卷上。

该模式的核心矛盾是：**客体内核与用户空间假定完整的 Linux VFS 语义**（POSIX 权限、符号链接、设备节点、`st_ino` 与硬链接关系等），而 **NTFS 与 Win32 的命名、安全与链接模型与之不对齐**。本文按「挑战 → 矛盾 → 当前/可行方案」展开；实现代码以 **`git/libkrun/src/devices/src/virtio/fs/windows/`** 为主（`passthrough.rs`、`reparse_point.rs`、`symlink.rs`、`ntfs_ea.rs` 等）。

```
┌─────────────────────────────────────────────────────────────────┐
│ Windows 宿主机 (NTFS 目录 = env_root / rootfs 源)               │
│  libkrun virtiofs 守护进程：Win32 路径 ↔ FUSE/virtio-fs 协议    │
└────────────────────────────┬────────────────────────────────────┘
                             │ virtio-fs 设备
┌────────────────────────────▼────────────────────────────────────┐
│ Linux guest：rootfs 挂载 virtiofs，exec 动态链接器、/usr 树等   │
└─────────────────────────────────────────────────────────────────┘
```

---

## 挑战 1：符号链接

### 矛盾

| 机制 | 权限 | 目录 | 文件 | 跨卷 | 与 Linux 语义 |
|------|------|------|------|------|-----------------|
| 硬链接 `mklink /H` | 常需提升 | 否 | 是 | 否 | 非 symlink |
| 目录联接 junction | 一般用户 | 是 | 否 | 本地卷 | 绝对路径，非 POSIX symlink |
| 符号链接 `mklink` / `mklink /D` | 常需开发者模式或管理员 | 是 | 是 | 是 | 最接近，但路径与特权仍与 Linux 不同 |

Linux rootfs 需要 **任意目标字符串**、**不强制目标存在**、**统一 `readlink`/`lstat` 行为**；单靠 Win32 符号链接无法在所有部署上满足。

### 分层策略（当前实现）

实现集中在 `symlink.rs`，用 **少量内部辅助函数** 表达分支，避免 `symlink_to_*` 与底层 `create_dir_junction` / `symlink_or_hardlink_or_copy_file` 重复堆砌：

| 辅助函数 | 含义 |
|----------|------|
| `link_existing_directory` | 目标**已是目录**：若 `can_create_symlinks()` → **`symlink_dir`**；否则 → **`create_dir_junction`**（唯一封装 `junction::create`） |
| `link_existing_file` | 目标**已是文件**：若可创建 symlink → **`symlink_or_hardlink_or_copy_file`**；否则 → **`create_lx_symlink`** |
| `link_missing_directory_intent` | **`symlink_to_directory` 且目标不存在**：可 symlink → **`symlink_dir`**；否则 **`create_dir_junction`** → 失败则 **LX** |
| `link_missing_file_intent` | **`symlink_to_file` 且目标不存在**：可 symlink → **`symlink_file`**；否则 **LX** |

对外 API：

- **`symlink()`**（未知类型）：已存在目录 → `link_existing_directory`；已存在文件 → `link_existing_file`；不存在 → **仅 LX**（`posix_target`）。
- **`symlink_to_directory()`** / **`symlink_to_file()`**：在「目标类型与意图冲突」时返回 **`InvalidInput`**。

**要点**：已存在目录时 **并非**「先 junction」——若探测到 **具备创建符号链接能力**，则 **优先原生 `symlink_dir`**；仅当 **`!can_create_symlinks()`** 时才用 junction。导出 **`create_dir_junction`**（目录联接）与 **`symlink_or_hardlink_or_copy_file`**（文件：symlink → 硬链 → 复制），名称直接反映行为；内部用私有 **`try_symlink_file`** 仅发起一次 `symlink_file` 调用。

**其它**：

- **已存在文件**且可创建 symlink：`symlink_file` → 失败则 **硬链接** → 再失败则 **复制**（与 epkg `lfs` 一致；语义上偏宿主优化，非 POSIX 纯 symlink）。
- **无 symlink 特权**且目录链接需兜底：junction；再失败则 **LX**（仅「目录意图 + 缺失目标」分支）。
- **无 symlink 特权**且文件链接：**LX**。
- **无法判断目标类型**（`symlink()` + 目标不存在）：**仅 LX**，目标串为客体 UTF-8 路径。

**LX 重解析点**：负载为 UTF-16LE 目标路径；**创建**时仅使用主标签 `0xA000001D`；若某环境对该标签要求特权，可**改源码**将 `create_lx_symlink` 中常量替换为 **`IO_REPARSE_TAG_LX_SYMLINK_ALT`（`0x8000001D`）**，**无运行时 try/fallback**。**读取**仍同时识别两种标签。

**标签与测试环境**：在部分环境中，**`IO_REPARSE_TAG_LX_SYMLINK`（`0xA000001D`）** 与 **`IO_REPARSE_TAG_LX_SYMLINK_ALT`（`0x8000001D`）** 均能成功创建；宿主若同时启用 **WSL1**，是否与行为相关尚不明确，仅作环境记录。

### lx-symlink 转换的理论完备性（virtiofs 只需处理末级分量）

1. **lx-symlink 仅在目标不存在时创建**，创建时无法预知目标将来是文件还是目录。
2. 若目标**最终是目录**，其后必有子文件或子目录在其下创建——那便是将 lx-symlink **升级为** `symlink_dir` 或 **junction** 的时机（见 `convert_lx_symlink_dir` 等路径）。
3. 因此在**引用或路径遍历**时，不会在长路径的中间分量上「单独遇到」仍表现为 lx-symlink 的占位：**它只可能是路径的最后一个分量**。
4. **virtiofs** 侧只需对**最后一个路径分量**做 lx-symlink 的检查与解析，无需在深层路径的每一级重复解析。

### `convert_lx_symlink_dir()` 语义

| 目标状态 | 结果 |
|----------|------|
| 存在且为目录 | 转换为 `symlink_dir` / junction |
| 存在但为文件 | `ENOTDIR` |
| 不存在 | `ENOENT` |

### Syscall 与创建/删除/读取路径覆盖

| 类别 | 操作 | 典型处理 | 状态 |
|------|------|----------|------|
| 创建 | `mkdir` / `create` / `mknod` / `symlink` | `convert_lx_symlink_dir(parent)` | ✓ |
| 创建 | `link` | `convert_lx_symlink_dir(newparent)` | ✓ |
| 创建 | `rename` | `convert_lx_symlink_dir(newdir)` | ✓ |
| 删除 | `unlink` | 直接删除文件（lx-symlink 在 NTFS 上为普通文件） | ✓ |
| 删除 | `rmdir` | 对 lx-symlink 报错（类型为文件而非目录） | ✓ |
| 读取 | `lookup` | 返回 `S_IFLNK`，由 FUSE 客户端跟随 | ✓ |
| 读取 | `readlink` / `getattr` | 返回 symlink 目标与属性 | ✓ |
| 读取 | `opendir` | 客户端先跟随；对 lx-symlink 得 `ENOTDIR` | ✓ |

### 路径假设

中间路径分量尽量使用 **junction / 可被内核解析的原生 symlink**；**LX symlink 通常只出现在末级分量**，避免在深层路径中依赖「仅 virtiofs 能解析」的 reparse 负载；与上一节「仅末级需解析」一致。

---

## 挑战 2：权限模型（POSIX ↔ NTFS）

### 核心矛盾

- Linux rootfs 需要 **rwxrwxrwx 语义、uid/gid、设备位、setuid/setgid 位** 等，供 `chmod`/`chown`、安全执行与包管理器脚本使用。
- **NTFS 默认以 ACL（DACL）为主**，没有与 Unix 模式位一一对应的原生字段；普通 Win32 API 不暴露「Unix 权限」。

### 解决方案：NTFS 扩展属性（EA）

在宿主文件上写入 **与 WSL 兼容的 EA 名**（如 `$LXUID`、`$LXGID`、`$LXMOD`、`$LXDEV`），在 virtiofs 的 `getattr`/`setattr`/`metadata_to_stat` 中 **优先** 用 EA 还原 `st_mode`、`st_uid`、`st_gid`、`st_rdev`（设备节点）。若 EA 缺失，则退化为合理默认（如目录 `0755`、普通文件 `0644`）。

**局限**：

- EA 与 ACL **并存**；若宿主工具改写 ACL，客体看到的「权限」仍以 EA 为准，需保持心理模型一致。
- **setuid 位**可存在于 EA 的 mode 中；**实际执行**仍受 Linux 内核与挂载选项（`nosuid` 等）约束，与 NTFS 是否「理解」setuid 无关。

### epkg 与 libkrun 的复用

- `ntfs_ea.rs` 可在 epkg 侧通过 `include!` 参与解压/安装路径（见 `src/main.rs` 与构建脚本），与 libkrun virtiofs **同源**可减少漂移。

---

## 挑战 3：特殊文件（设备节点、FIFO、socket）

### 矛盾

- Linux 需要 **字符设备、块设备、FIFO、Unix 套接字** 等 inode 类型；NTFS 上无对应真实内核对象。

### 解决方案：重解析点 + EA（virtiofs 侧）

- 使用 **自定义 IO_REPARSE_TAG**（`0x8000001E`～`0x80000021` 等）为各类节点创建 **占位** reparse 文件；**`mknod` 模式与设备号** 写入 `$LXMOD` / `$LXDEV`。
- `stat` 通过 `read_reparse_kind` + EA 返回正确的 `S_IF*` 与 `rdev`。
- `open` 对 FIFO/套接字/设备占位返回 **`ENXIO`**（或类似），避免在宿主上误当作普通文件打开。

详见 `reparse_point.rs` 与 `passthrough.rs` 中 `mknod`/`metadata_to_stat`/`open`。

### 由 guest init 覆盖的常见路径（VM 内）

客体中的 **init applet** 通常会对 **`/tmp`**、**`/run`**、**`/dev`** 挂载 **tmpfs**（或 **`/dev`** 为 devtmpfs）。因此：

- 常见临时文件、PID 文件、运行时 **Unix 套接字**、**设备节点** 多在 **真实 Linux 文件系统** 上创建与使用，**不经过** NTFS 上的 virtiofs。
- **设备文件**若仅在 **`/dev`** 下创建与使用，则 **不必** 依赖 virtiofs（NTFS）侧完整实现字符/块设备语义。

发行版与用户态软件大多遵循上述约定；对 **日常基本使用**，init 的这几处挂载通常已足够覆盖 **特殊文件** 需求，virtiofs 上的 reparse/EA 路径主要保障 **确需在 rootfs 树内** `mknod` 等时的行为。

---

## 挑战 4：硬链接与 inode（`st_ino`）

### 矛盾

- Linux 用户空间与部分工具依赖 **`st_ino` 稳定** 与 **同一 inode 多硬链接** 的识别。
- NTFS 使用 **MFT 记录号** 等内部标识，与 Linux `st_ino` 语义相近但不等同；virtiofs 的 `lookup`/`readdir` 必须在 **FUSE 层** 给出一致且唯一的 `st_ino`。

### 理想方向：宿主文件 ID

一种可行思路是用 **文件 ID** 映射为 64 位 inode，例如通过 `GetFileInformationByHandleEx(FileIdInfo)` 得到 **`FILE_ID_128`**，再组合 MFT 记录号与序列号等字段，保证 **唯一性与稳定性**（重命名同目录内通常保持同一文件 ID）：

```rust
// 概念示例：将 NTFS 文件 ID 映射为 64 位 st_ino（未在当前 Windows passthrough 中落地）
// fn inode_from_file_id(file_id: &FILE_ID_128) -> u64 { ... }
```

**工程注意**：`FILE_ID_128` 为 128 位，映射到 FUSE 常用 **u64** 需 **固定哈希或混合 volume 信息**，并评估 **碰撞** 与 **缓存**（`std::fs::Metadata` 不足以表达完整 file id，需 Win32/`windows` crate 等）。无真实宿主文件的 **合成入口**（如虚拟 init 相关对象）仍宜保留 **独立合成 inode**，与 file-id 路径并存。

### 当前实现（libkrun Windows passthrough）

**尚未**使用 `FILE_ID_128`。当前实现为：

- 维护 **单调递增** 的 `next_inode` 与 **`path_to_inode` / `inodes` 映射**；
- 每个 **路径** 首次 `lookup` 时分配一个 **u64** inode，并写入 `stat64.st_ino`；
- **`rename`** 会同步更新路径与 inode 映射，**同一路径**在重命名后仍可保持一致的 inode 分配策略；
- **`link()` 已实现**：宿主侧通过 `fs::hard_link` 创建 NTFS 硬链接后，会对 **目标路径** 调用 `get_or_create_inode`——**新路径会再分配一个 inode**，因此 **同一 NTFS 文件的两条路径可能对应两个不同的 `st_ino`**，与 POSIX「同一文件同一 inode」**不一致**；若在宿主侧预先建好硬链接且两条路径都被 lookup，同样会得到 **多个 inode**；
- **`metadata_to_stat` 中 `st_nlink` 固定为 1**（或等价简化），**不能**反映 NTFS 真实链接数；
- **`forget`** 在引用计数归零时通常 **不回收** inode 映射，长期运行可能带来 **映射表内存增长**（与路径策略正交，属有意简化）。

**结论**：不依赖 **跨路径 inode 相等**、不依赖正确 **`st_nlink`** 的工作负载（许多 rootfs 启动路径）可接受。若强依赖 **inode 去重**、**tar/find 硬链接语义** 等，当前实现 **不足**；长期方向仍为 **FILE_ID 映射** 或 **(volume_id, file_id) 哈希**，并需 **Win32 打开句柄 + 缓存** 控制开销，且 **`st_nlink`** 需 **单独从 NTFS 查询** 才能与 Linux 对齐。

---

## 挑战 5：大小写敏感

### 矛盾

- Linux rootfs 普遍假设 **大小写敏感**（`/usr` 与 `/USR` 不同）。
- Windows/Win32 默认 **大小写不敏感**（保留大小写，但匹配不敏感）。

### NTFS 每目录「大小写敏感」标志

Windows 10 **1803** 起，对 **NTFS** 支持 **按目录** 的 case-sensitive 标志（底层 `FILE_CASE_SENSITIVE_DIRECTORY` 等）。常见操作方式之一：

```bat
fsutil.exe file setCaseSensitiveInfo C:\path\to\rootfs enable
```

**注意**：

- 通常要求 **目录为空** 时才能启用（或受版本/策略限制）；**已有文件树** 时可能需先规划目录再启用。
- **是否需管理员权限** 取决于 Windows 版本与策略；企业环境可能限制 `fsutil`。
- **fsutil** 随 Windows 系统提供，路径一般 `%SystemRoot%\System32\fsutil.exe`；epkg 若需自动化，可通过 **绝对路径调用** 或 `where fsutil` 探测；**不能假定** 在极简环境（如部分容器宿主）中一定存在。

**建议**：在文档与安装脚本中说明「rootfs 目录宜预先创建并启用大小写敏感」，失败时给出 **降级说明**（例如仅用于开发、或仅非冲突路径）。

### epkg 中的继承用法（`create_dir_all_with_case_sensitivity`）

代码侧在创建关键根目录时使用 **`lfs::create_dir_all_with_case_sensitivity(...)`**：在 **父目录** 上启用大小写敏感后，**子目录会继承** 该策略（NTFS 为目录级标志的继承语义）。典型顺序：

1. **`env_root`**：先 `create_dir_all_with_case_sensitivity(env_root)`，再创建其下 `root/`、`usr/`、`var/` 等（见 `environment.rs` 中 `create_environment_dirs_early()`）。
2. **`store_tmp_dir`**：在创建 `fs/`、`info/` 等子目录 **之前** 对父目录设置。
3. **`epkg_store`**：在创建 `pkgline/` 等子目录 **之前** 对存储根设置。

**原则**：先设置 **要继承敏感性的父目录**，再 `create_dir_all` 子树，避免在已铺满文件后再改策略。

### 相关修改位置（摘录）

| 文件 | 函数 / 上下文 | 说明 |
|------|----------------|------|
| `store.rs` | `unpack_mv_package_with_format()` | `final_dir` 的父目录 |
| `link.rs` | `mirror_dir()` | `target_path` 目录 |
| `link.rs` | `mirror_symlink_file()` | `parent` 目录 |
| `tar_extract.rs` | `extract_archive_with_policy()` / `extract_archive()` / `unpack_tar_archive()` | 开头的 `target_dir` / `dest` |
| `tar_extract.rs` | `create_package_dirs()` | `fs/`、`info/format/`、`info/install/` 等 |
| `deb_pkg.rs` | `extract_tar()` | `target_dir` |
| `rpm_pkg.rs` | `extract_rpm_files()` | 父目录与目标目录本身 |
| `apk_pkg.rs` | `create_apk_trigger_hook()` | `install_dir` |
| `conda_pkg.rs` | `extract_zstd_tar_stream()` | `target_dir` |
| `aur.rs` | `extract_aur_source()` | `build_dir` |

（具体行号以仓库为准；上表用于快速定位「何处先保证大小写敏感父目录」。）

---

## 挑战 6：DAX 模式与内存映射

### 作用

virtiofs **DAX**（Direct Access）使客体可 **绕过 guest page cache**，将宿主文件页 **映射到 guest 地址空间**，对 **rootfs 大量可执行/共享库** 的启动路径能显著降低延迟（量级上可从「秒级」降到「更短」的 I/O 路径，具体取决于实现与负载）。

### Linux / macOS 侧（libkrun 现状）

在 `virtio/fs` 中，**Linux** 与 **macOS** passthrough 存在与 **mmap、`MAP_SHARED`**、以及（macOS）**HVF DAX 窗口** 相关的代码路径；FUSE 协议层也定义 **`HAS_INODE_DAX`** 等能力位（见 `fuse.rs`）。

### Windows 侧（libkrun 现状）

当前 **`windows/passthrough.rs` 中未** 检索到 `DAX` / `setupmapping` 等实现；即 **Windows 宿主 virtiofs 后端尚未实现与 Linux 同等的 DAX 映射路径**。

### 若要在 Windows 上补齐（设计讨论）

- Linux 侧典型路径：**mmap + MAP_SHARED** 与 hypervisor 共享内存窗口。
- Windows 宿主侧可能涉及 **`CreateFileMapping` / `MapViewOfFile`** 等与 **guest 物理页** 的绑定，并与 **WHPX（Windows Hypervisor Platform）** 的内存共享模型对齐。
- 需与 **virtiofs 协议** 中 `SETUP_MAPPING` / `REMOVE_MAPPING` 及 **inode DAX 标志** 一致，避免与 **Win32 文件句柄**、**句柄模式**（`inode-file-handles` 等 virtiofsd 选项）冲突。

**结论**：**DAX 对 rootfs 性能很重要**；在 **Linux/macOS 宿主** 上 libkrun 已有相关基础时，可优先验证；**Windows** 上需 **单独实现与验证**，当前 **不应** 在文档中宣称「Windows 端已完整支持 DAX」。

---

## 附录：实现文件索引

| 主题 | 主要源文件 |
|------|------------|
| Win32 passthrough + stat/open/symlink | `git/libkrun/.../windows/passthrough.rs` |
| LX / 自定义 reparse | `.../windows/reparse_point.rs` |
| symlink / junction / LX 策略 | `.../windows/symlink.rs` |
| NTFS EA | `.../windows/ntfs_ea.rs` |
| FUSE 能力位（含 DAX 标志位定义） | `.../virtio/fs/fuse.rs` |
| Linux passthrough（含 mmap 等） | `.../virtio/fs/linux/passthrough.rs` |

## 参考

- `docs/zh/plan/cross-platform-notes.md`
- `docs/design-notes/sandbox-vmm.md`
