混合模式

Embedded 需求背景
- yocto packages太少了，所以用epkg install (converted rpms/debs from other server os)
- 但epkg的环境依赖自包含机制对磁盘空间需求大，希望优化一下，不要严格自包含，复用一些hostos上的命令和库
- 引入兼容性风险，不推荐普通用户/场景使用

possible combinations
- hostos base commands + env extra commands
- hostos libc + env ELF lib/app

hostos one-time setup
- bind mount /usr to /opt/host-usr
  (necessary since elf-loader will mount-and-hide original /usr)

epkg install
- Set(env_packages_to_install) -= Set(hostos_installed_packages) & Set(whitelist_packages)
- hostos_installed_packages =
  - 对rpm/deb系统，直接查询hostos里的rpm/dpkg命令就可以拿到hostos里已安装的包和版本
  - 对yocto，应该也可以查某个系统命令或者数据库文件 
- whitelist_packages =
  - libc (if env version <= hostos version)
  - coreutils
  - bash
  - other known stable packages
- add symlinks $env/usr/** => /opt/host-usr/**
  (in addition to $env/usr/** => $storepath/**)

