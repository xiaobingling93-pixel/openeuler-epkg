#!/usr/bin/env bash


prep() {
  pushd /root/workspace
}

build() {
  echo "$build_system build"
  "$build_system"_build
}

install() {
  echo "$build_system install"
  "$build_system"_install
}

prep
build
install