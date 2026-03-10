# VM 内核架构设计

## 设计目标

1. **独立管理**：内核与 libkrunfw 动态库解耦，可独立更新
2. **简化部署**：下载预编译内核，无需从 .so 提取
3. **开发友好**：支持本地构建和快速迭代
4. **压缩传输**：使用 zstd 压缩，减少下载体积

## 架构演变

### 旧架构：libkrunfw.tarball

```
epkg self install
    ↓
下载 libkrunfw-$arch.tgz (GitHub releases)
    ↓
解压到临时目录
    ↓
安装 libkrunfw.so 到 ~/.epkg/envs/self/usr/lib/
    ↓
从 .so 提取 KERNEL_BUNDLE 符号
    ↓
写入 ~/.epkg/envs/self/boot/kernel
```

**问题**：
- 依赖 libkrunfw.so 动态库
- 内核打包在 .so 中，更新需要重新构建 libkrunfw
- 需要复杂的 ELF 符号提取逻辑

### 新架构：vmlinux.zst

```
epkg self install
    ↓
下载 vmlinux-$arch-$kver.zst (Gitee releases)
    ↓
zstd 解压
    ↓
写入 ~/.epkg/envs/self/boot/kernel-$version
    ↓
创建符号链接 kernel -> kernel-$version
```

**优势**：
- 不依赖动态库
- 内核独立更新
- 代码更简洁
- 下载文件更小（~5.5MB vs ~19MB）

## 核心代码结构

### init.rs

```
src/init.rs
├── REPO_VMLINUX              # gitee 仓库名 "libkrunfw"
├── InitPlan
│   ├── vmlinux_url           # 下载 URL
│   ├── vmlinux_sha_url       # SHA256 文件 URL
│   ├── vmlinux_version       # 内核版本号
│   └── vmlinux_path          # 本地下载路径
├── get_vmlinux_url()         # 获取下载信息
├── install_vmlinux()         # 解压安装内核
└── zstd_decompress_file()    # zstd 解压
```

### build.sh

```
git/libkrunfw/
├── build.sh                          # 构建脚本
├── config-libkrunfw_x86_64           # x86_64 内核配置
├── config-libkrunfw_aarch64          # aarch64 内核配置
├── config-libkrunfw_riscv64          # riscv64 内核配置
└── dist/                             # 发布产物
    ├── vmlinux-$arch-$kver.zst
    └── vmlinux-$arch-$kver.zst.sha256
```

## 关键函数

### get_vmlinux_url()

```rust
#[cfg(all(feature = "libkrun", target_os = "linux"))]
fn get_vmlinux_url() -> Result<Option<(String, String, String)>> {
    // 1. 检查架构支持
    // 2. 获取 gitee 最新 release
    // 3. 查找 vmlinux-$arch-$kver.zst 文件
    // 4. 返回 (url, sha256_url, version)
}
```

### install_vmlinux()

```rust
#[cfg(all(feature = "libkrun", target_os = "linux"))]
fn install_vmlinux(zst_path: &Path, version: &str) -> Result<()> {
    // 1. zstd 解压
    // 2. 写入 kernel-$version
    // 3. 创建符号链接 kernel -> kernel-$version
}
```

### install_kernel_for_libkrun() [bin/make.sh]

本地开发时的内核安装逻辑：

```bash
install_kernel_for_libkrun() {
    # 1. 检查 kernel 是否已存在
    # 2. 检查 git/linux/vmlinux 是否存在
    # 3. 复制/硬链接到 ~/.epkg/envs/self/boot/
    # 4. 提取版本号并创建符号链接
}
```

## 数据流

### 用户安装流程

```
用户运行: epkg self install

check_for_updates()
    ├── fetch_latest_release(GITEE_OWNER, REPO_VMLINUX)
    ├── 查找 vmlinux-$arch-$kver.zst 资源
    └── 构建 InitPlan

download_package_manager_files()
    ├── 下载 vmlinux-$arch-$kver.zst
    ├── 下载 vmlinux-$arch-$kver.zst.sha256
    └── install_vmlinux()
        ├── zstd 解压
        ├── 写入 kernel-$version
        └── 创建符号链接
```

### 开发构建流程

```
开发者运行: cd git/libkrunfw && ./build.sh --deploy

build.sh
    ├── 复制 config-libkrunfw_$arch 到 git/linux/.config
    ├── make olddefconfig
    ├── make vmlinux
    ├── 提取版本号
    ├── 本地安装到 ~/.epkg/envs/self/boot/ (如果 arch 匹配)
    └── --deploy 模式:
        ├── zstd -19 压缩
        └── 生成 .sha256 文件
```

## 内核配置管理

内核配置文件维护在 `git/libkrunfw/` 仓库：

| 文件 | 架构 | 说明 |
|------|------|------|
| config-libkrunfw_x86_64 | x86_64 | 标准 VM 内核 |
| config-libkrunfw_aarch64 | aarch64 | ARM64 VM 内核 |
| config-libkrunfw_riscv64 | riscv64 | RISC-V VM 内核 |

配置特点：
- 最小化配置，仅启用 VM 必要功能
- 支持 virtio 设备（virtiofs, virtio-net）
- 支持命名空间和 cgroup
- 不包含模块（无 /lib/modules 依赖）

## 发布流程

1. 更新内核配置（如需要）
2. 构建发布版本：
   ```bash
   cd git/libkrunfw
   ./build.sh --deploy
   ```
3. 上传到 gitee releases：
   - `vmlinux-$arch-$kver.zst`
   - `vmlinux-$arch-$kver.zst.sha256`
4. 用户运行 `epkg self install` 或 `epkg self upgrade`

## 兼容性考虑

### libkrun 内核格式

libkrun 支持两种内核格式：

| 格式值 | 格式名 | 架构 | 说明 |
|--------|--------|------|------|
| 0 | Raw | aarch64, riscv64 | Image 格式 |
| 1 | ELF | x86_64 | vmlinux 格式 |

epkg 通过检测文件魔数自动识别格式。

### 与旧版本的兼容

- 旧的 `libkrunfw.so` 仍然可用，但不再自动安装
- 如果 `~/.epkg/envs/self/usr/lib/libkrunfw.so` 存在，libkrun 仍可加载
- 新安装不再创建 libkrunfw.so，仅安装内核文件
