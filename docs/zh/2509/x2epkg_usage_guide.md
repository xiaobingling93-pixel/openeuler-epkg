# 使用x2epkg工具

## 介绍

本文介绍x2epkg软件包转换工具的简单使用。本文涉及操作结果示例均以root用户为例，无需安装。支持将rpm/deb/pkg.tar.zst安装包转换为[epkg](epkg_package_manager_usage_guide.md)安装包。

## 下载

```bash
git clone https://gitee.com/openeuler/epkg.git
```

## 快速上手

下载epkg仓库后直接进入x2epkg目录下使用。

```shell
cd epkg/x2epkg
./x2epkg.sh --help
```

## 参数解析

```txt
Usage:
    ./x2epkg xxx.rpm                                # 单个rpm包的转换
    ./x2epkg xxx.deb                                # 单个debian包的转换
    ./x2epkg xxx.pkg.tar.zst                        # 单个archlinux包的转换
    ./x2epkg file_path/*.rpm                        # 多个安装包的转换，同样适用于deb和pkg.tar.zst文件
    ./x2epkg xxx.rpm --out-dir PATH                 # 加上out-dir参数可以指明输出目录，如果不加则输出到包的同级目录下
```

## 运行案例

```shell
  cd /root
  wget https://mirrors.huaweicloud.com/archlinux/core/os/x86_64/fakeroot-1.37.1-1-x86_64.pkg.tar.zst
  cd epkg/x2epkg
  ./x2epkg.sh /root/fakeroot-1.37-1-x86_64.pkg.tar.zst
  ls /root/store/4t/4tmfsi5yikkq32rrulae6oi6u4txr5zu__fakeroot__1.37.1__1.epkg   # 查看生成结果
```
