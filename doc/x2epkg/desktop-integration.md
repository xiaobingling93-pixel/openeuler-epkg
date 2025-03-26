# files

/usr/share/mime/
/usr/share/applications/mimeapps.list

~/.config/mimeapps.list
~/.local/share/applications/
~/.local/share/mime/

# programs

xdg-mime
update-desktop-database
update-mime-database

# env vars

```
export XDG_DATA_DIRS=/opt/$APP/usr/share:${XDG_DATA_DIRS}

% echo $XDG_DATA_DIRS
/nix/store/45jcgz11arc2z49cgkxp236dvglxgdlr-desktops/share:/home/wfg/.nix-profile/share:/nix/profile/share:/home/wfg/.local/state/nix/profile/share:/etc/profiles/per-user/wfg/share:/nix/var/nix/profiles/default/share:/run/current-system/sw/share

% echo $XDG_CONFIG_DIRS
/etc/xdg:/home/wfg/.nix-profile/etc/xdg:/nix/profile/etc/xdg:/home/wfg/.local/state/nix/profile/etc/xdg:/etc/profiles/per-user/wfg/etc/xdg:/nix/var/nix/profiles/default/etc/xdg:/run/current-system/sw/etc/xdg

```

# desktop entries

存放程序的各种入口文件，开发者请按规范将对应的文件放到指定的目录进行打包，安装完成之后系统会自动链接到对应的系统目录。

```
文件夹          说明        软链地址
applications    应用图标    -> /usr/share/applications/
autostart       自启动      -> /etc/xdg/autostart/
services        服务        -> /usr/share/dbus-1/service/
plugins         插件        -> /usr/lib/
icons           图标        -> /usr/share/icons/hicolor/
polkit          工具集      -> /usr/share/polkit-1/actions/
mime            扩展类型    -> /usr/share/mime/packages/
fonts           字体集      -> /usr/share/fonts/truetype/
```

# references
<https://wiki.archlinux.org/title/Desktop_entries>
<https://wiki.archlinux.org/title/XDG_MIME_Applications>
<https://www.vvave.net/archives/how-to-build-a-debian-series-distros-installation-package.html>
