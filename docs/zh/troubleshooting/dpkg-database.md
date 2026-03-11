# dpkg 数据库问题排查

## 问题：维护者脚本找不到已安装的包

### 症状

```bash
# 维护者脚本输出错误
dpkg-query: package 'emacsen-common' is not installed and no information is available
emacsen-common: dpkg invocation failed
```

### 原因

维护者脚本调用真实的 `dpkg --status`，它读取 `/var/lib/dpkg/status`，
而不是 epkg 的 `installed-packages.json`。

### 排查步骤

```bash
# 1. 检查 dpkg status 文件是否存在
epkg -e <env> run -- ls -la /var/lib/dpkg/status

# 2. 检查包是否在 status 文件中
epkg -e <env> run -- dpkg --status <package>

# 3. 检查包是否在 epkg 中安装
epkg -e <env> info <package>
```

### 解决方案

epkg 应该自动生成 dpkg 数据库。如果出现问题，可以手动触发：

```bash
# 重新安装任何包会触发 dpkg 数据库生成
epkg -e <env> install <any-package>
```

## 问题：正在安装的包不可见

### 症状

在安装过程中，维护者脚本检查正在安装的包：

```bash
# 包 A 的 postinst 检查正在安装的包 B
if dpkg --status package-b > /dev/null 2>&1; then
    # 配置与 B 的集成
fi
```

但检查失败，因为包 B 还没有完全安装。

### 原因

`/var/lib/dpkg/status` 只在安装完成后更新，而不是在运行脚本前。

### 解决方案

epkg 使用 `pending-packages` 机制：

1. 运行脚本前：将正在安装的包追加到 status
2. 脚本执行时：`dpkg --status` 可以看到这些包
3. 安装完成后：重新生成完整的 status

## 问题：dpkg 错误解析 status 文件

### 症状

```bash
dpkg-query: error: parsing file '/var/lib/dpkg/status' near line 18 package 'gcc':
 duplicate value for 'Depends' field
```

### 原因

status 条目中有重复字段。这通常是因为控制文件中已有 `Depends`，
但 epkg 又添加了一个。

### 排查

```bash
# 查看 status 文件中问题包的内容
epkg -e <env> run -- grep -A 30 "^Package: gcc$" /var/lib/dpkg/status
```

### 解决方案

生成 status 条目时，跳过控制文件中已有的字段：

```rust
// 跟踪已看到的字段
let mut seen_depends = false;

for line in control.lines() {
    if line.starts_with("Depends:") {
        seen_depends = true;  // 标记已处理
    }
    // ...
}

// 只有控制文件没有时才添加
if !seen_depends && !info.depends.is_empty() {
    entry.push_str(&format!("Depends: ...\n"));
}
```

## 调试技巧

### 查看 dpkg 数据库内容

```bash
# 列出所有已安装的包
epkg -e <env> run -- dpkg -l

# 查看特定包详情
epkg -e <env> run -- dpkg --status gcc

# 查看 info 目录
epkg -e <env> run -- ls -la /var/lib/dpkg/info/ | head -20
```

### 验证符号链接

```bash
# 检查 info 文件是否正确链接
epkg -e <env> run -- sh -c '
  ls -la /var/lib/dpkg/info/gcc.* 2>/dev/null || echo "No gcc info files"
'
```

### 手动验证 status 格式

```bash
# 解析 status 文件
epkg -e <env> run -- awk '
  /^Package:/ { pkg = $2 }
  /^Status:/ { status = $2 " " $3 " " $4 }
  /^$/ { print pkg ": " status }
' /var/lib/dpkg/status | head -20
```

## 已知限制

1. **文件列表缺失**：`{pkg}.list` 文件不生成，`dpkg -L` 不工作
   - 替代方案：使用 `epkg run -- find /store/path -type f`

2. **版本约束简化**：Depends 字段中的版本约束被简化
   - 不影响大多数场景

3. **Conda 环境**：dpkg 数据库不适用于 Conda 环境
   - Conda 使用完全不同的包管理机制