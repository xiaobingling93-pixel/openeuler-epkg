# 路径和布局

本页面描述了 epkg 存储数据的位置以及用户（私有）与 root（共享）安装的目录布局。

## 用户私有安装

当 epkg 为单个用户安装时（例如非 root 用户运行 `epkg self install`，或安装脚本在用户模式下），使用以下路径（按数据流顺序，如 `epkg help` 所示）：

| 用途 | 路径 |
|------|------|
| Shell 集成 | `$HOME/.bashrc`（或 `.zshrc`）加载 `$HOME/.epkg/envs/self/usr/src/epkg/assets/shell/epkg.sh` |
| 下载缓存 | `$HOME/.cache/downloads/` |
| channel 元数据缓存 | `$HOME/.cache/channels/` |
| AUR 构建缓存 | `$HOME/.cache/aur_builds/`（如果使用） |
| 存储（包内容） | `$HOME/.epkg/store/` |
| 环境 | `$HOME/.epkg/envs/<env_name>/` |
| 每个环境的 epkg 配置 | `$HOME/.epkg/envs/<env_name>/etc/epkg/` |
| epkg 二进制文件 | `$HOME/.epkg/envs/self/usr/bin/epkg` |
| epkg 源代码（RC 脚本等） | `$HOME/.epkg/envs/self/usr/src/epkg/` |

在环境根目录内（例如 `$HOME/.epkg/envs/main/`）：

- **usr/** — 已安装的包文件（bin、lib、share 等）；**ebin/** 保存用于暴露命令的符号链接（或包装器），以便在环境注册时出现在 PATH 中。
- **etc/** — 环境特定配置（例如 `etc/epkg/`）。
- **var/** — 包所需的变量数据。

**self** 环境是特殊的：它包含 shell 包装器使用的 epkg 二进制文件和源代码；不用于一般包安装。

## Root 全局安装

当 epkg 系统范围内安装时（例如 root 运行安装程序或使用共享存储的 `epkg self install`），典型路径为：

| 用途 | 路径 |
|------|------|
| Shell 集成 | `$HOME/.bashrc`（root 的或每个用户的）；系统范围：`/etc/bash.bashrc` 等。 |
| 下载缓存 | `/opt/epkg/cache/downloads/` |
| channel 元数据缓存 | `/opt/epkg/cache/channels/` |
| 存储 | `/opt/epkg/store/` |
| 环境 | `/opt/epkg/envs/root/<env_name>/` |

因此，每个用户仍然有自己的 `$HOME/.bashrc`（以及可选的用于覆盖的 `$HOME/.epkg/`），但存储和环境根目录位于 `/opt/epkg/` 下。在此模式下，**公共**环境（使用 `-P` 创建）对其他用户可见，如 `owner/envname`，并且可以使用 `-e owner/envname` 以只读方式使用。

## 缓存 vs 存储

- **缓存** — 下载的原始数据：包文件（例如 .rpm、.deb、.apk）、channel 元数据（Release、repodata、APKINDEX 等）。可以重新下载；使用 `epkg gc` 或手动清除是安全的（如果接受重新下载）。
- **存储** — 内容寻址的包内容（解压并哈希）。每个存储条目通过链接被环境引用。删除仍被引用的存储内容可能会破坏环境；`epkg gc` 仅删除未被引用的存储数据。

## 理解缓存与存储的区别

缓存和存储之间的区别很重要：

- **缓存**（`~/.cache/epkg/` 或 `/opt/epkg/cache/`）— 可以重新生成的临时数据：
  - 下载的包文件（`.rpm`、`.deb`、`.apk` 等）
  - 仓库元数据（Release 文件、repodata、APKINDEX 等）
  - AUR 构建产物
  - 安全删除；需要时会重新下载

- **存储**（`~/.epkg/store/` 或 `/opt/epkg/store/`）— 内容寻址的包内容：
  - 解压并哈希的包文件
  - 通过链接（硬链接、符号链接等）被环境引用
  - **请勿手动删除** — 使用 `epkg gc` 安全地删除未被引用的条目
  - 每个存储条目由包内容衍生的哈希标识

当您安装一个包时：
1. 包文件下载到**缓存**
2. 包被解压并存储到**存储**（内容寻址）
3. 环境链接到存储条目（非复制）
4. 二进制文件暴露在 `env/ebin/` 中

这种设计实现了：
- **去重** — 相同的包内容在环境之间共享
- **高效** — 重复文件不占用重复存储空间
- **安全** — 存储条目在被引用期间保持存在

## 参考

- [README](../../../README.zh.md) — 高级概述和安装布局。
- [design-notes/epkg-layout.md](../../design-notes/epkg-layout.md) — 历史布局说明和卸载影响。
- [垃圾回收](../user-guide/advanced.md#垃圾回收) — 如何安全清理未使用的文件。
