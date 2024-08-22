#!/bin/bash

if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <rpm-list>"
    exit 1
fi

rpm_list=$1

# 设置要检查的目录路径
store_metadata="/srv/os-repo/epkg/openeuler/openEuler-22.03-LTS-SP3/OS/aarch64/pkg-info/"
store_rpms="$HOME/tmprpms/"


# 检查目录是否存在
if [ ! -d "$store_metadata" ]; then
  mkdir -p "$store_metadata"
fi

if [ ! -d "$store_rpms" ]; then
  mkdir -p "$store_rpms"
fi

# 汇总生成2203sp3OS的所有metadata.json
# 读取文件并遍历每一行
while IFS= read -r package; do
    echo "$package"
    sh metadata.sh $package $store_metadata $store_rpms
done < $rpm_list
