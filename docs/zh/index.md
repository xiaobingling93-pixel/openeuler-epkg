# epkg 文档

这是 **epkg**（轻量级多源 Linux 包管理器）的文档索引。有关高级概述、使用场景和快速入门，请参阅 [README](../../README.zh.md)。

## 文档导览

### 用户指南

- **[入门指南](user-guide/getting-started.md)** — 如何安装 epkg 并运行您的第一个命令（创建环境、安装包）。
- **[开发者快速入门](user-guide/developer-quick-start.md)** — 从源代码构建、开发循环、测试。
- **[环境管理](user-guide/environments.md)** — 环境生命周期：创建、删除、注册、取消注册、激活、停用、路径、配置，以及 `--root` / `.eenv` 发现。
- **[包操作](user-guide/package-operations.md)** — 安装、删除、更新、升级、列表、搜索、信息查询及示例输出。
- **[高级用法](user-guide/advanced.md)** — 在环境中运行命令（`run`）、服务管理、历史/恢复、垃圾回收、转换/解压/哈希、busybox。

### 参考文档

- **[命令参考](reference/commands.md)** — 完整的命令和全局选项列表（来自 `epkg help`）。
- **[软件源](reference/repositories.md)** — channel 列表和 `epkg repo list` 输出。
- **[路径和布局](reference/paths.md)** — 用户与 root 安装路径及目录布局。

### 其他

- **设计文档** — [design-notes/](../design-notes/) — 布局、仓库数据、包格式、构建系统等。
- **包格式** — [epkg-format.md](../epkg-format.md) — epkg 二进制包格式。
- **x2epkg** — [x2epkg/](../x2epkg/) — RPM/DEB 转换和桌面集成。
