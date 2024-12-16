#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.


remove_plugin() {
  if [ $# -eq 0 ]; then
    # 如果没有参数，直接返回
    return
  else
    # 如果有参数，执行命令
    for param in "$@"; do
      python3 /usr/share/java-utils/pom-editor.py pom_remove_plugin -r :maven-"$param"
    done
  fi
}

disable_module() {
  if [ $# -eq 0 ]; then
    # 如果没有参数，直接返回
    return
  else
    # 如果有参数，执行命令
    for param in "$@"; do
      python3 /usr/share/java-utils/pom-editor.py pom_disable_module -r :maven-"$param"
    done
  fi
}

delete_dir() {
  if [ $# -eq 0 ]; then
    # 如果没有参数，直接返回
    return
  else
    # 如果有参数，执行命令
    for param in "$@"; do
      rm -rf "$param"
    done
  fi
}

maven_build() {
  pip install maven xmvn
  if [ -n "${mavenPath}" ]; then
    pushd ${mavenPath}
  fi
  remove_plugin "$maven_remove_plugins"
  disable_module "$maven_disable_modules"
  delete_dir "$maven_rm_dirs"
  python3 /usr/share/java-utils/mvn_build.py -b -f
  if [ $? -eq 0 ]; then
    echo "maven build finished"
  else
    echo "maven build failed"
    exit 1
  fi
}

maven_install() {
  # 检查 name 字段是否存在
  if [ -z "$name" ]; then
    echo "name 字段不存在"
  else
    echo "name 字段的值是: $name"
  fi
  xmvn-install -R .xmvn-reactor -n "$name" -d "$BUILD_FS_DIR"
  if [ $? -eq 0 ]; then
    echo "maven package finished"
  else
    echo "maven package failed"
    exit 1
  fi
}

