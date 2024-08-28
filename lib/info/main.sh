#!/bin/bash

if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <rpm-list>"
    exit 1
fi

rpm_list=$1

# 设置要检查的目录路径
store_metadata="/srv/os-repo/epkg/channel/openEuler-24.09/everything/aarch64/pkg-info/"
store_rpms="/root/tmprpms"

if [ ! -d "$store_metadata" ]; then
  mkdir -p "$store_metadata"
fi

if [ ! -d "$store_rpms" ]; then
  mkdir -p "$store_rpms"
fi

while IFS= read -r package; do
    echo "$package"
    sh metadata.sh $package $store_metadata $store_rpms
done < $rpm_list
