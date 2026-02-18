# 包操作

本页面描述安装、删除、更新、升级、列表、搜索和信息查询。所有这些都适用于所选环境（默认：main，或使用 `-e ENV` / `--root DIR`）。

## 依赖解析

epkg 使用 SAT 解析器（resolvo）来解析依赖。当您安装一个包时，epkg：

1. 获取仓库元数据（如果需要，通过 `update`）
2. 递归解析所有依赖
3. 处理冲突和废弃
4. 根据标志考虑推荐/建议（`--no-install-recommends`、`--install-suggests`）
5. 显示一个计划，说明将安装/升级/移除的内容

安装计划中的 **DEPTH** 列显示依赖深度（0 = 直接依赖，更高 = 传递依赖）。这有助于您理解依赖树。

## 安装

```bash
epkg install [OPTIONS] [PACKAGE_SPEC]...
```

常用选项：

- **-y, --assume-yes** — 对提示回答“是”（适用于脚本）
- **--no-install-recommends** — 不安装推荐的包
- **--no-install-essentials** — 不自动安装核心包
- **--install-suggests** — 同时安装建议的包
- **-m, --ignore-missing** — 如果某些包缺失，继续执行
- **--dry-run** — 显示将执行的操作而不进行更改

示例：

```bash
epkg -e alpine install bash jq
```

示例输出（摘要）：

```
Packages to be freshly installed:
DEPTH       SIZE  PACKAGE
0       469.7 KB  bash__5.3.3-r1__x86_64
0       147.9 KB  jq__1.8.1-r0__x86_64
1       182.8 KB  oniguruma__6.9.10-r0__x86_64
2       403.6 KB  musl__1.2.5-r21__x86_64
...
Packages to be exposed:
- jq__1.8.1-r0__x86_64
- bash__5.3.3-r1__x86_64

0 upgraded, 19 newly installed, 0 to remove, 2 to expose, 0 to unexpose.
Need to get 4.6 MB archives.
After this operation, 11.0 MB of additional disk space will be used.
```

然后下载和安装继续进行；最后您可能会看到脚本消息和“Exposed package commands to …”。

## 删除

```bash
epkg remove [OPTIONS] [PACKAGE_SPEC]...
```

示例：

```bash
epkg -e alpine remove htop
```

示例输出：

```
Packages to remove:
- htop__3.4.1-r1__x86_64
- libncursesw__6.5_p20251123-r0__x86_64
Do you want to continue? [Y/n] y
```

## 更新（刷新元数据）

```bash
epkg update
```

为环境所使用的 channel 下载并刷新仓库元数据。在查看新包或执行升级之前需要先运行该命令。

## 升级

```bash
epkg upgrade [PACKAGE_SPEC]...
```

将已安装的所有包（或指定的包）升级到 channel 中可用的最新版本。

## 列表

```bash
epkg list [--installed|--available|--upgradable|--all] [PKGNAME_GLOB]
```

- **--installed**（默认）— 环境中已安装的包。
- **--available** — 从 channel 可用（不一定已安装）。
- **--upgradable** — 已安装且有新版本。
- **--all** — 所有（已安装 + 可用）。
- **PKGNAME_GLOB** — 可选的 glob 以按名称过滤。

示例：

```bash
epkg -e alpine list
```

示例输出：

```bash
Exposed/Installed/Available
| Upgradable
|/  Depth      Size  Name                                  Version                         Arch      Repo                Description
===-======-=========-=====================================-===============================-=========-===================-============================================================
E       0  520.1 KB  coreutils                             9.8-r1                          x86_64    main                The basic file, shell and text manipulation utilities
E       0  127.9 KB  htop                                  3.4.1-r1                        x86_64    main                Interactive process viewer
E       0  147.9 KB  jq                                    1.8.1-r0                        x86_64    main                A lightweight and flexible command-line JSON processor
I       1   14.0 KB  acl-libs                              2.3.2-r1                        x86_64    main                Access control list utilities (libraries)
I       1   16.5 KB  coreutils-env                         9.8-r1                          x86_64    main                The basic file, shell and text manipulation utilities
I       1   14.5 KB  coreutils-fmt                         9.8-r1                          x86_64    main                The basic file, shell and text manipulation utilities
I       1   14.4 KB  coreutils-sha512sum                   9.8-r1                          x86_64    main                The basic file, shell and text manipulation utilities
I       1    7.8 KB  libattr                               2.5.2-r2                        x86_64    main                utilities for managing filesystem extended attributes (libraries)
I       1  155.0 KB  libncursesw                           6.5_p20251123-r0                x86_64    main                Console display library (libncursesw)
I       1  182.8 KB  oniguruma                             6.9.10-r0                       x86_64    main                a regular expressions library
I       1    5.1 KB  utmps-libs                            0.1.3.1-r0                      x86_64    main                A secure utmp/wtmp implementation (libraries)
I       1    1.5 KB  yash-binsh                            2.60-r0                         x86_64    main                yash as /bin/sh
I       2    1.9 MB  libcrypto3                            3.5.5-r0                        x86_64    main                Crypto library from openssl
I       2  403.6 KB  musl                                  1.2.5-r21                       x86_64    main                the musl c library (libc) implementation
I       2   21.3 KB  ncurses-terminfo-base                 6.5_p20251123-r0                x86_64    main                Descriptions of common terminals
I       2   76.3 KB  skalibs-libs                          2.14.4.0-r0                     x86_64    main                Set of general-purpose C programming libraries for skarnet.org software. (libraries)
I       2  159.7 KB  yash                                  2.60-r0                         x86_64    main                Yet another shell
I       3  492.9 KB  busybox                               1.37.0-r30                      x86_64    main                Size optimized toolbox of many common UNIX utilities
Total: 18 packages, 4.2 MB, 9.6 MB if installed
```

## 搜索

```bash
epkg search [PATTERN]
```

搜索包名称和描述（以及可选的文件名）。输出是匹配包的列表，包含简短描述。

示例：

```bash
epkg -e debian search htop
```

## 信息

```bash
epkg info [PACKAGE]
```

显示包的详细信息（名称、版本、摘要、主页、架构、维护者、依赖、建议、大小、位置、状态等）。如果省略 PACKAGE，则列出所有已安装包的信息（或在所选环境的上下文中）。

示例：

```bash
epkg -e alpine info jq
```

示例样式输出：

```
pkgname: jq
version: 1.8.1-r0
summary: A lightweight and flexible command-line JSON processor
arch: x86_64
...
status: Installed
```

（确切的字段取决于 channel 格式。）

## 提示和最佳实践

### 定期更新元数据

定期运行 `epkg update` 以获取最新的包版本：

```bash
epkg update
```

### 在重大更改之前使用 --dry-run

预览将发生的情况：

```bash
epkg install --dry-run large-package
epkg upgrade --dry-run
```

### 检查可升级的包

查看可以升级的内容：

```bash
epkg list --upgradable
```

### 安装给定的软件包文件 (本地/远程URL)

如果您有 `.rpm` 文件：

```bash
epkg install package.rpm https://.../package2.rpm
```
