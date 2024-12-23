#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

rpm_package="$1"
output_directory="$2/runtimePhase"
mkdir -p "$output_directory"

get_phase_content() {
    local scripts_content=$(rpm -qp --nosignature --scripts "$rpm_package")

    declare -A script_files=(
        ["pre"]="pre.sh"
        ["pretrans"]="pretrans.sh"
        ["preinstall"]="preinstall.sh"
        ["preupgrade"]="preupgrade.sh"
        ["preuninstall"]="preuninstall.sh"
        ["post"]="post.sh"
        ["posttrans"]="posttrans.sh"
        ["postinstall"]="postinstall.sh"
        ["postupgrade"]="postupgrade.sh"
        ["postuninstall"]="postuninstall.sh"
    )

    local current_script=""
    local script_content=""

    while IFS= read -r line; do
        if [[ $line =~ ^(postinstall|preuninstall|preinstall|postuninstall|pretrans|posttrans|preupgrade|postupgrade|pre|post)\ scriptlet ]]; then
            if [ -n "$current_script" ]; then
                echo "$script_content" > "$output_directory/${script_files[$current_script]}"
            fi
            current_script=$(echo $line | awk '{print $1}')
            script_content=""
        else
            script_content+="$line"$'\n'
        fi
    done <<< "$scripts_content"

    if [ -n "$current_script" ]; then
        echo "$script_content" > "$output_directory/${script_files[$current_script]}"
    fi
}

get_phase_content
echo "Scripts have been extracted to individual files in $output_directory."
echo "========runtimePhase.sh has been generated========="
