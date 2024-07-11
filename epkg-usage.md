# epkg 使用指南

## 介绍
本文介绍EPKG包管理器工作环境初始化以及基本功能如何使用

## 安装教程
Step 1. 准备一个linux虚拟机/容器环境：

    Note：如果使用容器，建议采用openEuler 2203 LTS SP3版本docker image:

        docker run --privileged -v --name ${自定义容器name} -itd openEuler-22.03-LTS-SP3

Step 2. 安装依赖组件：

    dnf install patchelf findutils tar fakeroot file vim -y

Step 3. 准备epkg下载&初始化脚本：

    cd /home/
    curl -O https://eulermaker.compass-ci.openeuler.openatom.cn/api/ems1/repositories/epkg/downloader.sh

Step 4. 初始化epkg包管理器最小环境：

    sh downloader.sh
    Note: 支持root及非root用户执行


## EPKG包管理器使用说明
Usage:
    
    epkg install [--env ENV] PACKAGE
    epkg remove [--env ENV] PACKAGE（开发中...）
    epkg upgrade [PACKAGE] （开发中...）

    epkg search PACKAGE
    epkg list

    epkg env list
    epkg env create|remove ENV
    epkg env activate ENV
    epkg env enable|disable ENV
    epkg env history ENV
    epkg env rollback ENV

### 查询已安装软件
命令：

    epkg list

返回示例：

    [root@19e784a5bc38 bin]# epkg list
    tzdata-2020a-8.oe1.noarch
    openEuler-gpg-keys-1.0-3.0.oe1.aarch64
    openEuler-repos-1.0-3.0.oe1.aarch64
    openEuler-release-20.03LTS_SP1-38.oe1.aarch64
    setup-2.13.7-1.oe1.noarch
    ncurses-base-6.2-1.oe1.noarch
    ncurses-libs-6.2-1.oe1.aarch64
    libselinux-3.1-1.oe1.aarch64
    filesystem-3.14-1.oe1.aarch64
    basesystem-12-2.oe1.noarch
    bash-5.0-14.oe1.aarch64
    glibc-common-2.28-49.oe1.aarch64
    glibc-2.28-49.oe1.aarch64
    libsepol-3.1-1.oe1.aarch64
    readline-8.0-3.oe1.aarch64
    pcre2-10.35-1.oe1.aarch64
    bzip2-1.0.8-3.oe1.aarch64
    unzip-6.0-45.oe1.aarch64
    zip-3.0-26.oe1.aarch64

### 查询未安装软件
命令：

    epkg search ${package_name}

返回示例：

    [root@19e784a5bc38 bin]# epkg search vim
    Updating and loading repositories:
    Repositories loaded.
    Matched fields: name, summary
    vim-X11.aarch64: Vim for the X Window System i.e.gvim
    vim-common.aarch64: This contains some common files to use vim editor.
    vim-enhanced.aarch64: This is a package containing enhanced vim editor.
    vim-filesystem.noarch: The vim filesystem.
    vim-minimal.aarch64: This package provides the basic and minimal functionalities of vim editor.

### 安装软件
命令：

    epkg install ${package_name}

返回示例：

    [root@19e784a5bc38 bin]# epkg install dos2unix
    Invoking DNF installation...
    Updating and loading repositories:
    Repositories loaded.
    Downloading Packages:
    basesystem-0:12-2.oe1.noarch                   100% |   0.0   B/s |   0.0   B |  00m00s
    >>> Already downloaded
    bash-0:5.0-14.oe1.aarch64                      100% |   0.0   B/s |   0.0   B |  00m00s
    >>> Already downloaded
    filesystem-0:3.14-1.oe1.aarch64                100% |   0.0   B/s |   0.0   B |  00m00s
    >>> Already downloaded
    glibc-0:2.28-49.oe1.aarch64
    >>> Already downloaded
    dos2unix-0:7.4.1-1.oe1.aarch64                 100% |   3.3 MiB/s | 191.8 KiB |  00m00s
    Updating and loading repositories:
    Repositories loaded.
    Debug data written to "/root/.epkg/envs/common/profile-1/debugdata"

    Package                             Arch          Version              Repository         Size
    Installing:
    dos2unix                            aarch64       7.4.1-1.oe1          local              578.6 KiB
    Installing dependencies:
    basesystem                          noarch        12-2.oe1             local              0.0   B
 
### 列出环境列表
命令：

    epkg env list

返回示例：

    [root@19e784a5bc38 bin]# epkg env list
    Available environments:
    common  main
    You are in [main] now

 
### 创建环境
命令：

    epkg env create ${env_name}

返回示例：

    [root@b0e608264355 bin]# epkg env create work1
    YUM --installroot directory structure created successfully in: /root/.epkg/envs/work1/profile-1
    Environment 'work1' added to PATH.
    Environment 'work1' activated.
    Environment 'work1' created.
    re-open shell

### 使能/切换环境
命令：

    epkg env activate ${env_name}

返回示例：

    [root@9d991d463f89 bin]# epkg env activate main
    YUM --installroot directory structure created successfully in: /root/.epkg/envs/main/profile-1
    Environment 'main' activated.
    re-open shell
