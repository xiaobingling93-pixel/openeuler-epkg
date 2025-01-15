#!/usr/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# 6a6cca66c56a8c39c1714e26be632d1b24f766a0b4e003d59205d852a45520b3

# 获取当前环境的channel
# CHANNEL_CONF_PATH="/etc/epkg/channel.json"
# 获取环境中的repo的信息，如果没有则
# CHANNEL_CONF_PATH="$HOME/.epkg/channel.json"

packages_file=$(mktemp)
declare -A requires_array
declare -A channel_array


load_enabled_channel_conf() {
    local CHANNEL_CONF_PATH="$CURRENT_PROFILE_DIR/etc/epkg/channel.json"
    local arch=$(uname -m)
    local enabled_data=$($COMMON_PROFILE_LINK/bin/jq -c '[.. | objects | select(.enabled == "1")]' "$CHANNEL_CONF_PATH")

    while IFS= read -r item; do
        key=$(echo "$item" | $COMMON_PROFILE_LINK/bin/jq -r '.key')
        name=$(echo "$item" | $COMMON_PROFILE_LINK/bin/jq -r '.value.name')
        channel=$(echo "$item" | $COMMON_PROFILE_LINK/bin/jq -r '.value.channel')
        url=$(echo "$item" | $COMMON_PROFILE_LINK/bin/jq -r '.value.url')${arch}/
        gpgcheck=$(echo "$item" | $COMMON_PROFILE_LINK/bin/jq -r '.value.gpgcheck')
        gpgkey=$(echo "$item" | $COMMON_PROFILE_LINK/bin/jq -r '.value.gpgkey')
        channel_array[$key]="$name,$channel,$url,$gpgcheck,$gpgkey"
    done < <(echo "$enabled_data" | $COMMON_PROFILE_LINK/bin/jq -c 'to_entries[]')
}

find_pkg_metadata_json() {
    local pkg_name="__"$1"__"
    local repo_url=$2
    local epkg_hash=$3
    # example: .cache/epkg/channel/openEuler-24.09/everything/aarch64/pkg-info
    local search_dir=$EPKG_CHANNEL_CACHE_DIR/${repo_url##*/channel/}

    if [[ $epkg_hash == "" ]]; then
        find "$search_dir" -maxdepth 2 -mindepth 1 -type f -name "*$pkg_name*" | while read -r dir; do
            IFS='__' read -ra parts <<< "$(basename "$dir")"
            if [[ "__${parts[2]}__" == "$pkg_name" ]]; then
                echo "$dir"
                return
            fi
        done
    else
        local result=$(find "$search_dir" -type f -name "${epkg_hash}*" -print -quit)
        [[ -n "$result" ]] && echo "$result" && return
    fi
    echo ""
}

get_sources() {
    local pkg_name=$1
    load_enabled_channel_conf
    local channel_indexs=$(printf "%s\n" "${!channel_array[@]}" | sort -nr)
    for channel_index in $channel_indexs; do
        IFS=',' read -r name channel url gpgcheck gpgkey <<< "${channel_array[$channel_index]}"
        local channel_url=$url
        local pkg_info_path="$channel_url/pkg-info"
        local pkg_metadata_file_path="$(find_pkg_metadata_json $pkg_name $pkg_info_path "")"
        if [[ -f "$pkg_metadata_file_path" ]]; then
            local pkg_source=$($COMMON_PROFILE_LINK/bin/jq -r '.source' "$pkg_metadata_file_path")
            echo "$pkg_source"
            return
        fi
    done
}

get_requires() {
    local pkg_name=$1
    local channel_url=$2
    local channel_name=$3
    local channel_index=$4
    # example: https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.03-LTS/everything/aarch64/pkg-info/
    local pkg_info_path="$channel_url/pkg-info" 
    # example: .epkg/store/0cf5b7wjt0p4pwrhdse4345q75xty8wy__gmp__6.3.0__2.oe2403/info/package.json
    local pkg_metadata_file_path="$(find_pkg_metadata_json $pkg_name $pkg_info_path "")"

    if [[ ! -f "$pkg_metadata_file_path" ]]; then
        echo "-------Warning: no package.json for $pkg_name"
        return
    fi

    local pkg_epkg_name="$(basename ${pkg_metadata_file_path})"
    local pkg_hash=$($COMMON_PROFILE_LINK/bin/jq -r '.hash' "$pkg_metadata_file_path")

    if [[ -z "$pkg_hash" || "$pkg_hash" == "null" ]]; then
        echo "-------Warning: Unable to extract hash for $pkg_name"
        return
    fi

    if [[ -z "${requires_array[$pkg_hash]+x}" ]]; then
        requires_array["$pkg_hash"]="${pkg_name}   ${channel_url}/store/${pkg_epkg_name:0:2}/${pkg_epkg_name%.*}.epkg"
    fi

    # 遍历pkg_name关联的package.json中的requires字段，递归查询每一层requirement的requires对应的pkg name
    while IFS= read -r entry; do
        local epkg_hash=$(echo "$entry" | $COMMON_PROFILE_LINK/bin/jq -r '.value.hash')
        # 如果当前requirement已经被查询过，则跳过
        if [[ -n "${requires_array[$epkg_hash]+x}" ]]; then
            continue
        else
            local pkgname=$(echo "$entry" | $COMMON_PROFILE_LINK/bin/jq -r '.value.pkgname')

            if [[ $epkg_hash == "unknown" ]] || [[ $pkgname == "" ]];then
                echo "-------Warning: abnormal requirement [$epkg_hash]---[$pkgname]"
                continue
            fi
            # requires_array["$epkg_hash"]="${pkgname}   ${channel_url}/store/${pkg_epkg_name:0:2}/${pkg_epkg_name%.*}.epkg"
            local new_pkg_metadata_file_path="$(find_pkg_metadata_json $pkg_name $pkg_info_path $epkg_hash)"
            if [[ -f "$new_pkg_metadata_file_path" ]]; then
                get_requires $pkgname $channel_url $channel_name $channel_index
            else
                echo "-------Warning: no package.json for $pkgname"
                continue
            fi
        fi
    done < <($COMMON_PROFILE_LINK/bin/jq -c '(.depends // {}) | to_entries[]' "$pkg_metadata_file_path")
}

find_pkg_names() {
    local channel_url=$1
    local query_name=$2
    local search_dir="$channel_url/pkg-info"
    find "$search_dir" -maxdepth 1 -mindepth 1 -type d -name "*$query_name" | while read -r dir; do
        dir_name=$(basename "$dir")
        dir_name=${dir_name%.*}
        dir_name=${dir_name%-*}
        dir_name=${dir_name%-*}
        epkg_name=${dir_name#*-}
        if [[ $epkg_name == $query_name ]]; then
            echo "$epkg_name" >> $packages_file
        fi
    done
}


# 精准查询
accurate_query_requires() {
    local package_name=$1
    # step 1 加载本地的epkg channel配置
    load_enabled_channel_conf
    # 获取所有channel的key，并按大小倒序排序
    channel_indexs=$(printf "%s\n" "${!channel_array[@]}" | sort -nr)
    # 打印关联数组的内容，按照倒序的键顺序
    for channel_index in $channel_indexs; do
        IFS=',' read -r name channel url gpgcheck gpgkey <<< "${channel_array[$channel_index]}"
        channel_url=$url
        channel_name=$name
        get_requires $package_name $channel_url $channel_name $channel_index
    done
    show_require_list
}

# 模糊查询
fuzzy_query_requires() {
    local query_name=$1
    # step 1 加载本地的epkg channel配置
    load_enabled_channel_conf

    # 获取所有channel的key，并按大小倒序排序
    local channel_indexs=$(printf "%s\n" "${!channel_array[@]}" | sort -nr)
    # 打印关联数组的内容，按照倒序的键顺序
    for channel_index in $channel_indexs; do
        IFS=',' read -r name channel url gpgcheck gpgkey <<< "${channel_array[$channel_index]}"
        channel_url=$url
        channel_name=$name
        find_pkg_names $channel_url $query_name
        related_pkgs="$(cat $packages_file)"
        echo "Find related pakcges: [$related_pkgs]"
        while read -r package_name; do
            get_requires $package_name $channel_url $channel_name $channel_index
        done < "$packages_file"
    done
    show_require_list
}

show_require_list() {
    echo "All of requires:"
    echo "                                 HASH                                PACKAGE  CHANNEL"
    for key in "${!requires_array[@]}"; do
        echo "$key ${requires_array[$key]}"
    done
}

show_package_file_list() {
    local query_name=$1
    local pkg_info_path=
    local pkg_store_file_path=
    local pkg_metadata_file_path=
    local pkg_store_file_name=
    declare -A epkg_array

    # step 1 加载本地的epkg channel配置
    load_enabled_channel_conf
    # 获取所有channel的key，并按大小倒序排序
    channel_indexs=$(printf "%s\n" "${!channel_array[@]}" | /bin/sort -nr)

    for channel_index in $channel_indexs; do
        IFS=',' read -r name channel url gpgcheck gpgkey <<< "${channel_array[$channel_index]}"
        channel_url=$url
        channel_name=$name
        pkg_info_path="$channel_url/pkg-info"
        pkg_metadata_file_path="$(find_pkg_metadata_json $query_name $pkg_info_path "")"
        echo "pkg_metadata_file_path: $pkg_metadata_file_path"

        read name hash version release dist <<< $($COMMON_PROFILE_LINK/bin/jq -r '. | "\(.name) \(.hash) \(.version) \(.release) \(.dist)"' "$pkg_metadata_file_path")
               # b976e8f53bddb31373d7ba3ccf9dc20fd2af0e553fbda299261ba4843346e646-CUnit-2.1.3-24.oe2203sp3.epkg
        pkg_store_file_name="$hash"__"$name"__"$version"__"$release.$dist.epkg"

        if [[ -n "${epkg_array[$pkg_store_file_name]}" ]]; then
            continue
        else
        	epkg_array[$pkg_store_file_name]=1
        fi
        echo "Hash: $hash"
        echo "Version: $version"
        echo "Release: $release"
        echo "Dist: $dist"
        first_two=${hash:0:2}

        echo "pkg_store_file_name: $pkg_store_file_name"
        echo "$channel_url/store/$first_two/$pkg_store_file_name"
        curl -# -o /tmp/$pkg_store_file_name $channel_url/store/$first_two/$pkg_store_file_name
        #pkg_store_file_path=$channel_url/store/$first_two/$pkg_store_file_name
        pkg_store_file_path=/tmp/$pkg_store_file_name
        echo "The files list of $name:"
        tar --use-compress-program=zstd -xOf $pkg_store_file_path ./info/files | /bin/cat
    done
}

query_requires() {
    # 同步repo仓库
    local query_name=$1
    if echo "$query_name" | grep -q '\*'; then
        fuzzy_query_requires $query_name
    else
        accurate_query_requires $query_name
    fi
}
# # API: 精确查询
# accurate_query_requires $1

# # API: 模糊查询
# fuzzy_query_requires $1

# API: 查询files信息
# show_package_file_list $1
