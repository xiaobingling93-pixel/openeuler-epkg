# repodata requirements
- maintain repodata URL list for all the well-known distros, for easy user selection
- direct install rpm/deb packages under a given distro/app repo URL
- direct install rpm/deb local files, plus creating local repodata for them

- scalable: quick search&install from repo pools that contain millions of total available packages
- minimal dependency: avoid sqlite or other heavy weight database
- memory efficient: fast data indexing with low memory consumption, suitable for embedded systems
  - memory mapped file
  - disk-backed hashmap
  - zero-copy de-serialization
  - currently:
    - `RSS=2908kb  for \time -v dist/epkg-x86_64 env list`
    - `RSS=93736kb for \time -v dist/epkg-x86_64 install git`

- proper isolation to easy to change/optimize data structure and back-end in future
- search: quick counterpart for
  - `apt-file search`
  - `apt-cache` sub-commands:
    - gencaches
    - showpkg pkg...
    - showsrc pkg...
    - stats
    - dump
    - dumpavail
    - show pkg...
    - search regex... # in name/description fields
    - depends pkg...
    - rdepends pkg...
    - pkgnames [prefix]
  - `dnf repoquery`
    --whatdepends REQ   shows results that requires, suggests, supplements,
                        enhances,or recommends package provides and files REQ
- update:
  - robust update/download: all starts from index.json
    - race-condition: users see new index.json, however its referenced files not ready yet
  - minimal download and computing:
    - index.json to include slices for incremental updates
      Refer to <https://conda.discourse.group/t/how-we-reduced-condas-index-fetch-bandwidth-by-99/257>
    - make local indexing per-slice (or per-repo)
      - should be at least per-repo
        - to avoid consistence detection + re-indexing after user enable/disable a repo using vim at any time
        - to support per-repo package priority etc. policies
      - per-slice pro: can minimize indexing efforts on regular/weekly updates (ignorable time)
      - per-slice con: little complexity and time on "epkg install"
        (trade-off: may load small slices into per-repo in-memory hash, while accessing large slices on-disk)

# repodata slice files

principles:
- online published files: must be stable, well known or easy to understand formats
- local generate+consumed: may be high-performance binary index or on-disk hash of any kind

## online repo metadata
1. packages file
- option1: Debian Packages.xz, each paragraph is a valid package YAML
  - pro: human friendly
  - pro: fast if manual split parse
  - pro: fast: unzip / write / create index can be streamlined, better utilizing memory, L3 cache and multi-core CPU
- option2: packages.json, each line is a valid (subset of) package JSON
  - pro: grep friendly
  - pro: easy to parse
  - con: may consume 1 second on "epkg update"
  - Example:
```
[
# one line per package
# each line is indexed, can be standalone located & loaded as JSON into struct Package
{ "name": "perl-Compress-Raw-Zlib", "epoch": "1", "version": "2.206", ... },
{ "name": "atune-engine", "epoch": "0", "version": "1.2.0", ... },
...
]
```
- option3: packages.bson
  - pro: fast and zero-copy deserialization
  - con: not human friendly
  - con: how to create index?

2. filelist file
- option1: Debian Contents-amd64.lz4, each line is "/full/path section/pkgname"
  - pro: grep friendly
  - pro: human friendly
- option2: RPM filelist.xml => filelist.yaml, one long line per package "pkgname: path1 path2 ..."
  - pro: per-package organization

epkg layout proposal:
```
$repo_url/repodata/index.json
$repo_url/repodata/packages-$date-$hash1.txt.zst
$repo_url/repodata/packages-$date-$hash2.txt.zst
$repo_url/repodata/filelist-$date-$hash1.txt.zst
$repo_url/repodata/filelist-$date-$hash2.txt.zst
```

## local repo metadata

Create below files (per-slice)
- from packages.txt
- for `epkg install/remove/upgrade` to on-demand traverse depends DAG
1. `packages.idx` (on-disk) hash (pkgname => one or more package paragraph offset pairs)
   (may optimize the multi-value to multi-packages.idx, or use string value to store multiple hex)
2. `provide2pkgnames.idx` (on-disk) hash (capability => pkgname(s))
3. `essential_pkgnames.txt` (one line per pkgname)

## the 1M packages challenge

If packages go up by 10x-100x, it'll no longer be feasible to pre-cache all
repodata locally.

- the repo index file (store-paths.txt) will be ~100MB for 1M packages, according to this:
```
wfg ~/.cache/epkg/channel/openeuler:24.03-lts/everything/x86_64/repodata% wc -l store-paths
18688 store-paths
wfg ~/.cache/epkg/channel/openeuler:24.03-lts/everything/x86_64/repodata% du store-paths
1.3M    store-paths
```

- packages.txt will be even larger, so have to be split and downloaded on-demand
  during the depends DAG walking. Refer to <https://prefix.dev/blog/sharded_repodata>
  It creates one shard per *pkgname*.

The possible solution: append-only shards
- maintain per-slice shard index file
    name: `shards-$date-$hash.txt`
    lines: `shard_name shard_size`
    alternative: can be optimized to a big binary array, since sharding logic is predefined
- shard files must be stored online *uncompressed*, however may use HTTP compression to save download bandwidth
- split repodata into shards by *prefix of pkgname*
  - 2-digit: up to 36**2=1296 shard files, shards.txt size < `30B * 1296 = 37KB`, smaller when compressed
  - 3-digit: up to 36**3=46656 shard files, shards.txt size < `30B * 46656 = 1.3MB`, or `4B * 46656 = 182KB` for binary format
  - for "libxxx" packages, prefix shall count from "xxx"
  - to avoid inbalanced shards, could use `shard_name=hash_of(pkgname)`, however it'll reduce compress ratio
  - may start from 2-digit sharding, years later switch to 3-digit smoothly in forward compatible way
```
79MB: packages.json for 18k packages
79MB * 10/36/36.0 = 583kb: 2-digit shards for total 180k packages
79MB * 100/36/36/36.0 = 166kb: 3-digit shards for total 1.8M packages
```
- client side `epkg update/install`: slice + appending helps 100x less
  bandwidth: incremental download new contents, if the size in shard index is
  larger than local cache
- client side `epkg env create`: sharding helps 10x-1000x less bandwidth:
  typically one environment won't install too many packages (much less than /
  OS installation), it could be wasteful to pre-download all repodata for all
  packages.

## misc old ideas
```
- 1 store-paths.txt index (pkgname => pkgline(s))
- 1 filelist.idx hash index (pkgname => filelist.txt locations)
- 1 fulltext2pkgnames.idx index (full text words => pkgname(s))
```

## epkg install rpm flow

### brief steps
1. capability -> provide2pkgnames -> pkgname -> packages.idx -> Package
2. walk DAG, get Package depends
3. download rpm from `repo.url + Package.location`
4. convert rpm to epkg, install

### Package.location field
```
# rpm
<location href="Packages/Judy-1.0.5-19.oe2403.x86_64.rpm"/>
# deb
Filename: pool/main/0/0ad/0ad_0.0.26-3_amd64.deb
# epkg
location: packages/$pkgline.epkg
```

### pkgline for rpm/deb
- pkgline is only available after download/converting rpm to epkg
- for now: many data structure and logic assumes pkgline
- future: need support 2 DAG depends walk options at the same time:
  1) some packages may depend on other (pkgname + version constraints)
  2) some packages may depend on other (pkgline)
     i.e. their files have explicit/fixed references to others' `store_path/pkgline/xxx`
  3) some packages may depend on both scenarios

# data structure tree
```
    1 user, or epkg invocation
        X EnvConfig
        X ChannelConfig
            X*Y RepoData
                X*Y*Z repo slice, including:
                X*Y*Z StorePathsIndex
                X*Y*Z PkgInfoIndex
                X*Y*Z PkgFilesIndex // not yet implemented

    // now (to-be: X*Y*Z, in some new way)
    // PkgInfoIndex =>
        1 pkghash2spec
        1 pkgname2lines
    // StorePathsIndex =>
        1 provide2pkgnames
        1 essential_pkgnames
```
=>
```
    1 user, or epkg invocation
        {X} EnvConfig
        {X} ChannelConfig
            {X}*{Y} RepoIndex
                {X}*{Y}*[Z] RepoSlice
                    {X}*{Y}*[Z] StorePathsIndex
                    {X}*{Y}*[Z] PkgInfoIndex
                    {X}*{Y}*[Z] PkgFilesIndex
                    {X}*{Y}*[Z] pkgname2locations
                    {X}*{Y}*[Z] provide2pkgnames
                    {X}*{Y}*[Z] essential_pkgnames
```

# wrapping functions

```
map_pkgname2specs()
map_provide2pkgnames()
is_essential_pkgname()
```

# reference: file size expectation

## current epkg

```shell
# packages.json size expectation
wfg ~/.cache/epkg/channel/openeuler:24.03-lts/everything/x86_64/pkg-info% du -s .
79M     .

# others size expectation
wfg ~/.cache/epkg/channel/openeuler:24.03-lts/everything/x86_64/repodata% du *
0       essential_pkgnames.txt
4.0K    index.json
3.8M    pkg-info.zst
6.8M    provide2pkgnames.yaml
1.3M    store-paths
608K    store-paths.zst
```

## openEuler

```
wfg@crystal ~/repodata/2403-everything% du *
43M     3a11516e8e5eae0dbdbd86555d51052eb6d4f66f350221d93c13c8c84e8c88c1-primary.xml
4.0M    3a11516e8e5eae0dbdbd86555d51052eb6d4f66f350221d93c13c8c84e8c88c1-primary.xml.zst
9.1M    87ee4e92c1e5173adb9dd158ec9c2e7bd500bbe68c80fdd1cfc6721bbb15534f-primary.sqlite.bz2

211M    688e5b3da9855e391ca15fd7c54e213eab45c086c86f890e6c18fc344c540d81-filelists.xml
13M     688e5b3da9855e391ca15fd7c54e213eab45c086c86f890e6c18fc344c540d81-filelists.xml.zst
16M     6053ce0985494d70a5a1afa58c44c88709de96d333f564b5bc6093974acde32e-filelists.sqlite.bz2

80K     47979aac96ccd53f185c83406d6651f8f7bfaa0b151cd4fbba026f7240002ca7-normal.xml
12K     47979aac96ccd53f185c83406d6651f8f7bfaa0b151cd4fbba026f7240002ca7-normal.xml.zst

14M     6535ebec36083915216443da3cec498352cafeda8dd5588fe2aef04ba65d2f0e-other.xml
1.3M    6535ebec36083915216443da3cec498352cafeda8dd5588fe2aef04ba65d2f0e-other.xml.zst
2.7M    82b5b069de16988679db1bccb603fa4d577d50e7db2d16a9d968b447ac924f57-other.sqlite.bz2

80K     normal.xml
4.0K    repomd.xml
4.0K    TRANS.TBL
```

## Debian

```
https://mirrors.tuna.tsinghua.edu.cn/debian/dists/Debian12.10/main/binary-amd64/Packages.xz
8.4M    Packages.xz
=>
/var/lib/apt/lists/mirrors.tuna.tsinghua.edu.cn_debian_dists_trixie_main_binary-amd64_Packages
48M     Packages

62M     /var/lib/apt/lists/mirrors.tuna.tsinghua.edu.cn_debian_dists_trixie_main_Contents-all.lz4
21M     /var/lib/apt/lists/mirrors.tuna.tsinghua.edu.cn_debian_dists_trixie_main_Contents-amd64.lz4
# if manual extract =>
156M    mirrors.tuna.tsinghua.edu.cn_debian_dists_trixie_main_Contents-amd64

46M     /var/cache/apt/pkgcache.bin
45M     /var/cache/apt/srcpkgcache.bin
```

# reference: grep speed

Conclusion:
- grep performs better than `apt-cache` by 20x !
   RSS 2200kb vs 67432kb!
- `lz4cat | grep` performs 2x faster than `apt-file`

All tests are cache warm.

## apt-cache vs. awk vs. grep
```
# -rw-rw-r-- 1 wfg wfg  48M 2025-03-15 16:45 Packages

wfg ~/repodata/debian% time apt-cache search acoc
libjacoco-java - free code coverage library for Java
noglob apt-cache search acoc  0.74s user 0.03s system 99% cpu 0.768 total

wfg ~/repodata/debian% time awk '/xxxxxxxxxx/{print}' Packages
awk '/xxxxxxxxxx/{print}' Packages  0.19s user 0.02s system 99% cpu 0.208 total
awk '/xxxxxxxxxx/{print}' Packages  0.18s user 0.01s system 99% cpu 0.194 total
awk '/xxxxxxxxxx/{print}' Packages  0.18s user 0.01s system 99% cpu 0.192 total
wfg ~/repodata/debian% time grep 'xxxxxxxxxx' Packages
grep --color 'xxxxxxxxxx' Packages  0.01s user 0.02s system 97% cpu 0.030 total
grep --color 'xxxxxxxxxx' Packages  0.01s user 0.02s system 97% cpu 0.033 total
grep --color -F 'xxxxxxxxxx' Packages  0.01s user 0.02s system 97% cpu 0.031 total
grep --color -F 'web.*xxxxxxxxxx' Packages  0.05s user 0.00s system 98% cpu 0.047 total

wfg ~/repodata/debian% time grep -c '^Package: ' Packages
63467
grep -c '^Package: ' Packages  0.06s user 0.02s system 98% cpu 0.076 total
ugrep -c '^Package: ' Packages  0.02s user 0.02s system 98% cpu 0.038 total
```

## apt-file vs. lz4cat+grep
```
# -rw-r--r-- 1 wfg wfg  21M 2025-05-13 08:40 mirrors.tuna.tsinghua.edu.cn_debian_dists_trixie_main_Contents-amd64.lz4

wfg ~/repodata/debian% time apt-file search lz4cat
firejail-profiles: /etc/firejail/lz4cat.profile
fish-common: /usr/share/fish/completions/lz4cat.fish
lz4: /usr/bin/lz4cat
lz4: /usr/share/man/man1/lz4cat.1.gz
apt-file search lz4cat  1.10s user 0.35s system 145% cpu 0.998 total

wfg ~/repodata/debian% time lz4cat *.lz4 | grep 'xxxxxxxxxx'
lz4cat *.lz4  0.05s user 0.10s system 103% cpu 0.154 total
grep --color 'xxxxxxxxxx'  0.07s user 0.06s system 85% cpu 0.154 total

wfg ~/repodata/debian% time lz4cat *.lz4 | awk '/xxxxxxxxxx/{print}'
lz4cat *.lz4  0.08s user 0.10s system 41% cpu 0.423 total
awk '/xxxxxxxxxx/{print}'  0.36s user 0.06s system 97% cpu 0.423 total

wfg ~/repodata/debian% time apt-file search xxxxxxxxxx
apt-file search xxxxxxxxxx  0.58s user 0.42s system 154% cpu 0.648 total

```

# reference: DEB/RPM local repodata

## Debian packages YAML
```
/var/lib/apt/lists/mirrors.tuna.tsinghua.edu.cn_debian_dists_trixie_main_binary-amd64_Packages
# one package:

Package: 0ad
Version: 0.0.26-3
Installed-Size: 28591
Maintainer: Debian Games Team <pkg-games-devel@lists.alioth.debian.org>
Architecture: amd64
Depends: 0ad-data (>= 0.0.26), 0ad-data (<= 0.0.26-3), 0ad-data-common (>= 0.0.26), 0ad-data-common (<= 0.0.26-3), libboost-filesystem1.74.0 (>= 1.74.0), libc6 (>= 2.34), libcurl3-gnutls (>= 7.32.0), libenet7, libfmt9 (>= 9.1.0+ds1), libfreetype6 (>= 2.2.1), libgcc-s1 (>= 3.4), libgloox18 (>= 1.0.24), libicu72 (>= 72.1~rc-1~), libminiupnpc17 (>= 1.9.20140610), libopenal1 (>= 1.14), libpng16-16 (>= 1.6.2-1), libsdl2-2.0-0 (>= 2.0.12), libsodium23 (>= 1.0.14), libstdc++6 (>= 12), libvorbisfile3 (>= 1.1.2), libwxbase3.2-1 (>= 3.2.1+dfsg), libwxgtk-gl3.2-1 (>= 3.2.1+dfsg), libwxgtk3.2-1 (>= 3.2.1+dfsg-2), libx11-6, libxml2 (>= 2.9.0), zlib1g (>= 1:1.2.0)
Pre-Depends: dpkg (>= 1.15.6~)
Description: Real-time strategy game of ancient warfare
Homepage: https://play0ad.com/
Description-md5: d943033bedada21853d2ae54a2578a7b
Tag: game::strategy, interface::graphical, interface::x11, role::program,
 uitoolkit::sdl, uitoolkit::wxwidgets, use::gameplaying,
 x11::application
Section: games
Priority: optional
Filename: pool/main/0/0ad/0ad_0.0.26-3_amd64.deb
Size: 7891488
MD5sum: 4d471183a39a3a11d00cd35bf9f6803d
SHA256: 3a2118df47bf3f04285649f0455c2fc6fe2dc7f0b237073038aa00af41f0d5f2

```

## RPM packages XML

```
# one package in 3a11516e8e5eae0dbdbd86555d51052eb6d4f66f350221d93c13c8c84e8c88c1-primary.xml
<package type="rpm">
  <name>Judy</name>
  <arch>x86_64</arch>
  <version epoch="0" ver="1.0.5" rel="19.oe2403"/>
  <checksum type="sha256" pkgid="YES">d1ea4fd748aada488d9fd801b5944ae6aa46d59eecdc14a9d2e44cf3dd9d500f</checksum>
  <summary>C library array</summary>
  <description>The package provides the most advanced core technology, the main
advantages are scalability, high performance and memory efficiency.</description>
  <packager>http://openeuler.org</packager>
  <url>http://sourceforge.net/projects/judy/</url>
  <time file="1716988965" build="1716095280"/>
  <size package="116985" installed="351470" archive="0"/>
  <location href="Packages/Judy-1.0.5-19.oe2403.x86_64.rpm"/>
  <format>
    <rpm:license>LGPLv2+</rpm:license>
    <rpm:vendor></rpm:vendor>
    <rpm:group>Unspecified</rpm:group>
    <rpm:buildhost>dc-64g.compass-ci</rpm:buildhost>
    <rpm:sourcerpm>Judy-1.0.5-19.oe2403.src.rpm</rpm:sourcerpm>
    <rpm:header-range start="768" end="4337"/>
    <rpm:provides>
      <rpm:entry name="Judy" flags="EQ" epoch="0" ver="1.0.5" rel="19.oe2403"/>
      <rpm:entry name="Judy(x86-64)" flags="EQ" epoch="0" ver="1.0.5" rel="19.oe2403"/>
      <rpm:entry name="libJudy.so.1()(64bit)"/>
    </rpm:provides>
    <rpm:requires>
      <rpm:entry name="/bin/sh" pre="1"/>
      <rpm:entry name="/bin/sh"/>
      <rpm:entry name="rtld(GNU_HASH)"/>
      <rpm:entry name="libc.so.6(GLIBC_2.14)(64bit)"/>
    </rpm:requires>
    <file>/etc/ima/digest_lists.tlv/0-metadata_list-compact_tlv-Judy-1.0.5-19.oe2403.x86_64</file>
    <file>/etc/ima/digest_lists/0-metadata_list-compact-Judy-1.0.5-19.oe2403.x86_64</file>
  </format>
</package>
```

## Debian files txt

```
21M     /var/lib/apt/lists/mirrors.tuna.tsinghua.edu.cn_debian_dists_trixie_main_Contents-amd64.lz4

# 1,806,352 lines, mapping each path to package
usr/bin/health-check                                    admin/health-check
usr/bin/heaptrack                                       devel/heaptrack
usr/bin/heaptrack_gui                                   devel/heaptrack-gui
usr/bin/heaptrack_print                                 devel/heaptrack
usr/bin/heartbleeder                                    net/heartbleeder
usr/bin/heatshrink                                      libdevel/heatshrink
usr/bin/hebcal                                          utils/hebcal
usr/bin/heif-convert                                    video/libheif-examples
usr/bin/heif-dec                                        video/libheif-examples
usr/bin/heif-enc                                        video/libheif-examples
usr/bin/heif-info                                       video/libheif-examples
usr/bin/heif-thumbnailer                                graphics/heif-thumbnailer
usr/bin/heimdal-history                                 net/krb5-strength
usr/bin/heimdal-strength                                net/krb5-strength
usr/bin/heimdall                                        devel/heimdall-flash
usr/bin/heimdall-frontend                               devel/heimdall-flash-frontend
usr/bin/heimtools                                       net/heimdal-clients
usr/bin/heka2itx                                        science/biosig-tools
usr/bin/helcor                                          science/c-munipack
...
```

## RPM filelist XML

```
# one file in 688e5b3da9855e391ca15fd7c54e213eab45c086c86f890e6c18fc344c540d81-filelists.xml
<package pkgid="47b7590563e919cb678fe6530027c7d61a2b54ccd042e5ea91c870da1c9531fc" name="389-ds-base" arch="x86_64">
  <version epoch="0" ver="2.3.2" rel="6.oe2403"/>
  <file type="dir">/etc/dirsrv</file>
  <file type="dir">/etc/dirsrv/config</file>
  <file>/etc/dirsrv/config/certmap.conf</file>
  <file>/etc/dirsrv/config/slapd-collations.conf</file>
  <file type="dir">/etc/dirsrv/schema</file>
```
=>
```
# one package per paragraph
# one "pkgname file" per line
# ignore dirs
# no global sort and strip unique files like Debian
# each paragraph may start with an optional comment line for the package info

389-ds-base etc/dirsrv/config/certmap.conf
389-ds-base etc/dirsrv/config/slapd-collations.conf
...

```


# references
<https://docs.rs/odht/latest/odht/>
<https://github.com/rust-lang/odht>
<https://github.com/rkyv/rkyv>
<https://blog.cloudflare.com/scalable-machine-learning-at-cloudflare/#zero-copy-deserialization>
<https://github.com/datalust/diskmap-index/blob/dev/diskmap.rs>
<https://github.com/djkoloski/rust_serialization_benchmark>
<https://github.com/cloudflare/entropy-map>
<https://users.rust-lang.org/t/whats-the-fastest-way-to-store-300-million-unique-values-in-a-hashset/96939/11>
<https://docs.rs/sled/latest/sled/doc/index.html>
