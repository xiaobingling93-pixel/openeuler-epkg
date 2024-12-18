#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

SCRIPT_DIR=$(dirname "$(readlink -f "$0")")
source "$SCRIPT_DIR/../lib/epkg/hash.sh"

if [ "$#" -ne 4 ]; then
    echo "Usage: $0 <rpm-package> $1 <output dir>  $2 <store rpms>"
    exit 1
fi

echo "*************Start to generate metadata for $1*****************"
rpm_package="$1"
output_dir="$2"
output_parent_dir=$2
abnormal_output_dir="$2/wait_for_check"
store_rpms="$3"
json_data=""
full_rpm_names=()
has_unknown_requires=0
epkg_hash_exec=$4

query_rpm_names() {
    local input_item=$rpm_package

    epoch=$(rpm -qp --qf %{epoch} "$input_item" 2>/dev/null)
    if [[ "$epoch" == "(none)" ]]; then
        epoch="0"
    fi
    rpm_names=$(rpm -qp --qf '%{NAME}-'$epoch':%{version}-%{release}.%{arch}' "$input_item" 2>/dev/null)
    # rpm_names=$(dnf repoquery --whatprovides "$input_item_noext" 2>/dev/null)

    IFS=$'\n' read -r -d '' -a full_rpm_names <<< "$rpm_names"
}

cp_input_rpm () {
    # local package=$1
    # dnf download --destdir=$store_rpms $package 2>/dev/null
    cp $rpm_package "$store_rpms"
    echo "$rpm_package $store_rpms"
}

get_package_depends() {
    for depend_package in $(dnf repoquery --requires --resolve $rpm_package --forcearch aarch64 2>/dev/null); do
        IFS=':' read -r depend_rpm_name_epoch depend_version_release_dist_arch <<< $depend_package
        depend_rpm_name=${depend_rpm_name_epoch%-*}
        depend_file_name="$depend_rpm_name-$depend_version_release_dist_arch.rpm"

        if grep -q "^$depend_file_name:" "$HOME/file_hash"; then
            hash=$(grep "^$depend_file_name:" "$HOME/file_hash" | cut -d: -f2-)
            requirement_rpm_info[$hash]+="$depend_rpm_name|;"
            continue
        fi

        dnf download --destdir=$store_rpms $depend_package 2>/dev/null
        # package_file_name="$package-$depend_version_release_dist_arch.rpm"
        package_hash=$(rpm_hash "${store_rpms}/${depend_file_name}" $epkg_hash_exec)
        echo "Downloaded $depend_package and calculated hash: $package_hash"
        requirement_rpm_info[$package_hash]+="$depend_rpm_name|;"
    done
}
# /srv/os-repo/fedora/releases/40/Server/aarch64/os/Packages/h/hunspell-1.7.2-7.fc40.aarch64.rpm
# dnf repoquery --requires --resolve /srv/os-repo/fedora/releases/40/Server/x86_64/os/Packages/h/hunspell-es-BO-2.8-3.fc40.noarch.rpm  --forcearch x86_64 2>/dev/null
convert_requiremennts_to_json () {
    local package_hash="$1"
    local data="$2"
    IFS=';' read -r -a entries <<< "$data"
    local pkgname=${entries[0]%%|*}

    # 创建 JSON 对象
    local json=$(jq -n \
        --arg package_hash "$package_hash" \
        --arg pkgname "$pkgname" \
        '{
            depends: [
                {
                    hash: $package_hash,
                    pkgname: $pkgname,
                }
            ]
        }'
    )
    json_data=$json
}

convert_package_info_to_json () {
    echo "$@"
    local package=$1
    local package_hash=$2
    local package_epoch=$3
    local package_version=$4
    local package_release=$5
    local package_dist=$6
    local package_arch=$7
    # IFS=' ' read -r package package_hash package_epoch package_version package_release package_dist package_arch<<< $result
    # 创建 JSON 对象
    local json=$(jq -n \
        --arg name "$package" \
        --arg hash "$package_hash" \
        --arg epoch "$package_epoch" \
        --arg version "$package_version" \
        --arg release "$package_release" \
        --arg dist "$package_dist" \
        --arg arch "$package_arch" \
        '{
            name: $name,
            hash: $hash,
            epoch: $epoch,
            version: $version,
            release: $release,
            dist: $dist,
            arch: $arch
        }'
    )
    json_data=$json
}

generate_metadata_json () {
    local package_elements=$@
    output_json=$(jq -n '{}')

    # 获取并解析rpm包信息
    convert_package_info_to_json $package_elements
    output_json=$(echo "$output_json" | jq --argjson new_obj "$json_data" '. * $new_obj')

    # depends: [{}]
    # if [[ "$has_unknown_requires" -eq 0 ]];then
    #     unset requirement_rpm_info["unknown"]
    # fi

    get_package_depends
    for key in "${!requirement_rpm_info[@]}"; do
        # data=${requirement_rpm_info[$key]}
        convert_requiremennts_to_json "$key" "${requirement_rpm_info[$key]}"
        output_json=$(echo "$output_json" | jq --argjson new_obj "$json_data" '.depends += $new_obj.depends')
    done

    # requires: []
    requires_json=$(rpm -q --requires $rpm_package 2>/dev/null| jq -R . | jq -s .)
    output_json=$(echo "$output_json" | jq --argjson requires "$requires_json" '. + { "requires": $requires }')

    # provides: []
    provides_json=$(rpm -q --provides $rpm_package 2>/dev/null| jq -R . | jq -s 'map(select(length > 0))')
    output_json=$(echo "$output_json"  | jq --argjson provides "$provides_json" '. + { "provides": $provides }')

    # other rpm info
    recommends=$(rpm -q --recommends $rpm_package 2>/dev/null)
    suggests=$(rpm -q --suggests $rpm_package 2>/dev/null)
    supplements=$(rpm -q --supplements $rpm_package 2>/dev/null)
    enhances=$(rpm -q --enhances $rpm_package 2>/dev/null)
    if [ -n "$recommends" ];then
        recommends_json=$(echo "$recommends" | jq -R . | jq -s .)
        output_json=$(echo "$output_json" | jq --argjson recommends "$recommends_json" '. + { "recommends": $recommends }')
    fi
    if [ -n "$suggests" ];then
        suggests_json=$(echo "$suggests" | jq -R . | jq -s .)
        output_json=$(echo "$output_json" | jq --argjson suggests "$suggests_json" '. + { "suggests": $suggests }')
    fi
    if [ -n "$supplements" ];then
        supplements_json=$(echo "$supplements" | jq -R . | jq -s .)
        output_json=$(echo "$output_json" | jq --argjson supplements "$supplements_json" '. + { "supplements": $supplements }')
    fi
    if [ -n "$enhances" ];then
        enhances_json=$(echo "$enhances" | jq -R . | jq -s .)
        output_json=$(echo "$output_json" | jq --argjson enhances "$enhances_json" '. + { "enhances": $enhances }')
    fi

    output_file="package.json"
    echo "$output_json" | jq '.' > "$output_file"
    echo "$output_json" | jq '.'
}

restore_metadata_json() {
    local package=$1
    local restore_dir=$2
    if [[ "$has_unknown_requires" -eq 1 ]];then
        abnormal_output_dir="$abnormal_output_dir/$package"
        if [ ! -d "$abnormal_output_dir" ]; then
            mkdir -p "$abnormal_output_dir"
        fi
        mv "package.json" $abnormal_output_dir
        echo "---------Get abnormal requires for $package, move json to $abnormal_output_dir"
        echo "$package" >> ./need_check
        return
    fi
    if [ ! -d "$restore_dir" ]; then
        mkdir -p "$restore_dir"
    fi
    mv "package.json" $restore_dir
    echo "********JSON has been moved to $restore_dir**********"
}

clean_tmp_files() {
    rm "$requires_file" "$dependencies_file" "$provides_file"
    unset file_requirements
    unset so_requirements
    unset bin_requirements
    unset requirement_rpm_info
    unset rpm_provides_info    
    unset rpm_hashs
}

process_all_rpms() {
    for element in "${full_rpm_names[@]}"; do
        echo "=======================process $element"
        has_unknown_requires=0
        requires_file=$(mktemp)
        provides_file=$(mktemp)
        dependencies_file=$(mktemp)

        declare -A file_requirements
        declare -A so_requirements
        declare -A bin_requirements
        declare -A requirement_rpm_info
        declare -A rpm_provides_info
        declare -A rpm_hashs

        IFS=':' read -r rpm_name_epoch version_release_dist_arch <<< $element
        package=${rpm_name_epoch%-*}
        package_epoch=${rpm_name_epoch##*-}
        package_version=${version_release_dist_arch%-*}
        release_dist_arch=${version_release_dist_arch##*-}
        package_arch=${release_dist_arch##*.}
        release_dist=${release_dist_arch%.*}
        package_dist=${release_dist##*.}
        package_release=${release_dist%.*}
        package_file_name="$package-$version_release_dist_arch.rpm"
        echo "$package $package_file_name $package_epoch $package_version $package_release $package_dist $package_arch"
        # 暂停执行，等待用户输入
        
        # step 1 download rpm
        cp_input_rpm

        # step 2 check rpm's sha256 and package.json existed or not
        # package_hash=$(sha256sum $store_rpms/$package_file_name | awk '{print $1}')
        package_hash=$(rpm_hash $store_rpms/$package_file_name $epkg_hash_exec)
        echo $package_hash
    
        # store_dir="$output_dir/$package_hash"__"$package"__"$package_version"__"$package_release"."$package_dist"
        store_dir="$output_dir"
        if [[ -f "$store_dir/package.json" ]]; then
            echo "==========$store_dir/package.json already existed"
            rm "$store_dir/package.json"
        fi

        # step 3 query original requires and provides info of rpm
        # query_requirements $package
        # query_provides $package_file_name $package
        echo "========Query original requires and provides info Done========"

        # step 3 
        # classify_requirements
        # echo "========Classify original requires info Done========"

        # step 4
        # init_requirement_rpm_info
        # init_rpm_provides_info_info
        # echo "========Turn original requires and provides info to array Done========"

        # step 5
        package_elements="$package $package_hash $package_epoch $package_version $package_release $package_dist $package_arch"
        generate_metadata_json $package_elements
        echo "========Json has been generated========="

        # step 6
        restore_metadata_json $package $store_dir

        #step 7 clean
        clean_tmp_files
    done
}

# step 1: check rpm exist or not
query_rpm_names
if [[ $status -eq 0 ]]; then
    echo "-----------Found rpms for $rpm_package:"
    for element in "${full_rpm_names[@]}"; do
        echo "$element"
    done
else
    echo "==========Warning: $rpm_package is abnormal"
    exit 0
fi


process_all_rpms
