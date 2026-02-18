# 高级用法

本页面涵盖在环境中运行命令、服务管理、历史/恢复、垃圾回收以及包工具（转换、解压、哈希、busybox）。

## 在环境中运行命令

```bash
epkg run [OPTIONS] <COMMAND> [--] [ARGS...]
```

在环境的命名空间（mount + user 命名空间）中运行 **COMMAND**。环境的 `usr`、`etc`、`var` 被绑定挂载，以便命令看到环境的文件和库。使用 `--` 来分隔 epkg 选项与命令及其参数。

示例：

```bash
epkg -e alpine run jq --version
# jq-1.8.1

epkg -e alpine run htop --version
# htop 3.4.1-3.4.1

epkg run gcc -- --version
#（使用已激活或第一个注册的包含 gcc 的环境）
```

如果当前目录位于具有 `.eenv` 目录的项目下，您可以运行脚本并让 epkg 从该路径解析环境：

```bash
epkg run ./myscript.sh
epkg run /path/to/project/subdir/myscript.sh
```

## 服务管理

```bash
epkg service <start|stop|restart|status|reload> [SERVICE_NAME]
```

管理环境中安装的 systemd（或兼容）服务。start/stop/restart/status/reload 应用于给定的服务，或者如果没有指定名称，则应用于环境中所有服务。

## 历史和恢复

- **history** — 显示环境的事务历史（生成版本 ID、时间戳、操作、包数量、命令行）。

```bash
epkg history
```

  示例输出：

```
% epkg history -e alpine
_________________________________  ENVIRONMENT HISTORY  _________________________________
id  | timestamp                  | action   | packages | command line
----+----------------------------+----------+----------+---------------------------------
1   | 2026-02-16 13:05:50 +0800  | Create   |          | epkg env create alpine -c alpine
2   | 2026-02-16 13:05:58 +0800  | Install  | +6       | epkg -e alpine install /bin/sh
3   | 2026-02-16 13:45:13 +0800  | Install  | +2       | epkg -e alpine install jq
4   | 2026-02-17 13:45:50 +0800  | Install  | +9       | epkg -e alpine install coreutils
```

- **restore** — 将环境回滚到给定的生成版本（按 ID 或 `-N` 表示回滚 N 个版本）。

```bash
epkg restore <GEN_ID|-N>
```

  恢复后，会创建一个新的生成版本（回滚操作）。再次使用 `epkg history` 确认。

## 垃圾回收

```bash
epkg gc
```

从缓存和存储中删除未使用的文件（例如不再被任何环境引用的包文件和元数据）。当您希望在删除环境或许多包后回收磁盘空间时使用。

## 包工具

### hash

计算目录的内容哈希（用于存储键和验证）：

```bash
epkg hash <STORE_DIR>
```

示例：
```
% epkg hash ~/.epkg/store/rzvdceiy4gmlg6fod4fjzhjndqauh4bu__bash__5.2.37-7.oe2509__x86_64
rzvdceiy4gmlg6fod4fjzhjndqauh4bu
```

### unpack

将 rpm/deb/apk/... 包文件解压到存储中。示例：

```bash
% epkg unpack /home/wfg/.cache/epkg/downloads/openeuler/openEuler-25.09/everything/x86_64/Packages/selinux-policy-40.7-9.oe2509.noarch.rpm
/home/wfg/.epkg/store/qf5m4eqovu3ho7lz6vxku5c6oliz6zjj__selinux-policy__40.7-9.oe2509__noarch
```

### busybox

运行常见 Linux 命令的内置实现（busybox 风格），无需安装完整包：

```bash
epkg busybox <COMMAND> [ARGS...]
```

示例：
```bash
% epkg busybox whoami
wfg
```

在最小化或容器环境中很有用，您希望避免从channel中拉取完整的 coreutils/sed/grep 等。

## 最佳实践

### 使用历史和恢复以确保安全

在进行重大更改之前，注意当前的生成版本：

```bash
epkg history  # 注意最新的生成版本 ID
epkg install large-package
# 如果出现问题：
epkg restore <GEN_ID>  # 回滚
```

### 定期垃圾回收

定期清理未使用的文件：

```bash
epkg gc  # 删除未使用的缓存和存储文件
```

这在删除环境或许多包后特别有用。

### 服务管理

对于通过 epkg 安装的 systemd 服务：

```bash
epkg service status  # 检查所有服务
epkg service start redis
epkg service stop  redis
```

服务在环境的命名空间中运行，与主机隔离。

### 使用 .eenv 的项目工作流

对于项目特定的依赖：

1. 在项目根目录创建 `.eenv`：`epkg env create --root ./.eenv -c alpine`
2. 安装依赖：`epkg --root ./.eenv install <deps>`
3. 在脚本中使用：`epkg run ./script.sh`（自动发现 `.eenv`）
4. 与团队共享：导出环境配置或记录channel/版本

## 全局选项（回顾）

在命令中有用的选项：

- **-e, --env ENV** — 按名称选择环境（或公共环境的 `owner/name`）。
- **-r, --root DIR** — 按根路径选择环境。
- **-y, --assume-yes** — 非交互式；对提示回答是。
- **--dry-run** — 显示将执行的操作而不更改系统。
- **--download-only** — 仅获取包而不安装。
- **-q, --quiet** / **-v, --verbose** — 较少或更多输出。
- **--proxy URL** — 用于下载的 HTTP 代理。
- **--parallel-download N** — 并行下载线程数。

有关完整列表，请参阅[命令参考](../reference/commands.md)。

## 另请参阅

- [包操作](package-operations.md) — 安装、删除、更新、升级
- [环境管理](environments.md) — 环境管理
