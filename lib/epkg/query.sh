#!/usr/bin/bash


if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <rpm_name/rpm_name*>"
    exit 1
fi

CHANNEL_CONF_PATH="/etc/epkg-confs/channel.json"

query_name=$1
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
    find "$search_dir" -maxdepth 1 -mindepth 1 -type d | while read -r dir; do
        dir_name=$(basename "$dir")
        dir_name=${dir_name%-*}
        dir_name=${dir_name%-*}
        rpm_name=${dir_name#*-}
        if [[ $rpm_name == "$pkg_name" ]]; then
            echo "$dir/package.json"
            return
        fi
    done
    echo ""
}

get_requires() {
    local pkg_name=$1
    local channel_url=$2
    local channel_name=$3
    local channel_index=$4
    local pkg_info_path="$channel_url/pkg-info"

    echo "get requires for $pkg_name, from $pkg_info_path"
    pkg_metadata_file_path="$(find_pkg_metadata_json $pkg_name $pkg_info_path)"
    echo "find_pkg_metadata_json: $pkg_metadata_file_path"
    if [[ ! -f "$pkg_metadata_file_path" ]]; then
        echo "-------Warning: no package.json for $pkg_name"
        return
    fi

    # 遍历pkg_name关联的package.json中的requires字段，递归查询每一层requirement的requires对应的pkg name
    while IFS= read -r entry; do
        key=$(echo "$entry" | jq -r '.key')
        pkgname=$(echo "$entry" | jq -r '.value.pkgname')
        # 忽略unkonwn的requirement
        if [[ $key == "unknown" ]] || [[ $pkgname == "" ]];then
            echo "-------Warning: abnormal requirement [$key]---[$pkgname]"
            continue
        fi
        
        # 如果当前requirement已经被查询过，则跳过
        if [[ -n "${requires_array[$key]+x}" ]]; then
            continue
        else
            requires_array["$key"]="$pkgname $channel_index $channel_name"
            new_pkg_metadata_file_path="$(find_pkg_metadata_json $pkg_name $pkg_info_path)"
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
    local search_dir="$channel_url/pkg-info"
    find "$search_dir" -maxdepth 1 -mindepth 1 -type d | while read -r dir; do
        dir_name=$(basename "$dir")
        dir_name=${dir_name%-*}
        dir_name=${dir_name%-*}
        rpm_name=${dir_name#*-}
        if [[ $rpm_name == $query_name ]]; then
            echo "$rpm_name" >> $packages_file
        fi
    done
    cat $packages_file
}

# 精准查询
accurate_query_requires() {
    local package_name=$query_name
    # 获取所有channel的key，并按大小倒序排序
    channel_indexs=$(printf "%s\n" "${!channel_array[@]}" | sort -nr)
    # 打印关联数组的内容，按照倒序的键顺序
    for channel_index in $channel_indexs; do
        IFS=',' read -r name os_version remote url gpgcheck gpgkey <<< "${channel_array[$channel_index]}"
        channel_url=$url
        channel_name=$name
        get_requires $package_name $channel_url $channel_name $channel_index
    done
    # 打印当前元组的内容
    for key in "${!requires_array[@]}"; do
        echo "$key: ${requires_array[$key]}"
    done
}

# 模糊查询
fuzzy_query_requires() {
    # 获取所有channel的key，并按大小倒序排序
    channel_indexs=$(printf "%s\n" "${!channel_array[@]}" | sort -nr)
    # 打印关联数组的内容，按照倒序的键顺序
    for channel_index in $channel_indexs; do
        IFS=',' read -r name os_version remote url gpgcheck gpgkey <<< "${channel_array[$channel_index]}"
        channel_url=$url
        channel_name=$name
        find_pkg_names $channel_url
        while read -r package_name; do
            get_requires $package_name $channel_url $channel_name $channel_index
        done < "$packages_file"
    done
    # 打印当前元组的内容
    for key in "${!requires_array[@]}"; do
        echo "$key: ${requires_array[$key]}"
    done
}

# step 1 加载本地的epkg channel配置
load_enabled_channel_conf

# case1: 精确查询
accurate_query_requires

# case2: 模糊查询
fuzzy_query_requires