#!/usr/bin/env bash

meson_build() {
  pip install ninja
  if [ -n "${mesonPath}" ]; then
    pushd ${mesonPath}
  fi
  arch=`uname -m`
  meson setup . "$(arch)_compile_gnu"
  meson compile -C "$(arch)_compile_gnu" -j 8 --verbose
}

meson_install() {
  arch=`uname -m`
  DESTDIR=/opt/buildroot meson install -C "$(arch)_compile_gnu" --no-rebuild
}