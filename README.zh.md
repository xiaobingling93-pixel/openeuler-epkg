# epkg

一个绿色轻量的 Linux 包管理器。
在常见Linux Host OS下，创建 environments，安装使用常见 Linux 软件仓的软件包。
支持普通用户安装，在env粒度，组合使用不同软件仓。

```yaml
# 概念示意
host: openeuler | centos | debian | ...
  env1: openeuler   → PATH += $env_root/ebin
  env2: ubuntu      → PATH += $env_root/ebin
```

## 使用场景

1. **一般用户**：在Host OS之外，安装更多来源的、新旧版本的软件
   (空间)组合安装 多OS软件
   (时间)原子升级 安全回退

2. **开发用户**：为一个软件项目，定义开发环境依赖。在一个环境内安装所有类型依赖: OS系统软件包 + Python语言包 + ...
   ALL-DEPENDS-IN-ONE-ENV
   可重复 可分发 开发环境

3. **容器/嵌入式**：替代dnf/apt/apk/pacman等官方包管理器，安装OS软件
   RPM系 瘦身100MB (dnf -> epkg)
   DEB系 瘦身 20MB (apt -> epkg)

4. **本地 AI开发环境 + 文件系统沙箱**
   轻量容器 for develpment
   海量软件 for AI agents

场景 1-3 已支持；场景 4 待支持。

## 功能亮点

1. **普通用户安装** — 一般使用无需 root 权限
2. **多发行版支持** — 从任意 Linux Host OS，安装 openEuler、Fedora、Debian、Ubuntu、Alpine、Arch Linux、AUR、Conda 软件源
3. **环境隔离** — 每个环境绑定到特定 channel；注册多个环境并将它们的二进制文件组合到 PATH 中
4. **同时覆盖OS、多语言依赖** — 一个env内，满足项目常见软件依赖需求，并与用户/开发者的HostOS解耦，简化项目管理
5. **环境复制** — 通过 env 定义、export/import，支持环境的分发、复制
6. **高效** — 文件级去重，并行/分块下载，约 1300 个全球镜像，快速查询（list比 dnf 快 17 倍，内存少3-9倍）
7. **便携** — 静态 musl 可执行文件（约 11MB）；内置lua解析器、近90款busybox applets；适合在容器/嵌入式环境中替代 dnf/apt
8. **可靠** — 基于 SAT 的依赖解析（resolvo）；带有回滚的事务历史

## 快速开始

```bash
wget https://raw.atomgit.com/openeuler/epkg/raw/master/bin/epkg-installer.sh
bash epkg-installer.sh

# 然后启动一个新的 shell 以更新 PATH
bash

# 创建一个环境，并安装、运行软件包
epkg env create myenv -c alpine
epkg -e myenv install htop bash
epkg -e myenv run htop
epkg -e myenv run bash
```

默认环境为 `main`。使用 `-e ENV` 以选择其他环境，或使用 `epkg env register <ENV>` 将环境添加到您的 PATH。

## 支持的发行版（channel）

- **RPM**系: openEuler、Fedora、CentOS、AlmaLinux、Rocky、EPEL 等。
- **DEB**系: Debian、Ubuntu、Linux Mint、Deepin 等。
- **Alpine**: main、community
- **Arch**: core、extra、multilib、AUR、arch4edu 等。
- **Conda**: conda-forge、main、free 等

运行 `epkg repo list` 查看完整的 channel 表格。

## 主要命令（概述）

| 领域 | 命令 |
|------|----------|
| 自管理 | `self install`, `self upgrade`, `self remove` |
| 软件包 | `install`, `update`, `upgrade`, `remove`, `list`, `info`, `search` |
| 环境 | `env list`, `create`, `remove`, `register`, `unregister`, `activate`, `deactivate`, `export`, `path`, `config` |
| 历史 | `history`, `restore <gen_id>` |
| 执行 | `run`, `service`, `busybox` |
| 其他 | `gc`, `repo list`, `hash`, `unpack`, `convert` |

完整帮助请参阅[命令参考](docs/zh/reference/commands.md)。

## 安装布局

- **用户私有**: `~/.epkg/envs`, `~/.epkg/store`, 以及在 `~/.cache/epkg` 下: `downloads/`, `channels/`, `aur_builds/`, `iploc/`。
- **共享（root）**: `/opt/epkg`（cache/、store/、以及 `/opt/epkg/envs/root/` 下的一个个环境）。

详细信息：[路径和布局](docs/zh/reference/paths.md)。

## 工作原理（简要）

- **`epkg run`** 在环境的命名空间（挂载 + 用户命名空间）中运行命令。环境的 `usr`、`etc`、`var` 被绑定挂载，以便已安装的二进制文件和脚本正确运行。
- **安装流程**: 解析（SAT 求解器）→ 下载+解包 → 链接（存储 → 环境）→ 脚本 → 触发器 → 将二进制文件暴露到 `ebin/` 以供 PATH 查找。

## 从源码构建

```bash
git clone https://atomgit.com/openeuler/epkg
cd epkg
make dev-depends
make
target/debug/epkg self install
# 然后：启动一个新的 shell, 尝试运行epkg
```

完整的开发设置和测试：[开发者快速开始](docs/zh/user-guide/developer-quick-start.md)。

## 文档

| 文档 | 描述 |
|----------|-------------|
| [文档索引](docs/zh/index.md) | 概述及所有文档链接 |
| [开发者快速开始](docs/zh/user-guide/developer-quick-start.md) | 从源码构建、开发循环、测试 |
| [入门指南](docs/zh/user-guide/getting-started.md) | 安装及第一步 |
| [环境管理](docs/zh/user-guide/environments.md) | 创建、注册、激活、路径、配置 |
| [包操作](docs/zh/user-guide/package-operations.md) | 安装、删除、更新、升级、列表、搜索、信息 |
| [高级用法](docs/zh/user-guide/advanced.md) | run、service、history/restore、gc、convert、unpack |
| [故障排除](docs/zh/user-guide/troubleshooting.md) | 常见问题及解决方案 |
| [命令参考](docs/zh/reference/commands.md) | 所有命令和选项 |
| [软件源](docs/zh/reference/repositories.md) | channel 列表及仓库列表 |
| [路径和布局](docs/zh/reference/paths.md) | 安装目录及布局 |

设计说明和格式规范：[docs/design-notes/](docs/design-notes/), [docs/epkg-format.md](docs/epkg-format.md)。

## 链接

- 仓库: [atomgit.com/openeuler/epkg](https://atomgit.com/openeuler/epkg)
- 镜像配置: [sources/mirrors.json](https://atomgit.com/openeuler/epkg/tree/master/sources/mirrors.json)
