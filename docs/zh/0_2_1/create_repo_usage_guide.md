# 使用create_repo工具

## 介绍

本文介绍create_repo软件包转换工具的简单使用。本文涉及操作结果示例均以root用户为例，无需安装。支持将epkg包目录结构生成本地repo源。

## 下载

```bash
git clone https://gitee.com/openeuler/epkg.git
```

## 快速上手

下载epkg仓库后直接进入create_repo目录下使用。

```shell
cd epkg/create_repo
python3 create_repo.py --help
```

## 前置条件

输入目录是一个包目录结构，结构如下：

```txt
# tree store/
store/
|-- 45
|   `-- 45dqn3vzum73pf7igmmu6zbcxndqqebr__glibc-devel__2.38__29.oe2403.epkg
|-- at
|   `-- atqwhznwbp6lzvur5stgb4jjem6tzllf__redis__4.0.14__6.oe2403.epkg
|-- bt
|   `-- btblk472ob4teixd522qgv6b2c7tk4v4__ncurses-base__5.9+20140118__1ubuntu1.epkg
|-- f7
|   `-- f7ealc3ghq6tdmvrh22l5vlxycgndwam__atlas__3.10.3__10.oe1.epkg
|-- jl
|   `-- jlrwn3rcjb4gbdx4uf77lslxg37rodyf__glibc__2.38__29.oe2403.epkg
|-- kc
|   `-- kclugzl6mtkqeqkgbmpf2vq4kpkeleqb__atlas-devel__3.10.3__10.oe1.epkg
`-- of
    `-- ofdjbysy76gs5tzzgxulaoumgqjfe6d2__audit__4.0.3__1.epkg

8 directories, 7 files
```

## 参数解析

```txt
usage: create_repo.py [-h] -s STORE -c CONFIG

create repo参数

optional arguments:
  -h, --help            查看命令参数使用
  -s STORE, --store STORE
                        输入epkg包仓的store目录
  -c CONFIG, --config CONFIG
                        输入repo清单配置文件的地址
```

## 运行案例

```shell
  cd epkg/x2epkg
  python3 create_repo.py -s /root/store -c /root/config.yaml
  ls /root/repodata  # 查看生成结果，输出所在路径与store仓库所在路径一致
```
