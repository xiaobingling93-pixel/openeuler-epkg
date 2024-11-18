#!/usr/bin/env bash


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