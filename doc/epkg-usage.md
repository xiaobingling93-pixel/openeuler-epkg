# 介绍

本文介绍EPKG包管理器工作环境如何初始化，以及基本功能如何使用。本文涉及操作结果示例均以非root用户为例。

## 快速上手

下面的实例介绍了安装不同软件包版本的方式

```bash
# curl 方式安装epkg
# 安装时可选user/global安装模式，user模式仅当前安装用户可用，global模式全局用户可用
# 仅root用户可使用global安装模式
wget https://repo.oepkgs.net/openeuler/epkg/rootfs/epkg-installer.sh
bash epkg-installer.sh

# 卸载epkg
wget https://repo.oepkgs.net/openeuler/epkg/rootfs/epkg-uninstaller.sh
bash epkg-uninstaller.sh

# 初始化epkg
# user模式安装：自动初始化
# global模式安装：root用户自动初始化，其他用户需要手动初始化
epkg init
bash // 重新执行.bashrc, 获得新的PATH

# 创建环境t1
epkg env create t1
epkg install tree
tree --version
which tree

# 查看repo
[root@vbox ~]# epkg repo list
EPKG_ACTIVE_ENV:
------------------------------------------------------------------------------------------------------------------------------------------------------
channel                        | repo            | url
------------------------------------------------------------------------------------------------------------------------------------------------------
openEuler-20.03-LTS-SP4        | everything      | https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-20.03-LTS-SP4/everything/
openEuler-22.03-LTS-SP4        | everything      | https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-22.03-LTS-SP4/everything/
openEuler-24.03-LTS            | everything      | https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.03-LTS/everything/
openEuler-24.09                | everything      | https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/
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
    epkg install <package>
    epkg remove [-y] <package>
    epkg upgrade <package> （开发中...）
    
    epkg list <glob-pattern>

    epkg env list
    epkg env create <env_name> [--repo <repo_name>]
    epkg env remove <env_name>
    epkg env activate <env_name> [--pure]
    epkg env deactivate <env_name>
    epkg env register|unregister <env_name>
    epkg env history
    epkg env rollback <history_id>
```

### 安装软件

功能描述：
 在当前activate的环境中安装软件
 若无环境激活，默认安装到main环境中：`epkg env activate <env_name>`

命令：
 `epkg install <package>`

示例：

```bash
[root@vbox ~]# epkg env create t1
EPKG_ACTIVE_ENV:
Environment t1 not exist.
Environment 't1' has been created.
Environment 't1' activated.
[root@vbox ~]# epkg install tree
EPKG_ACTIVE_ENV: t1
Attention: Install success: 8jkd3nbg9td5jnc738yhrz5yjwy5qzha__openEuler-repos__1.0__3.7.oe2403 2px41kqhx9matg9e5zgy36s06qqdn3nj__glibc-all-langpacks__2.38__29.oe24....
[root@vbox ~]# epkg install htop
EPKG_ACTIVE_ENV: t1
Warning: The following packages are already installed and will be skipped:
- 6sgyzx3s7624r0x7rpe4w8642p2d181r__fuse__2.9.9__11.oe2403
- 3gypc46xq6mqd37ya3mhztz2zfkjghw1__libsigsegv__2.14__1.oe2403
....
Attention: Install success: v0wrq5sv9r5znsgtgxkbax24r7f6nq80__htop__3.3.0__1.oe2403
[root@vbox ~]#
```

### 卸载软件

功能描述：
 在当前activate的环境中安装软件
 若无环境激活，默认安装到main环境中：`epkg env activate <env_name>`

命令：
 `epkg remove <package>`

示例：

```bash
[root@vbox ~]# epkg env activate t1
Environment 't1' activated.
[root@vbox ~]# epkg remove htop
Packages to remove:
- v0wrq5sv9r5znsgtgxkbax24r7f6nq80__htop__3.3.0__1.oe2403
Do you want to continue with uninstallation? (y/n):
y
Attention: Remove success: v0wrq5sv9r5znsgtgxkbax24r7f6nq80__htop__3.3.0__1.oe2403
[root@vbox ~]#
```

### 列出环境

功能描述：
 列出当前epkg所有环境，及激活和注册的环境

命令：
 `epkg env list`

示例：

```bash
[root@vbox ~]# epkg env list
EPKG_ACTIVE_ENV: t1
Environment                    Status
-----------------------------------
t1                          activated
main                       registered
```

### 创建环境

功能描述：
 创建新环境，默认激活创建的环境

命令：
 `epkg env create <env_name>`

示例：

```bash
[root@vbox ~]# epkg env create t2
EPKG_ACTIVE_ENV: t1
Environment t2 not exist.
Environment 't2' has been created.
Environment 't2' activated.
[root@vbox ~]# epkg env list
EPKG_ACTIVE_ENV: t2
Environment                    Status
-----------------------------------
t2                          activated
t1
main                       registered
```

### 删除环境

功能描述：
 删除环境

命令：
 `epkg env remove <env_name>`

示例：

```bash
[root@vbox ~]# epkg env remove t2
EPKG_ACTIVE_ENV: t2
Environment t2 exist.
Environment t2 not registered.
Environment t2 has been removed.
[root@vbox ~]# epkg env list
EPKG_ACTIVE_ENV:
Environment                    Status
-----------------------------------
t1
main                       registered
```

### 激活环境

功能描述：
 激活指定的环境，刷新PATH，并将激活环境设为第一优先级

命令：
 `epkg env activate <env_name>`

示例：

```bash
[root@vbox ~]# epkg env activate main
Environment 'main' activated.
[root@vbox ~]# epkg env list
EPKG_ACTIVE_ENV: main
Environment                    Status
-----------------------------------
t1
main             activated|registered
[root@vbox ~]# echo $PATH
/root/.epkg/envs/main/profile-current/usr/app-bin:/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/root/bin
```

### 去激活环境

功能描述：
 去激活环境，去激活当前已激活的环境，刷新PATH

命令：
 `epkg env deactivate`

示例：

```bash
[root@vbox ~]# epkg env activate t1
Environment 't1' activated.
[root@vbox ~]# echo $PATH
/root/.epkg/envs/t1/profile-current/usr/app-bin:/root/.epkg/envs/main/profile-current/usr/app-bin:/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/root/bin
[root@vbox ~]# epkg env deactivate
Environment 't1' deactivated.
[root@vbox ~]# echo $PATH
/root/.epkg/envs/main/profile-current/usr/app-bin:/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/root/bin
```

### 注册环境

功能描述：
 持久化注册指定环境，刷新PATH
 注册的环境在新的shell中依然生效

命令：
 `epkg env register <env_name>`

示例：

```bash
[root@vbox ~]# epkg env register t1
EPKG_ACTIVE_ENV:
Environment t1 exist.
Environment 't1' has been registered.
[root@vbox ~]# echo $PATH
/root/.epkg/envs/t1/profile-current/usr/app-bin:/root/.epkg/envs/main/profile-current/usr/app-bin:/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/root/bin
[root@vbox ~]# epkg env list
EPKG_ACTIVE_ENV:
Environment                    Status
-----------------------------------
t1                         registered
main                       registered
```

### 去注册环境

功能描述：
 持久化去注册指定环境，刷新PATH

命令：
 `epkg env unregister <env_name>`

示例：

```bash
[root@vbox ~]# epkg env unregister t1
EPKG_ACTIVE_ENV:
Environment t1 exist.
Environment t1 had been registered.
Environment 't1' has been unregistered from PATH.
```

### 环境历史

功能描述：
 查看当前激活环境的历史记录

命令：
 `epkg env history`

示例：

```bash
[root@vbox ~]# epkg env activate t1
Environment 't1' activated.
[root@vbox ~]# epkg env history
--------------------------------------------------  t1 env history  --------------------------------------------------
id  | timestamp                  | action     | new_packages | del_packages | command line
----+----------------------------+------------+--------------+--------------+-----------------------------------------
1   | 2025-03-06 20:23:31 +08:00 | install    | 70           | 0            | /opt/epkg/users/public/envs/common/profile-current/usr/bin/epkg install tree
2   | 2025-03-06 20:23:38 +08:00 | install    | 1            | 0            | /opt/epkg/users/public/envs/common/profile-current/usr/bin/epkg install htop
3   | 2025-03-06 20:23:45 +08:00 | remove     | 0            | 1            | /opt/epkg/users/public/envs/common/profile-current/usr/bin/epkg remove htop
```

### 环境回退

功能描述：
    回退激活环境，history_id即epkg env history中查询的id列

命令：
    `epkg env rollback <history_id>`

示例：

```bash
[root@vbox ~]# epkg env rollback 2
Rollback informaton:
New: ["v0wrq5sv9r5znsgtgxkbax24r7f6nq80__htop__3.3.0__1.oe2403"], Del: []
```
