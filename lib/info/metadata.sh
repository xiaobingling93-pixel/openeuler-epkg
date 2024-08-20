#!/bin/bash

if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <rpm-package>"
    exit 1
fi

rpm_package="$1"
requires_file=$(mktemp)
dependencies_file=$(mktemp)
json_file=$(mktemp)

dnf download $rpm_package
dnf repoquery --requires "$rpm_package" > "$requires_file"
cat $requires_file

declare -A file_deps
declare -A so_deps
declare -A bin_deps

while read -r dep; do
    echo "read dep: $dep"
    if [[ "$dep" =~ \.so ]]; then
        echo "catch soname: $dep"
        so_deps["$dep"]=1
    elif [[ "$dep" =~ / ]]; then
        echo "catch file: $dep"
        file_deps["$dep"]=1
    else
        echo "catch binary: $dep"
        bin_deps["$dep"]=1
    fi
done < "$requires_file"

get_sha256() {
    local rpm_name="$1"
    rpm_file_name=$(ls | grep "^${rpm_name}.*\.rpm$")
    sha256=$(sha256sum $rpm_file_name | awk '{print $1}')
    echo $sha256
}

get_package_info() {
    local rpm_name="$1"
    local sha256=$(get_sha256 "$rpm_name")

    local files=$(rpm -ql "$rpm_name" | jq -R . | jq -s .)
    local sonames=$(rpm -q --requires "$rpm_name" | grep 'so' | jq -R . | jq -s .)
    local binaries=$(rpm -q --filesbypackage "$rpm_name" | grep 'bin' | jq -R . | jq -s .)

    echo "{\"hash\": \"$sha256\", \"pkgname\": \"$rpm_name\", \"files\": $files, \"sonames\": $sonames, \"binaries\": $binaries}"
}

declare -A rpm_info
rpm_info["unknown"]=""

for dep in "${!file_deps[@]}"; do
    rpm_name=$(dnf repoquery --whatprovides "$dep" | awk -F '-' '{print $1}')
    if [ -n "$rpm_name" ];then
        echo "download rpm dep: $rpm_name"
        dnf download $rpm_name
        sha256=$(get_sha256 "$rpm_name")
        echo "get sha256 for $rpm_name: $sha256"
        rpm_info[$sha256]+="$rpm_name|files|$dep "
    else
        rpm_info["unknown"]+="$rpm_name|files|$dep "
    fi
done

for dep in "${!so_deps[@]}"; do
    rpm_name=$(dnf repoquery --whatprovides "$dep" | awk -F '-' '{print $1}')
    if [ -n "$rpm_name" ];then
        echo "download rpm dep: $rpm_name"
        dnf download $rpm_name
        sha256=$(get_sha256 "$rpm_name")
        echo "get sha256 for $rpm_name: $sha256"
        rpm_info[$sha256]+="$rpm_name|sonames|$dep "
    else
        rpm_info["unknown"]+="$rpm_name|sonames|$dep "
    fi
done


for dep in "${!bin_deps[@]}"; do
    rpm_name=$(dnf repoquery --whatprovides "$dep" | awk -F '-' '{print $1}')
    if [ -n "$rpm_name" ];then
        echo "download rpm dep: $rpm_name"
        dnf download $rpm_name
        sha256=$(get_sha256 "$rpm_name")
        echo "get sha256 for $rpm_name: $sha256"
        rpm_info[$sha256]+="$rpm_name|binaries|$dep "
    else
        rpm_info["unknown"]+="$rpm_name|binaries|$dep "
    fi
done


# 初始化 JSON 数据
json="{"

# 提取和分割数据
for key in "${!rpm_info[@]}"; do
    data="${rpm_info[$key]}"
    IFS=' ' read -r -a entries <<< "$data"
    
    # 提取 pkgname
    pkgname=${entries[0]%%|*}
    
    # 存储 files，sonames 和 binaries
    files=()
    sonames=()
    binaries=()
    for entry in "${entries[@]:1}"; do
        IFS='|' read -r category value <<< "$entry"
        case "$category" in
            "files")
                sonames+=("${value}")
                ;;
            "sonames")
                sonames+=("${value}")
                ;;
            "binaries")
                binaries+=("${value}")
                ;;
        esac
    done
    
    # 格式化 sonames 和 binaries
    files_json=$(printf '%s\n' "${files[@]}" | jq -R . | jq -s .)
    sonames_json=$(printf '%s\n' "${sonames[@]}" | jq -R . | jq -s .)
    binaries_json=$(printf '%s\n' "${binaries[@]}" | jq -R . | jq -s .)
    
    # 将数据添加到 JSON 对象中
    json+="\"$key\": {
        \"pkgname\": \"$pkgname\",
        \"files\": $files_json,
        \"sonames\": $sonames_json,
        \"binaries\": $binaries_json
    },"
done

# 关闭 JSON 对象
json+="}"

# 打印 JSON 数据
echo "$json" | jq '.'