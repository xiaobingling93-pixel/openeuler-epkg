# 环境管理

环境是相互隔离的根目录：每个环境有一个 channel（例如 debian、alpine、fedora，可选版本）、自己的一组已安装包，以及一个目录 `usr/ebin`，其中链接了暴露的二进制文件。您可以**注册**环境，将其 `usr/ebin` 添加到 PATH 中，或者只在当前 shell 中**激活**一个环境。

## 列出环境

```bash
epkg env list
```

示例输出：

```
 Type      Status         Environment                         Root
=========-==============-===================================-================================
 private                  __c__compass-ci__.eenv              /c/compass-ci/.eenv
 private                  __c__epkg__scripts__mirror__.eenv   /c/epkg/scripts/mirror/.eenv
 private                  aa                                  /home/wfg/.epkg/envs/aa
 private                  alpine                              /home/wfg/.epkg/envs/alpine
 private                  archlinux                           /home/wfg/.epkg/envs/archlinux
 private                  conda                               /home/wfg/.epkg/envs/conda
 private                  debian                              /home/wfg/.epkg/envs/debian
 private                  fedora                              /home/wfg/.epkg/envs/fedora
 private   registered@0   main                                /home/wfg/.epkg/envs/main
 private                  openeuler                           /home/wfg/.epkg/envs/openeuler
 private                  opensuse                            /home/wfg/.epkg/envs/opensuse
 private                  ubuntu                              /home/wfg/.epkg/envs/ubuntu
```

列：**Environment** 名称、**Type**（private/public）、**Status**（例如 registered@ORDER）。在共享存储模式下，其他用户的公共环境可能以所有者前缀出现（例如 `root/envname`）。

## 创建环境

```bash
epkg env create [ENV_NAME] [-c|--channel CHANNEL] [-P|--public] [-i|--import FILE]
```

- **ENV_NAME** — 新环境的名称。
- **-c, --channel** — channel（例如 `debian` (默认值)、`ubuntu`、`alpine`、`fedora`、`openeuler`、`archlinux`、`conda`）。
- **-P, --public** — 使环境公开（在共享存储模式下，其他用户可以以只读方式使用 `owner/envname`）。
- **-i, --import** — 从配置文件导入。

示例：

```bash
epkg env create mydebian -c debian
# Creating environment 'mydebian' in $HOME/.epkg/envs/mydebian

epkg env create myalpine -c alpine
# Creating environment 'myalpine' in $HOME/.epkg/envs/myalpine
```

### 按路径创建（--root）

您可以在任意路径创建环境；epkg 从路径生成名称：

```bash
epkg env create --root /tmp/myproject/.eenv -c alpine
# Creating environment '__tmp__myproject__.eenv' in /tmp/myproject/.eenv
# Note: environment name was auto-generated from path
```

使用 **`.eenv`** 作为目录名可实现**隐式环境发现**：从该树下的脚本中，`epkg run ./script.sh`（或 `epkg run /path/to/project/subdir/script.sh`）可以从包含的 `.eenv` 目录解析环境。

## 删除环境

```bash
epkg env remove [ENV_NAME]
```

如果环境已注册，则首先取消注册。示例：

```
# Environment 'myenv' is not registered.
# Environment 'myenv' has been removed.
```

## 注册和取消注册

**注册**将环境的 `usr/ebin` 添加到您的默认 PATH（跨 shell 持久化）。**取消注册**将其移除。

```bash
epkg env register [ENV_NAME] [--path-order N]
epkg env unregister [ENV_NAME]
```

注册后，命令会打印新的 PATH；您可以在当前 shell 中运行 `eval "$(epkg env path)"` 以应用它，或者如果您的 RC 文件加载了 epkg 的路径助手，则依赖它。

示例：

```bash
epkg env register myalpine
# Registering environment 'myalpine' with PATH order 100
# export PATH="/home/user/.epkg/envs/myalpine/usr/ebin:/home/user/.epkg/envs/main/usr/ebin:..."

epkg env unregister myalpine
# export PATH="/home/user/.epkg/envs/main/usr/ebin:..."
# Environment 'myalpine' has been unregistered.
```

**--path-order** — 数字越小，在 PATH 中越靠前。默认为 100。

## 激活和停用

**激活**仅为当前 shell 设置环境（会话特定）。**停用**清除它。

```bash
epkg env activate [ENV_NAME]
epkg env deactivate
```

激活后，shell 的 PATH 会更新，以便优先使用激活的环境。对于临时专注于一个环境而不更改已注册环境很有用。

## 路径和配置

- **Path** — 打印包含所有已注册（以及可选的已激活）环境的当前 PATH。顺序为：

  1. 已激活的环境（最近激活的在前）
  2. 具有较低 `--path-order` 的已注册环境（前置侧）
  3. 原始/系统 PATH
  4. 具有负 `--path-order` 的已注册环境（追加侧）

  ```bash
  epkg env path
  # export PATH="/home/user/.epkg/envs/main/usr/ebin:..."
  ```

- **Config** — 查看或编辑每个环境的配置：

  ```bash
  epkg env config edit
  epkg env config get <key>
  epkg env config set <key> <value>
  ```

  示例：`env_root`、`public`（布尔值）。

## 为命令选择环境

对于任何在环境上操作的命令，您可以使用以下方式指定：

- **-e, --env ENV_NAME** — 名称（例如 `main`、`alpine`），或在共享存储中为 `owner/envname`。
- **-r, --root DIR** — 环境的根目录（例如在 `env create --root /path` 之后）。

如果两者同时指定，优先使用 `-r`。
如果两者都未给出，epkg 会查找 .eenv/ 环境，使用**已激活**环境，或**已注册**环境（对于 `run`，使用 PATH 中提供命令的第一个环境），或者回退到 **main**。

示例：

```bash
epkg -e alpine install htop
epkg -e alpine list
epkg -e alpine run htop --version
epkg --root /tmp/myproject/.eenv run jq --version
```

## `epkg run` 的沙箱模式

在环境中运行命令时，epkg 可以在每个环境的根文件系统之上增加额外的隔离：

- **env**（默认）— 使用用户命名空间和挂载命名空间，并将环境通过 bind 挂载到 `/usr`、`/etc`、`/var`、`/run` 等。提供兼容性隔离，并非强安全边界。
- **fs** — 通过 `pivot_root` 将环境目录作为新根；在其下挂载 proc、tmpfs（/tmp、/dev）等伪文件系统。更强的文件系统隔离。
- **vm** — 在轻量级虚拟机内运行命令，环境根通过 virtiofs 共享。设计与依赖（VMM、内核、virtiofsd）见 `docs/design-notes/sandbox-vmm.md`。

可按命令选择沙箱模式：

```bash
epkg -e mydebian run --sandbox=env  bash
epkg -e mydebian run --sandbox=fs   python3 script.py
epkg -e mydebian run --sandbox=vm   bash
```

也可在 `env_root/etc/epkg/env.yaml` 中为该环境设置**默认沙箱**：

```bash
# 将此环境的默认沙箱设为 fs
epkg -e mydebian env config set sandbox.sandbox_mode fs

# 之后，直接执行 `epkg -e mydebian run <cmd>` 将使用 fs，除非用 --sandbox 覆盖
epkg -e mydebian run bash
```

用户级默认值可在 `~/.epkg/config/options.yaml` 中设置（同样使用 `sandbox.sandbox_mode`）。命令行 `--sandbox` 会覆盖上述两者。

沙箱依赖宿主机上的用户命名空间及 `newuidmap`/`newgidmap`，可安装最小依赖集：

```bash
cd /c/epkg
./bin/make.sh sandbox-depends
```

会为当前发行版安装相应的 `uidmap`/`shadow`/`shadow-uidmap` 等包；用户命名空间相关错误详见[故障排除](troubleshooting.md)。

## 公共环境（共享存储）

当 epkg 与**共享**存储一起使用时（例如 root 使用 `/opt/epkg`），环境可以是**公共**的。其他用户可以：

- 列出它们（它们在 `epkg env list` 中以 `owner/envname` 出现）。
- 以只读方式使用它们：`epkg -e owner/envname run <cmd>`、`epkg -e owner/envname search <pkg>` 等。

使用 `-P` 创建的环境是公共的。`main` 环境不能是公共的。

## 存储模式规则（self install）

- **epkg self install** 可以接受 `--store private|shared|auto`。
- **auto**（默认）：如果不是 root，则为 private；如果是 root，则为 shared。

公共/私有仅适用于共享存储模式。

## 最佳实践

### 按用途组织环境

- **main** — 用于一般用途的默认环境
- **project-name** — 具有特定依赖的每个项目环境
- **distro-name** — 用于尝试特定发行版软件包的环境
- **tool-name** — 用于特定工具或工具链的环境

### 使用 path-order 控制 PATH 顺序

在注册多个环境时，使用 `--path-order` 控制哪个优先：

```bash
epkg env register dev-env  --path-order 5   # PATH 中靠前
epkg env register test-env --path-order 20  # PATH 中靠后
```

数字越小 = 在 PATH 中越靠前。

### 项目特定环境

对于需要隔离依赖的项目：

```bash
cd /path/to/project
epkg env create --root ./.eenv -c alpine
# 添加到 .gitignore: .eenv/
# 在 README 中记录："运行: epkg run ./setup.sh"
```

这保持了项目依赖的隔离性，并使项目可移植。

### 清理未使用的环境

定期审查并删除不再需要的环境：

```bash
epkg env list
epkg env remove old-env
epkg gc  # 清理未使用的存储文件
```

## 另请参阅

- [包操作](package-operations.md) — 在环境中安装软件包
- [高级用法](advanced.md) — 运行命令和服务
