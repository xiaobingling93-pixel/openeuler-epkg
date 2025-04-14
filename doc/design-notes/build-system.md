# build systems

guix 的build-system list很清晰:

```
wfg /c/os/guix/guix/build-system% ls
android-ndk.scm  chicken.scm  dub.scm    glib-or-gtk.scm  haskell.scm       meson.scm     ocaml.scm   rakudo.scm  scons.scm
ant.scm          clojure.scm  dune.scm   gnu.scm          julia.scm         minetest.scm  perl.scm    renpy.scm   texlive.scm
asdf.scm         cmake.scm    emacs.scm  go.scm           linux-module.scm  minify.scm    python.scm  r.scm       trivial.scm
cargo.scm        copy.scm     font.scm   guile.scm        maven.scm         node.scm      qt.scm      ruby.scm    waf.scm
```

build system可以通过扫描源码目录, 识别build language/files后确定.

```
wfg /c/rpm-software-management/autospec% gg -C2 config.set_build_pattern
    autospec/buildreq.py-728-
    autospec/buildreq.py-729-            if "Cargo.toml" in files:
    autospec/buildreq.py:730:                config.set_build_pattern('cargo', default_score)
    autospec/buildreq.py-731-
    autospec/buildreq.py-732-            if "CMakeLists.txt" in files and "configure.ac" not in files:
    autospec/buildreq.py:733:                config.set_build_pattern("cmake", default_score)
    autospec/buildreq.py-734-
    autospec/buildreq.py-735-            if "configure" in files and os.access(dirpath + '/configure', os.X_OK):
    autospec/buildreq.py:736:                config.set_build_pattern("configure", default_score)
    autospec/buildreq.py-737-            elif any(is_qmake_pro(f) for f in files):
    autospec/buildreq.py:738:                config.set_build_pattern("qmake", default_score)
    autospec/buildreq.py-739-
    autospec/buildreq.py-740-            if "pyproject.toml" in files and not requires_path:
    --
    autospec/buildreq.py-746-                if not python_req_in_filtered_path(requires_path):
    autospec/buildreq.py-747-                    requires_path = req_path
    autospec/buildreq.py:748:                    config.set_build_pattern("pyproject", default_score)
    autospec/buildreq.py-749-            elif "setup.py" in files and not setup_path:
    autospec/buildreq.py-750-                s_path = os.path.join(dirpath, "setup.py")
    autospec/buildreq.py-751-                if not python_req_in_filtered_path(s_path):
    autospec/buildreq.py-752-                    setup_path = s_path
    autospec/buildreq.py:753:                    config.set_build_pattern("distutils3", default_score)
    autospec/buildreq.py-754-
    autospec/buildreq.py-755-            if "requires.txt" in files and not requires_path:
    --
    autospec/buildreq.py-764-
    autospec/buildreq.py-765-            if "Makefile.PL" in files or "Build.PL" in files:
    autospec/buildreq.py:766:                config.set_build_pattern("cpan", default_score)
    autospec/buildreq.py-767-
    autospec/buildreq.py-768-            if "SConstruct" in files:
    autospec/buildreq.py:769:                config.set_build_pattern("scons", default_score)
```

一个build system会设置一组如下属性
- build.sh -- defines `phase.prepare|configure|build|install|...`
- `defineFlags`
- `configureFlags|makeFlags|cflags|...`
- `buildRequires`

实体形式,会是两个文件, 以cmake为例:
- cmake.sh
- cmake.yaml

对一般的软件包, 自动设定build system之后, `phase.xxx` 应该是空的, 由build.sh提供缺省实现.

如果需要定制,可以设置两大类属性
- set `configureFlags|makeFlags|...`, which will be referenced by `phase.xxx`
- set hooks `phase.preXxx|phase.postXxx`, which will be called by `phase.xxx`

## nix example

```
/c/os/NixOS/nixpkgs/pkgs/stdenv/generic/default-builder.sh
/c/os/NixOS/nixpkgs/pkgs/stdenv/generic/setup.sh
```

## gentoo example

这些eclass及其ebuild应用都是纯bash, 很有借鉴意义. 我们可以对它们做两大改进
- 运行于build environment,定制过程与构建过程纠缠在一起 => 分离出pure configure vars DAG
- 命令式 => 声明式

```
/c/os/gentoo/gentoo/eclass/cmake.eclass
    EXPORT_FUNCTIONS src_prepare src_configure src_compile src_test src_install

/c/os/gentoo/gentoo/app-emulation/nemu/nemu-3.0.0.ebuild
    inherit cmake linux-info

    src_configure() {
            # -DNM_WITH_QEMU: Do not embbed qemu.
            local mycmakeargs=(
                    -DNM_WITH_DBUS=$(usex dbus)
                    -DNM_WITH_NETWORK_MAP=$(usex network-map)
                    -DNM_WITH_REMOTE=$(usex remote-api)
                    -DNM_WITH_OVF_SUPPORT=$(usex ovf)
                    -DNM_WITH_QEMU=off
                    -DNM_WITH_SPICE=$(usex spice)
                    -DNM_WITH_VNC_CLIENT=$(usex vnc-client)
            )
            cmake_src_configure
    }

    src_install() {
            cmake_src_install
            docompress -x /usr/share/man/man1/nemu.1.gz
    }
```

## rpmbuild in build system POV

rpmbuild本质上在做两件事
- expand rpm macros
- write `phase.unpack|patch|build|install|check|clean` one by one to script file and run it

```
/c/rpm-software-management/rpm/build/build.c

static rpmRC buildSpec(rpmts ts, BTA_t buildArgs, rpmSpec spec, int what)
{
            (rc = doScript(spec, RPMBUILD_PREP, "%prep",
            (rc = doScript(spec, RPMBUILD_BUILD, "%build",
            (rc = doScript(spec, RPMBUILD_INSTALL, "%install",
            (rc = doScript(spec, RPMBUILD_CHECK, "%check",
            (rc = doScript(spec, RPMBUILD_CLEAN, "%clean",
```

## yaml源码包自主构建

所以我们可以自行构建.

1) define build system
```
builder/cmake.sh        # 定义通用phases
builder/cmake.yaml      # 定制通用fields
```

2) write package.yaml phase.xxx fields to package/phase.sh

3) write build.sh and run it
```
source builder/cmake.sh
source package/phase.sh # if package defined the same phase, can override the default ones provided by build system
call build phases one by one # refer to nix example
```

## 面向yaml加包

1) 通用情况处理, 写入构建系统; package.yaml本身应保持简单和干净
2) 特殊情况, 则在package.yaml设定 xxxFlags, or 设定 phase.pre/post hooks
3) 转spec时, 调用上述build.sh, 兼容rpmbuild, 但完全绕过rpm macro体系
