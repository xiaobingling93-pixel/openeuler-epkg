#!/usr/bin/bash

# 6a6cca66c56a8c39c1714e26be632d1b24f766a0b4e003d59205d852a45520b3

CHANNEL_CONF_PATH="/etc/epkg-confs/channel.json"
CHANNEL_CONF_PATH="$HOME/.epkg/channel.json"

packages_file=$(mktemp)
declare -A requires_array
declare -A channel_array

load_channel_conf() {
    json="$(cat $CHANNEL_CONF_PATH)"
    # 使用 jq 解析 JSON 并将其转换为键值对
    # 遍历 "channel" 中的每个元组
    for index in $(echo "$json" | jq -r '.channel | keys[]'); do
        echo "Processing channel[$index]"
        # 解析当前元组的键值对
        while IFS="=" read -r key value; do
            # 将键名加上前缀以区分不同的元组
            full_key="channel_${index}_${key}"
            channel_array[$full_key]=$value
        done < <(echo "$json" | jq -r ".channel[\"$index\"] | to_entries[] | \"\(.key)=\(.value)\"")
        # 打印当前元组的内容
        for key in "${!channel_array[@]}"; do
            if [[ $key == ${index}_* ]]; then
                echo "$key: ${channel_array[$key]}"
            fi
        done
        echo ""
    done
}

load_enabled_channel_conf() {
    json=$(cat $CHANNEL_CONF_PATH)
    enabled_data=$(echo "$json" | jq -c '
        .channel |
        to_entries |
        map(select(.value.enabled == "1")) |
        from_entries
    ')
    
    while IFS= read -r item; do
        key=$(echo "$item" | jq -r '.key')
        name=$(echo "$item" | jq -r '.value.name')
        os_version=$(echo "$item" | jq -r '.value.os_version')
        remote=$(echo "$item" | jq -r '.value.remote')
        url=$(echo "$item" | jq -r '.value.url')
        gpgcheck=$(echo "$item" | jq -r '.value.gpgcheck')
        gpgkey=$(echo "$item" | jq -r '.value.gpgkey')
        channel_array[$key]="$name,$os_version,$remote,$url,$gpgcheck,$gpgkey"
    done < <(echo "$enabled_data" | jq -c 'to_entries[]')
}

find_pkg_metadata_json() {
    local pkg_name=$1
    local search_dir=$2
    local epkg_hash=$3

    if [[ $epkg_hash == "" ]]; then
        find "$search_dir" -maxdepth 1 -mindepth 1 -type d -name "*$pkg_name*"| while read -r dir; do
            # 形如：ebe594c852e852f774472fa73aca86f4ac30c7ea43db9cf9055550d5357c92db-fftw-libs-3.3.8-11.oe2203sp3
            dir_name=$(basename "$dir")
            dir_name=${dir_name%.*}
            dir_name=${dir_name%-*}
            dir_name=${dir_name%-*}
            epkg_name=${dir_name#*-}
            if [[ $epkg_name == "$pkg_name" ]]; then
                echo "$dir/package.json"
                return
            fi
        done
    else
        result=$(find $search_dir -type d -name "$epkg_hash*" | head -n 1)
        if [[ $result != "" ]]; then
            echo "$result/package.json"
            return
        fi
    fi
    echo ""
}

get_requires() {
    local pkg_name=$1
    local channel_url=$2
    local channel_name=$3
    local channel_index=$4
    local pkg_info_path="$channel_url/pkg-info"

    pkg_metadata_file_path="$(find_pkg_metadata_json $pkg_name $pkg_info_path "")"
    if [[ ! -f "$pkg_metadata_file_path" ]]; then
        # echo "-------Warning: no package.json for $pkg_name"
        return
    fi

    # 遍历pkg_name关联的package.json中的requires字段，递归查询每一层requirement的requires对应的pkg name
    while IFS= read -r entry; do
        epkg_hash=$(echo "$entry" | jq -r '.value.pkgname')
        # 如果当前requirement已经被查询过，则跳过
        if [[ -n "${requires_array[$epkg_hash]+x}" ]]; then
            continue
        else
            pkgname=$(echo "$entry" | jq -r '.value.pkgname')
            echo "        $pkgname $epkg_hash"

            if [[ $epkg_hash == "unknown" ]] || [[ $pkgname == "" ]];then
                echo "-------Warning: abnormal requirement [$epkg_hash]---[$pkgname]"
                continue
            fi
            requires_array["$epkg_hash"]="$pkgname   $channel_name"
            new_pkg_metadata_file_path="$(find_pkg_metadata_json $pkg_name $pkg_info_path $epkg_hash)"
            if [[ -f "$new_pkg_metadata_file_path" ]]; then
                get_requires $pkgname $channel_url $channel_name $channel_index
            else
                echo "-------Warning: no package.json for $pkgname"
                continue
            fi
        fi
    done < <(jq -c '.requires | to_entries[]' "$pkg_metadata_file_path")
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
        IFS=',' read -r name os_version remote url gpgcheck gpgkey <<< "${channel_array[$channel_index]}"
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
        IFS=',' read -r name os_version remote url gpgcheck gpgkey <<< "${channel_array[$channel_index]}"
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
    
    # step 1 加载本地的epkg channel配置
    load_enabled_channel_conf
    # 获取所有channel的key，并按大小倒序排序
    channel_indexs=$(printf "%s\n" "${!channel_array[@]}" | sort -nr)

    for channel_index in $channel_indexs; do
        IFS=',' read -r name os_version remote url gpgcheck gpgkey <<< "${channel_array[$channel_index]}"
        channel_url=$url
        channel_name=$name
        pkg_info_path="$channel_url/pkg-info"
        pkg_metadata_file_path="$(find_pkg_metadata_json $query_name $pkg_info_path "")"
        echo "pkg_metadata_file_path: $pkg_metadata_file_path"

        read name hash version release dist <<< $(jq -r '.package | "\(.name) \(.hash) \(.version) \(.release) \(.dist)"' "$pkg_metadata_file_path")
        echo "Hash: $hash"
        echo "Version: $version"
        echo "Release: $release"
        echo "Dist: $dist"
        first_two=${hash:0:2}
        # b976e8f53bddb31373d7ba3ccf9dc20fd2af0e553fbda299261ba4843346e646-CUnit-2.1.3-24.oe2203sp3.epkg
        pkg_store_file_name="$hash-$name-$version-$release.$dist.epkg"
        echo "pkg_store_file_name: $pkg_store_file_name"

        pkg_store_file_path=$channel_url/store/$first_two/$pkg_store_file_name
        tar --use-compress-program=zstd -xvf $pkg_store_file_path ./info/files
        echo "The files list of $name:"
        cat ./info/files
        rm -rf ./info
    done
}

query_requires() {
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