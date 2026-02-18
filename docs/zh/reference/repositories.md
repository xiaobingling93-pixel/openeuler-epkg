# 软件源（channel）

epkg 使用 **channel** 来指代一个发行版和版本（例如 `debian`、`ubuntu`、`alpine`、`fedora`、`openeuler`、`archlinux`、`conda`）。每个 channel 有一个默认版本和一个或多个 **软件源**（例如 main、community、extra）。镜像自动选择；索引 URL 中的占位符 `$mirror` 会被替换为来自 [sources/mirrors.json](https://atomgit.com/openeuler/epkg/tree/master/sources/mirrors.json) 的镜像。

## 列出 channel 和软件源

```bash
epkg repo list
```

输出是一个包含以下列的表格：

- **channel** — channel 名称（可能还有变体，例如 `fedora/rpmfusion`）。
- **default version** — 默认发行版版本（例如 `13`、`3.23`、`latest`）。
- **repos** — 逗号分隔的软件源名称（例如 `main,community`、`Everything`）。
- **index_url** — 索引 URL 的模板；`$mirror`、`$version`、`$arch`、`$repo`（有时还有 `$conda_arch`、`$conda_repofile`、`$app_version` 等）会被替换。

示例（缩写）：

```
--------------------------------------------------------------------------------------------------------------------------------------------
channel              | default version | repos                                         | index_url
--------------------------------------------------------------------------------------------------------------------------------------------
alpine               | 3.23            | community,main                                | $mirror/v$version/$repo/$arch/APKINDEX.tar.gz
archlinux            | latest          | core,multilib,extra                           | $mirror/$repo/os/$arch/$repo.files.tar.gz
archlinux            | latest          | aur                                           | https://aur.archlinux.org/packages-meta-ext-v1.json.gz
conda                | latest          | free,r,main,pro                               | $mirror/pkgs/$repo/$conda_arch/$conda_repofile
conda                | latest          | conda-forge,pytorch,MindSpore                 | $mirror/cloud/$repo/$conda_arch/$conda_repofile
debian               | 13              | Official                                      | $mirror/debian/dists/$version/Release
fedora               | 43              | Everything                                    | $mirror/releases/$version/$repo/$arch/os/repodata/repomd.xml
openeuler            | 25.09           | update,EPOL/update/main,everything,EPOL/main  | $mirror/openEuler-$VERSION/$repo/$arch/repodata/repomd.xml
ubuntu               | 25.10           | Official                                      | $mirror/dists/$version/Release
...
--------------------------------------------------------------------------------------------------------------------------------------------
```

## 使用channel

当您使用 `-c CHANNEL` 创建一个环境时（例如 `epkg env create myenv -c alpine`），该环境将绑定到该channel及其默认版本。包操作（`install`、`update`、`upgrade`、`list`、`search`、`info`）随后使用该channel的软件源和镜像。

元数据缓存在 `~/.cache/channels/`（用户）或 `/opt/epkg/cache/channels/`（root）下。使用 `epkg update` 刷新它。

## 添加或更改channel

常见的预定义channel定义位于 epkg 源代码树中（例如 `sources/` 下）。要添加或更改channel，您需要添加或编辑相应的源配置并重新构建或重新部署 epkg。确切的格式是特定于发行版的（YAML、仓库定义等）。有关详细信息，请参阅仓库和 [design-notes/repodata.md](../../design-notes/repodata.md)。