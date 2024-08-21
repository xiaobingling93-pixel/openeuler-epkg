#!/bin/bash

if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <rpm-list>"
    exit 1
fi

rpm_list=$1
# 汇总生成2203sp3OS的所有metadata.json
# 读取文件并遍历每一行
while IFS= read -r package; do
    echo "$package"
    sh metadata.sh $package
done < $rpm_list
