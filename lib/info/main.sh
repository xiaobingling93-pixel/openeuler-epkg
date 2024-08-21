#!/bin/bash

if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <rpm-list>"
    exit 1
fi

rpm_list=$1

# 设置要检查的目录路径
DIRECTORY="$HOME/epkg_metadata/"

# 检查目录是否存在
if [ ! -d "$DIRECTORY" ]; then
  # 目录不存在，创建它
  mkdir -p "$DIRECTORY"
  echo "Directory $DIRECTORY created."
else
  # 目录已经存在
  echo "Directory $DIRECTORY already exists."
fi

# 汇总生成2203sp3OS的所有metadata.json
# 读取文件并遍历每一行
while IFS= read -r package; do
    echo "$package"
    sh metadata.sh $package $DIRECTORY
done < $rpm_list
