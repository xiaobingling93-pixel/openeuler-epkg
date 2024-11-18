#!/usr/bin/env bash


autogen_build() {
    chmod 755 ${autogen_file}
    ./"${autogen_file}"
}

autogen_install() {
    rm -rf /opt/buildroot
    mkdir /opt/buildroot
    make install DESTDIR=/opt/buildroot
}