# Homebrew/Linuxbrew 包支持架构设计

## 背景

Homebrew 是 macOS 和 Linux 上流行的包管理器。epkg 需要支持安装和运行 Homebrew 预编译包（bottles），同时保持沙箱隔离特性。

## 核心挑战

1. **固定前缀要求**：Homebrew bottles 是预编译的二进制文件，包含硬编码的 HOMEBREW_PREFIX 路径
2. **跨平台差异**：不同平台使用不同的 HOMEBREW_PREFIX
3. **动态链接器路径**： bottles 依赖特定路径的动态链接器和库
4. **沙箱隔离**：需要在保持隔离的同时允许访问 brew 包

## HOMEBREW_PREFIX 设计

### 平台特定前缀

| 平台 | HOMEBREW_PREFIX |
|------|-----------------|
| Linux | `/home/linuxbrew/.linuxbrew` |
| macOS ARM | `/opt/homebrew` |
| macOS Intel | `/usr/local` |

### 前缀选择理由

- **Linux 使用 `/home/linuxbrew/.linuxbrew`**：
  - 避免写入系统目录（/usr/local 通常需要 root）
  - 与官方 Homebrew on Linux 推荐路径一致
  - 用户 home 目录下可写，无需 sudo

- **macOS ARM 使用 `/opt/homebrew`**：
  - Apple Silicon Mac 的官方推荐路径
  - 与 Intel 版区分

- **macOS Intel 使用 `/usr/local`**：
  - 历史兼容性
  - Intel Mac 的传统路径

## 隔离模式实现

### Env 模式（--isolate=env）

**适用条件**：HOMEBREW_PREFIX 目录在主机上存在且可写

**挂载策略**：
```
$env_root -> //home/linuxbrew/.linuxbrew
```

**实现细节**：
- 将环境根目录绑定挂载到 HOMEBREW_PREFIX
- 跳过标准的 /usr 挂载（brew 包自包含）
- 保留主机的动态链接器访问

**回退机制**：如果 HOMEBREW_PREFIX 不存在，自动回退到 Fs 模式

### Fs 模式（--isolate=fs）

**适用场景**：HOMEBREW_PREFIX 不存在或需要完全隔离

**目录结构**：
```
$env_root/
├── home/
│   └── linuxbrew/
│       └── .linuxbrew -> ../../../../  (相对符号链接指向 env_root)
├── usr/
│   ├── bin/
│   ├── lib/
│   └── lib64/ -> usr/lib64
└── ...
```

**挂载策略**：
1. **不挂载主机 /home**：避免 shadow 环境自身的 home 目录
2. **仅挂载必要目录**：
   - /opt/epkg（只读，用于包操作）
   - epkg 二进制目录（用于 vm-daemon 等）
3. **挂载主机库目录**（用于动态链接）：
   - /lib64 -> $env_root/usr/lib64
   - /lib/x86_64-linux-gnu -> $env_root/usr/lib/x86_64-linux-gnu

**符号链接设计**：
- `home/linuxbrew/.linuxbrew -> ../../../../`
- 从 `.linuxbrew` 向上 4 层到达 `env_root`
- pivot_root 后，该链接指向新的根目录 `/`

### Vm 模式（--isolate=vm）

- 使用虚拟机隔离
- 在 guest 内设置与 Fs 模式相同的目录结构
- 通过 virtiofs 共享必要目录

## ELF 文件处理

### RPATH 重写

**问题**：Homebrew bottles 包含 `@@HOMEBREW_PREFIX@@` 占位符，需要在安装时替换为实际路径。

**重写目标**：
- `@@HOMEBREW_PREFIX@@` -> `/home/linuxbrew/.linuxbrew`
- `@@HOMEBREW_CELLAR@@` -> `/home/linuxbrew/.linuxbrew/Cellar`

**实现位置**：`src/brew_pkg.rs`

**技术细节**：
- 使用 `goblin` 库解析 ELF 文件
- 修改 PT_INTERP 段（动态链接器路径）
- 修改 DT_RPATH/DT_RUNPATH 动态标签
- 原地修改文件内容，保持文件结构

### 重写流程

```rust
1. 读取 ELF 文件内容到 Vec<u8>
2. 使用 goblin::elf::Elf::parse() 解析
3. 提取需要修改的偏移量和长度
4. 替换占位符为实际路径
5. 写回文件
```

## 命令查找

### brew 环境特殊处理

**查找路径**（在 `src/run.rs` 中实现）：
1. `$HOMEBREW_PREFIX/bin/`
2. `$HOMEBREW_PREFIX/libexec/bin/`
3. `$HOMEBREW_PREFIX/Cellar/<pkg>/<version>/bin/`

**路径转换**：
- 在沙箱内，命令路径需要转换为相对于新根的路径
- 例如：`/home/wfg/.epkg/envs/test-brew/bin/tree` -> `/home/linuxbrew/.linuxbrew/bin/tree`（Env 模式）

## 环境创建流程

### 创建时操作

1. **创建目录结构**：
   ```bash
   mkdir -p $env_root/home/linuxbrew
   ```

2. **创建符号链接**（Fs/Vm 模式）：
   ```bash
   ln -s ../../../../ $env_root/home/linuxbrew/.linuxbrew
   ```

3. **创建标准目录**：
   ```bash
   mkdir -p $env_root/usr/bin $env_root/usr/lib $env_root/usr/lib64
   ```

4. **提取 bottle 包**：
   - 下载 .tar.gz 文件
   - 解压到 $env_root
   - 重写 ELF 文件中的占位符

## 运行时行为

### Env 模式运行时

1. 检查 HOMEBREW_PREFIX 是否存在
2. 存在：绑定挂载 $env_root -> HOMEBREW_PREFIX
3. 不存在：回退到 Fs 模式

### Fs 模式运行时

1. 创建必要的库目录
2. 挂载主机库目录（用于动态链接）
3. 挂载 /opt/epkg（只读）
4. pivot_root 到 $env_root
5. 符号链接自动指向新的根目录

## 依赖处理

### glibc 依赖

**问题**：Linux brew bottles 通常不捆绑 glibc，依赖系统提供的 libc.so.6

**解决方案**：
- Fs/Vm 模式：挂载主机的 /lib64 和 /lib/x86_64-linux-gnu
- Env 模式：使用主机的动态链接器

### 可选依赖

- `glibc`：标记为 essential 包（TODO）
- `gcc`：某些编译工具需要
- `libgcc`：C++ 程序需要

## 跨平台注意事项

### Linux 特定

- 使用 `/home/linuxbrew/.linuxbrew` 作为前缀
- 需要处理 ELF RPATH 重写
- 需要挂载主机库目录

### macOS 特定

- 使用 `/opt/homebrew`（ARM）或 `/usr/local`（Intel）
- 不需要 ELF 重写（使用 Mach-O 格式）
- 不需要挂载主机库（系统库路径固定）

## 调试与故障排除

### 常见问题

1. **命令找不到**：
   - 检查 HOMEBREW_PREFIX 符号链接是否正确
   - 验证 env_root/home/linuxbrew/.linuxbrew 是否存在

2. **动态链接失败**：
   - 检查 /lib64 和 /lib/x86_64-linux-gnu 是否正确挂载
   - 验证 ELF 文件的 PT_INTERP 路径

3. **库找不到**：
   - 检查 RPATH 重写是否成功
   - 确认库文件在 $HOMEBREW_PREFIX/lib 中

### 调试命令

```bash
# 查看挂载情况
epkg run -e <env> --isolate=fs -- cat /proc/mounts

# 检查符号链接
epkg run -e <env> --isolate=fs -- ls -la /home/linuxbrew/

# 检查 ELF 文件
readelf -l /path/to/binary | grep interpreter
readelf -d /path/to/binary | grep RPATH
```

## 未来改进

1. **glibc 捆绑**：考虑在 brew 环境中捆绑 glibc，完全消除主机依赖
2. **多架构支持**：支持 ARM64 Linux brew bottles
3. ** bottle 缓存**：优化 bottle 下载和缓存策略
4. **依赖解析**：实现完整的 Homebrew 依赖解析
