#!/usr/bin/env bash


# TODO: rename to autotools_build()/autotools_install(), refer to /c/os/gentoo/gentoo/eclass/cmake.eclass
# TODO: add configure() phase
configure() {
  ./configure ${configureFlags}
}


autotools_build() {
    if [ -n "${configurePath}" ]; then
        pushd ${configurePath}
    fi
    if [ ! -f "configure" ]; then
        autoreconf -vif
    fi
    configure
    make -j8 ${makeFlags}
}

autotools_install() {
    # XXX
    rm -rf /opt/buildroot
    mkdir /opt/buildroot
    make install DESTDIR=/opt/buildroot
}
