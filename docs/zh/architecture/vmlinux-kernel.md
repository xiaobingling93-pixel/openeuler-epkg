# VM 内核架构设计

## 设计目标

1. **独立管理**：内核独立管理，不依赖动态库
2. **简化部署**：下载预编译内核，无需从 .so 提取
3. **开发友好**：支持本地构建和快速迭代
4. **压缩传输**：使用 zstd 压缩，减少下载体积

## 架构

```
epkg self install
    ↓
下载 vmlinux-$kver-$arch.zst (Gitee releases)
    ↓
zstd 解压
    ↓
写入 ~/.epkg/envs/self/boot/vmlinux-$kver-$arch
    ↓
创建符号链接 vmlinux -> vmlinux-$kver-$arch (仅 host arch)
```

**优势**：
- 不依赖动态库
- 内核独立更新
- 代码更简洁
- 下载文件更小（~5.5MB）
- 多架构内核可共存

## 核心代码结构

### init.rs

```
src/init.rs
├── REPO_VMLINUX              # gitee 仓库名 "sandbox-kernel"
├── InitPlan
│   ├── vmlinux_url           # 下载 URL
│   ├── vmlinux_sha_url       # SHA256 文件 URL
│   ├── vmlinux_version       # 内核版本号
│   └── vmlinux_path          # 本地下载路径
├── get_vmlinux_url()         # 获取下载信息
├── install_vmlinux()         # 解压安装内核
└── zstd_decompress_file()    # zstd 解压
```

### sandbox-kernel 仓库

```
git/sandbox-kernel/
├── kconfig/
│   ├── common                      # 所有架构共享配置
│   └── arch/
│       ├── x86_64                  # x86_64 特定配置
│       ├── aarch64                 # aarch64 特定配置
│       └── riscv64                 # riscv64 特定配置
├── scripts/
│   ├── setup-env.sh                # 安装构建依赖
│   └── build.sh                    # 构建脚本
├── boot/                           # 本地开发缓存
│   ├── vmlinux-$kver-$arch         # 内核文件
│   ├── config-$kver-$arch          # 对应配置
│   └── vmlinux -> vmlinux-$kver-$arch  # 符号链接 (host arch)
└── dist/                           # 发布产物
    ├── vmlinux-$kver-$arch.zst
    └── vmlinux-$kver-$arch.zst.sha256
```

## 关键函数

### get_vmlinux_url()

```rust
#[cfg(all(feature = "libkrun", target_os = "linux"))]
fn get_vmlinux_url() -> Result<Option<(String, String, String)>> {
    // 1. 检查架构支持
    // 2. 获取 gitee 最新 release
    // 3. 查找 vmlinux-$kver-$arch.zst 文件
    // 4. 返回 (url, sha256_url, version)
}
```

### install_vmlinux()

```rust
#[cfg(all(feature = "libkrun", target_os = "linux"))]
fn install_vmlinux(zst_path: &Path, version: &str) -> Result<()> {
    // 1. zstd 解压
    // 2. 写入 vmlinux-$version-$arch
    // 3. 创建符号链接 vmlinux -> vmlinux-$version-$arch (仅 host arch)
}
```

### install_kernel_for_libkrun() [bin/make.sh]

本地开发时的内核安装逻辑：

```bash
install_kernel_for_libkrun() {
    # 1. 检查 vmlinux 是否已存在
    # 2. 检查 git/sandbox-kernel/linux-stable/vmlinux 是否存在
    # 3. 复制到 ~/.epkg/envs/self/boot/
    # 4. 提取版本号并创建符号链接
}
```

## 数据流

### 用户安装流程

```
用户运行: epkg self install

check_for_updates()
    ├── fetch_latest_release(GITEE_OWNER, REPO_VMLINUX)
    ├── 查找 vmlinux-$kver-$arch.zst 资源
    └── 构建 InitPlan

download_package_manager_files()
    ├── 下载 vmlinux-$kver-$arch.zst
    ├── 下载 vmlinux-$kver-$arch.zst.sha256
    └── install_vmlinux()
        ├── zstd 解压
        ├── 写入 vmlinux-$version-$arch
        └── 创建符号链接
```

### 开发构建流程

```
开发者运行: cd git/sandbox-kernel && ./scripts/build.sh $arch

build.sh
    ├── 合并 kconfig/common + kconfig/arch/$arch 到 .config
    ├── make olddefconfig
    ├── make vmlinux
    ├── 提取版本号
    ├── 本地安装到 boot/ 和 ~/.epkg/envs/self/boot/ (如果 arch 匹配)
    └── ALL 模式:
        ├── zstd -19 压缩到 dist/
        └── 生成 .sha256 文件
```

## 内核配置管理

配置文件采用分层结构，最终 `.config` 由共享配置和架构特定配置合并生成：

```
.config = kconfig/common + kconfig/arch/$arch
```

| 目录 | 说明 |
|------|------|
| kconfig/common | 所有架构共享配置（VirtIO、VSOCK、EROFS 等） |
| kconfig/arch/x86_64 | x86_64 特定配置（KVM guest、ACPI 等） |
| kconfig/arch/aarch64 | ARM64 特定配置 |
| kconfig/arch/riscv64 | RISC-V 特定配置 |

配置特点：
- 最小化配置，仅启用 VM 必要功能
- 支持 virtio 设备（virtiofs, virtio-net）
- 支持命名空间和 cgroup
- 不包含模块（无 /lib/modules 依赖）

## 发布流程

1. 更新内核配置（如需要）
2. 构建发布版本：
   ```bash
   cd git/sandbox-kernel
   ./scripts/build.sh ALL
   ```
3. 上传到 gitee releases：
   - `vmlinux-$kver-$arch.zst`
   - `vmlinux-$kver-$arch.zst.sha256`
4. 用户运行 `epkg self install` 或 `epkg self upgrade`

## 兼容性考虑

### libkrun 内核格式

libkrun 支持两种内核格式：

| 格式值 | 格式名 | 架构 | 说明 |
|--------|--------|------|------|
| 0 | Raw | aarch64, riscv64 | Image 格式 |
| 1 | ELF | x86_64 | vmlinux 格式 |

epkg 通过检测文件魔数自动识别格式。