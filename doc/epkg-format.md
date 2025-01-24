# epkg format

## .epkg文件：epkg软件二进制包

.epkg后缀的文件，格式为zstd压缩的tar包。解压后的目录结构：

```
  - fs/                    <- 软件包编译产物>
    +-- bin/
    +-- lib/
    +-- etc/
    +-- include/
    +-- share/
  - info/
    +-- package.json        <- 软件包描述信息>
    +-- buildinfo.json      <- 软件包构建元数据>（930暂不考虑）
    +-- files               <- 软件包打包文件列表>
    +-- install/            <- 软件包安装/卸载执行脚本（预留）>
        +-- pre
        +-- post
    +-- pgp/                <- 软件包签名文件>
```

## reproducible tar

<https://www.gnu.org/software/tar/manual/html_node/Reproducibility.html>
<https://github.com/drivendataorg/repro-tarfile>

```
enforce file order:
       -T, --files-from=FILE
              Get names to extract or create from FILE.
```

## epkg hash

- 使用SHA-256, refer to <https://fedoraproject.org/wiki/Features/StrongerHashes>
- 合法字符范围: 0-9 a-z
- hash长度: 32
- hash内容:
  - fs/
  - info/install/

## info/package.json: epkg软件包元数据

```json
{
  "name": "mypackage",
  "version": "2.1.3",
  "release": 24,
  "epoch": null,
  "hash": null,
  "dist": null,
  "arch": null,
  "source": "xxxxx",
  "summary": "xxxxxxx",
  "description": "xxxxxxxx",
  "depends": [
    {
      "hash": "hash1",
      "pkgname": "pkgname1"
    }
  ],
  "requires": [
    "rpmlib(xxxx) <= xxx",
    "pkgname1"
  ],
  "provides": [
    "/bin/sh",
    "libc.so.6(GLIBC_2.34)(64bit)",
    "xxx-libs"
  ],
  "recommends": null,
  "suggests": null
}
```

## info/buildinfo.json: 可重复构建信息

## epkg repo: 软件源格式

### Dir layout in repo server

```
https://repo.oepkgs.net/openeuler/epkg/channel/${osv}/${repo}/${arch}/
    store/{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}.epkg
    repodata/index.json
    repodata/store-paths-{filehash}.txt.zst
    repodata/pkg-info-{filehash}.tar.zst
    repodata/pkg-files-{filehash}.tar.zst
```

epkg channel index规模会是rpm repodata 10x以上, 需要考虑用户同步数据的高效性。基本原理借鉴视频文件的I帧/P帧的(初始全量+若干增量)搭配思路: {I P P P P P P} {I P P} ...
Debian也是这个思路。客户端组合数据的方法: 使用index.json里规定的{I+P+P+...}组合。

### repodata/index.json 示例

```json
{
  "store-paths": [
    {
      "filename": "store-paths-{filehash1}.txt.zst",
      "checksum": null,
      "datetime": null
    },
    {
      "filename": "store-paths-{filehash2}.txt.zst",
      "checksum": null,
      "datetime": null
    },
    {
      "filename": "store-paths-{filehash3}.txt.zst",
      "checksum": null,
      "datetime": null
    }
  ],
  "pkg-info": [
    {
      "filename": "pkg-info-{filehash1}.tar.zst",
      "checksum": null,
      "datetime": null
    },
    {
      "filename": "pkg-info-{filehash2}.tar.zst",
      "checksum": null,
      "datetime": null
    },
    {
      "filename": "pkg-info-{filehash3}.tar.zst",
      "checksum": null,
      "datetime": null
    }
  ],
  "pkg-files": [
    {
      "filename": "pkg-files-{filehash1}.tar.zst",
      "checksum": null,
      "datetime": null
    },
    {
      "filename": "pkg-files-{filehash2}.tar.zst",
      "checksum": null,
      "datetime": null
    },
    {
      "filename": "pkg-files-{filehash3}.tar.zst",
      "checksum": null,
      "datetime": null
    }
  ]
}
```

### repodata/store-paths.txt.zst 示例

```
09c88c8eb9820a3570d9a856b91f419c__libselinux__3.3__5.oe2203sp3
0d5a3b5c87db1f79b24db99528d4595f__filesystem__3.14__1.oe1
11b68df5774cecc94b69ab3b84b523f0__gpm-libs__1.20.7__22.oe1
17621c79039c7ef5547150f8cdc78ea7__tzdata__2022a__16.oe2203sp3
1cc0272bb2a2f2de650530167f2afd1e__pcre2__10.35__1.oe1
2b9c7bd3aca01b8539830ad7eef35bc4__openEuler__release__20.03LTS_SP1-38.oe1
3682cf9bbdef657ab2486054e213ae68__vim-common__8.2__1.oe1
3682cf9bbdef657ab2486054e213ae68__vim-enhanced__8.2__1.oe1
3682cf9bbdef657ab2486054e213ae68__vim-filesystem__8.2__1.oe1
3abe6ef79e456fcc6f9d2b962d7cdab2__libacl__2.3.1__2.oe2203sp3
```

### repodata/pkg-info.tar.zst 示例

tar文件列表:
```
{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}.json
{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}.json
{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}.json
{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}.json
```
其中的每一个json文件，都对应epkg包中的info/package.json内容。

### repodata/pkg-files.tar.zst 示例

tar文件列表:
```
{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}.files.txt
{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}.files.txt
{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}.files.txt
{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}.files.txt
```

其中的每一个files.txt文件，内容是对应epkg软件包fs/目录下的文件路径清单，示例：
```
/usr/bin/tmux
/usr/share/man/man1/tmux.1.gz
```

## epkg env软件源配置文件
```yaml
# $HOME/.epkg/envs/${env}/profile-current/etc/epkg/channel.yaml
channel:
  name: "openEuler-24.03-LTS"
  baseurl: "https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.03-LTS/"

repos:
  everything:
    # url: defaults to ${channel.baseurl}/$reponame
    # XXX
    # gpgcheck: true
    # gpgkey: "http://repo.openeuler.org/openEuler-24.03-LTS/everything/${channel.arch}/RPM-GPG-KEY-openEuler"
  mysql:
      enabled = false
      # a repo can specify its own url
      url = "http://third.party/repo/dir"
```

## epkg cache: 本地软件源缓存

首次安装epkg软件包（epkg install xxx），或者手动执行epkg update cache，默认在本地初始化/更新epkg软件源cache，其目录结构与服务侧软件源类似，但内容已经被解压:

```
$HOME/.cache/epkg/channel/${osv}/${repo}/${arch}/repodata/index.json

$HOME/.cache/epkg/channel/${osv}/${repo}/${arch}/repodata/store-paths-{filehash}.txt
$HOME/.cache/epkg/channel/${osv}/${repo}/${arch}/repodata/store-paths-{filehash}.txt
$HOME/.cache/epkg/channel/${osv}/${repo}/${arch}/repodata/store-paths-{filehash}.txt
...
$HOME/.cache/epkg/channel/${osv}/${repo}/${arch}/pkg-info/{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}.json
$HOME/.cache/epkg/channel/${osv}/${repo}/${arch}/pkg-info/{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}.json
...
$HOME/.cache/epkg/channel/${osv}/${repo}/${arch}/pkg-files/{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}.files.txt
$HOME/.cache/epkg/channel/${osv}/${repo}/${arch}/pkg-files/{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}.files.txt
...
$HOME/.cache/epkg/channel/${osv}/${repo}/${arch}/store/{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}.epkg
$HOME/.cache/epkg/channel/${osv}/${repo}/${arch}/store/{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}.epkg
...
```

## epkg store: 本地软件仓

location:
```
      /opt/epkg/store/{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}/ (global install)
    $HOME/.epkg/store/{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}/ (private install)
```

以上每个软件里的目录结构如下
```
# 以下为从.epkg解压所得
fs/
info/
```

## installed package tracking

per-env tracking:
```json
# /home/${user}/.epkg/envs/${env}/profile-current/installed-packages.json
{
  "${pkghash1}__${pkgname}__${pkgver}__${pkgrel}": {
    "install_time": null,
    "depend_depth": 0
  },
  "${pkghash2}__${pkgname}__${pkgver}__${pkgrel}": {
    "install_time": null,
  }
}
```

per-store tracking is done by finding all `installed-packages.json` in all user/envs
and check them together.

## epkg db: 本地软件信息数据库与索引

It may be enough to create the yaml lookup file in (1) on updated repodata and
use for dependency lookup. If too large and slow, try (2) embedded rust kv store.
Try (3) sqlite as last resort since it introduces complexity and lib.so dependency.

### yaml lookup files

Manual create and parse these files, not via the slow yaml library.
Ensure one line per package to simplify parse or grep.

These can be quickly loaded into internal HashMap for forward/reverse lookup,
or one-shot grep for substring or regex pattern.

The depend/rdepend lookup yamls size may be ~3MB for 30k packages.

Files under dir: $HOME/.cache/epkg/channel/${osv}/${repo}/${arch}/repodata/pkg-info-{filehash}/
- provide2pkgnames.yaml     # depend lookup
```
libcunit.so.1()(64bit): CUnit
```
- require2pkgnames.yaml     # rdepend lookup
```
/bin/sh: CUnit
ld-linux-aarch64.so.1()(64bit): CUnit
ld-linux-aarch64.so.1(GLIBC_2.17)(64bit): CUnit
rtld(GNU_HASH): CUnit
libc.so.6(GLIBC_2.17)(64bit): CUnit
```
- recommend2pkgnames.yaml   # rdepend lookup
- suggest2pkgnames.yaml     # rdepend lookup
- pkgname2files.yaml        # grep for files, only the critical files in repodata primary.xml
```
bash: file1 file2 ...
CUnit: file1 file2 file3
```
- pkgname2summary.yaml      # grep for package summary
- pkgname2description.yaml  # grep for package details

### rust embedded key/value store

https://github.com/cberner/redb
https://github.com/fjall-rs/fjall

### installed package db

The db is optional, mainly for speeding up common queries by reverse index.

sqlite db, per store:
- $HOME/.epkg/db/installed-packages.sqlite (private install)
- /opt/epkg/db/installed-packages.sqlite   (global install)

## reference

<https://www.archlinuxcn.org/now-using-zstandard-instead-of-xz-for-package-compression/>

<https://man.freebsd.org/cgi/man.cgi?mtree(5)>
