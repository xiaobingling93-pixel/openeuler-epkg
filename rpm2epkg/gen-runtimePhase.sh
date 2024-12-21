#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

rpm_package="$1"
output_file="$2/runtimePhase.sh"
> $output_file

gen_phase_func() {
    local phase_name=$1
    local phase_content=$2
    cat << EOF >> $output_file
${phase_name}() {
$phase_content
}

EOF
}

get_phase_content() {
    local scripts_content=$(rpm -qp --nosignature --scripts "$rpm_package")
    local content_nums=$(echo "$scripts_content" | wc -l)
    local preinstall_line=$(echo "$scripts_content" | grep -n "preinstall scriptlet"  | cut -d: -f1)
    local postinstall_line=$(echo "$scripts_content" | grep -n "postinstall scriptlet"  | cut -d: -f1)
    local preuninstall_line=$(echo "$scripts_content" | grep -n "preuninstall scriptlet"  | cut -d: -f1)
    local postuninstall_line=$(echo "$scripts_content" | grep -n "postuninstall scriptlet" | cut -d: -f1 )

    if [ -n "$preinstall_line" ]; then
        local startline=$((preinstall_line+1))
        local endline=
        if [ -n "$postinstall_line" ]; then
            endline=$((postinstall_line-1))
        elif [ -n "$preuninstall_line" ]; then
            endline=$((preuninstall_line-1))
        elif [ -n "$postuninstall_line" ]; then
            endline=$((postuninstall_line-1))
        else
            endline=$content_nums
        fi

        local preinstall_content=$(echo "$scripts_content" | sed -n "${startline},${endline}p" | grep -v '^$' | sed 's/^/\t/')
        gen_phase_func pre_install "$preinstall_content"
    fi

    if [ -n "$postinstall_line" ]; then
        local startline=$((postinstall_line+1))
        local endline=
        if [ -n "$preuninstall_line" ]; then
            endline=$((preuninstall_line-1))
        elif [ -n "$postuninstall_line" ]; then
            endline=$((postuninstall_line-1))
        else
            endline=$content_nums
        fi

        local postinstall_content=$(echo "$scripts_content" | sed -n "${startline},${endline}p" | grep -v '^$' | sed 's/^/\t/')
        gen_phase_func post_install "$postinstall_content"
    fi

    if [ -n "$preuninstall_line" ]; then
        local startline=$((preuninstall_line+1))
        local endline=
        if [ -n "$postuninstall_line" ]; then
            endline=$((postuninstall_line-1))
        else
            endline=$content_nums
        fi

        local preuninstall_content=$(echo "$scripts_content" | sed -n "${startline},${endline}p" | grep -v '^$' | sed 's/^/\t/')
        gen_phase_func pre_uninstall "$preuninstall_content"
    fi

    if [ -n "$postuninstall_line" ]; then
        local startline=$((postuninstall_line+1))
        local endline=$content_nums
        local postuninstall_content=$(echo "$scripts_content" | sed -n "${startline},${endline}p" | grep -v '^$' | sed 's/^/\t/')
        gen_phase_func post_uninstall "$postuninstall_content"
    fi
}

get_phase_content
echo "========runtimePhase.sh has been generated========="
cat "$output_file"
