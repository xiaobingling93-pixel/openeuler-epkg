# PKGBUILD case study

## install script percent: 3%

```shell
wfg /c/os/archlinux/community% ls */trunk/PKGBUILD|wc -l
6815
wfg /c/os/archlinux/community% grep install= */trunk/PKGBUILD|wc -l
254
```

## Typical actions in PKGBUILD install=$pkgname.install
1. echo message
2. add user/group
3. add /etc/shells
4. chown/chgrp/chmod, suid/setcap
5. ldconfig, fc-cache, glib-compile-schemas, gtk-update-icon-cache, update-desktop-database, texconfig-sys rehash, ...
6. remove: rm .cache, .pyc

## 声明式 add user/group

### archlinux cases

```
/c/os/archlinux/community/chrony/trunk/PKGBUILD:
source=(https://download.tuxfamily.org/chrony/${pkgname}-${pkgver}.tar.gz
        ...
        chrony.sysusers
        chrony.tmpfiles)
...
  install -Dm 644 "${srcdir}/chrony.sysusers" "${pkgdir}/usr/lib/sysusers.d/chrony.conf"
  install -Dm 644 "${srcdir}/chrony.tmpfiles" "${pkgdir}/usr/lib/tmpfiles.d/chrony.conf"

chrony.sysusers:
    u chrony - "Network Time Protocol" /var/lib/chrony

chrony.tmpfiles:
    d /var/lib/chrony 0755 chrony chrony - -
```

### openEuler cases

系统用户, 文件目录属性, 可以通过以下方式声明, 避免命令式操作.

```
openEuler% ls /usr/lib/sysusers.d
basic.conf  dbus.conf  dnsmasq.conf  systemd.conf

openEuler% ls /usr/lib/tmpfiles.d
colord.conf               legacy.conf      radvd.conf                     systemd.conf
cryptsetup.conf           libselinux.conf  rpcbind.conf                   systemd-nologin.conf
dbus.conf                 lvm2.conf        rpm.conf                       systemd-nspawn.conf
dnf.conf                  man-db.conf      samba.conf                     systemd-tmp.conf
etc.conf                  mdadm.conf       selinux-policy.conf            tmp.conf
gluster.conf              named.conf       setup.conf                     tuned.conf
gvfsd-fuse-tmpfiles.conf  net-snmp.conf    spice-vdagentd.conf            var.conf
home.conf                 openssh.conf     static-nodes-permissions.conf  x11.conf
iptraf-ng.conf            pam.conf         subscription-manager.conf
journal-nocow.conf        portables.conf   sudo.conf

openEuler% man sysusers.d

       sysusers.d - Declarative allocation of system users and groups

openEuler% man tmpfiles.d

       tmpfiles.d - Configuration for creation, deletion and cleaning of volatile and temporary files
```

### sysusers format

- <https://fedoraproject.org/wiki/Changes/Adopting_sysusers.d_format>
- <https://fedoraproject.org/wiki/Changes/RPMSuportForSystemdSysusers>
- <https://www.freedesktop.org/software/systemd/man/latest/sysusers.d.html>
