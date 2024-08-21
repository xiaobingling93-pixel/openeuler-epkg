#!/bin/bash
json_data=""

if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <rpm-package>"
    exit 1
fi

echo "Start to generate metadata for $1"
rpm_package="$1"
requires_file=$(mktemp)
provides_file=$(mktemp)
dependencies_file=$(mktemp)
json_file=$(mktemp)
declare -A file_requirements
declare -A so_requirements
declare -A bin_requirements
declare -A requirement_rpm_info
declare -A rpm_provides_info


query_rpm_name() {
    local input_item=$1
    if [[ "/bin/sh" == $input_item ]];then
        input_item="bash"
    fi
    # rpm_name_epoch=$(dnf repoquery --whatprovides "$input_item" | awk -F ':' '{print $1}')
    # 查询input_item对应的rpm包名信息，形如：audit-devel-1:3.0.1-11.oe2203sp3.aarch64，需要依次解析各个字段
    full_rpm_name=$(dnf repoquery --whatprovides "$input_item")
    IFS=':' read -r rpm_name_epoch version_release_dist_arch <<< $full_rpm_name
    rpm_name=${rpm_name_epoch%-*}
    epoch=${rpm_name_epoch##*-}
    version=${version_release_dist_arch%-*}
    release_dist_arch=${version_release_dist_arch##*-}
    arch=${version_release_dist_arch##*.}
    IFS='.' read -r release dist arch <<< $release_dist_arch
    rpm_file_name="$rpm_name-$version_release_dist_arch.rpm"
    echo "$rpm_name $rpm_file_name $epoch $version $release_dist_arch $arch"
}

query_requirements() {
    dnf repoquery --requires "$rpm_package" > "$requires_file"
    echo "==============Requirements:"
    cat $requires_file
}

get_provides () {
    rpm_file_name=$(ls | grep "^${rpm_package}.*\.rpm$")
    rpm -qp --provides $rpm_file_name > $provides_file
    echo "===============Provides:"
    cat $provides_file
}

download_input_rpm () {
    dnf download $rpm_package
}

classify_requirements () {
    while read -r requirement; do
        if [[ "$requirement" =~ \.so ]]; then
            so_requirements["$requirement"]=1
        elif [[ "$requirement" =~ / ]]; then
            file_requirements["$requirement"]=1
        else
            bin_requirements["$requirement"]=1
        fi
    done < "$requires_file"
}

update_requirement_checksum () {
    local requirement=$1
    local type=$2

    result=$(query_rpm_name $requirement)
    IFS=' ' read -r rpm_name rpm_file_name epoch version release_dist_arch arch<<< $result
    if [ -n "$rpm_file_name" ];then
        if [[ ! -f "$rpm_file_name" ]]; then
            dnf download $rpm_name
        fi
        sha256=$(sha256sum $rpm_file_name | awk '{print $1}')
        echo "get sha256 for $rpm_name: $sha256"
        requirement_rpm_info[$sha256]+="$rpm_name|$type|$requirement "
    else
        requirement_rpm_info["unknown"]+="$rpm_name|$type|$requirement "
    fi
}

init_rpm_provides_info_info () {
    while read -r provide; do
        if [[ "$provide" =~ \.so ]]; then
            rpm_provides_info["$provide"]="soname"
        elif [[ "$provide" =~ / ]]; then
            rpm_provides_info["$provide"]="file"
        else
            rpm_provides_info["$provide"]="binary"
        fi
    done < "$provides_file"
}

init_requirement_rpm_info () {
    requirement_rpm_info["unknown"]=""

    for requirement in "${!file_requirements[@]}"; do
        update_requirement_checksum $requirement file
    done

    for requirement in "${!so_requirements[@]}"; do
        update_requirement_checksum $requirement soname
    done

    for requirement in "${!bin_requirements[@]}"; do
        update_requirement_checksum $requirement binary
    done
}

convert_requiremennts_to_json () {
    local key="$1"
    local data="$2"
    IFS=' ' read -r -a entries <<< "$data"
    local pkgname=${entries[0]%%|*}
    local files=()
    local sonames=()
    local binaries=()
    for entry in "${entries[@]}"; do
        IFS='|' read -r pkgname category value <<< "$entry"
        case "$category" in
            "file")
                files+=("${value}")
                ;;
            "soname")
                sonames+=("${value}")
                ;;
            "binary")
                binaries+=("${value}")
                ;;
        esac
    done

    # 创建 JSON 对象
    local json=$(jq -n \
        --arg key "$key" \
        --arg pkgname "$pkgname" \
        --argjson files "$(printf '%s\n' "${files[@]}" | jq -R . | jq -s .)" \
        --argjson sonames "$(printf '%s\n' "${sonames[@]}" | jq -R . | jq -s .)" \
        --argjson binaries "$(printf '%s\n' "${binaries[@]}" | jq -R . | jq -s .)" \
        '{
            requires: {
                ($key): {
                    pkgname: $pkgname,
                    files: $files,
                    sonames: $sonames,
                    binaries: $binaries
                }
            }
        }'
    )
    json_data=$json
}

convert_provides_to_json () {
    local files=()
    local sonames=()
    local binaries=()

    for provide in "${!rpm_provides_info[@]}"; do
        type=${rpm_provides_info[$provide]}
        echo "provide: $provide; type: $type"
        case "$type" in
            "file")
                files+=("${provide}")
                ;;
            "soname")
                sonames+=("${provide}")
                ;;
            "binary")
                binaries+=("${provide}")
                ;;
        esac
    done

    # 创建 JSON 对象
    local json=$(jq -n \
        --argjson files "$(printf '%s\n' "${files[@]}" | jq -R . | jq -s .)" \
        --argjson sonames "$(printf '%s\n' "${sonames[@]}" | jq -R . | jq -s .)" \
        --argjson binaries "$(printf '%s\n' "${binaries[@]}" | jq -R . | jq -s .)" \
        '{
            provides: {
                    files: $files,
                    sonames: $sonames,
                    binaries: $binaries
            }
        }'
    )
    json_data=$json
}

convert_package_info_to_json () {
    result=$(query_rpm_name $rpm_package)
    IFS=' ' read -r rpm_name rpm_file_name epoch version release_dist_arch arch<<< $result
    if [ -n "$rpm_file_name" ];then
        if [[ ! -f "$rpm_file_name" ]]; then
            dnf download $rpm_name
        fi
        sha256=$(sha256sum $rpm_file_name | awk '{print $1}')
        echo "get sha256 for $rpm_name: $sha256"
        
    else
        echo "=============Warning: invalid package input: $rpm_package"
        exit 0
    fi
    # 创建 JSON 对象
    local json=$(jq -n \
        --arg name "$rpm_name" \
        --arg epoch "$epoch" \
        --arg version "$version" \
        --arg release "$release_dist_arch" \
        --arg hash "$sha256" \
        --arg arch "$arch" \
        '{
            package: {
                    name: $name,
                    epoch: $epoch,
                    version: $version,
                    release: $release,
                    hash: $sha256,
                    arch: $arch
            }
        }'
    )
    json_data=$json
}

generate_metadata_json () {
    output_json=$(jq -n '{}')

    # 获取并解析rpm包信息
    convert_package_info_to_json
    output_json=$(echo "$output_json" | jq --argjson new_obj "$json_data" '. * $new_obj')
    
    # 获取并解析requires信息
    for key in "${!requirement_rpm_info[@]}"; do
        data=${requirement_rpm_info[$key]}
        convert_requiremennts_to_json "$key" "${requirement_rpm_info[$key]}"
        output_json=$(echo "$output_json" | jq --argjson new_obj "$json_data" '. * $new_obj')
    done

    # 获取并解析provides信息
    convert_provides_to_json
    output_json=$(echo "$output_json" | jq --argjson new_obj "$json_data" '. * $new_obj')

    output_file="output.json"
    echo "$output_json" | jq '.' > "$output_file"
    echo "JSON has been written to $output_file"
}

# step 1 download rpm
download_input_rpm

# step 2 query original requires and provides info of rpm
query_requirements
get_provides
echo "========Query original requires and provides info Done========"

# step 3 
classify_requirements
echo "========Classify original requires info Done========"

# step 4
init_requirement_rpm_info
init_rpm_provides_info_info
echo "========Turn original requires and provides info to array Done========"

# step 5
generate_metadata_json