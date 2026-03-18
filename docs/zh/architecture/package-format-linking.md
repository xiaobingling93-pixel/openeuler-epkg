# 包格式与链接策略

不同包格式对动态链接的处理方式不同，这决定了它们能否共享 store 以及所需的链接类型。

## Conda 包

**特点：**
- 二进制文件已内置 `@rpath` 和 `LC_RPATH` 命令
- 运行时通过 rpath 自动定位依赖库
- 包之间通过相对路径解耦

**链接策略：**
- 可使用 Symlink、Hardlink、Reflink
- 多个环境可共享同一个 store 包
- 无需在链接时修改二进制文件

```
store/pkg__1.0__x86_64/fs/lib/libfoo.dylib
    → @rpath/libbar.dylib  (内置 rpath)

env/lib/libbar.dylib → store/dep__1.0__x86_64/fs/lib/libbar.dylib
```

## Brew 包 (Homebrew)

**特点：**
- 二进制包含占位符路径：`@@HOMEBREW_CELLAR@@`、`@@HOMEBREW_PREFIX@@`
- 占位符需要在安装时替换为实际路径
- 上游 Homebrew 替换为 Cellar 内的绝对路径

**链接策略：**
- 必须使用 LinkType::Move
- 文件从 store 移动到 env（不可共享）
- 链接时重写 dylib 路径为 env 绝对路径

```
原始: @@HOMEBREW_PREFIX@@/opt/oniguruma/lib/libonig.5.dylib
重写: /path/to/env/lib/libonig.5.dylib
```

**为什么不能用 DYLD_LIBRARY_PATH：**
- 上游 Homebrew 不使用此方案
- 需要额外配置，增加复杂度
- 可能与其他环境变量冲突

## 设计原则

| 格式 | 能否共享 store | 链接类型 | 是否修改二进制 |
|------|---------------|----------|---------------|
| Conda | 是 | Symlink/Hardlink | 否 |
| Brew | 否 | Move | 是 |
| Epkg/RPM/APK | 是 | Symlink/Hardlink | 否 |

**关键洞察：**
- 包格式的设计决定了链接策略
- "一次构建，到处运行"需要包格式原生支持可重定位性
- Brew 的占位符设计使其无法实现 store 共享