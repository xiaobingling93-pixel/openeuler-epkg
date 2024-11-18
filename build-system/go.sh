#!/usr/bin/env bash

go_build() {
  if [ -n "${goPath}" ]; then
    pushd ${goPath}
  fi
  go build
}

go_install() {
  export GOPATH="/opt/buildroot"
  export PATH=$PATH:$GOPATH/bin
  go install
}