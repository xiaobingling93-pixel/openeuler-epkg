# 内核配置精简

## 设计目标

### 最小化原则

沙箱 VM 内核只保留必要的功能，移除不需要的驱动和子系统：

- **移除物理设备驱动**: INPUT、USB、HID、SOUND、DRM 等 libkrun 不支持的设备
- **移除多余文件系统**: XFS、EXT4、FAT、BTRFS 等，仅保留 virtiofs 必需的 FUSE
- **移除安全子系统**: AUDIT、SELINUX 等非必要安全模块
- **移除调试功能**: DEBUG_KERNEL、DEBUG_INFO 等

### 已禁用配置项

| 配置项 | 理由 |
|--------|------|
| `CONFIG_IPV6` | 沙箱仅需 IPv4 |
| `CONFIG_KSM` | KSM 用于内存超卖，隔离 VM 不需要 |
| `CONFIG_COMPACTION` | 内存整理增加内核复杂度 |
| `CONFIG_TRANSPARENT_HUGEPAGE` | THP 可能导致延迟抖动 |
| `CONFIG_PCIEASPM` | VM 不需要 PCIe 电源管理 |
| `CONFIG_BTRFS_FS` | 仅使用 virtiofs，不需要 Btrfs |
| `CONFIG_DEBUG_KERNEL` | 调试功能增加内核大小和运行时开销 |

## 关键配置

### VMM 后端差异

| 后端 | 内核格式 | 串口 |
|------|----------|------|
| QEMU | bzImage | ttyS0 |
| libkrun | vmlinux | hvc0 |

### 必须保留的配置

```kconfig
# KVM guest 运行
CONFIG_KVM_GUEST=y
CONFIG_PARAVIRT=y

# virtio 设备支持
CONFIG_VIRTIO=y
CONFIG_VIRTIO_PCI=y
CONFIG_VIRTIO_NET=y
CONFIG_VIRTIO_BLK=y
CONFIG_VIRTIO_FS=y

# 串口控制台（QEMU）
CONFIG_SERIAL_8250=y
CONFIG_SERIAL_8250_CONSOLE=y

# ACPI 电源管理
CONFIG_ACPI=y
CONFIG_PM=y
```

### GPU 直通支持（可选）

```kconfig
CONFIG_IOMMU_SUPPORT=y
CONFIG_IOMMUFD=y
CONFIG_VFIO=y
CONFIG_VFIO_PCI=y
CONFIG_INTEL_IOMMU=y
```

### 文件系统

```kconfig
# FUSE/virtiofs 根文件系统 (必需)
CONFIG_FUSE_FS=y
CONFIG_VIRTIO_FS=y

# 其他禁用的文件系统
# CONFIG_BTRFS_FS is not set
# CONFIG_XFS_FS is not set
# CONFIG_EXT4_FS is not set

# 临时文件系统和虚拟文件系统
CONFIG_EROFS_FS=y
CONFIG_TMPFS=y
CONFIG_PROC_FS=y
CONFIG_SYSFS=y
```

## 构建流程

### 配置文件管理

源配置文件位于 `/c/epkg/git/libkrunfw/config-libkrunfw_x86_64`，构建时复制到内核目录：

```bash
cp /c/epkg/git/libkrunfw/config-libkrunfw_x86_64 /c/epkg/git/linux/.config
```

### 编译内核

```bash
cd /c/epkg/git/linux
make -j$(nproc)
```

输出：
- `arch/x86/boot/bzImage` - QEMU 使用（约 6.4M）
- `vmlinux` - libkrun 使用（约 24M，未压缩）

## 调试命令

### 检查配置状态

```bash
# 检查禁用项
grep -E "^# CONFIG_(KSM|COMPACTION|TRANSPARENT_HUGEPAGE|IPV6) is not set" \
  /c/epkg/git/linux/.config

# 检查启用的关键配置
grep -E "^CONFIG_(KVM_GUEST|VIRTIO_PCI|SERIAL_8250|ACPI)=" \
  /c/epkg/git/linux/.config
```

### 测试内核

```bash
# QEMU 模式
timeout 9 epkg run --sandbox=vm --vmm=qemu \
  --kernel=/c/epkg/git/linux/arch/x86/boot/bzImage ls /

# libkrun 模式（需要 vmlinux 格式）
epkg run --sandbox=vm --vmm=libkrun \
  --kernel=/c/epkg/git/linux/vmlinux ls /
```

### 常见问题

**QEMU 无串口输出**: 检查 `CONFIG_SERIAL_8250_CONSOLE=y`

**virtiofs 挂载失败**: 检查 `CONFIG_PCI=y` 和 `CONFIG_VIRTIO_PCI=y`

**VM 关机失败**: 检查 `CONFIG_ACPI=y` 和 `CONFIG_PM=y`
