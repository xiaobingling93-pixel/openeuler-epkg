# 使用epkg包管理器

epkg 是 openEuler 提供的一款轻量级、可扩展的软件包管理工具，支持跨OS的软件管理、环境隔离、多源协同等能力。本文档为 [openEuler epkg 项目](https://gitee.com/openeuler/epkg) 编写的用户使用文档，内容分为四个部分：

1. 快速上手
2. 全局命令
3. 环境管理命令
4. 软件包管理命令

## 1. 快速上手

### 场景1：安装软件包

下载epkg安装脚本：

```bash
[root@51bc2f1c8444 /]# wget https://repo.oepkgs.net/openeuler/epkg/rootfs/epkg-installer.sh
```

执行epkg安装脚本：

```bash
[root@51bc2f1c8444 /]# bash epkg-installer.sh

Source URL: https://repo.oepkgs.net/openeuler/epkg/rootfs/
Destination: /root/.cache/epkg/downloads/epkg

Downloading epkg-aarch64.sha256 ...
############################################################################################################################################################################################################### 100.0%
Downloading epkg-aarch64 ...
############################################################################################################################################################################################################### 100.0%
epkg-aarch64: OK

Installation mode: shared (system-wide) # root用户为shared
Store mode: auto
         ____  _  ______
   ____ |  _ \| |/ / ___|
  ( ___)| |_) | ' / |  _
   )__) |  __/| . \ |_| |
  (____)|_|   |_|\_\____|
Downloading epkg source code from https://gitee.com/openeuler/epkg/repository/archive/master.tar.gz
Downloading elf-loader from https://repo.oepkgs.net/openeuler/epkg/rootfs/
...
=================================================
              Installation Complete
=================================================
Usage:
  epkg search [pattern]  - Search for packages
  epkg install [pkg]     - Install packages
  epkg remove [pkg]      - Remove packages
  epkg list              - List packages
  epkg update            - Update repo data
  epkg upgrade           - Upgrade packages
  epkg --help            - Show detailed help
```

初始化epkg：

```bash
# 因为修改了bashrc，执行bash是为了刷新PATH
[root@51bc2f1c8444 /]# bash
```

使用 epkg 安装公开仓库中的软件包：

```bash
epkg install <pkg-name>
```

示例：

```bash
[root@51bc2f1c8444 ~]# epkg install htop
Packages to be freshly installed:
# depend_depth   package
0                diffutils__1:3.10-4__arm64
0                gzip__1.13-1__arm64
...
2                libsigsegv2__2.14-1+b2__arm64
3                gcc-14-base__14.2.0-19__arm64
3                libaudit-common__1:4.0.2-2__all
3                readline-common__8.2-6__all

0 upgraded, 66 newly installed, 0 to remove and 0 not upgraded.
Need to get 25.1 MB/25.1 MB of archives.
After this operation, 117.8 MB of additional disk space will be used.

Do you want to continue? [Y/n] y
[00:00:00] [==========] 0 B/s        (0s) Downloaded /opt/epkg/cache/downloads/debian/pool/main/c/coreutils/coreutils_9.7-3_arm64.deb
[00:00:00] [==========] 0 B/s        (0s) Downloaded /opt/epkg/cache/downloads/debian/pool/main/o/openssl/libssl3t64_3.5.0-2_arm64.deb
....
Adding 'diversion of /lib/aarch64-linux-gnu/libhistory.so.8 to /lib/aarch64-linux-gnu/libhistory.so.8.usr-is-merged by libreadline8t64'
...
update-alternatives: using /usr/bin/which.debianutils to provide /usr/bin/which (which) in auto mode
update-alternatives: using /usr/share/man/man7/bash-builtins.7.gz to provide /usr/share/man/man7/builtins.7.gz (builtins.7.gz) in auto mode
update-alternatives: using /usr/sbin/rmt-tar to provide /usr/sbin/rmt (rmt) in auto mode
update-alternatives: using /bin/more to provide /usr/bin/pager (pager) in auto mode
Exposed package commands to /root/.epkg/envs/t1/usr/ebin:
htop__3.4.1-4__arm64                 htop
Installation successful - Total packages: 66, ebin packages: 1
```

验证安装情况：

```bash
[root@51bc2f1c8444 /]# which htop
/root/.epkg/envs/main/usr/ebin/htop
[root@51bc2f1c8444 /]# htop
```

### 场景2：指定环境channel，安装软件包

epkg 支持环境隔离安装，可在指定的环境设置 channel 下拉取和管理软件。

创建一个 repo 配置：

```bash
[root@51bc2f1c8444 channel]# epkg env create t2 -c fedora
Creating environment 't2' in /root/.epkg/envs/t2
```

激活环境：

```bash
[root@51bc2f1c8444 channel]# epkg env activate t2
export EPKG_SESSION_PATH="/tmp/deactivate-2794-9b8beb08"
# Activate environment 't2'
export EPKG_ACTIVE_ENV=t2
export PATH="/root/.epkg/envs/t2/usr/ebin:/root/.epkg/envs/main/usr/ebin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
```

在该环境中安装软件：

```bash
[root@51bc2f1c8444 channel]# epkg install gcc
```

运行已安装程序（环境隔离）：

```bash
[root@51bc2f1c8444 ~]# epkg run gcc -- --version
gcc (GCC) 15.1.1 20250521 (Red Hat 15.1.1-2)
Copyright (C) 2025 Free Software Foundation, Inc.
This is free software; see the source for copying conditions.  There is NO
warranty; not even for MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.
```

验证软件包：

```bash
[root@51bc2f1c8444 ~]# which gcc
/root/.epkg/envs/t2/usr/ebin/gcc
[root@51bc2f1c8444 ~]# cat hello.c
#include <stdio.h>

int main(){
    printf("hello world!\n");
    return 0;
}
[root@51bc2f1c8444 ~]# gcc hello.c -o hello
[root@51bc2f1c8444 ~]# ./hello
hello world!
```

### 场景3：实现多OS软件组合使用

epkg支持通过创建并注册多个环境，实现在同一个bash中运行不同OS下的软件。

```bash
[root@51bc2f1c8444 /]# epkg env list
Environment      Type        Owner       Status
-------------------------------------------------------
main             private                 registered
```

分别创建两个环境，指定不同的channel。

```bash
# t1这里使用了默认的channel debian
[root@51bc2f1c8444 /]# epkg env create t1
Creating environment 't1' in /root/.epkg/envs/t1
# t1这里使用了channel fedora
[root@51bc2f1c8444 /]# epkg env create t2 -c fedora
Creating environment 't2' in /root/.epkg/envs/t2
```

指定环境，安装软件。

```bash
# 指定t1环境，安装gcc
[root@51bc2f1c8444 /]# epkg install -e t1 gcc
Packages to be freshly installed:
# depend_depth   package
0                init-system-helpers__1.68__all
0                sed__4.9-2+b1__arm64
0                gzip__1.13-1__arm64
0                debianutils__5.23.1__arm64
0                util-linux__2.41-5__arm64
0                gcc__4:14.2.0-1__arm64
0                hostname__3.25__arm64
0                libc-bin__2.41-9__arm64
...
update-alternatives: using /usr/sbin/rmt-tar to provide /usr/sbin/rmt (rmt) in auto mode
update-alternatives: using /usr/share/man/man7/bash-builtins.7.gz to provide /usr/share/man/man7/builtins.7.gz (builtins.7.gz) in auto mode
update-alternatives: using /usr/bin/which.debianutils to provide /usr/bin/which (which) in auto mode
update-alternatives: using /bin/more to provide /usr/bin/pager (pager) in auto mode
Exposed package commands to /root/.epkg/envs/t1/usr/ebin:
gcc__4:14.2.0-1__arm64               c89-gcc c99-gcc
Installation successful - Total packages: 94, ebin packages: 4

# 指定t2环境，安装gcc
[root@51bc2f1c8444 /]# epkg install -e t2 gcc
Packages to be freshly installed:
# depend_depth   package
0                gcc__15.1.1-2.fc42__aarch64
1                bash__5.2.37-1.fc42__aarch64
1                zlib-ng-compat__2.2.4-3.fc42__aarch64
1                libatomic__15.1.1-2.fc42__aarch64
1                cpp__15.1.1-2.fc42__aarch64
1                binutils__2.44-3.fc42__aarch64
1                make__1:4.4.1-10.fc42__aarch64
1                libasan__15.1.1-2.fc42__aarch64
1                gmp__1:6.3.0-4.fc42__aarch64
1                mpfr__4.2.2-1.fc42__aarch64
...
[00:00:00] [==========] 0 B/s        (0s) Downloaded /opt/epkg/cache/downloads/fedora/updates/42/Everything/aarch64/Packages/l/libsemanage-3.8.1-2.fc42.aarch64.rpm
[00:00:00] [==========] 0 B/s        (0s) Downloaded /opt/epkg/cache/downloads/fedora/updates/42/Everything/aarch64/Packages/l/libgcc-15.1.1-2.fc42.aarch64.rpm
[00:00:00] [==========] 0 B/s        (0s) Downloaded /opt/epkg/cache/downloads/fedora/updates/42/Everything/aarch64/Packages/c/crypto-policies-20250707-1.gitad370a8.fc42.noarch.rpm
[00:00:00] [==========] 0 B/s        (0s) Downloaded /opt/epkg/cache/downloads/fedora/releases/42/Everything/aarch64/os/Packages/n/ncurses-base-6.5-5.20250125.fc42.noarch.rpmExposed package commands to /root/.epkg/envs/t2/usr/ebin:
cpp__15.1.1-2.fc42__aarch64          cpp cc1
gcc__15.1.1-2.fc42__aarch64          aarch64-redhat-linux-gcc aarch64-redhat-linux-gcc-15 c89 c99 gcc gcc-ar gcc-nm gcc-ranlib gcov gcov-dump gcov-tool lto-dump collect2 lto-wrapper lto1
Installation successful - Total packages: 91, ebin packages: 8
```

执行`t1`和`t2`环境注册。

```bash
[root@51bc2f1c8444 /]# epkg env register t1
# Registering environment 't1' with priority 10
export PATH="/root/.epkg/envs/t1/usr/ebin:/root/.epkg/envs/main/usr/ebin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
[root@51bc2f1c8444 /]# epkg env register t2
# Registering environment 't2' with priority 10
export PATH="/root/.epkg/envs/t1/usr/ebin:/root/.epkg/envs/main/usr/ebin:/root/.epkg/envs/t2/usr/ebin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

[root@51bc2f1c8444 /]# epkg env list
Environment      Type        Owner       Status
-------------------------------------------------------
main             private                 registered
t1               private                 registered
t2               private                 registered

[root@51bc2f1c8444 /]# epkg env path
export PATH="/root/.epkg/envs/t1/usr/ebin:/root/.epkg/envs/main/usr/ebin:/root/.epkg/envs/t2/usr/ebin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
```

从上面PATH可以看出，包含了`t1`和`t2`两个环境的目录，如果安装不同软件，可以直接查找执行。如果为同一软件，可以通过`epkg run -e \<env>`的方式来执行。

```bash
[root@51bc2f1c8444 /]# epkg run -e t1 gcc -- --version
gcc (Debian 14.2.0-19) 14.2.0
Copyright (C) 2024 Free Software Foundation, Inc.
This is free software; see the source for copying conditions.  There is NO
warranty; not even for MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.

[root@51bc2f1c8444 /]# epkg run -e t2 gcc -- --version
gcc (GCC) 15.1.1 20250521 (Red Hat 15.1.1-2)
Copyright (C) 2025 Free Software Foundation, Inc.
This is free software; see the source for copying conditions.  There is NO
warranty; not even for MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.
```

通过上面的方式，用户可以组合多个OS的命令，在命令行中直接调用。

### 场景4：普通用户安装软件包

epkg支持普通用户执行软件包的安装，软件包将会安装到用户目录下。

下载安装脚本：

```bash
# 在普通用户权限下安装epkg
[duan@51bc2f1c8444 ~]$ wget https://repo.oepkgs.net/openeuler/epkg/rootfs/epkg-installer.sh
--2025-07-16 15:53:50--  https://repo.oepkgs.net/openeuler/epkg/rootfs/epkg-installer.sh
Resolving repo.oepkgs.net (repo.oepkgs.net)... 28.0.4.67
Connecting to repo.oepkgs.net (repo.oepkgs.net)|28.0.4.67|:443... connected.
HTTP request sent, awaiting response... 200 OK
Length: 5491 (5.4K) [application/octet-stream]
Saving to: 'epkg-installer.sh'

epkg-installer.sh                                     100%[=======================================================================================================================>]   5.36K  --.-KB/s    in 0s

2025-07-16 15:53:50 (123 MB/s) - 'epkg-installer.sh' saved [5491/5491]
```

执行安装：

```bash
[duan@51bc2f1c8444 ~]$ bash epkg-installer.sh

Source URL: https://repo.oepkgs.net/openeuler/epkg/rootfs/
Destination: /home/duan/.cache/epkg/downloads/epkg

Downloading epkg-aarch64.sha256 ...
############################################################################################################################################################################################################### 100.0%
Downloading epkg-aarch64 ...
############################################################################################################################################################################################################### 100.0%
epkg-aarch64: OK

Installation mode: private (user-local)
Store mode: auto
         ____  _  ______
   ____ |  _ \| |/ / ___|
  ( ___)| |_) | ' / |  _
   )__) |  __/| . \ |_| |
  (____)|_|   |_|\_\____|
Downloading epkg source code from https://gitee.com/openeuler/epkg/repository/archive/master.tar.gz
Downloading elf-loader from https://repo.oepkgs.net/openeuler/epkg/rootfs/
[00:00:00] [==========] 425 B/s      (0s) Downloaded /home/duan/.cache/epkg/downloads/epkg/elf-loader-aarch64.sha256
[00:00:02] [==========] 0 B/s        (0s) Downloaded /home/duan/.cache/epkg/downloads/epkg/master.tar.gz
[00:00:00] [==========] 258.65 KiB/s (0s) Downloaded /home/duan/.cache/epkg/downloads/epkg/elf-loader-aarch64                                                                                                         Extracting epkg source code to: /home/duan/.epkg/envs/self/usr/src
Creating symlink: /home/duan/.epkg/envs/self/main/usr/ebin/epkg -> /home/duan/.epkg/envs/self/usr/bin/epkg
Creating environment 'self' in /home/duan/.epkg/envs/self
Creating environment 'main' in /home/duan/.epkg/envs/main
# Registering environment 'main' with priority 10
export PATH="/home/duan/.epkg/envs/main/usr/ebin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
Updating shell RC file: /home/duan/.bashrc
Notice: for changes to take effect, close and re-open your current shell.
```

安装软件包：

```bash
[duan@51bc2f1c8444 ~]$ epkg install tree
[00:00:00] [==========] 241.41 KiB/s (0s) Downloaded /home/duan/.cache/epkg/downloads/debian/dists/trixie/Release
[00:00:26] [==========] 348.57 KiB/s (0s) Downloaded /home/duan/.cache/epkg/downloads/debian/dists/trixie/main/binary-arm64/by-hash/SHA256/afcf397fdbd8df208e8b1f26bd05a8146a3ff3c6849e78fea6153df5695c595e
[00:00:07] [==========] 554.97 KiB/s (0s) Downloaded /home/duan/.cache/epkg/downloads/debian/dists/trixie/main/binary-all/by-hash/SHA256/1f25a3ea5ba0520dd04e2c4351064fa3184be5c04172262fde5583e9974ed2eb
[00:00:00] [==========] 129.56 KiB/s (0s) Downloaded /home/duan/.cache/epkg/downloads/debian/dists/trixie/contrib/binary-all/by-hash/SHA256/af50b189a590c24f41bd48e4b7ee6ec8b30ae55317b558694207422e0a5c30fb
[00:00:00] [==========] 172.88 KiB/s (0s) Downloaded /home/duan/.cache/epkg/downloads/debian/dists/trixie/contrib/binary-arm64/by-hash/SHA256/cc404424a1de75b8e39dde868805c54e104cd53a4cdf0abce327965922668987
[00:00:00] [==========] 319.33 KiB/s (0s) Downloaded /home/duan/.cache/epkg/downloads/debian/dists/trixie/non-free/binary-arm64/by-hash/SHA256/0dd7644bf4a904e92e1bfa6915b37dbc39d61517bd9d04fc0c95c54d99654273
[00:00:00] [==========] 104.28 KiB/s (0s) Downloaded /home/duan/.cache/epkg/downloads/debian/dists/trixie/non-free/binary-all/by-hash/SHA256/135cdc51f730fdfac6a953571fbbae1a662f57d4c3a80a8f6c383b382bdea6f9         Packages to be freshly installed:
# depend_depth   package
0                sysvinit-utils__3.14-4__arm64
0                init-system-helpers__1.68__all
0                hostname__3.25__arm64
0                tree__2.2.1-1__arm64
0                coreutils__9.7-3__arm64
0                sed__4.9-2+b1__arm64
0                debianutils__5.23.1__arm64
0                tar__1.35+dfsg-3.1__arm64
...
[00:00:00] [==========] 228.22 KiB/s (0s) Downloaded /home/duan/.cache/epkg/downloads/debian/pool/main/t/tree/tree_2.2.1-1_arm64.deb
[00:00:00] [==========] 284.31 KiB/s (0s) Downloaded /home/duan/.cache/epkg/downloads/debian/pool/main/a/audit/libaudit1_4.0.2-2+b2_arm64.deb
[00:00:00] [==========] 271.99 KiB/s (0s) Downloaded /home/duan/.cache/epkg/downloads/debian/pool/main/g/gcc-14/libgcc-s1_14.2.0-19_arm64.debAdding 'diversion of /lib/aarch64-linux-gnu/libhistory.so.8 to /lib/aarch64-linux-gnu/libhistory.so.8.usr-is-merged by libreadline8t64'
Adding 'diversion of /lib/aarch64-linux-gnu/libhistory.so.8.2 to /lib/aarch64-linux-gnu/libhistory.so.8.2.usr-is-merged by libreadline8t64'
Adding 'diversion of /lib/aarch64-linux-gnu/libreadline.so.8 to /lib/aarch64-linux-gnu/libreadline.so.8.usr-is-merged by libreadline8t64'
Adding 'diversion of /lib/aarch64-linux-gnu/libreadline.so.8.2 to /lib/aarch64-linux-gnu/libreadline.so.8.2.usr-is-merged by libreadline8t64'
Use of uninitialized value in concatenation (.) or string at /usr/share/perl5/Debconf/Config.pm line 23.
Use of uninitialized value in concatenation (.) or string at /usr/share/perl5/Debconf/Config.pm line 23.
update-alternatives: using /usr/bin/which.debianutils to provide /usr/bin/which (which) in auto mode
update-alternatives: using /bin/more to provide /usr/bin/pager (pager) in auto mode
update-alternatives: using /usr/sbin/rmt-tar to provide /usr/sbin/rmt (rmt) in auto mode
update-alternatives: using /usr/share/man/man7/bash-builtins.7.gz to provide /usr/share/man/man7/builtins.7.gz (builtins.7.gz) in auto mode
Exposed package commands to /home/duan/.epkg/envs/main/usr/ebin:
tree__2.2.1-1__arm64                 tree
Installation successful - Total packages: 65, ebin packages: 1
```

测试验证：

```bash
[duan@51bc2f1c8444 ~]$ which tree
/home/duan/.epkg/envs/main/usr/ebin/tree
[duan@51bc2f1c8444 ~]$ tree
.
`-- epkg-installer.sh

1 directory, 1 file
```

## 2. 全局命令

| **命令**                 | **描述**                               |
| ------------------------ | -------------------------------------- |
| epkg self install        | 安装epkg包管理器（安装脚本自动执行） |
| epkg self remove         | 卸载epkg包管理器                       |
| epkg repo list           | 列出所有repo源                         |
| epkg hash                | 计算指定目录的hash                     |
| epkg convert             | 将 rpm/deb/apk/... 转换为epkg包        |
| epkg unpack              | 解epkg包                               |
| epkg run \<pkg> -- [args] | 运行某个包中提供的程序                 |
| epkg help                | 帮助信息                               |
| epkg --version           | 查看 epkg 当前版本                     |

### epkg self install：安装epkg包管理器

此脚本在执行`epkg-installer.sh`脚本时会被默认执行，重复执行会提示`已初始化`。

```bash
[root@51bc2f1c8444 /]#  epkg self install
epkg was already initialized for current user
```

### epkg self remove：卸载epkg包管理器

```bash
[root@51bc2f1c8444 yk65hht5o4hvpmrdwk5m3543owekwa5n__htop__3.4.1-4__arm64]# epkg self remove

=== Epkg Deinitialization Plan (personal) ===

Directories to remove:
  /root/.epkg

Shell configuration files to modify:
  /root/.bashrc
  /root/.cshrc
  /root/.tcshrc

Do you want to continue with deinitialization? [y/N] y
Modified shell configuration: /root/.bashrc
Removing directory: /root/.epkg
Epkg deinitialization completed successfully.
For changes to take effect, close and re-open your current shell.
```

### epkg repo list：列出所有repo源

当前主要支持了debian，ubuntu，fedora，openEuler，alpine，archlinux；其他OS待验证。

```bash
[root@51bc2f1c8444 /]# epkg repo list
--------------------------------------------------------------------------------------------------------------------------------------------
channel              | default version | repos                                         | index_url
--------------------------------------------------------------------------------------------------------------------------------------------
almalinux            | 9               | extras,BaseOS                                 | $mirror/$version/$repo/$arch/os/repodata/repomd.xml
alpine               | 3.22            | community,main                                | $mirror/v$version/$repo/$arch/APKINDEX.tar.gz
anaconda             | latest          | main,free,r                                   | $mirror/pkgs/$repo/linux-$arch/repodata.json.zst
archlinux            | latest          | core,extra,multilib                           | $mirror/$repo/os/$arch/$repo.files.tar.gz
archlinux/arch4edu   | latest          | x86_64,any                                    | $mirror/$arch/$repo.db.tar.xz
armbian              | 24.04           | main,armbian-tools,utils                      | $mirror/dists/$version/Release
artixlinux           | system          | world,community,extra,system                  | $mirror/repos/$version/os/$arch/$version.db.tar.xz
centos               | 10-stream       | baseos,appstream                              | $mirror/$version/$repo/$arch/os/repodata/repomd.xml
centos/cloud         | 9               | extras,cloud-common,cloud-x86_64              | $mirror/centos/$version/cloud/$arch/openstack-$release/repodata/repomd.xml
debian               | 12              | main,contrib,non-free                         | $mirror/debian/dists/$version/Release
debian/elts          | 9               | non-free,contrib,main                         | $mirror/elts/dists/$version/Release
deepin               | 23              | main,contrib,non-free,commercial              | $mirror/deepin/dists/$version/Release
endeavouros          | latest          | endeavouros-testing,endeavouros               | $mirror/repo/$repo/$arch/$repo.db.tar.xz
fedora               | 42              | Everything                                    | $mirror/releases/$version/$repo/$arch/os/repodata/repomd.xml
fedora/rpmfusion     | 42              | nonfree-updates,nonfree,free,free-updates     | $mirror/free/fedora/releases/$version/Everything/$arch/os/repodata/repomd.xml
linuxmint            | 21.3            | backport,main,upstream,import                 | $mirror/packages/dists/$version/Release
manjaro              | stable          | extra,community,core,multilib                 | $mirror/$version/$repo/$arch/$repo.db.tar.xz
mxlinux              | 23              | main,test,ahs                                 | $mirror/mx/repo/dists/$version/Release
openeuler            | 25.03           | EPOL/update/main,everything,EPOL/main,update  | $mirror/openEuler-$VERSION/$repo/$arch/repodata/repomd.xml
opensuse             | 16.0            | non-oss,oss                                   | $mirror/distribution/leap/$version/repo/$repo/$arch/repodata/repomd.xml
raspbian             | 12              | contrib,main,rpi,non-free                     | $mirror/raspbian/dists/$version/Release
rocky                | 9               | BaseOS,extras                                 | $mirror/$version/$repo/$arch/os/repodata/repomd.xml
ubuntu               | 24.04           | multiverse,restricted,universe,main           | $mirror/dists/$version/Release
ubuntu/ros2          | 24.04           | main,universe                                 | $mirror/ubuntu/dists/$version/Release
--------------------------------------------------------------------------------------------------------------------------------------------
```

### epkg hash：计算指定目录的hash

计算目录的hash值，主要用于计算软件包的hash，可以验证软件包是否被修改。

```bash
[root@51bc2f1c8444 ~]# cd /opt/epkg/store/yk65hht5o4hvpmrdwk5m3543owekwa5n__htop__3.4.1-4__arm64
[root@51bc2f1c8444 yk65hht5o4hvpmrdwk5m3543owekwa5n__htop__3.4.1-4__arm64]# epkg hash .
yk65hht5o4hvpmrdwk5m3543owekwa5n
```

### epkg convert：将 rpm/deb/apk/... 转换为epkg包

```bash
[root@51bc2f1c8444 tmp]# epkg convert  --origin-url  "https://repo.openeuler.org/openEuler-24.03-LTS-SP2/OS/aarch64/Packages/" tree-2.1.1-1.oe2403sp2.aarch64.rpm
./pt555455keitr5pvgmivmaixr7zb3e45__tree__2.1.1-1.oe2403sp2__aarch64.epkg

[root@51bc2f1c8444 tmp]# file ./pt555455keitr5pvgmivmaixr7zb3e45__tree__2.1.1-1.oe2403sp2__aarch64.epkg
./pt555455keitr5pvgmivmaixr7zb3e45__tree__2.1.1-1.oe2403sp2__aarch64.epkg: Zstandard compressed data (v0.8+), Dictionary ID: None
```

### epkg unpack：解epkg包

```bash
[root@51bc2f1c8444 tmp]# epkg unpack pt555455keitr5pvgmivmaixr7zb3e45__tree__2.1.1-1.oe2403sp2__aarch64.epkg
/opt/epkg/store/pt555455keitr5pvgmivmaixr7zb3e45__tree__2.1.1-1.oe2403sp2__aarch64
```

### epkg run：运行某个包中提供的程序

实现方式与通过透明容器直接执行命令不同。

run命令相比透明容器，实现了sub_uid, sub_gid的用户映射，支持在环境中执行postinstall等改变环境属主和用户等操作，主要适用于服务类的软件执行。

epkg run会先挂在环境目录，然后执行目录中bin/，sbin/下的命令；实现间接被安装软件包的运行。

```bash
[root@51bc2f1c8444 /]# epkg run htop -- --help
htop 3.4.1
(C) 2004-2019 Hisham Muhammad. (C) 2020-2025 htop dev team.
Released under the GNU GPLv2+.

-C --no-color                   Use a monochrome color scheme
-d --delay=DELAY                Set the delay between updates, in tenths of seconds
-F --filter=FILTER              Show only the commands matching the given filter
-h --help                       Print this help screen
-H --highlight-changes[=DELAY]  Highlight new and old processes
-M --no-mouse                   Disable the mouse
-n --max-iterations=NUMBER      Exit htop after NUMBER iterations/frame updates
-p --pid=PID[,PID,PID...]       Show only the given PIDs
   --readonly                   Disable all system and process changing features
-s --sort-key=COLUMN            Sort by COLUMN in list view (try --sort-key=help for a list)
-t --tree                       Show the tree view (can be combined with -s)
-u --user[=USERNAME]            Show only processes for a given user (or $USER)
-U --no-unicode                 Do not use unicode but plain ASCII
-V --version                    Print version info

Press F1 inside htop for online help.
See 'man htop' for more information.
```

### epkg help：帮助信息

系统帮助命令：

```bash
[root@51bc2f1c8444 /]# epkg help
The EPKG package manager

USAGE: epkg [OPTIONS] <COMMAND>

COMMANDS:
  deinit   Deinitialize epkg installation
  init     Initialize personal epkg dir layout
  env      Environment management
  list     List packages
  info     Show package information
  install  Install packages
  upgrade  Upgrade packages
  remove   Remove packages
  history  Show environment history
  restore  Restore environment to a specific generation
  update   Update package metadata
  repo     Repository management
  hash     Compute binary package hash
  build    Build package from source
  unpack   Unpack package file(s) into a store directory
  convert  Convert rpm/deb/apk/... packages to epkg format
  run      Run command in environment namespace
  search   Search for packages and files
  help     Print this message or the help of the given subcommand(s)

OPTIONS:
  -C, --config <FILE>               Configuration file to use
  -e, --env <ENV>                   Select the environment
      --arch <ARCH>                 Select the CPU architecture
      --dry-run                     Simulated run without changing the system
      --download-only               Download packages without installing
  -q, --quiet                       Suppress output
  -v, --verbose                     Verbose operation, show debug messages
  -y, --assume-yes                  Automatically answer yes to all prompts
  -m, --ignore-missing              Ignore missing packages
      --metadata-expire <SECONDS>   Metadata expiration time in seconds (0=never, -1=always)
      --proxy <URL>                 HTTP proxy URL (e.g., http://proxy.example.com:8080)
      --nr-parallel <NUMBER>        Number of parallel download threads
      --parallel-processing <BOOL>  Enable parallel processing for metadata updates (true/false) [possible values: true, false]
  -h, --help                        Print help
  -V, --version                     Print version
```

命令详细帮助信息 `epkg <COMMAND> -h`。

```bash
[root@51bc2f1c8444 tmp]# epkg install -h
Install packages

Usage: epkg install [OPTIONS] [PACKAGE_SPEC]...

Arguments:
  [PACKAGE_SPEC]...  Package specifications to install

Options:
      --install-suggests       Consider suggested packages as a dependency for installing
      --no-install-recommends  Do not consider recommended packages as a dependency for installing
      --local                  Install packages from local filesystem
      --fs <DIR>               Local filesystem directory to install packages
      --symlink <DIR>          Local symlink directory to install packages
      --ebin                   Install package binaries to ebin/
  -h, --help                   Print help
[root@51bc2f1c8444 /]# epkg env list -h
List all environments

Usage: epkg env list

Options:
  -h, --help  Print help

[root@51bc2f1c8444 /]# epkg env create -h
Create a new environment

Usage: epkg env create [OPTIONS] <ENV_NAME>

Arguments:
  <ENV_NAME>  Name of the new environment

Options:
  -c, --channel <CHANNEL>  Set the channel for the environment
  -P, --public             Usable by all users in the machine
  -p, --path <PATH>        Specify custom path for the environment
  -i, --import <FILE>      Import from config file
  -h, --help               Print help
```

### epkg --version：查看 epkg 当前版本

```bash
[root@51bc2f1c8444 /]# epkg --version
epkg 0.1.0
```

## 3. 环境管理命令

epkg 支持多环境隔离管理，方便进行不同架构/依赖的管理。

| **命令**                  | **描述**                  |
| ------------------------- | ------------------------- |
| epkg env list             | 列出所有环境              |
| epkg env create \<env>     | 创建新环境                |
| epkg env remove \<env>     | 删除指定环境              |
| epkg env register \<env>   | 注册指定环境              |
| epkg env unregister \<env> | 注销指定环境              |
| epkg env activate \<env>   | 激活指定环境              |
| epkg env deactivate       | 退出当前激活环境          |
| epkg env path             | 查看当前env下的PATH       |
| epkg env config           | 查看或配置当前环境        |
| epkg history              | 查看环境历史              |

### epkg env list：列出所有环境

```bash
[root@51bc2f1c8444 /]# epkg env list
Environment      Type        Owner       Status
-------------------------------------------------------
main             private                 registered
```

### epkg env create：创建新环境

```bash
[root@51bc2f1c8444 /]# epkg env create t1
Creating environment 't1' in /root/.epkg/envs/t1
```

### epkg env remove：删除指定环境

```bash
[root@51bc2f1c8444 channel]# epkg env remove t1
# Environment 't1' is not registered.
# Environment 't1' has been removed.
```

### epkg env register：注册指定环境

注册是一个持久性动作，用户退出后重新进入系统，依旧生效。

main环境仍为默认环境。

```bash
[root@51bc2f1c8444 /]# epkg env register t1
# Registering environment 't1' with priority 10
export PATH="/root/.epkg/envs/t1/usr/ebin:/root/.epkg/envs/main/usr/ebin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
```

### epkg env unregister：注销指定环境

```bash
[root@51bc2f1c8444 /]# epkg env unregister t1
export PATH="/root/.epkg/envs/main/usr/ebin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
# Environment 't1' has been unregistered.
```

### epkg env activate：激活指定环境

与注册不同，激活是一次性动作，仅对当前的shell有效，适用于对当前环境有过多操作时候执行。

```bash
[root@51bc2f1c8444 /]# epkg env activate t1
export EPKG_SESSION_PATH="/tmp/deactivate-1465-8d5b532e"
# Activate environment 't1'
export EPKG_ACTIVE_ENV=t1
export PATH="/root/.epkg/envs/t1/usr/ebin:/root/.epkg/envs/main/usr/ebin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
```

### epkg env deactivate：退出当前激活环境

```bash
[root@51bc2f1c8444 /]# epkg env deactivate
unset EPKG_SESSION_PATH
unset EPKG_ACTIVE_ENV

# Deactivate environment 't1'
export PATH="/root/.epkg/envs/main/usr/ebin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
```

### epkg env path：查看当前env下的PATH

```bash
[root@51bc2f1c8444 ~]# epkg env path
export PATH="/root/.epkg/envs/main/usr/ebin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
```

### epkg env config：查看或配置当前环境

```bash
# 配置当前环境
[root@51bc2f1c8444 ~]# epkg env config edit

# 获取env中config的指定key
[root@51bc2f1c8444 ~]# epkg env config get env_root
String("/root/.epkg/envs/main")

# 设置env中config的指定key
[root@51bc2f1c8444 ~]# epkg env config set public false
[root@51bc2f1c8444 ~]# epkg env config get public
Bool(false)
```

### epkg history：查看环境执行历史

```bash
[root@51bc2f1c8444 ~]# epkg history
--------------------------------------------------  main env history  --------------------------------------------------
id  | timestamp                  | action     | new_packages | del_packages | command line
----+----------------------------+------------+--------------+--------------+-----------------------------------------
1   | 2025-07-14 21:44:02 +0800  | create     | 0            | 0            | ./epkg-aarch64 init --store=auto
2   | 2025-07-15 09:06:22 +0800  | install    | 65           | 0            | epkg install tree
3   | 2025-07-15 09:10:51 +0800  | install    | 66           | 0            | /opt/epkg/envs/root/self/usr/bin/epkg install htop

# 查看指定环境历史
[root@51bc2f1c8444 ~]# epkg history -e t1
--------------------------------------------------  t1 env history  --------------------------------------------------
id  | timestamp                  | action     | new_packages | del_packages | command line
----+----------------------------+------------+--------------+--------------+-----------------------------------------
1   | 2025-07-15 09:12:37 +0800  | create     | 0            | 0            | /opt/epkg/envs/root/self/usr/bin/epkg env create t1
2   | 2025-07-15 09:30:04 +0800  | install    | 66           | 0            | /opt/epkg/envs/root/self/usr/bin/epkg install htop
```

### epkg restore：回退环境

```bash
[root@51bc2f1c8444 ~]#  epkg restore 2
--------------------------------------  Rollback information  ---------------------------------------
action | hash                             | pkg                  | version    | release | dist
-------+----------------------------------+----------------------+------------+---------+------------
Rollback success!

[root@51bc2f1c8444 ~]# epkg history
--------------------------------------------------  main env history  --------------------------------------------------
id  | timestamp                  | action     | new_packages | del_packages | command line
----+----------------------------+------------+--------------+--------------+-----------------------------------------
1   | 2025-07-14 21:44:02 +0800  | create     | 0            | 0            | ./epkg-aarch64 init --store=auto
2   | 2025-07-15 09:06:22 +0800  | install    | 65           | 0            | epkg install tree
3   | 2025-07-15 09:10:51 +0800  | install    | 66           | 0            | /opt/epkg/envs/root/self/usr/bin/epkg install htop
4   | 2025-07-15 09:34:23 +0800  | rollback   | 0            | 2            | /opt/epkg/envs/root/self/usr/bin/epkg restore 2
```

## 4. 软件包管理命令

| **命令**              | **描述**                   |
| --------------------- | -------------------------- |
| epkg install \<pkg>    | 安装指定软件包             |
| epkg remove \<pkg>     | 卸载已安装软件包           |
| epkg list             | 列出当前环境中安装的软件包 |
| epkg search \<keyword> | 搜索可用软件包             |
| epkg info \<pkg>       | 查看某个软件包的详细信息   |
| epkg update           | 更新当前环境中软件包的索引 |
| epkg upgrade          | 升级所有已安装软件包       |

### epkg install：安装软件包

```bash
[root@51bc2f1c8444 ~]# epkg install htop
Packages to be freshly installed:
# depend_depth   package
0                diffutils__1:3.10-4__arm64
0                gzip__1.13-1__arm64
...
2                libsigsegv2__2.14-1+b2__arm64
3                gcc-14-base__14.2.0-19__arm64
3                libaudit-common__1:4.0.2-2__all
3                readline-common__8.2-6__all

0 upgraded, 66 newly installed, 0 to remove and 0 not upgraded.
Need to get 25.1 MB/25.1 MB of archives.
After this operation, 117.8 MB of additional disk space will be used.

Do you want to continue? [Y/n] y
[00:00:00] [==========] 0 B/s        (0s) Downloaded /opt/epkg/cache/downloads/debian/pool/main/c/coreutils/coreutils_9.7-3_arm64.deb
[00:00:00] [==========] 0 B/s        (0s) Downloaded /opt/epkg/cache/downloads/debian/pool/main/o/openssl/libssl3t64_3.5.0-2_arm64.deb
....
Adding 'diversion of /lib/aarch64-linux-gnu/libhistory.so.8 to /lib/aarch64-linux-gnu/libhistory.so.8.usr-is-merged by libreadline8t64'
...
update-alternatives: using /usr/bin/which.debianutils to provide /usr/bin/which (which) in auto mode
update-alternatives: using /usr/share/man/man7/bash-builtins.7.gz to provide /usr/share/man/man7/builtins.7.gz (builtins.7.gz) in auto mode
update-alternatives: using /usr/sbin/rmt-tar to provide /usr/sbin/rmt (rmt) in auto mode
update-alternatives: using /bin/more to provide /usr/bin/pager (pager) in auto mode
Exposed package commands to /root/.epkg/envs/t1/usr/ebin:
htop__3.4.1-4__arm64                 htop
Installation successful - Total packages: 66, ebin packages: 1
```

### epkg remove：卸载已安装软件包

```bash
[root@51bc2f1c8444 ~]# epkg remove htop
Packages to remove:
- htop__3.4.1-4__arm64
- libncursesw6__6.5+20250216-2__arm64

Do you want to continue? [Y/n] y
Removal successful. 2 packages removed.
```

### epkg list：列出当前环境中的软件包

列出当前环境，已经安装的软件包。

```bash
[root@51bc2f1c8444 channel]# epkg list
Installation=Exposed/Installed/Available
| Depth=0-9/Essential/_(not-installed)
|/ Upgrade=Upgradable/ (no-upgrade-available)
||/ Name                           Version                        Arch         Repo               Description
+++-==============================-==============================-============-==================-========================================
IE  base-files                     13.8                           arm64        main               Debian base system miscellaneous files
IE  base-passwd                    3.6.7                          arm64        main               Debian base system master password and group files
IE  bash                           5.2.37-2+b3                    arm64        main               GNU Bourne Again SHell
IE  bsdutils                       1:2.41-5                       arm64        main               basic utilities from 4.4BSD-Lite
....

Total: 65 packages
```

列出当前环境，所有可用软件包。

```bash
[root@51bc2f1c8444 ~]# epkg list --all
Installation=Exposed/Installed/Available
| Depth=0-9/Essential/_(not-installed)
|/ Upgrade=Upgradable/ (no-upgrade-available)
||/ Name                           Version                        Arch         Repo               Description
+++-==============================-==============================-============-==================-========================================
IE  base-files                     13.8                           arm64        main               Debian base system miscellaneous files
IE  base-passwd                    3.6.7                          arm64        main               Debian base system master password and group files
IE  bash                           5.2.37-2+b3                    arm64        main               GNU Bourne Again SHell
IE  bsdutils                       1:2.41-5                       arm64        main               basic utilities from 4.4BSD-Lite
IE  coreutils                      9.7-3                          arm64        main               GNU core utilities
```

### epkg search：搜索可用软件包

```bash
[root@51bc2f1c8444 ~]# epkg search htop
bash - GNU Bourne Again SHell
bash-builtins - Bash loadable builtins - headers & examples
bash-doc - Documentation and examples for the GNU Bourne Again SHell
bash-static - GNU Bourne Again SHell (static version)
bashbro - bash web file browser
bashtop - Resource monitor that shows usage and stats
bashtop - Resource monitor that shows usage and stats
education-common - Debian Edu common basic packages
debian-edu-router-config - Debian Edu Router Configuration
far2l - Linux port of FAR v2
hollywood - fill your console with Hollywood melodrama technobabble
libjs-htmx - framework for performing various javascript actions from html
htop - interactive processes viewer
htop - interactive processes viewer
live-task-extra - Live extra environment support
reform-desktop-minimal - MNT Reform Desktop Environment -- essential components
bash-doc - Documentation and examples for the GNU Bourne Again SHell
bashbro - bash web file browser
bashtop - Resource monitor that shows usage and stats
bashtop - Resource monitor that shows usage and stats
debian-edu-router-config - Debian Edu Router Configuration
hollywood - fill your console with Hollywood melodrama technobabble
live-task-extra - Live extra environment support
reform-desktop-minimal - MNT Reform Desktop Environment -- essential components
```

### epkg info : 查看某个软件包的详细信息

```bash
[root@51bc2f1c8444 ~]# epkg info htop
pkgname: htop
version: 3.4.1-4
summary: interactive processes viewer
homepage: https://htop.dev/
arch: arm64
maintainer: Daniel Lange <DLange@debian.org>
requires: libc6 (>= 2.38), libncursesw6 (>= 6), libtinfo6 (>= 6)
suggests: lm-sensors, lsof, strace
size: 163680
installedSize: 464000
section: utils
priority: optional
location: pool/main/h/htop/htop_3.4.1-4_arm64.deb
sha256: 5197a90c86118a1eb84827fd74af11e5fa2bfef4209a19d1e7a4314b8e1d3085
tag: admin::monitoring, implemented-in::c, interface::text-mode, role::program, scope::utility, uitoolkit::ncurses, use::monitor, works-with::software:running
pkgkey: htop__3.4.1-4__arm64
repodataName: main
status: Available
```

### epkg update：更新软件包仓库索引

```bash
[root@51bc2f1c8444 ~]# epkg update
[00:00:00] [==========] 171.62 KiB/s (0s) Downloaded /opt/epkg/cache/downloads/debian/dists/trixie/Release
[00:00:00] [==========] 409.85 KiB/s (0s) Downloaded /opt/epkg/cache/downloads/debian/dists/trixie/contrib/by-hash/SHA256/802725f5c409ee26e9eeaabbc3e4c6c506c5dc31ff539291ee4a6a5600949c10
[00:00:00] [==========] 360.80 KiB/s (0s) Downloaded /opt/epkg/cache/downloads/debian/dists/trixie/contrib/by-hash/SHA256/7ed7175acc7ecb0cc871281153763d822dc3cebd08f005627d287b47f8ade709
[00:00:03] [==========] 1.39 MiB/s   (0s) Downloaded /opt/epkg/cache/downloads/debian/dists/trixie/main/binary-all/by-hash/SHA256/f0f538d7a60ccbf297928c02ce62ba5f1c83b5b16d758a50e61bc7f162c982f5
[00:00:03] [==========] 3.76 MiB/s   (0s) Downloaded /opt/epkg/cache/downloads/debian/dists/trixie/main/by-hash/SHA256/a64223d03319b716c6ea3afd7b9c48b12379908110ff4d7cbe6c6fd0d2a835f5
[00:00:03] [==========] 10.18 MiB/s  (0s) Downloaded /opt/epkg/cache/downloads/debian/dists/trixie/main/by-hash/SHA256/23b3468940eec0243682eb7aa5ae08ce980c59f50097470c6af72816d4ba8990
[00:00:03] [==========] 2.64 MiB/s   (0s) Downloaded /opt/epkg/cache/downloads/debian/dists/trixie/main/binary-arm64/by-hash/SHA256/c612b4422d638408a607f71148ad26db357ff21024ed0935d141b7488ac63e3f
[00:00:00] [==========] 1.96 MiB/s   (0s) Downloaded /opt/epkg/cache/downloads/debian/dists/trixie/non-free/by-hash/SHA256/70b73b93c1bf62b180712c8c60995b2394eccf36d3ade7dbf81231733b68ece6
[00:00:00] [==========] 176.29 KiB/s (0s) Downloaded /opt/epkg/cache/downloads/debian/dists/trixie/non-free/by-hash/SHA256/225281f565d10df35c9ff31d86ba5b8f3f4426b380bc58d8aa8774ceefe4704b
```

### epkg upgrade：升级所有已安装软件包

```bash
[root@51bc2f1c8444 ~]# epkg upgrade
No packages to upgrade.
```

## 5. 补充说明

- epkg 支持与 openEuler 原生 rpm 包共存。
- epkg有不同的安装模式，支持普通用户直接安装。
- 每个环境下安装的软件互不影响，便于构建测试/隔离实验环境。

## 6. 参考资料

- 项目地址：<https://gitee.com/openeuler/epkg>
- 社区支持：<https://www.openeuler.org/zh/sig/sig-epkg>
