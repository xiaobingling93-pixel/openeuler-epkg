# VM 内核管理

## 概述

epkg 使用 Linux 内核来运行虚拟机（通过 libkrun 或 QEMU）。内核是 VM 启动的核心组件，本节介绍如何管理和自定义 VM 内核。

## 内核来源

epkg 的 VM 内核可以来自以下途径：

### 1. 自动下载（推荐）

运行 `epkg self install` 时，会自动从 gitee releases 下载预编译的内核：

```
~/.epkg/envs/self/boot/
├── kernel -> kernel-6.19.6    # 符号链接指向当前内核
└── kernel-6.19.6              # 实际内核文件
```

### 2. 本地构建

开发者可以自行编译内核：

```bash
# 构建并安装到 ~/.epkg/envs/self/boot/
cd git/libkrunfw
./build.sh

# 指定架构构建
./build.sh aarch64
```

### 3. 手动指定

运行 VM 时通过 `--kernel` 参数指定内核路径：

```bash
epkg run --kernel /path/to/vmlinux <package>
```

## 内核版本命名

内核文件命名格式：`kernel-$version`

- `version` 来自内核构建时的 `Linux version` 字符串
- 例如：`kernel-6.19.6`, `kernel-6.12.68`

## 支持的架构

| 架构 | 内核格式 | 状态 |
|------|----------|------|
| x86_64 | ELF vmlinux | 支持 |
| aarch64 | Image | 支持 |
| riscv64 | Image | 支持 |
| loongarch64 | - | 不支持 |

## 常见用例

### 查看当前内核版本

```bash
ls -la ~/.epkg/envs/self/boot/
# 或
file ~/.epkg/envs/self/boot/kernel
```

### 切换内核版本

如果存在多个内核版本，可以修改符号链接：

```bash
ln -sf kernel-6.12.68 ~/.epkg/envs/self/boot/kernel
```

### 自定义内核配置

内核配置文件位于 `git/libkrunfw/config-libkrunfw_$arch`。

修改配置后重新构建：

```bash
cd git/libkrunfw
# 编辑 config-libkrunfw_x86_64
./build.sh
```

## 内核下载机制

`epkg self install` 会：

1. 从 gitee 获取 libkrunfw 仓库的最新 release
2. 查找匹配当前架构的 `vmlinux-$arch-$kver.zst` 文件
3. 下载并解压到 `~/.epkg/envs/self/boot/`

下载的文件格式：
- `vmlinux-x86_64-6.19.6.zst` - zstd 压缩的内核
- `vmlinux-x86_64-6.19.6.zst.sha256` - 校验文件

## 与 libkrun 的关系

当使用 libkrun 运行 VM 时：

- 如果指定了 `--kernel`，使用指定的内核
- 如果未指定，使用 `~/.epkg/envs/self/boot/kernel`（默认内核）
- 如果默认内核不存在，libkrun 无法启动 VM

注意：新架构不再依赖 `libkrunfw.so` 动态库，内核作为独立文件管理。
