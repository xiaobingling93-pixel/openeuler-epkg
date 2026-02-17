# epkg build

## 前提条件

- initial set of epkg repos available: binary conversion from openEuler RPM
- `epkg env/install` commands working

## inputs

- an epkg source yaml
- the source yaml's concretize options: `version, use.*, build.*` etc.
- base channel/repos
- the depends selection options

## outputs

- epkg package
- sbom json
- package 语义



## stage1: concretize build depends/params/scripts

### generate build parameters from source epkg yaml

input: yaml fields
```
channel
repos
pkgname
version
release
source.*
outputs
system
buildSystem
patches
use.*
build.*
```

output: pkgvars.sh

- gentoo/yocto style customization: run customization scripts at build time, in build machine, very dirty
- nix/spack style: first customize/concretize, then build

### generate exact build depends

intput: `buildRequires` packages

output: resolve to /opt/epkg/store/xxx paths

- RPM style solver: run yum install and use its solver at build time, in build machine
- nix/spack style: determine/solve the exact build params/depends together,
  since there may be constraints between packages.

### generate build phase functions from source epkg yaml

input: yaml `phase.*` fields

output: phase.sh

### bootstrap

1. create a bootstrap toolchains env based on openEuler
2. use commands/libs in it (extra PATH entry) to build epkg core packages
3. use epkg's own core packages to rebuild and stablize the epkg core packages



## stage2: run build machine, do actual build

input:
```
pkgvars.sh
phase.sh
buildinputs.sh
```

run:
```
epkg create build env, install depends

run phases
  fetch # download source.*
  unpack
  patch
  configure
  build
  install
  pack
```

output:

```
create build/log-$phase.txt
create epkg store
update epkg repodata
```





## epkg build module



### 信息提取 package-buildinfo-generator

Input: yaml

output: pkgvars.sh,  buildrequire, phase.sh, runPhase.sh

```
基于软件包的source.yaml生成最终的构建脚本
1. 生成环境变量：包括使用那个bootstrap工具链，使用repo仓库，待构建的软件包，版本，source，patch，编译选项，构建系统等，构建依赖; pkgvars.sh and  buildrequire
2. 构建依赖具象化：基于repo和yaml，得到构建依赖的详细数据(hash)  buildrequire -> buildrequire_hash
3. 生成构建执行过程：phase.sh
4. 生成安装执行过程：runPhase.sh
```



#### 生成pkgvars.sh

基于yaml生成 pkgvars.sh

```yaml
# 软件包元数据
pkgname
version
release
source.*
system  # openeuler ?
patches
# 编译选项 merge后？
use.*
build.*

# 构建系统
buildSystem
bootstrap

# 拆包策略
outputs

# 设置环境源
channel
repos
```

#### 生成 hash buildrequire

基于repo 获取buildrequires的hash值，用户在执行构建时候的依赖环境生成

首次可以先试用yum install来完成构建

epkg repo makecache -> 将channel中所有repo的repodata 下载后，基于files和provides 形成db数据库

在数据库中 搜索whatprovies buildrequires

```yaml
Buildrequires:
 - $hash__$name__$version__$release.$arch.epkg
   url: xxx # 下载地址
 - $hash__$name__$version__$release.$arch.epkg
   url: xxx
```

多个包需要利用dag方式，得看依赖的包是否在待构建任务中，如果在，则需要依赖待构建的包



#### 生成phase.sh

基于yaml，提取其中的

phase.prepare ，configure， compile ，test ，install 等配置；生成phase.sh

```bash
# phase.xxx 如果没有，generic-build.sh会安排直接继承buildsystem中的
configure()
{
	local mycmakeargs=(
                -DA4_PAPER=$(usex metric)
                -DNO_FONTCONFIG=$(usex fontconfig off on)
                -DNO_TEXT_SELECT=$(usex textselect off on)
                -DOPI_SUPPORT=$(usex opi)
                -DSPLASH_CMYK=$(usex cmyk)
                -DWITH_LIBPAPER=$(usex libpaper)
                -DWITH_LIBPNG=$(usex png)
                -DXPDFWIDGET_PRINTING=$(usex cups)
                -DSYSTEM_XPDFRC="${EPREFIX}/etc/xpdfrc"
        )
   cmake_configure
}

```

#### generic-build.sh

source $buildsystem.sh

refer to

```
https://gitee.com/openeuler-customization/design_meeting/blob/master/autopkg/240612/build-system.md
https://github.com/NixOS/nixpkgs/blob/master/pkgs/stdenv/generic/default-builder.sh
https://github.com/NixOS/nixpkgs/blob/master/pkgs/stdenv/generic/setup.sh
```

buildsystem 为cmake时

```shell
RT_FUNCTIONS prepare configure compile test install
prepare() {
	pre_prepare // hook，默认为空，支持上层定义
	cmake_prepare
	post_prepare
}

configure() {
	pre_configure
	cmake_configure
	post_configure
}

compile() {
	pre_compile
	cmake_compile
	post_compile
}

install() {
	pre_install
	cmake_install
	post_install
}

test() {
	pre_test
	cmake_test
	post_test
}

cmake_prepare() {}
cmake_configure() {}
cmake_compile() {}
cmake_install() {}
cmake_test() {}
```





### 执行构建 packege-build-runner

Input: pkgvars.sh,  buildrequire, phase.sh, runtimePhase.sh

output: epkg软件包

```
执行最终构建脚本；
run env_build（yum install => epkg install）
run build_requires_install（yum install => epkg install）
run phase.sh
run split/create subpackage，buildsystem config: file pattern => -dev/-doc subpackages
run generate subpackage's info/{package.json, buildinfo.json, files ...}
run pack epkg
```

#### 执行环境部署

基于bootstrap，安装构建环境

基于buildrequires，在环境中安装依赖的epkg包，当前可先使用yum来实现



#### 执行构建 phase.sh

基于buildsystem，依次执行 `prepare, configure, compile, test, install`

生成软件包二进制



#### 执行split/create subpackage

基于正则配置，将不同的二进制，放入不同的子包中



#### 执行subpackage元数据生成

基于fs，yaml，生成软件包的 package.json, buildinfo.json, files, runtimePhase.sh



#### 执行打包epkg

执行epkg pack

```
$subpackage
  - fs/                    <- 软件包编译产物>
    ├── bin/
    ├── lib/
    ├── etc/
    ├── include/
    ├── share/
  - info/
    ├── package.json        <- 软件包描述信息>
    ├── buildinfo.json      <- 软件包构建元数据>（930暂不考虑）
    ├── files           <- 软件包打包文件列表>
    ├── runtimePhase.sh     <- 软件包安装/卸载执行脚本（预留）>
    ├── pgp/                <- 软件包签名文件>
```

执行打包

`tar --zstd -cf  $hash__$name__$version__$release.$arch.epkg -C $subpackage`



## 实现过程

先用rpm转出来的epkg，生成首版本的epkg 工具环境

基于epkg工具环境，编译epkg工具变成

使用新的epkg工具环境，完成软件包的编译构建


### epkg build 生成编译环境和脚本实现流程

整体流程：
	 Input:  pkg.yaml
	 Output: pkgvars.sh | phase.sh
```txt
输入 pkg.yaml 描述，输出编译脚本
1. 解析yaml -> 生成 pkgvars.sh phase.sh
2. 下载源码和patch
3. 执行generic-build.sh
```

generic-build.sh：主脚本，只需要运行该脚本即可完成编译
```bash
source pkgvars.sh
source "$build_system".sh
source phase.sh

prep() {
    # XXX: use configurable variable, refer to $sourceRoot detection in nix
	pushd /root/workspace
}

patch() {
	load patch
}

# XXX: define these in build system sh, by helper function
build() {
	echo "$build_system build"
	"$build_system"_build
}

install() {
	echo "$build_system install"
	"$build_system"_install
}

phases="prep patch build install"
# XXX: use some_name coding style in shell script, except if the variable comes from pkg.yaml
for curPhase in ${phases[*]}; do
	runPhase "curPhase"
done
```

pkgvars.sh: 构建编译环境，根据yaml生成，涉及环境创建，requires包hash计算，安装包等
```bash
# base params
name=pkg.yaml.name
version=pkg.yaml.version
source=pkg.yaml.source
build_system=pkg.yaml.buildSystem

# build_requires
build_pkg_requires=pkg.yaml.buildRequires
# XXX: merge build system yaml into package yaml in python
build_system_requires=${build_system}.yaml.buildRequires
# get pkg hash value
build_requires=
for require in ${build_pkg_requires[*]}; do
	hash_pkg=epkg_hash(require)
	build_requires="hash_pkg $build_requires"
done
for require in ${build_system_requires[*]}; do
	hash_pkg=epkg_hash(require)
	build_requires="hash_pkg $build_requires"
done

# build env create $ requires install
epkg env create build_env
epkg install $build_requires

```

build_system.sh: 以make为例
```bash
make_build() {
  if [ -n "${makePath}" ]; then
    pushd ${makePath}
  fi
  make -j8 ${makeFlags}
}

make_install() {
  rm -rf /opt/buildroot
  mkdir /opt/buildroot
  make install DESTDIR=/opt/buildroot
}
```


## references

- pkg-format-references/spack-build-env.txt
- pkg-format-references/nix-drv.json

