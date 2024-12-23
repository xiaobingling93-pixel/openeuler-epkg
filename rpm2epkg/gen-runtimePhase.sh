#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

rpm_package="$1"
output_directory="$2/install"
mkdir -p "$output_directory"
scripts_record=()

generate_to_file() {
    local cur_scripts=$1
    local content=$2
    if [[ -n "$cur_scripts" ]]; then
        if [[ -z "${script_files[$cur_scripts]}" ]]; then
            echo "Error: Invalid script type '$cur_scripts'. Aborting." >&2
            exit 1
        fi
        echo "$content" > "$output_directory/${script_files[$cur_scripts]}"
        chmod +x "$output_directory/${script_files[$cur_scripts]}"
    fi
}

extract_install_scripts() {
    local current_script=""
    local script_content=""
    
    # https://github.com/rpm-software-management/rpm/blob/master/tests/rpmbuild.at
    declare -A script_files=(
        ["pretrans"]="pretrans"
        ["preinstall"]="pre"
        ["postinstall"]="post"
        ["preuninstall"]="preun"
        ["postuninstall"]="postun"
        ["posttrans"]="posttrans"
        ["preuntrans"]="preuntrans"
        ["postuntrans"]="postuntrans"
        ["verify"]="verifyscript"
        ["triggerprein"]="triggerprein"
        ["triggerin"]="triggerin"
        ["triggerun"]="triggerun"
        ["triggerpostun"]="triggerpostun"
        ["filetriggerin"]="filetriggerin"
        ["filetriggerun"]="filetriggerun"
        ["transfiletriggerin"]="transfiletriggerin"
        ["transfiletriggerun"]="transfiletriggerun"
    )

    # example scripts_content:
    # postinstall scriptlet (using /bin/sh):
    # if [ $1 -eq 1 ] && [ -x "/usr/lib/systemd/systemd-update-helper" ]; then
    #    # Initial installation
    #    /usr/lib/systemd/systemd-update-helper install-system-units nginx.service || :
    # fi
    while IFS= read -r line; do
        if [[ $line =~ ^([a-z]+)\ scriptlet ]]; then
            generate_to_file "$current_script" "$script_content"
            current_script=${line%% *}
            scripts_record+=($current_script)
            script_content=""
        else
            script_content+="$line"$'\n'
        fi
    done <<< "$(rpm -qp --nosignature --scripts "$rpm_package")"
    generate_to_file "$current_script" "$script_content"
    echo "Install scriptlets extracted to $output_directory: ${scripts_record[@]}"
}

extract_install_scripts
