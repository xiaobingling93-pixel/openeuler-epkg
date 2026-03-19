# 命令参考（Command reference）

本页面总结了 `epkg help` 的输出。有关子命令的详细信息，请运行 `epkg <command> -h`（例如 `epkg install -h`、`epkg env create -h`）。

## 用法

```
epkg [选项] <命令>
```

## 命令（分组）

### 自管理

| 命令 | 描述 |
|------|------|
| `self install [--store private\|shared\|auto]` | 安装或升级 epkg 自身（用户或共享）。 |
| `self upgrade` | 升级 epkg 安装。 |
| `self remove` | 移除 epkg 安装（反初始化）。 |

### 包操作

| 命令 | 描述 |
|------|------|
| `install [PACKAGE_SPEC]...` | 安装包。 |
| `update` | 更新所选环境的包元数据。 |
| `upgrade [PACKAGE_SPEC]...` | 升级包。 |
| `remove [PACKAGE_SPEC]...` | 卸载包。 |

### 环境管理

| 命令 | 描述 |
|------|------|
| `env create [-c\|--channel CHANNEL] [-P\|--public] [-i\|--import FILE] <ENV_NAME\|--root ENV_ROOT>` | 创建新环境。 |
| `env remove \| register \| unregister \| activate \| export` | 移除、注册、取消注册、激活或导出环境；参数：`<ENV_NAME\|--root ENV_ROOT>`。 |
| `env deactivate` | 停用当前会话环境。 |
| `env path` | 打印包含已注册（和激活）环境的 PATH。 |
| `env config <edit\|get\|set>` | 编辑或获取/设置每个环境的配置。 |

### 历史和回滚

| 命令 | 描述 |
|------|------|
| `history` | 显示环境事务历史。 |
| `restore <GEN_ID\|-N>` | 将环境恢复到某个生成版本。 |

### 垃圾回收

| 命令 | 描述 |
|------|------|
| `gc` | 清理未使用的缓存和存储文件。 |

### 信息和查询

| 命令 | 描述 |
|------|------|
| `list [--installed\|--available\|--upgradable\|--all] [PKGNAME_GLOB]` | 列出包。 |
| `info [PACKAGE]` | 显示包信息。 |
| `search [PATTERN]` | 搜索包和文件。 |
| `repo list` | 列出软件源（channel）。 |

### 运行命令

| 命令 | 描述 |
|------|------|
| `run <COMMAND> [--] [ARGS...]` | 在环境命名空间中运行命令。 |
| `service <start\|stop\|restart\|status\|reload> [SERVICE]` | 服务管理。 |
| `busybox <COMMAND> [ARGS...]` | 运行内置命令实现。 |

### 包工具

| 命令 | 描述 |
|------|------|
| `hash [DIR]` | 计算内容哈希（例如用于存储）。 |
| `unpack <FILE.epkg> [DIR]` | 将 epkg 文件解压到存储或 DIR 目录。 |
| `convert [OPTIONS] <PACKAGE_FILE>...` | 将 rpm/deb/apk/... 转换为 epkg 格式。 |

### 构建

| 命令 | 描述 |
|------|------|
| `build` | 从源代码构建包（开发用）。 |

### 帮助

| 命令 | 描述 |
|------|------|
| `help` | 打印帮助（本摘要）。 |

## 全局选项

| 选项 | 描述 |
|------|------|
| `--config <FILE>` | 配置文件。 |
| `-e, --env <ENV_NAME>` | 按名称或所有者/名称选择环境。 |
| `-r, --root <DIR>` | 按根目录选择环境。 |
| `--arch <ARCH>` | 覆盖 CPU 架构。 |
| `--dry-run` | 模拟运行，不进行更改。 |
| `--download-only` | 仅下载包，不安装。 |
| `-q, --quiet` | 抑制输出。 |
| `-v, --verbose` | 详细/调试输出。 |
| `-y, --assume-yes` | 对提示回答是。 |
| `--assume-no` | 对提示回答否。 |
| `-m, --ignore-missing` | 忽略缺失的包。 |
| `--metadata-expire <SECONDS>` | 元数据缓存（0=从不，-1=始终）。 |
| `--proxy <URL>` | HTTP 代理。 |
| `--retry <N>` | 下载重试次数。 |
| `--parallel-download <N>` | 并行下载线程数。 |
| `--parallel-processing <N>` | 并行处理工作线程数（上限为 CPU 核心数；`1` 表示串行）。 |
| `-h, --help` | 帮助。 |
| `-V, --version` | 版本。 |

## 路径（来自帮助）

**用户私有安装**（数据流顺序）：

- `$HOME/.bashrc` — 加载 epkg RC（例如 `$HOME/.epkg/envs/self/usr/src/epkg/assets/shell/epkg.sh`）。
- `$HOME/.cache/downloads/` — 已下载的包文件
- `$HOME/.cache/channels/` — 仓库元数据缓存
- `$HOME/.epkg/store/` — 内容寻址包存储
- `$HOME/.epkg/envs/$env_name/` — 环境根目录
- `$HOME/.epkg/envs/$env_name/etc/epkg/` — 每个环境的配置
- `$HOME/.epkg/envs/self/usr/bin/epkg` — epkg 二进制文件
- `$HOME/.epkg/envs/self/usr/src/epkg/` — epkg 源代码和 RC 脚本

**Root 全局安装：**

- `$HOME/.bashrc`（或系统范围的 `/etc/bash.bashrc`）
- `/opt/epkg/cache/downloads/` — 共享下载缓存
- `/opt/epkg/cache/channels/` — 共享元数据缓存
- `/opt/epkg/store/` — 共享包存储
- `/opt/epkg/envs/root/$env_name/` — Root 用户的环境
- `/opt/epkg/envs/$owner/$env_name/` — 其他用户的公共环境（如果为共享模式）

有关缓存与存储以及目录结构的详细说明，请参阅[路径和布局](paths.md)。