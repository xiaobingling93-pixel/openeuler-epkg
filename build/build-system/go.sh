#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

go_build() {
  if [ -n "${goPath}" ]; then
    pushd ${goPath}
  fi
  export GOPATH="/opt/buildroot"
  export PATH=$PATH:$GOPATH/bin
  export GOOS=linux
  # 定义输出目录
  OUTPUT_DIR="/root/output"

  # 检查输出目录是否存在，如果不存在则创建
  if [ ! -d "$OUTPUT_DIR" ]; then
    mkdir -p "$OUTPUT_DIR"
  fi

  # 查找当前目录及其子目录下的所有 main.go 文件
  if [ ! -f "go.mod" ]; then
    cp *.mod go.mod
  fi
  if [ ! -f "go.sum" ]; then
    cp *.sum go.sum
  fi
  find . -type f -name "main.go" | while read -r file; do
    # 获取 main.go 文件所在的目录
    dir=$(dirname "$file")
    pushd "$dir" || { echo "Failed to cd to $dir"; continue; }
    project_name=$(basename "$dir")
    # 执行 go build 命令
    go build -o "$project_name"
    # 检查 go build 是否成功
    if [ $? -eq 0 ]; then
      # 复制生成的可执行文件到输出目录
      cp "$project_name" "$OUTPUT_DIR"
      echo "Built and copied $project_name to $OUTPUT_DIR"
    else
      echo "Failed to build $project_name"
    fi
    popd
  done
}

go_install() {
  mkdir -p "$BUILD_FS_DIR"/usr/bin
  cd /root/output
  cp * "$BUILD_FS_DIR"/usr/bin
}