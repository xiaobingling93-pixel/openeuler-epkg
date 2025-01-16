# epkg 使用指南

## 介绍
本文介绍EPKG包管理器工作环境如何初始化，以及基本功能如何使用。本文涉及操作结果示例均以非root用户为例。

## 快速上手

下面的实例介绍了安装不同软件包版本的方式

```bash
# curl 方式安装epkg
# 安装时可选user/global安装模式，user模式仅当前安装用户可用，global模式全局用户可用
# 仅root用户可使用global安装模式
wget https://repo.oepkgs.net/openeuler/epkg/rootfs/epkg-installer.sh
bash epkg-installer.sh

# 初始化epkg
# user模式安装：自动初始化
# global模式安装：root用户自动初始化，其他用户需要手动初始化
epkg init
bash // 重新执行.bashrc, 获得新的PATH

# 创建环境1
epkg env create t1
epkg install tree
tree --version
which tree

# 查看repo
[root@vm-4p64g ~]# epkg repo list
------------------------------------------------------------------------------------------------------------------------------------------------------
channel                        | repo            | url
------------------------------------------------------------------------------------------------------------------------------------------------------
openEuler-22.03-LTS-SP3        | OS              | https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-22.03-LTS-SP3/OS/aarch64/
openEuler-24.09                | everything      | https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64/
openEuler-24.09                | OS              | https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/OS/aarch64/
------------------------------------------------------------------------------------------------------------------------------------------------------

# 创建环境2, 指定repo
epkg env create t2 --repo openEuler-22.03-LTS-SP3
epkg install tree 
tree --version
which tree

# 切换回环境1
epkg env activate t1
```

## EPKG包管理器使用说明

```bash
Usage:
    epkg install PACKAGE 
    epkg install [--env ENV] PACKAGE （开发中...）
    epkg remove [--env ENV] PACKAGE （开发中...）
    epkg upgrade [PACKAGE] （开发中...）

    epkg search PACKAGE （开发中...）
    epkg list （开发中...）
    
    epkg env list
    epkg env create|remove ENV
    epkg env activate ENV
    epkg env deactivate ENV
    epkg env register|unregister ENV
    epkg env history ENV （开发中...）
    epkg env rollback ENV （开发中...）
```

软件包安装：
```bash
    epkg env create $env // 创建环境
    epkg install $package // 在环境中安装软件包
    epkg env create $env2 --repo $repo // 创建环境2，指定repo
    epkg install $package // 在环境2中安装软件包
```

软件包构建：
```bash
    epkg build ${yaml_path}/$pkg_name.yaml
```

### 安装软件
功能描述：

    在当前所在环境安装软件（建议操作前确认当前所在环境）

命令：

    epkg install ${package_name}

返回示例：
```
[root@2d785c36ee2e /]# epkg env activate t1
Add common to path
Add t1 to path
Environment 't1' activated.
Environment 't1' activated.
[root@2d785c36ee2e /]# epkg install tree
EPKG_ENV_NAME: t1
Caching repodata for: "OS"
Cache for "OS" already exists. Skipping...
Caching repodata for: "OS"
Cache for "OS" already exists. Skipping...
Caching repodata for: "everything"
Cache for "everything" already exists. Skipping...
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/FF/FFCRTKRFGFQ6S2YVLOSUF6PHSMRP7A2N__ncurses-libs__6.4__8.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/D5/D5BOEFTRBNV3E4EXBVXDSRNTIGLGWVB7__glibc-all-langpacks__2.38__34.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/VX/VX6SUOPGEVDWF6E5M2XBV53VS7IXSFM5__openEuler-repos__1.0__3.3.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/LO/LO6RYZTBB2Q7ZLG6SWSICKGTEHUTBWUA__libselinux__3.5__3.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/EP/EPIEEK2P5IUPO4PIOJ2BXM3QPEFTZUCT__basesystem__12__3.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/2G/2GYDDYVWYYIDGOLGTVUACSBHYVRCRJH3__setup__2.14.5__2.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/HC/HCOKXTWQQUPCFPNI7DMDC6FGSDOWNACC__glibc__2.38__34.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/OJ/OJQAHJTY3Y7MZAXETYMTYRYSFRVVLPDC__glibc-common__2.38__34.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/FJ/FJXG3K2TSUYXNU4SES2K3YSTA3AHHUMB__tree__2.1.1__1.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/KD/KDYRBN74LHKSZISTLMYOMTTFVLV4GPYX__readline__8.2__2.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/MN/MNJPSSBS4OZJL5EB6YKVFLMV4TGVBUBA__tzdata__2024a__2.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/S4/S4FBO2SOMG3GKP5OMDWP4XN5V4FY7OY5__bash__5.2.21__1.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/EJ/EJGRNRY5I6XIDBWL7H5BNYJKJLKANVF6__libsepol__3.5__3.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/TZ/TZRQZRU2PNXQXHRE32VCADWGLQG6UL36__bc__1.07.1__12.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/WY/WYMBYMCARHXD62ZNUMN3GQ34DIWMIQ4P__filesystem__3.16__6.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/KQ/KQ2UE3U5VFVAQORZS4ZTYCUM4QNHBYZ7__openEuler-release__24.09__55.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/HD/HDTOK5OTTFFKSTZBBH6AIAGV4BTLC7VT__openEuler-gpg-keys__1.0__3.3.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/EB/EBLBURHOKKIUEEFHZHMS2WYF5OOKB4L3__pcre2__10.42__8.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/YW/YW5WTOMKY2E5DLYYMTIDIWY3XIGHNILT__info__7.0.3__3.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/E4/E4KCO6VAAQV5AJGNPW4HIXDHFXMR4EJV__ncurses-base__6.4__8.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start install FFCRTKRFGFQ6S2YVLOSUF6PHSMRP7A2N__ncurses-libs__6.4__8.oe2409
start install D5BOEFTRBNV3E4EXBVXDSRNTIGLGWVB7__glibc-all-langpacks__2.38__34.oe2409
start install VX6SUOPGEVDWF6E5M2XBV53VS7IXSFM5__openEuler-repos__1.0__3.3.oe2409
start install LO6RYZTBB2Q7ZLG6SWSICKGTEHUTBWUA__libselinux__3.5__3.oe2409
start install EPIEEK2P5IUPO4PIOJ2BXM3QPEFTZUCT__basesystem__12__3.oe2409
start install 2GYDDYVWYYIDGOLGTVUACSBHYVRCRJH3__setup__2.14.5__2.oe2409
start install HCOKXTWQQUPCFPNI7DMDC6FGSDOWNACC__glibc__2.38__34.oe2409
start install OJQAHJTY3Y7MZAXETYMTYRYSFRVVLPDC__glibc-common__2.38__34.oe2409
start install FJXG3K2TSUYXNU4SES2K3YSTA3AHHUMB__tree__2.1.1__1.oe2409
start install KDYRBN74LHKSZISTLMYOMTTFVLV4GPYX__readline__8.2__2.oe2409
start install MNJPSSBS4OZJL5EB6YKVFLMV4TGVBUBA__tzdata__2024a__2.oe2409
start install S4FBO2SOMG3GKP5OMDWP4XN5V4FY7OY5__bash__5.2.21__1.oe2409
start install EJGRNRY5I6XIDBWL7H5BNYJKJLKANVF6__libsepol__3.5__3.oe2409
start install TZRQZRU2PNXQXHRE32VCADWGLQG6UL36__bc__1.07.1__12.oe2409
start install WYMBYMCARHXD62ZNUMN3GQ34DIWMIQ4P__filesystem__3.16__6.oe2409
start install KQ2UE3U5VFVAQORZS4ZTYCUM4QNHBYZ7__openEuler-release__24.09__55.oe2409
start install HDTOK5OTTFFKSTZBBH6AIAGV4BTLC7VT__openEuler-gpg-keys__1.0__3.3.oe2409
start install EBLBURHOKKIUEEFHZHMS2WYF5OOKB4L3__pcre2__10.42__8.oe2409
start install YW5WTOMKY2E5DLYYMTIDIWY3XIGHNILT__info__7.0.3__3.oe2409
start install E4KCO6VAAQV5AJGNPW4HIXDHFXMR4EJV__ncurses-base__6.4__8.oe2409
```

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

    创建新环境（创建成功后，默认激活新环境，即切换进新环境；但是不全局注册）

命令：

    epkg env create ${env_name}

返回示例：

    [small_leek@b0e608264355 bin]# epkg env create work1
    YUM --installroot directory structure created successfully in: /root/.epkg/envs/work1/profile-1
    Environment 'work1' added to PATH.
    Environment 'work1' activated.
    Environment 'work1' created.

### 激活环境
功能描述：

    激活指定环境，刷新EPKG_ENV_NAME和RPMDB_DIR（用于安装软件至指定环境时，指向--dbpath），刷新PATH，包含指定环境及common环境，并将指定环境设为第一优先级

命令：

    epkg env activate ${env_name}

返回示例：

    [small_leek@9d991d463f89 bin]# epkg env activate main
    Environment 'main' activated

### 取消激活环境
功能描述：

    取消激活指定环境，刷新EPKG_ENV_NAME和RPMDB_DIR，刷新PATH，默认指向main环境

命令：

    epkg env deactivate ${env_name}

返回示例：

    [small_leek@398ec57ce780 bin]# epkg env deactivate w1
    Environment 'w1' deactivated.


### 注册环境
功能描述：

    注册指定环境，持久化刷新PATH，包含epkg所有已注册环境，并将指定环境设为第一优先级

命令：

    epkg env register ${env_name}

返回示例：

    [small_leek@5042ae77dd75 bin]# epkg env register lkp
    EPKG_ACTIVE_ENV: 
    Environment 'lkp' has been registered to PATH.

### 取消注册环境
功能描述：

    去注册指定环境，持久化刷新PATH，包含除指定环境外的epkg所有已注册环境

命令：

    epkg env unregister ${env_name}

返回示例：

    [small_leek@69393675945d /]# epkg env unregister w4
    EPKG_ACTIVE_ENV: 
    Environment 'w4' has been unregistered from PATH.

### 编译epkg软件包
功能描述：

    根据autopkg提供的yaml编译epkg软件包

命令：

    epkg build ${yaml_path}/$pkg_name.yaml

返回示例：

    [small_leek@69393675945d /]#  epkg build /root/epkg/build/test/tree/package.yaml
    pkg_hash: fbfqtsnza9ez1zk0cy23vyh07xfzsydh, dir: /root/.cache/epkg/build-workspace/result
    Compress success: /root/.cache/epkg/build-workspace/epkg/fbfqtsnza9ez1zk0cy23vyh07xfzsydh__tree__2.1.1__0.oe2409.epkg