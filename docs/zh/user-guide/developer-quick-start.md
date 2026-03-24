# 开发者快速入门

本指南将帮助您从源代码构建 epkg，并在开发环境中运行您的第一个命令。

## 1. 安装构建依赖

```bash
git clone https://atomgit.com/openeuler/epkg
cd epkg
make dev-depends
```

## 2. 构建并安装 epkg

```bash
make
target/debug/epkg self install
```

然后启动一个新的 shell（或 `source ~/.bashrc`）以更新 PATH。

## 3. 开发循环

在 `self install` 之后，epkg 二进制文件位于 `~/.epkg/envs/self/usr/bin/epkg`。后续的 `make` 可以将构建的二进制文件复制到那里，以便您可以运行：

```bash
make
epkg --version
```

使用 `make [static]` 然后 `epkg ...` 进行快速的编辑-测试循环，无需重新安装。

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
