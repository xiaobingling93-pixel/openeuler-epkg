# 开发者快速入门

本指南将帮助您从源代码构建 epkg，并在开发环境中运行您的第一个命令。

## 1. 安装构建依赖

**支持的平台和主机操作系统：**
- Linux (x86_64, aarch64, riscv64, loongarch64): Debian/Ubuntu, openEuler, Fedora, Archlinux
- macOS (x86_64, aarch64) with homebrew
- Windows (x86_64)，在 WSL2 Debian/Ubuntu 中

```bash
git clone https://atomgit.com/openeuler/epkg
cd epkg
make dev-depends
```

## 2. 构建并安装 epkg

### Linux / macOS

```bash
make
target/debug/epkg self install
```

然后启动一个新的 shell（或 `source ~/.bashrc` / 重启终端）以更新 PATH。

### Windows

```bash
make cross-windows  # 或：cross-windows-release
target/debug/epkg.exe self install
```

在 WSL2 中，您可以直接运行 Windows .exe 文件，无需 Wine 或单独的 Windows 机器。

调试环境变量设置：
```
export WSLENV=EPKG_DEBUG_LIBKRUN/p:RUST_LOG/p
export EPKG_DEBUG_LIBKRUN=1
export RUST_LOG=debug
target/debug/epkg.exe ...
```

或更灵活的方式：
```
/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe -Command "
  \$env:RUST_LOG='trace'
  \$env:LIBKRUN_WINDOWS_VERBOSE_DEBUG='1'
  C:\Users\epkg\.epkg\envs\self\usr\bin\epkg.exe run -e alpine ls /proc
  Write-Host 'Exit code:' \$LASTEXITCODE
"
```

### 从 Debian 构建发布版本

我们从 Debian Linux 构建和发布 epkg[.exe]。

**构建：**
```bash
make crossdev-depends   # 只需运行一次
make release-all
```

生成的二进制文件位于 `target/<triple>/release/epkg[.exe]`，链接到 `dist/` 并在那里计算 sha256。

## 3. 开发循环

在 `self install` 之后，epkg 二进制文件位于 `~/.epkg/envs/self/usr/bin/epkg`。后续的 `make` 可以将构建的二进制文件复制到那里，以便您可以运行：

```bash
make
epkg --version
```

使用 `make` 然后 `epkg ...` 进行快速的编辑-测试循环，无需重新安装。

## 4. 尝试一个 channel

创建一个环境并安装一个包：

```bash
export os=alpine   # 或 openeuler, fedora, debian, ubuntu, archlinux, conda
epkg env create $os -c $os
epkg -e $os install bash
epkg -e $os run bash
```

## 测试

- **单元测试** — 树内的 `.rs` 测试：`cargo test`（或使用项目的测试运行器）。
- **tests/solver** — 解析器测试。
- **tests/lua** — Lua 测试。
- **tests/busybox** — BusyBox 小程序测试和上游测试套件集成。
- **tests/e2e** — 端到端脚本位于 `tests/e2e/cases/`（如 bare-rootfs、export-import、history-restore、public-multi-user、env-register-activate、install-remove-upgrade）。从 `tests/e2e/` 运行（请参阅 [tests/e2e/README.md](../../../tests/e2e/README.md)）。

## 另请参阅

- [入门指南](getting-started.md) — 用户安装和第一步
- [命令参考](../reference/commands.md) — 所有命令和选项