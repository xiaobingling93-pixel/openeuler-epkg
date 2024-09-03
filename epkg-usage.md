# epkg 使用指南

## 介绍
本文介绍EPKG包管理器工作环境如何初始化，以及基本功能如何使用。本文涉及操作结果示例均以非root用户为例。

## 安装教程
方法1：
    // epkg下载脚本
    cd /tmp/
    curl -O https://eulermaker.compass-ci.openeuler.openatom.cn/api/ems1/repositories/epkg/epkg_downloader.sh | bash -
    bash // 重新执行.bashrc, 获得新的PATH
    epkg install $package
    
方法2：
    sudo yum install epkg
    epkg init
    bash // 重新执行.bashrc, 获得新的PATH
    epkg install $package

## EPKG包管理器使用说明
Usage:

    epkg install [--env ENV] PACKAGE （开发中...）
    epkg remove [--env ENV] PACKAGE （开发中...）
    epkg upgrade [PACKAGE]

    epkg search PACKAGE
    epkg list

    epkg env list
    epkg [env] create|remove ENV
    epkg [env] activate ENV
    epkg [env] deactivate
    epkg [env] enable|disable ENV
    epkg [env] history ENV （开发中...）
    epkg [env] rollback ENV （开发中...）

### 查询已安装软件
功能描述：

    查询当前所在环境中，已经安装的软件包信息
命令：

    epkg list

返回示例：

    [small_leek@19e784a5bc38 bin]# epkg list
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
功能描述：

    基于当前所在环境已挂载repo源，查询指定软件信息

命令：

    epkg search ${package_name}

返回示例：

    [small_leek@19e784a5bc38 bin]# epkg search vim
    Updating and loading repositories:
    Repositories loaded.
    Matched fields: name, summary
    vim-X11.aarch64: Vim for the X Window System i.e.gvim
    vim-common.aarch64: This contains some common files to use vim editor.
    vim-enhanced.aarch64: This is a package containing enhanced vim editor.
    vim-filesystem.noarch: The vim filesystem.
    vim-minimal.aarch64: This package provides the basic and minimal functionalities of vim editor.

### 安装软件
功能描述：

    在当前所在环境安装软件（建议操作前确认当前所在环境）

命令：

    epkg install ${package_name}

返回示例：

    [small_leek@19e784a5bc38 bin]# epkg install dos2unix
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
功能描述：

    列出当前epkg所有环境（$EPKG_ENVS_ROOT目录下），及当前处于哪个环境

命令：

    epkg env list

返回示例：

    [small_leek@19e784a5bc38 bin]# epkg env list
    Available environments(sort by time):
    w1
    main
    common
    You are in [main] now

 
### 创建环境
功能描述：

    创建新环境（创建成功后，默认激活新环境，即切换进新环境；但是不全局使能）

命令：

    epkg create ${env_name}

返回示例：

    [small_leek@b0e608264355 bin]# epkg create work1
    YUM --installroot directory structure created successfully in: /root/.epkg/envs/work1/profile-1
    Environment 'work1' added to PATH.
    Environment 'work1' activated.
    Environment 'work1' created.
    re-open shell

### 激活环境
功能描述：

    激活指定环境，刷新EPKG_ENV_NAME和RPMDB_DIR（用于安装软件至指定环境时，指向--dbpath），刷新PATH，包含指定环境及common环境，并将指定环境设为第一优先级

命令：

    epkg activate ${env_name}

返回示例：

    [small_leek@9d991d463f89 bin]# epkg activate main
    YUM --installroot directory structure created successfully in: /root/.epkg/envs/main/profile-1
    Environment 'main' activated.
    re-open shell

### 取消激活环境
功能描述：

    取消激活指定环境，刷新EPKG_ENV_NAME和RPMDB_DIR，刷新PATH，默认指向main环境

命令：

    epkg deactivat ${env_name}

返回示例：

    [small_leek@398ec57ce780 bin]# epkg deactivate w1
    Environment 'w1' deactivated.
    re-open shell


### 使能环境
功能描述：

    使能指定环境，持久化刷新PATH，包含epkg所有已使能环境，并将指定环境设为第一优先级

命令：

    epkg enable ${env_name}

返回示例：

    [small_leek@5042ae77dd75 bin]# epkg enable lkp
    add common to path
    add main to path
    add xsl to path
    add lkp to path
    Environment 'lkp' added to PATH.
    re-open shell

### 取消使能环境
功能描述：

    去使能指定环境，持久化刷新PATH，包含除指定环境外的epkg所有已使能环境

命令：

    epkg disable ${env_name}

返回示例：

    [small_leek@69393675945d /]# epkg disable w4
    Warning: Don't try to disable current env!
    Warning: you are trying to disable current env!
    sure to continue? (y: continue, others: exit)
    y
    Environment 'w4' removed from PATH.
    re-open shell