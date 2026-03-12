---
name: epkg-usage
description: epkg 包管理器 概念、场景、功能、命令用法、环境管理、软件包安装/查询/运行
---

# epkg 包管理器技能文档

> 本文档旨在帮助 AI Agent 快速理解并熟练掌握 epkg 包管理器的核心概念、使用场景和操作方法。

---

## 一、核心概念

### 1.1 什么是 epkg？

epkg 是一个**轻量级、多源 Linux 包管理器**，核心特点：

- **无需 root**：用户空间安装，普通用户即可使用
- **多发行版支持**：同时支持 RPM、DEB、Alpine、Arch、Conda 等格式
- **环境隔离**：每个环境独立，可混合不同发行版的包
- **高效存储**：文件级去重、并行下载、内容寻址存储
- **沙箱执行**：支持 namespace 隔离和虚拟机隔离

### 1.2 核心概念模型

```
┌─────────────────────────────────────────────────────────────┐
│                        Host System                           │
│  (任意 Linux 发行版: openEuler, Debian, Fedora, Arch...)    │
├─────────────────────────────────────────────────────────────┤
│  ~/.epkg/envs/                                               │
│  ├── main/          ← 默认环境 (每个用户自动创建)            │
│  │   ├── ebin/      ← 暴露的二进制入口点 (PATH)             │
│  │   ├── usr/       ← 软件文件 (symlink → store)            │
│  │   └── etc/       ← 配置文件                              │
│  ├── alpine/        ← Alpine 环境独立                        │
│  ├── debian/        ← Debian 环境独立                        │
│  └── fedora/        ← Fedora 环境独立                        │
├─────────────────────────────────────────────────────────────┤
│  ~/.epkg/store/     ← 内容寻址存储 (所有环境共享)            │
│  └── <hash>__<name>__<version>__<arch>/                     │
│      └── fs/        ← 解压后的包文件                         │
└─────────────────────────────────────────────────────────────┘
```

### 1.3 关键术语

| 术语 | 含义 |
|------|------|
| **Environment (环境)** | 独立的软件安装上下文，有自己的 channel 和已安装包列表 |
| **Channel** | 软件源配置，如 `debian`、`alpine`、`fedora`、`conda` |
| **Store** | 内容寻址存储，所有包解压后存放在此，环境通过 symlink 引用 |
| **ebin** | 环境的二进制入口目录，被添加到 PATH |
| **Register** | 将环境的 ebin 持久化添加到 PATH |
| **Activate** | 临时为当前 shell 激活某个环境 |
| **Generation** | 环境的历史版本，支持回滚 |

---

## 二、需求场景

### 2.1 终端用户场景

| 场景 | 传统痛点 | epkg 解决方案 |
|------|---------|--------------|
| 安装额外软件 | 需要 root、系统包版本旧 | 无需 root，可选多个发行版源 |
| 使用新版本软件 | 系统仓库版本滞后 | 选择 Fedora/Arch 等滚动更新源 |
| 混用不同发行版软件 | 依赖冲突、无法共存 | 多环境隔离，PATH 组合 |
| 系统回滚 | 需要系统级快照 | 每次安装记录 generation，一键回滚 |

### 2.2 开发者场景

| 场景 | 传统痛点 | epkg 解决方案 |
|------|---------|--------------|
| 项目依赖管理 | 污染系统环境 | 项目目录 `.eenv` 独立环境 |
| 多语言开发 | pip/npm 全局污染 | 每环境独立的工具链 |
| 跨发行版测试 | 需要多台机器或容器 | 一键创建 Debian/Fedora/Alpine 环境 |
| CI/CD 环境 | 镜像大、构建慢 | 静态二进制、去重存储 |

### 2.3 容器/嵌入式场景

| 场景 | 传统痛点 | epkg 解决方案 |
|------|---------|--------------|
| 镜像大小 | RPM/DEB 元数据臃肿 | 精简存储、可选 busybox applets |
| 基础镜像 | 必须选择特定发行版 | 可混合多发行版包 |
| 安全隔离 | 容器逃逸风险 | 支持 VM 级沙箱 |

---

## 三、功能概览

### 3.1 命令分组速查

```
epkg <command>

┌─ 自管理 ──────────────────────────────────────────────────┐
│  self install    安装 epkg 自身                            │
│  self upgrade    升级 epkg                                 │
│  self remove     移除 epkg                                 │
├─ 包操作 ──────────────────────────────────────────────────┤
│  install         安装包                                    │
│  update          更新仓库元数据                            │
│  upgrade         升级包                                    │
│  remove          删除包                                    │
│  list            列出包                                    │
│  info            查看包信息                                │
│  search          搜索包                                    │
├─ 环境管理 ────────────────────────────────────────────────┤
│  env list        列出所有环境                              │
│  env create      创建环境                                  │
│  env remove      删除环境                                  │
│  env register    注册环境到 PATH (持久化)                  │
│  env unregister  取消注册                                  │
│  env activate    激活环境 (当前 shell)                     │
│  env deactivate  停用环境                                  │
│  env path        显示 PATH                                 │
│  env config      环境配置                                  │
│  env export      导出环境配置                              │
├─ 执行与沙箱 ──────────────────────────────────────────────┤
│  run             在环境中运行命令                          │
│  service         服务管理                                  │
│  busybox         内置命令实现                              │
├─ 历史与回滚 ──────────────────────────────────────────────┤
│  history         查看环境历史                              │
│  restore         回滚到指定版本                            │
│  gc              垃圾回收                                  │
└───────────────────────────────────────────────────────────┘
```

### 3.2 全局选项

```bash
epkg [OPTIONS] <command>

环境选择:
  -e, --env <NAME>      按名称选择环境
  -r, --root <DIR>      按路径选择环境
  --arch <ARCH>         指定架构 (x86_64, aarch64, riscv64)

执行控制:
  --dry-run             模拟运行，不修改系统
  --download-only       只下载，不安装
  -y, --assume-yes      自动确认
  -q, --quiet           静默模式
  -v, --verbose         详细输出

网络:
  --proxy <URL>         HTTP 代理
  --parallel-download N 并行下载线程数
```

### 3.3 支持的发行版 (Channel)

| 格式 | 发行版 |
|------|--------|
| **RPM** | openEuler, Fedora, CentOS, AlmaLinux, Rocky, EPEL |
| **DEB** | Debian, Ubuntu, Linux Mint, Deepin |
| **APK** | Alpine (main, community) |
| **Pacman** | Arch Linux (core, extra, multilib, AUR) |
| **Conda** | conda-forge, main, free |

查看完整列表：`epkg repo list`

---

## 四、典型工作流程

### 4.1 快速入门流程

```bash
# 1. 安装 epkg
wget https://raw.atomgit.com/openeuler/epkg/raw/master/bin/epkg-installer.sh
bash epkg-installer.sh
bash  # 启动新 shell

# 2. 创建环境并安装软件
epkg env create myalpine -c alpine
epkg -e myalpine install bash htop

# 3. 运行命令
epkg -e myalpine run htop

# 4. 注册环境 (持久化 PATH)
epkg env register myalpine
htop --version  # 直接可用
```

### 4.2 多环境混合使用

```bash
# 创建不同发行版的环境
epkg env create debian-env -c debian
epkg env create fedora-env -c fedora
epkg env create alpine-env -c alpine

# 在各环境安装软件
epkg -e debian-env install python3
epkg -e fedora-env install rustc
epkg -e alpine-env install nodejs

# 注册到 PATH (按优先级)
epkg env register alpine-env --path-order 10   # 最高优先
epkg env register fedora-env  --path-order 20
epkg env register debian-env  --path-order 30

# 现在所有环境的二进制都在 PATH 中可用
python3 --version   # 来自 debian-env
rustc --version     # 来自 fedora-env
node --version      # 来自 alpine-env
```

### 4.3 项目特定环境

```bash
cd /path/to/myproject

# 在项目目录创建 .eenv
epkg env create --root ./.eenv -c alpine
epkg --root ./.eenv install py3-pip py3-requests

# 添加到 .gitignore
echo ".eenv/" >> .gitignore

# 团队成员 clone 后运行脚本
epkg run ./setup.sh    # 自动发现 .eenv
epkg run ./main.py
```

### 4.4 回滚与历史管理

```bash
# 查看环境历史
epkg history

# 输出示例:
# id | timestamp           | action  | packages | command line
# ---+---------------------+---------+----------+---------------------------
# 1  | 2026-03-11 10:00:00 | Create  |          | epkg env create alpine
# 2  | 2026-03-11 10:05:00 | Install | +6       | epkg -e alpine install bash
# 3  | 2026-03-11 11:00:00 | Install | +2       | epkg -e alpine install jq

# 回滚到指定版本
epkg restore 2        # 回滚到 generation 2
epkg restore -1       # 回滚一个版本
```

### 4.5 沙箱模式

```bash
# 默认: namespace 隔离 (env 模式)
epkg -e myenv run bash

# 文件系统隔离 (pivot_root)
epkg -e myenv run --sandbox=fs bash

# 虚拟机隔离 (最安全)
epkg -e myenv run --sandbox=vm bash

# 选择 VMM 后端
epkg -e myenv run --sandbox=vm --vmm=libkrun,qemu bash
```

---

## 五、常用命令详解

### 5.1 包安装

```bash
# 基本安装
epkg install package-name

# 指定环境
epkg -e myenv install package-name

# 安装多个包
epkg install bash jq htop

# 安装本地/远程包文件
epkg install ./package.rpm https://example.com/package.deb

# 选项
epkg install --dry-run package          # 预览
epkg install -y package                 # 非交互
epkg install --no-install-recommends    # 不安装推荐包
```

### 5.2 包查询

```bash
# 列出已安装包 (默认)
epkg list
epkg list bash*          # glob 过滤

# 列出可升级包
epkg list --upgradable

# 列出仓库可用包
epkg list --available

# 搜索包
epkg search htop
epkg search --files ".desktop"   # 搜索文件

# 包详情
epkg info bash
epkg info bash --arch aarch64    # 其他架构
```

### 5.3 环境管理

```bash
# 列出环境
epkg env list

# 创建环境
epkg env create myenv -c alpine
epkg env create myenv -c debian:13       # 指定版本
epkg env create --root /tmp/test/.eenv   # 指定路径

# 注册/取消注册
epkg env register myenv --path-order 10
epkg env unregister myenv

# 激活/停用 (临时)
epkg env activate myenv
epkg env deactivate

# 查看 PATH
epkg env path
# 输出: export PATH="/home/user/.epkg/envs/main/ebin:..."

# 环境配置
epkg env config get sandbox.sandbox_mode
epkg env config set sandbox.sandbox_mode fs
```

### 5.4 运行命令

```bash
# 在环境中运行
epkg -e myenv run command --args

# 示例
epkg -e alpine run jq --version
epkg -e debian run python3 -c "print('hello')"

# 自动发现 .eenv
cd /project
epkg run ./script.sh

# 内置 busybox 命令
epkg busybox ls -la
epkg busybox cat /etc/passwd
epkg busybox sha256sum file.txt
```

---

## 六、目录结构与文件

### 6.1 用户私有安装布局

```
~/.epkg/
├── envs/                          # 环境目录
│   ├── self/                      # epkg 自身环境
│   │   ├── usr/bin/epkg           # epkg 二进制
│   │   └── usr/src/epkg/          # 源代码和资源
│   ├── main/                      # 默认环境
│   │   ├── ebin/                  # 暴露的二进制 (PATH)
│   │   ├── usr/                   # symlink → store
│   │   ├── etc/                   # 配置
│   │   └── generations/           # 历史版本
│   └── <env-name>/                # 其他环境
├── store/                         # 内容寻址存储
│   └── <hash>__<name>__<ver>__<arch>/fs/
└── config/                        # 配置文件
    ├── options.yaml               # 全局选项
    └── envs/<env>.yaml            # 环境配置

~/.cache/epkg/
├── downloads/                     # 下载缓存
├── channels/                      # 仓库元数据缓存
└── aur_builds/                    # AUR 构建缓存
```

### 6.2 Root 共享安装布局

```
/opt/epkg/
├── cache/downloads/               # 共享下载缓存
├── cache/channels/                # 共享元数据缓存
├── store/                         # 共享存储
└── envs/
    ├── root/<env>/                # root 用户环境
    └── <owner>/<env>/             # 其他用户公共环境
```

### 6.3 关键配置文件

| 文件 | 用途 |
|------|------|
| `~/.epkg/config/options.yaml` | 全局选项 (默认沙箱、代理等) |
| `~/.epkg/config/envs/<env>.yaml` | 环境配置 (channel、public 等) |
| `<env_root>/etc/epkg/env.yaml` | 环境内配置 |
| `~/.bashrc` | 加载 epkg shell 集成 |

---

## 七、高级功能

### 7.1 沙箱模式对比

| 模式 | 隔离级别 | 性能 | 使用场景 |
|------|---------|------|---------|
| `env` | namespace | 最高 | 日常开发 |
| `fs` | pivot_root | 高 | 需要文件系统隔离 |
| `vm` | 虚拟机 | 较低 | 运行不可信代码 |

### 7.2 VMM 后端

| 后端 | 特点 |
|------|------|
| `libkrun` | 轻量 microVM，快速启动 |
| `qemu` | 完整虚拟机，广泛兼容 |

### 7.3 内置 Busybox 命令

```bash
epkg busybox --list    # 查看可用命令

# 常用命令
epkg busybox ls        # 列目录
epkg busybox cat       # 查看文件
epkg busybox grep      # 搜索文本
epkg busybox sed       # 流编辑
epkg busybox wget      # 下载
epkg busybox tar       # 归档
epkg busybox sha256sum # 哈希计算
```

---

## 八、故障排除

### 8.1 常见问题

| 问题 | 解决方案 |
|------|---------|
| `command not found` | 确认环境已注册: `epkg env list` |
| 下载失败 | 检查网络/代理: `--proxy` |
| 权限错误 | 确认在用户命名空间内运行 |
| 依赖冲突 | 检查 `--dry-run` 输出 |

### 8.2 调试选项

```bash
# 详细输出
epkg -v install package

# 调试日志
RUST_LOG=debug epkg install package

# 模拟运行
epkg --dry-run install package
```

### 8.3 清理与恢复

```bash
# 垃圾回收
epkg gc

# 清理旧下载
epkg gc --old-downloads 7  # 7天前的

# 历史回滚
epkg history
epkg restore <gen_id>
```

---

## 九、最佳实践

### 9.1 环境命名规范

```
main           # 默认环境
dev-<project>  # 项目开发环境
<distro>       # 发行版测试环境 (debian, fedora)
<tool>         # 工具环境 (rust, python, node)
```

### 9.2 PATH 优先级策略

```bash
# 开发工具优先
epkg env register dev-tools --path-order 5

# 系统兼容环境
epkg env register compat --path-order 50

# 追加到 PATH 末尾
epkg env register fallback --path-order -10
```

### 9.3 项目工作流

```
project/
├── .eenv/           # epkg 环境 (加入 .gitignore)
├── .gitignore
├── README.md        # 文档: "运行 epkg run ./setup.sh"
└── src/
```

---

## 十、与类似工具对比

| 特性 | epkg | Nix | Conda | Docker |
|------|------|-----|-------|--------|
| 无 root | ✅ | ✅ | ✅ | ❌ |
| 多发行版 | ✅ | 部分 | ❌ | ✅ |
| 混合包 | ✅ | ✅ | ❌ | ❌ |
| 原生性能 | ✅ | ✅ | ✅ | 有开销 |
| 学习曲线 | 低 | 高 | 低 | 中 |

---

## 附录：命令速查表

```bash
# 安装
wget ... && bash epkg-installer.sh && bash

# 环境
epkg env list
epkg env create <name> -c <channel>
epkg env remove <name>
epkg env register <name>
epkg env activate <name>

# 包
epkg install <pkg>
epkg remove <pkg>
epkg update
epkg upgrade
epkg list
epkg search <pattern>
epkg info <pkg>

# 运行
epkg -e <env> run <cmd>
epkg run ./script.sh

# 历史
epkg history
epkg restore <gen>

# 其他
epkg gc
epkg repo list
epkg --version
```

---

*文档版本: v1.0*
*适用于: epkg 0.2.4+*
