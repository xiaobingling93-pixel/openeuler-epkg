#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

install_file="$1"
output_directory="$2/install"
mkdir -p "$output_directory"


# 检查参数
if [ $# -ne 1 ]; then
    echo "用法: $0 <package.pkg.tar.zst>"
    exit 1
fi

pkg_file="$1"
output_dir="$2/install"

# 创建输出目录
mkdir -p "$output_dir" || exit 1

# 提取并处理 .INSTALL 文件内容
tar --use-compress-program=unzstd -xOf "$pkg_file" "$install_file" 2>/dev/null | \
awk -v dir="$output_dir" \
    -v pre_install="pre" \
    -v post_install="post" \
    -v pre_upgrade="preup" \
    -v post_upgrade="postup" \
    -v pre_remove="preun" \
    -v post_remove="postun" \
'
BEGIN {
    # 定义函数名到文件名的映射
    map["pre_install"] = pre_install
    map["post_install"] = post_install
    map["pre_upgrade"] = pre_upgrade
    map["post_upgrade"] = post_upgrade
    map["pre_remove"] = pre_un
    map["post_remove"] = postun

    in_function = ""
    depth = 0
}

# 匹配函数声明行（兼容换行和单行格式）
/^(function )?(pre_install|post_install|pre_upgrade|post_upgrade|pre_remove|post_remove)[[:space:]]*\(\)/ {
    fn_name = $1
    sub(/\(.*/, "", fn_name)  # 提取函数名

    if (fn_name in map) {
        in_function = map[fn_name]
        output_file = dir "/" in_function
        printf "" > output_file  # 清空文件
        depth = 0

        # 检查当前行是否有 {
        if (index($0, "{") > 0) {
            depth++
            # 提取第一个 { 后的内容（排除声明行）
            body_part = substr($0, index($0, "{") + 1)
            if (body_part != "") {
                print body_part >> output_file
            }
        }
    }
    next
}

# 仅在目标函数中处理内容
in_function != "" {
    # 统计当前行的大括号数量
    line_depth = gsub(/{/, "&") - gsub(/}/, "&")
    depth += line_depth

    # 写入内容（排除外层括号）
    if (depth > 0) {
        print $0 >> output_file
    }

    # 结束条件：depth 归零
    if (depth <= 0) {
        close(output_file)
        in_function = ""
    }
}

'