# env requirements
- support linux, macos, windows (long term goal)
- support running commands from different OS (register multiple envs)
- support typical development flow: python/C/C++/.. projects
- channel: install packages from multiple repos of the same channel
- scope: a) private; b) public
- location: a) default location; b) any specified dir; c) /
- gc support: all env can be auto found/tracked/managed
- rollback: record generations and switch to them
- GRUB boot to history generations of / env
- register: prepend/append `$env_root/usr/ebin` to $PATH in order of priority, 效果类似于`uv tool`
- activate: a) --stack: append $env to `$EPKG_ACTIVE_ENV` + prepend `$env_root/usr/ebin` to $PATH;
            b) non-stack: deactivate `$EPKG_ACTIVE_ENV` first
- choot: `chroot $env_root; PATH=$env_root/usr/sbin:$env_root/usr/bin`
- paths: auto setup paths on activate / register
  - run time: `PATH MANPATH PYTHONPATH NODE_PATH CLASSPATH GOPATH RUBYLIB PHP_INCLUDE_PATH R_LIBS GHC_PACKAGE_PATH`
  - compile time: `ACLOCAL_PATH PKG_CONFIG_PATH CMAKE_PREFIX_PATH CPATH LIBRARY_PATH`
- `env_vars`: user configured vars to set/unset on activate/deactivate
- import: create env from other's env-$name.yaml
- export: export to env-$name.yaml (EnvConfig + ChannelConfig + installed-packages.json)
- convert to container image
- convert to VM image
- 'base' env: auto created for holding epkg manager files
- 'main' env: auto created as default env
- 3rd party integration: direnv, IDE

# command and options

## `epkg env list [-a|--all-user] [--user USER]`

## `epkg env config [--env ENV_NAME] edit`
## `epkg env config [--env ENV_NAME] get <name>`
## `epkg env config [--env ENV_NAME] set <name> <value>`

Similar to `git config`, edit `$HOME/.epkg/config/envs/$env_name.yaml` config file.

Example:
```
epkg env config set env_var.FOO BAR
```

## `epkg env export [--env ENV_NAME] [--output CONFIG_FILE]`

Export an environment config and installed packages, so that others can
reproduce the same environment via `epkg env create ENV_NAME --config CONFIG_FILE`

params:
- `ENV_NAME`: either given in `--env ENV_NAME`, or default to `EPKG_ACTIVE_ENV`, or default to `main`
- `FILE`: either givn in `--to FILE`, or default to `./env-$ENV_NAME.yaml`; if `FILE` is `-`, print to stdout.

## `epkg env create <ENV_NAME> [--public] [--path ENV_ROOT] [--channel CHANNEL] [--config CONFIG_FILE]`
## `epkg env remove <ENV_NAME>`

## `epkg env register <ENV_NAME> [--priority PRIORITY]`
## `epkg env unregister <ENV_NAME>`

## `epkg env activate <ENV_NAME> [--stack]`
## `epkg env deactivate`

- activate/deactivate always work in pair in some live session.
  So introduce ENV vars `EPKG_SESSION_PATH` and `EPKG_ACTIVE_ENV` to track the status

- all the desired side effects are about changing/restoring ENV vars
- need change parent shell/IDE session's ENV vars
  So `epkg activate` command should
  - show export/unset commands for the caller to eval
  - save deactivate commands to a session shell script for use by the paired `epkg deactivate` command

```
# The below $ORIGIN_PATH refers to the content of $PATH right now
$ epkg activate aaa
=>
# action1: write out export vars, the caller shell should eval lines starting with "; "
echo '; export EPKG_SESSION_PATH="$(mktemp deactivate-XXXXXXXXXX)"' if $EPKG_SESSION_PATH not exist
echo '; export EPKG_ACTIVE_ENV="aaa"'
echo '; export PATH=/path/to/aaa/usr/ebin:$PATH'

# action2: write out deactivate shell for run by later "epkg deactivate"
echo '
; unset EPKG_SESSION_PATH
; unset EPKG_ACTIVE_ENV
; export PATH=$ORIGIN_PATH
# restore env_vars like USER_VAR_TO_SET_ON_ACTIVATE ...
; export USER_VAR_TO_SET_ON_ACTIVATE="ORIGIN_VALUE" (or unset USER_VAR_TO_SET_ON_ACTIVATE)
' > $EPKG_SESSION_PATH-aaa.sh

$ epkg activate bbb --stack
=>
echo '; export EPKG_ACTIVE_ENV="bbb:aaa"'
echo '; export PATH=/path/to/bbb/usr/ebin:/path/to/aaa/usr/ebin:$PATH'
echo '; export USER_VAR_TO_SET_ON_ACTIVATE="FOO"'

echo '
; export EPKG_ACTIVE_ENV="aaa"
; export PATH=/path/to/aaa/usr/ebin:$ORIGIN_PATH
' > $EPKG_SESSION_PATH-bbb.sh

$ epkg deactivate
=> found EPKG_ACTIVE_ENV="bbb:aaa", got/remove first part "bbb"
=> show vars in $EPKG_SESSION_PATH-bbb.sh then remove the file
echo '; export EPKG_ACTIVE_ENV="aaa"'
echo '; export PATH=/path/to/aaa/usr/ebin:$ORIGIN_PATH'

$ epkg deactivate
=> show vars in $EPKG_SESSION_PATH-aaa.sh then remove the file
echo '; unset EPKG_ACTIVE_ENV'
echo '; export PATH=$ORIGIN_PATH'
```

## `epkg history [--env ENV_NAME] [-N]`
## `epkg switch [--env ENV_NAME] <generation-id>|-<generations-to-rollback>`

# dir layout

## envs index

All environments create/managed by epkg can be found in the below places:

- private: `env_base=$HOME/.epkg/envs/$env`
- public: `env_base=/opt/epkg/envs/$USER/$env` (create/managed by $USER, for use by others)

`$env_base` can be
- dir: an environment in default location
- symlink to dir: an environment in user specified dir
- let `$env_root=$(readlink $env_base)`

## `$env_root` dir layout

```
# current FHS view and metadata
$env_root/{usr,bin,lib,etc} # FHS rootfs view, typically symlinks/hardlinks
$env_root/generations/current => 2 # 当前生效环境

# generation records, 按需保留 历史版本
$env_root/generations/1/command.json
$env_root/generations/1/installed-packages.json
$env_root/generations/1/{usr,bin,lib,etc} # optional for history
$env_root/generations/2/command.json
$env_root/generations/2/installed-packages.json

```

Dir `$env_root/generations/N/`
- created by `epkg install`
- must have metadata files: command.json installed-packages.json

FHS files
- put in `$env_root` instead of `$env_root/generations/current` to be more user friendly and efficient
- only keep history records in some past generations for / env, for GRUB boot menu selection, to save inode space

## 'base' env layout

- on `epkg init --store=shared`, auto run `epkg env create base --public`
  (store is shared == base env is public)

- when there are public base env, normal user may still create personal
  base env with explicit command `epkg env create base`

- runtime search path is
  - first try find private base env
  - then try find public base env

Example private base env dir layout:
```
~/.epkg/envs/base/usr/bin/epkg
~/.epkg/envs/base/usr/bin/init -> epkg
~/.epkg/envs/base/usr/bin/x2epkg -> epkg
~/.epkg/envs/base/usr/bin/create-repo -> epkg
~/.epkg/envs/base/usr/bin/elf-loader
~/.epkg/envs/base/usr/src/epkg -> epkg-master
~/.epkg/envs/base/usr/src/epkg-master  # general form: epkg-$version, source code in epkg.git
~/.epkg/envs/base/usr/src/epkg-master/lib/epkg-rc.sh # sourced by .bashrc/.zshrc
```

## ebin/ entry-point executable wrappers

When install an application (e.g. rust) in an epkg env, the below will be setup
in order to run the installed rustc:
```
PATH=~/.epkg/envs/main/usr/ebin:$ORIGIN_PATH

# symlink from $env_root/FHS to epkg store file
~/.epkg/envs/main/usr/bin/rustc -> /opt/epkg/store/0a0tk9nvmydhpebc4jwnwbhzvaz21s74__rust__1.77.0__3.oe2403/fs/usr/bin/rustc

# entry-point wrapper
~/.epkg/envs/main/usr/ebin/rustc

# unmodified ELF binary by x2epkg from RPM
/opt/epkg/store/0a0tk9nvmydhpebc4jwnwbhzvaz21s74__rust__1.77.0__3.oe2403/fs/usr/bin/rustc
```

The `elf-loader` does minimal setup to run the target binary in a "light-weight container",
so the "entry-point" executable wrapper means entry-point to the `$env_root` mount namespace:
```
create mount namespace
mount --bind $env/etc /etc
mount --bind $env/usr /usr
load and run target elf binary in epkg store
```

### when to create wrapper

Typically when users run "epkg install xxx" in an environment, they just want
to run several binaries of xxx, but not all the hundreds of depends (e.g.
coreutils binaries) in the environment. So `epkg install` will
- download/unpack xxx package and all its recursive depends to epkg store
- create FHS under `$env_root` with symlinks to the epkg store files
- create entry-point wrapper in ebin/ for
  - xxx binaries
  - all yyy binaries where yyy.sourcePkg == xxx.sourcePkg

### ELF executable wrappers

rustc example setup:
```
cp ~/.epkg/envs/base/usr/bin/elf-loader ~/.epkg/envs/main/usr/ebin/rustc
binary replace content of env_root="{{SOURCE_ENV_DIR LONG0 LONG1 LONG2 ..." to env_root="~/.epkg/envs/main"
binary replace content of target_elf_path="{{TARGET_ELF_PATH LONG0 LONG1 LONG2 ..." to target_elf_path="/opt/epkg/store/0a0tk9nvmydhpebc4jwnwbhzvaz21s74__rust__1.77.0__3.oe2403/fs/usr/bin/rustc"
```

### shell-bang executable wrappers

To create an executable wrapper for a shell-bang script in `$env_root/usr/bin/`, e.g.

```
~/.epkg/envs/main/bin -> usr/bin
~/.epkg/envs/main/usr/bin/sh -> bash
~/.epkg/envs/main/usr/bin/bash -> /opt/epkg/store/zkkh21atsr5tr518jht2n9ffter5cwgk__bash__5.2.15__9.oe2403/fs/usr/bin/bash
~/.epkg/envs/main/usr/bin/zcat -> /opt/epkg/store/9vk19mfb1qejaf47bmjw5fn97ew8g3k7__gzip__1.12__4.oe2403/fs/usr/bin/zcat
    #!/bin/sh
    ...

# entry-point wrapper
~/.epkg/envs/main/usr/ebin/sh -> bash
~/.epkg/envs/main/usr/ebin/bash
~/.epkg/envs/main/usr/ebin/zcat
    #!~/.epkg/envs/main/usr/ebin/sh
    ...
```

```
cd $env_root  # ~/.epkg/envs/main
shell_bang_line=$(head -n1 usr/bin/zcat)
env_shell_bang_line=$(replace the interpreter path in $shell_bang_line to its env wrapper path)  # ~/.epkg/envs/main/usr/ebin/bash

echo "$env_shell_bang_line" > usr/ebin/zcat
echo "language_specific_exec" >> usr/ebin/zcat
chmod +x usr/ebin/zcat
```

where `language_specific_exec` should transfer execution from one script to
another of the same language within the same interpreter process, avoiding the
overhead of launching a new interpreter:

- shell
```
exec $script_in_store # /opt/epkg/store/9vk19mfb1qejaf47bmjw5fn97ew8g3k7__gzip__1.12__4.oe2403/fs/usr/bin/zcat
```
- python

```
exec(open($script_in_store).read())
```
- ruby
```
load($script_in_store)
```

- lua
```
dofile($script_in_store)
```

# file format

## environment config file

Example `$HOME/.epkg/config/envs/aaa.yaml`
```
name: aaa
env_base: /home/wfg/.epkg/envs/aaa
env_root: content of $(readlink $env_base)
public: false
register_to_path: true
register_priority: 10
channel: openeuler:24.03-lts
repos:
  everything:
env_vars:
    USER_VAR_TO_SET_ON_ACTIVATE: "FOO"
```

# data structure

## EnvConfig

```
pub struct EnvConfig {
    pub name: String,
    pub env_base: String,
    pub env_root: String,

    pub public: bool,

    pub register_to_path: bool,
    pub register_priority: i32,

    pub env_vars: HashMap<String, String>,
}
```

epkg rust should implement `get_env_config(env_name)`:
```
    return self.envs_config[$env_name] if exists
    for file in $HOME/.epkg/config/*.yaml
        env_name = $file basename
        return self.envs_config[$env_name] = load yaml from $file
```

# Major scenes

## 开发者目录环境支持

1. 目录结构

```
# direnv配置文件
~/project-xxx/.envrc
    epkg activate ~/project-xxx/.envs/dev | grep '^; '

# created by: epkg env create project-xxx-dev --path ~/project-xxx/.envs/dev
~/project-xxx/.envs/dev   # 开发环境
~/project-xxx/.envs/prod  # 生产环境

~/project-xxx/{.git,src,Makefile,doc,...} # your upstream project-xxx files
```

2. 环境激活

```
# 进入项目目录自动激活
$ cd ~/project-xxx
(epkg:dev) $ python main.py

# 手动切换环境
$ epkg env activate --stack project-xxx-prod
```

## epkg 的根目录环境支持（类 NixOS 模式）

1. 目录结构设计

- no need create ebin/ entry-point for / env

```
$env_base=/opt/epkg/envs/root/system -> /
$env_root=/

/（根目录）
├── generations/        # 各代系统环境
│    ├── 1/             # 完整系统快照（只读）
│    ├── 2/
│    └── current -> 2   # 当前生效环境
├── opt/epkg
│    └── store/         # 共享包存储
├── etc
├── usr
│    ├── bin/
│    │    ├── bash -> /opt/epkg/store/.../usr/bin/bash
│    │    └── python3.13 -> /opt/epkg/store/.../usr/bin/python3.13
│    └── lib/
└── var
```

2. 关键操作流程

系统创建：
```
epkg init --store=shared
epkg env create system --public --path /
epkg switch system <generation-id>
```

启动管理：

在 /boot/loader/entries 为每个生成创建 GRUB 条目：
```
title epkg OS generation 5
linux /generations/5/boot/vmlinuz
initrd /generations/5/boot/initrd
options root=/dev/sda1 init=/opt/epkg/envs/root/base/usr/bin/init generation=5
```

系统切换：
```
# run in init
epkg switch system <generation-id> if /proc/cmdline has generation=N kernel parameter
exec /sbin/init -> ../lib/systemd/systemd
```

# references
https://docs.conda.org.cn/projects/conda/en/stable/user-guide/tasks/manage-environments.html
https://mamba.readthedocs.io/en/latest/user_guide/concepts.html
https://spack.readthedocs.io/en/latest/environments.html
https://spack.readthedocs.io/en/latest/env_vars_yaml.html
https://nixos.wiki/wiki/Change_root
https://nixos.wiki/wiki/Bootloader

https://nixos.wiki/wiki/Development_environment_with_nix-shell
https://nix.dev/manual/nix/2.28/command-ref/nix-shell.html

https://www.jetbrains.com/help/pycharm/conda-support-creating-conda-virtual-environment.html#create-a-conda-environment
